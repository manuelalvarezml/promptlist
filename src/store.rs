use crate::{Result, message, now};
use fs2::FileExt;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

pub struct Store {
    pub(crate) db: Connection,
    _lock: Option<File>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        if dir.is_symlink() {
            return Err(message(
                "Refusing a symlink for the private data directory.",
            ));
        }
        fs::create_dir_all(dir).map_err(|_| message("Cannot create private data directory."))?;
        #[cfg(unix)]
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|_| message("Cannot protect data directory."))?;
        let lock = private_file(&dir.join("app.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| message("Promptlist is already using this data directory."))?;
        let path = dir.join("catalog.sqlite");
        private_file(&path)?;
        let db = Connection::open(path)?;
        let mut store = Self {
            db,
            _lock: Some(lock),
        };
        store.init()?;
        crate::history::init(&store.db)?;
        store.clear_legacy_features()?;
        store.purge_expired()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<()> {
        self.db.execute_batch("PRAGMA secure_delete=ON; PRAGMA journal_mode=DELETE;
            CREATE TABLE IF NOT EXISTS records(account TEXT NOT NULL, kind TEXT NOT NULL, payload TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY(account,kind));
            CREATE TABLE IF NOT EXISTS tag_cache(account TEXT NOT NULL, identity TEXT NOT NULL, payload TEXT NOT NULL, expires INTEGER NOT NULL, PRIMARY KEY(account,identity));")?;
        Ok(())
    }

    // Older coverage rows lacked provider freshness provenance. Keep measurements,
    // but never reuse or persist those feature values as a cache.
    fn clear_legacy_features(&mut self) -> Result<()> {
        let records: Vec<(String, String)> = {
            let mut statement = self
                .db
                .prepare("SELECT account,payload FROM records WHERE kind='coverage'")?;
            statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?
        };
        for (account, raw) in records {
            let Ok(mut coverage) = serde_json::from_str::<crate::model::Coverage>(&raw) else {
                continue;
            };
            for evidence in coverage.tracks.values_mut() {
                evidence.feature_count = evidence.feature_count.max(evidence.features.len());
                if evidence.features_expires_at <= now() {
                    evidence.features.clear();
                }
            }
            // Do not renew the baseline TTL during migration.
            self.db.execute(
                "UPDATE records SET payload=? WHERE account=? AND kind='coverage'",
                params![serde_json::to_string(&coverage)?, account],
            )?;
        }
        Ok(())
    }

    pub fn get<T: DeserializeOwned>(&self, account: &str, kind: &str) -> Result<Option<T>> {
        let raw: Option<String> = self
            .db
            .query_row(
                "SELECT payload FROM records WHERE account=? AND kind=? AND (kind='snapshot' OR updated>=?)",
                params![account, kind, now() - 86400],
                |r| r.get(0),
            )
            .optional()?;
        raw.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }

    pub fn put<T: Serialize>(&mut self, account: &str, kind: &str, data: &T) -> Result<()> {
        let raw = serde_json::to_string(data)?;
        if raw.len() > 50_000_000 {
            return Err(message("Local snapshot exceeds the 50 MB safety limit."));
        }
        self.db.execute("INSERT INTO records VALUES(?,?,?,?) ON CONFLICT(account,kind) DO UPDATE SET payload=excluded.payload,updated=excluded.updated", params![account,kind,raw,now()])?;
        Ok(())
    }

    pub fn promote<T: Serialize>(&mut self, account: &str, snapshot: &T) -> Result<()> {
        let raw = serde_json::to_string(snapshot)?;
        if raw.len() > 50_000_000 {
            return Err(message("Local snapshot exceeds the 50 MB safety limit."));
        }
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO records VALUES(?,'snapshot',?,?) ON CONFLICT(account,kind) DO UPDATE SET payload=excluded.payload,updated=excluded.updated",params![account,raw,now()])?;
        tx.execute(
            "DELETE FROM records WHERE account=? AND kind IN ('draft','coverage')",
            [account],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, account: &str, kind: &str) -> Result<()> {
        self.db.execute(
            "DELETE FROM records WHERE account=? AND kind=?",
            params![account, kind],
        )?;
        Ok(())
    }
    pub fn disconnect(&mut self, account: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM records WHERE account=?", [account])?;
        tx.execute("DELETE FROM tag_cache WHERE account=?", [account])?;
        tx.execute("DELETE FROM prompt_runs WHERE account=?", [account])?;
        tx.execute("DELETE FROM app_events WHERE account=?", [account])?;
        tx.execute("DELETE FROM event_daily WHERE account=?", [account])?;
        tx.execute("DELETE FROM audit_users WHERE account=?", [account])?;
        tx.commit()?;
        Ok(())
    }
    pub fn tags(
        &self,
        account: &str,
    ) -> Result<std::collections::BTreeMap<String, crate::model::CachedTags>> {
        let mut stmt = self
            .db
            .prepare("SELECT identity,payload FROM tag_cache WHERE account=? AND expires>?")?;
        let rows = stmt.query_map(params![account, now()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut output = std::collections::BTreeMap::new();
        for row in rows {
            let (key, raw) = row?;
            output.insert(key, serde_json::from_str(&raw)?);
        }
        Ok(output)
    }
    pub fn put_tags(
        &mut self,
        account: &str,
        key: &str,
        value: &crate::model::CachedTags,
    ) -> Result<()> {
        if value.expires_at <= now() {
            return Ok(());
        }
        self.db.execute("INSERT INTO tag_cache VALUES(?,?,?,?) ON CONFLICT(account,identity) DO UPDATE SET payload=excluded.payload,expires=excluded.expires",params![account,key,serde_json::to_string(value)?,value.expires_at])?;
        Ok(())
    }
    pub fn purge_expired(&mut self) -> Result<()> {
        self.db.execute(
            "DELETE FROM app_events WHERE created<?",
            [now() - 90 * 86400],
        )?;
        self.db.execute(
            "DELETE FROM event_daily WHERE day<?",
            [(now() / 86400 - 364) * 86400],
        )?;
        self.db.execute(
            "DELETE FROM records WHERE kind!='snapshot' AND updated < ?",
            [now() - 86400],
        )?;
        self.db
            .execute("DELETE FROM tag_cache WHERE expires<=?", [now()])?;
        Ok(())
    }
}

fn private_file(path: &Path) -> Result<File> {
    if path.is_symlink() {
        return Err(message("Refusing a symlink in the private data directory."));
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .map_err(|_| message("Cannot open private data file."))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| message("Cannot protect private data file."))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_draft_cannot_replace_active_snapshot() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path()).unwrap();
        s.promote("a", &vec![1, 2]).unwrap();
        s.put("a", "draft", &vec![3]).unwrap();
        assert_eq!(
            s.get::<Vec<i32>>("a", "snapshot").unwrap(),
            Some(vec![1, 2])
        );
    }
    #[test]
    fn isolates_accounts_and_deletes_on_disconnect() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path()).unwrap();
        s.put("a", "snapshot", &1).unwrap();
        s.put("b", "snapshot", &2).unwrap();
        s.disconnect("a").unwrap();
        assert_eq!(s.get::<i32>("a", "snapshot").unwrap(), None);
        assert_eq!(s.get::<i32>("b", "snapshot").unwrap(), Some(2));
    }
    #[test]
    fn promotion_clears_stale_evidence_and_draft() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path()).unwrap();
        s.put("a", "draft", &1).unwrap();
        s.put("a", "coverage", &1).unwrap();
        s.promote("a", &2).unwrap();
        assert_eq!(s.get::<i32>("a", "draft").unwrap(), None);
        assert_eq!(s.get::<i32>("a", "coverage").unwrap(), None);
    }
    #[test]
    fn prevents_two_processes_using_one_store() {
        let d = tempfile::tempdir().unwrap();
        let _s = Store::open(d.path()).unwrap();
        assert!(Store::open(d.path()).is_err());
    }

    #[test]
    fn legacy_numeric_cache_is_removed_without_erasing_counts_or_renewing_ttl() {
        let d = tempfile::tempdir().unwrap();
        let mut s = Store::open(d.path()).unwrap();
        let mut coverage = crate::model::Coverage::default();
        coverage.tracks.insert(
            "fixture".into(),
            crate::model::Evidence {
                features: std::collections::BTreeMap::from([("energy".into(), 0.7)]),
                ..Default::default()
            },
        );
        s.put("a", "coverage", &coverage).unwrap();
        let at = now() - 100;
        s.db.execute("UPDATE records SET updated=?", [at]).unwrap();
        drop(s);
        let s = Store::open(d.path()).unwrap();
        let loaded: crate::model::Coverage = s.get("a", "coverage").unwrap().unwrap();
        assert!(loaded.tracks["fixture"].features.is_empty());
        assert_eq!(loaded.tracks["fixture"].feature_count, 1);
        assert_eq!(
            s.db.query_row("SELECT updated FROM records", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            at
        );
    }
    #[test]
    fn library_survives_expiry_and_reopen_but_evidence_and_account_delete_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut s = Store::open(dir.path()).unwrap();
            s.put("a", "snapshot", &crate::model::Snapshot::default())
                .unwrap();
            s.put("a", "preview", &42).unwrap();
            s.db.execute("UPDATE records SET updated=?", [now() - 86401])
                .unwrap();
            s.put_tags(
                "a",
                "artist",
                &crate::model::CachedTags {
                    tags: vec!["metal".into()],
                    expires_at: now() + 86400,
                    checked_at: now(),
                },
            )
            .unwrap();
            s.put_tags(
                "a",
                "no-store",
                &crate::model::CachedTags {
                    tags: vec!["rock".into()],
                    expires_at: 0,
                    checked_at: now(),
                },
            )
            .unwrap();
        }
        let mut s = Store::open(dir.path()).unwrap();
        assert!(
            s.get::<crate::model::Snapshot>("a", "snapshot")
                .unwrap()
                .is_some()
        );
        assert!(s.get::<i32>("a", "preview").unwrap().is_none());
        assert_eq!(s.tags("a").unwrap().len(), 1);
        assert!(s.tags("b").unwrap().is_empty());
        s.disconnect("a").unwrap();
        assert!(s.tags("a").unwrap().is_empty());
        assert!(
            s.get::<crate::model::Snapshot>("a", "snapshot")
                .unwrap()
                .is_none()
        );
    }
}
