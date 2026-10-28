//! Private, account-scoped product history. Never serialize requests, sessions or provider bodies.
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
    // A process restart cannot resume an in-memory worker. Keep its evidence honest.
    db.execute("UPDATE prompt_runs SET payload=json_set(payload,'$.status','interrupted','$.updated_at',?) WHERE json_extract(payload,'$.status')='running'", [now()])?;
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
            users.push(json!({"user_id":user_id,"runs":runs,"events":events}));
        }
    }
    tx.commit()?;
    Ok(
        json!({"schema_version":1,"exported_at":now(),"app_version":env!("CARGO_PKG_VERSION"),"policy":"Private developer review only. Not anonymous. No model training or external upload authorized.","users":users}),
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
