//! Private, account-scoped product history. Never serialize HTTP requests, sessions or provider bodies.
use crate::{Result, model::Snapshot, now, preview::Preview};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResultTrack {
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedResult {
    pub name: String,
    pub tracks: Vec<ResultTrack>,
    pub criteria: Vec<String>,
    pub warnings: Vec<String>,
    pub phase: crate::preview::Phase,
    pub playlist_id: Option<String>,
}

impl SavedResult {
    pub fn capture(preview: &Preview, snapshot: &Snapshot) -> Self {
        Self {
            name: preview.name.clone(),
            tracks: preview
                .track_ids
                .iter()
                .map(|id| {
                    let track = snapshot.tracks.get(id);
                    ResultTrack {
                        id: id.clone(),
                        name: track.map(|t| t.name.clone()).unwrap_or_default(),
                        artists: track.map(|t| t.artists.clone()).unwrap_or_default(),
                    }
                })
                .collect(),
            criteria: preview.criteria.clone(),
            warnings: preview.warnings.clone(),
            phase: preview.phase.clone(),
            playlist_id: preview.playlist_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub schema_version: u8,
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub status: String,
    pub prompt: String,
    pub controls: Value,
    pub library_count: usize,
    pub snapshot_at: Option<i64>,
    pub interpretation: Option<Value>,
    pub initial_result: Option<SavedResult>,
    pub latest_result: Option<SavedResult>,
    pub elapsed_ms: Option<u64>,
    pub error: Option<String>,
}

pub fn init(db: &Connection) -> Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS audit_users(account TEXT PRIMARY KEY, user_id TEXT NOT NULL UNIQUE);
        CREATE TABLE IF NOT EXISTS prompt_runs(account TEXT NOT NULL, id TEXT NOT NULL, created INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(account,id));
        CREATE INDEX IF NOT EXISTS prompt_runs_recent ON prompt_runs(account,created DESC);
        CREATE TABLE IF NOT EXISTS app_events(id INTEGER PRIMARY KEY AUTOINCREMENT, account TEXT NOT NULL, created INTEGER NOT NULL, kind TEXT NOT NULL, run_id TEXT, payload TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS app_events_recent ON app_events(account,created);")?;
    init_daily(db)?;
    // A process restart cannot resume an in-memory worker. Keep its evidence honest.
    db.execute("UPDATE prompt_runs SET payload=json_set(payload,'$.status','interrupted','$.updated_at',?) WHERE json_extract(payload,'$.status')='running'", [now()])?;
    Ok(())
}

fn init_daily(db: &Connection) -> Result<()> {
    let tx = db.unchecked_transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS history_migrations(version INTEGER PRIMARY KEY);
        CREATE INDEX IF NOT EXISTS app_events_expiry ON app_events(created);
        CREATE TABLE IF NOT EXISTS event_daily(
            account TEXT NOT NULL, day INTEGER NOT NULL, kind TEXT NOT NULL,
            outcome TEXT NOT NULL, events INTEGER NOT NULL,
            duration_ms INTEGER NOT NULL, timed_events INTEGER NOT NULL,
            PRIMARY KEY(account,day,kind,outcome));
        CREATE INDEX IF NOT EXISTS event_daily_expiry ON event_daily(day);
        CREATE VIEW IF NOT EXISTS event_rollup_source AS
            SELECT id,account,created-created%86400 AS day,kind,
                CASE WHEN json_extract(payload,'$.status')='cancelled' THEN 'cancelled'
                    WHEN json_extract(payload,'$.status') IN ('failed','interrupted')
                        OR json_extract(payload,'$.http_status')>=400 THEN 'error'
                    WHEN json_extract(payload,'$.status')='completed'
                        OR json_extract(payload,'$.http_status') BETWEEN 200 AND 399 THEN 'ok'
                    ELSE 'observed' END AS outcome,
                CASE WHEN json_type(payload,'$.elapsed_ms')='integer' THEN max(0,json_extract(payload,'$.elapsed_ms'))
                    WHEN json_type(payload,'$.request_ms')='integer' THEN max(0,json_extract(payload,'$.request_ms'))
                    ELSE 0 END AS duration_ms,
                CASE WHEN json_type(payload,'$.elapsed_ms')='integer'
                    OR json_type(payload,'$.request_ms')='integer' THEN 1 ELSE 0 END AS timed
            FROM app_events;
        CREATE TRIGGER IF NOT EXISTS event_daily_insert AFTER INSERT ON app_events BEGIN
            INSERT INTO event_daily(account,day,kind,outcome,events,duration_ms,timed_events)
                SELECT account,day,kind,outcome,1,duration_ms,timed FROM event_rollup_source WHERE id=NEW.id
                ON CONFLICT(account,day,kind,outcome) DO UPDATE SET
                    events=events+excluded.events,
                    duration_ms=duration_ms+excluded.duration_ms,
                    timed_events=timed_events+excluded.timed_events;
        END;")?;
    // Backfill once, in the same transaction as the migration receipt.
    if tx.execute("INSERT OR IGNORE INTO history_migrations VALUES(1)", [])? == 1 {
        tx.execute(
            "INSERT INTO event_daily(account,day,kind,outcome,events,duration_ms,timed_events)
            SELECT e.account,day,kind,outcome,count(*),sum(duration_ms),sum(timed)
            FROM event_rollup_source e JOIN audit_users u ON u.account=e.account
            GROUP BY e.account,day,kind,outcome",
            [],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn ensure_user(db: &Connection, account: &str) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO audit_users VALUES(?,?)",
        params![account, crate::provider::random_secret()],
    )?;
    Ok(())
}

pub fn put(db: &Connection, account: &str, run: &Run) -> Result<()> {
    db.execute("INSERT INTO prompt_runs VALUES(?,?,?,?) ON CONFLICT(account,id) DO UPDATE SET payload=excluded.payload",params![account,run.id,run.created_at,serde_json::to_string(run)?])?;
    Ok(())
}

pub fn get(db: &Connection, account: &str, id: &str) -> Result<Option<Run>> {
    let raw: Option<String> = db
        .query_row(
            "SELECT payload FROM prompt_runs WHERE account=? AND id=?",
            params![account, id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
        .transpose()
}

pub fn list(db: &Connection, account: &str, page: usize) -> Result<Vec<Run>> {
    let mut stmt = db.prepare("SELECT payload FROM prompt_runs WHERE account=? ORDER BY created DESC,rowid DESC LIMIT 26 OFFSET ?")?;
    let rows = stmt.query_map(params![account, page.min(1_000_000) * 25], |r| {
        r.get::<_, String>(0)
    })?;
    rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
}

pub fn event(
    db: &Connection,
    account: &str,
    kind: &str,
    run: Option<&str>,
    detail: Value,
) -> Result<()> {
    // INSERT SELECT prevents a late worker resurrecting analytics after deletion.
    db.execute("INSERT INTO app_events(account,created,kind,run_id,payload) SELECT account,?,?,?,? FROM audit_users WHERE account=?",params![now(),kind,run,serde_json::to_string(&detail)?,account])?;
    Ok(())
}

/// Developer-only filesystem export. Contains personal prompts/results despite pseudonymous IDs.
pub fn export(db: &mut Connection) -> Result<Value> {
    let tx = db.transaction()?;
    let mut users = Vec::new();
    {
        let mut stmt = tx.prepare("SELECT account,user_id FROM audit_users ORDER BY user_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (account, user_id) = row?;
            let mut runs_stmt = tx.prepare(
                "SELECT payload FROM prompt_runs WHERE account=? ORDER BY created,rowid",
            )?;
            let runs = runs_stmt
                .query_map([&account], |r| r.get::<_, String>(0))?
                .map(|r| Ok(serde_json::from_str::<Value>(&r?)?))
                .collect::<Result<Vec<_>>>()?;
            let mut event_stmt = tx.prepare("SELECT id,created,kind,run_id,payload FROM app_events WHERE account=? AND created>=? ORDER BY id")?;
            let events = event_stmt.query_map(params![account,now()-90*86400],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?)))?
                .map(|r| {let (id,at,kind,run,raw)=r?;Ok(json!({"id":id,"at":at,"kind":kind,"run_id":run,"detail":serde_json::from_str::<Value>(&raw)?}))}).collect::<Result<Vec<_>>>()?;
            let mut daily_stmt = tx.prepare("SELECT day,kind,outcome,events,duration_ms,timed_events FROM event_daily WHERE account=? AND day>=? ORDER BY day,kind,outcome")?;
            let daily = daily_stmt.query_map(params![account,(now()/86400-364)*86400],|r|Ok(json!({
                "day_utc":r.get::<_,i64>(0)?,"kind":r.get::<_,String>(1)?,"outcome":r.get::<_,String>(2)?,
                "events":r.get::<_,i64>(3)?,"duration_ms_sum":r.get::<_,i64>(4)?,"timed_events":r.get::<_,i64>(5)?
            })))?.collect::<std::result::Result<Vec<_>,_>>()?;
            users.push(json!({"user_id":user_id,"runs":runs,"events":events,"daily":daily}));
        }
    }
    tx.commit()?;
    Ok(
        json!({"schema_version":2,"exported_at":now(),"app_version":env!("CARGO_PKG_VERSION"),"retention":{"events_days":90,"daily_utc_days":365,"requests":"until account deletion"},"policy":"Private developer review only. Pseudonymous, not anonymous. No model training or third-party analytics. Cloud archival requires the approved private deployment.","users":users}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(id: &str) -> Run {
        Run {
            schema_version: 1,
            id: id.into(),
            created_at: now() - 100 * 86400,
            updated_at: now(),
            status: "running".into(),
            prompt: "weird romantic music".into(),
            controls: json!({"prompt":"weird romantic music"}),
            library_count: 12,
            snapshot_at: Some(now()),
            interpretation: None,
            initial_result: None,
            latest_result: None,
            elapsed_ms: None,
            error: None,
        }
    }

    #[test]
    fn history_survives_restart_and_old_running_jobs_become_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = crate::store::Store::open(dir.path()).unwrap();
            ensure_user(&s.db, "a").unwrap();
            put(&s.db, "a", &run("r")).unwrap();
        }
        let s = crate::store::Store::open(dir.path()).unwrap();
        assert_eq!(get(&s.db, "a", "r").unwrap().unwrap().status, "interrupted");
        assert!(get(&s.db, "b", "r").unwrap().is_none());
    }

    #[test]
    fn export_is_pseudonymous_and_deletion_removes_history_and_late_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = crate::store::Store::open(dir.path()).unwrap();
        for account in ["private-account-a", "private-account-b"] {
            ensure_user(&s.db, account).unwrap();
            put(&s.db, account, &run("r")).unwrap();
            event(&s.db, account, "view", None, json!({"tab":"history"})).unwrap();
        }
        let exported = export(&mut s.db).unwrap();
        assert_eq!(exported["users"].as_array().unwrap().len(), 2);
        assert!(!exported.to_string().contains("private-account"));
        s.disconnect("private-account-a").unwrap();
        event(&s.db, "private-account-a", "late_job", None, json!({})).unwrap();
        assert!(get(&s.db, "private-account-a", "r").unwrap().is_none());
        let exported = export(&mut s.db).unwrap();
        assert_eq!(exported["users"].as_array().unwrap().len(), 1);
        assert_eq!(exported["users"][0]["events"].as_array().unwrap().len(), 1);
        assert_eq!(exported["users"][0]["daily"].as_array().unwrap().len(), 1);
        assert_eq!(
            s.db.query_row(
                "SELECT count(*) FROM event_daily WHERE account='private-account-a'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn daily_counts_preserve_outcomes_and_timing_without_prompt_content() {
        let mut db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        ensure_user(&db, "a").unwrap();
        for _ in 0..100 {
            event(
                &db,
                "a",
                "build_submit",
                None,
                json!({"http_status":200,"request_ms":12}),
            )
            .unwrap();
        }
        event(
            &db,
            "a",
            "build_submit",
            None,
            json!({"http_status":400,"request_ms":8}),
        )
        .unwrap();
        event(
            &db,
            "a",
            "job_finished",
            None,
            json!({"status":"cancelled","elapsed_ms":200}),
        )
        .unwrap();
        event(&db, "a", "view", None, json!({"tab":"history"})).unwrap();
        let data = export(&mut db).unwrap();
        let daily = data["users"][0]["daily"].as_array().unwrap();
        assert_eq!(daily.len(), 4);
        let success = daily
            .iter()
            .find(|d| d["kind"] == "build_submit" && d["outcome"] == "ok")
            .unwrap();
        assert_eq!(success["events"], 100);
        assert_eq!(success["duration_ms_sum"], 1200);
        assert_eq!(success["timed_events"], 100);
        assert!(daily.iter().any(|d| d["outcome"] == "cancelled"));
        assert_eq!(
            daily.iter().find(|d| d["kind"] == "view").unwrap()["timed_events"],
            0
        );
        assert!(!serde_json::to_string(daily).unwrap().contains("tab"));
    }

    #[test]
    fn existing_events_are_backfilled_once() {
        let db = Connection::open_in_memory().unwrap();
        init(&db).unwrap();
        ensure_user(&db, "a").unwrap();
        db.execute_batch("DROP TRIGGER event_daily_insert; DELETE FROM history_migrations;")
            .unwrap();
        event(&db, "a", "view", None, json!({})).unwrap();
        init(&db).unwrap();
        init(&db).unwrap();
        event(&db, "a", "view", None, json!({})).unwrap();
        assert_eq!(
            db.query_row("SELECT sum(events) FROM event_daily", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn summaries_outlive_raw_events_but_expire_after_one_year() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = crate::store::Store::open(dir.path()).unwrap();
        ensure_user(&s.db, "a").unwrap();
        put(&s.db, "a", &run("retained")).unwrap();
        for days in [100, 366] {
            s.db.execute(
                "INSERT INTO app_events(account,created,kind,payload) VALUES('a',?,'view','{}')",
                [now() - days * 86400],
            )
            .unwrap();
        }
        s.purge_expired().unwrap();
        let data = export(&mut s.db).unwrap();
        assert!(data["users"][0]["events"].as_array().unwrap().is_empty());
        assert_eq!(data["users"][0]["daily"].as_array().unwrap().len(), 1);
        assert_eq!(data["users"][0]["runs"].as_array().unwrap().len(), 1);
        assert_eq!(
            s.db.query_row("SELECT count(*) FROM event_daily", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn event_retention_and_paginated_history_are_separate() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = crate::store::Store::open(dir.path()).unwrap();
        ensure_user(&s.db, "a").unwrap();
        for i in 0..27 {
            put(&s.db, "a", &run(&format!("r{i}"))).unwrap();
        }
        event(&s.db, "a", "old", None, json!({})).unwrap();
        s.db.execute("UPDATE app_events SET created=?", [now() - 91 * 86400])
            .unwrap();
        s.purge_expired().unwrap();
        assert_eq!(list(&s.db, "a", 0).unwrap().len(), 26);
        assert_eq!(list(&s.db, "a", 1).unwrap().len(), 2);
        assert!(list(&s.db, "b", 0).unwrap().is_empty());
        assert!(
            export(&mut s.db).unwrap()["users"][0]["events"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
