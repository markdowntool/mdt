//! One history store for saved Markdown, Emm observations and pinned queries.
use super::{
    history_store,
    store::{self, Observation, Store},
    tree::{self, Tree},
};
use crate::{
    error::{Error, Result},
    snapshots,
};
use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) struct Retained {
    database: PathBuf,
    path: PathBuf,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: i64,
    pub revision: String,
    pub observed_at: Option<String>,
}

fn map(error: Error) -> Error {
    match error.code {
        "history_unavailable" => Error::new(
            "revision_unavailable",
            "Required retained version is unavailable",
        ),
        "database_busy" => Error::new(
            "locked",
            "History storage is busy; no source write committed",
        ),
        _ => error,
    }
}
impl Retained {
    pub fn new(database: &Path, path: &Path) -> Self {
        Self {
            database: database.into(),
            path: path.into(),
        }
    }
    pub fn observe(&self, source: &str) -> Result<String> {
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        let previous = store::read(&tx, &self.path)?;
        let now = Utc::now()
            .timestamp_micros()
            .max(store::floor(&tx)?.unwrap_or(0));
        let tree = Tree::new(source);
        let (times, subtree_times) = match previous.as_ref() {
            Some(previous) if previous.profile == tree::PROFILE => {
                let old = Tree::new(&previous.source);
                if old.nodes.len() != previous.times.len()
                    || old.nodes.len() != previous.subtree_times.len()
                {
                    return Err(Error::new("io", "Stored observation shape is inconsistent"));
                }
                tree::compare(&old, &tree, &previous.times, &previous.subtree_times, now)
            }
            Some(_) => return Err(Error::new("io", "Unsupported stored observation profile")),
            None => (vec![now; tree.nodes.len()], vec![now; tree.nodes.len()]),
        };
        history_store::record(
            &tx,
            &self.path,
            previous.as_ref().map(|p| p.source.as_str()),
            source,
            now,
        )
        .map_err(map)?;
        store::write(
            &tx,
            &self.path,
            &Observation {
                tracked_from: previous.map_or(now, |p| p.tracked_from),
                profile: tree::PROFILE.into(),
                source: source.into(),
                times,
                subtree_times,
            },
        )?;
        store::advance_clock(&tx, now.saturating_add(1))?;
        history_store::prune(&tx, now)?;
        tx.commit().map_err(store::db_error).map_err(map)?;
        let revision = snapshots::revision(&self.path, source);
        // Retention may reject an oversized version or evict it under capacity.
        // Never issue a token that cannot be resolved, especially before writes.
        self.load(&revision)?;
        Ok(revision)
    }
    pub fn load(&self, revision: &str) -> Result<String> {
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        history_store::prune(&tx, Utc::now().timestamp_micros())?;
        let index = history_store::index(&tx, &self.path)?;
        let entry = index
            .entries
            .iter()
            .find(|e| e.revision == revision)
            .ok_or_else(|| {
                Error::new(
                    "revision_unavailable",
                    "Required revision is no longer retained for this source",
                )
            })?;
        let payload = history_store::fetch(&tx, &self.path, entry).map_err(map)?;
        tx.commit().map_err(store::db_error).map_err(map)?;
        payload.decode(&self.path).map_err(map)
    }
    pub fn history(&self) -> Result<Vec<Entry>> {
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        history_store::prune(&tx, Utc::now().timestamp_micros())?;
        let index = history_store::index(&tx, &self.path)?;
        tx.commit().map_err(store::db_error).map_err(map)?;
        index
            .entries
            .into_iter()
            .map(|e| {
                Ok(Entry {
                    id: e.id,
                    revision: e.revision,
                    observed_at: e.observed_at.map(super::format_time).transpose()?,
                })
            })
            .collect()
    }
    pub fn forget(&self, dry_run: bool) -> Result<usize> {
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        let count = history_store::index(&tx, &self.path)?.entries.len();
        if !dry_run {
            history_store::clear(&tx, &self.path)?;
            let key = self.path.as_os_str().as_encoded_bytes();
            tx.execute("DELETE FROM documents WHERE canonical_path = ?1", [key])
                .map_err(store::db_error)?;
            tx.execute("DELETE FROM cli_queries WHERE canonical_path = ?1", [key])
                .map_err(store::db_error)?;
            tx.commit().map_err(store::db_error).map_err(map)?;
        }
        Ok(count)
    }
    pub fn cursor(&self, query: &impl Serialize, json: bool) -> Result<String> {
        let query = serde_json::to_string(query).map_err(|e| Error::new("io", e.to_string()))?;
        if query.len() > 2 * 1024 * 1024 {
            return Err(Error::new(
                "io",
                "Continuation exceeds local storage bounds",
            ));
        }
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let now = Utc::now().timestamp_micros();
        let token = format!(
            "C{}{:x}",
            if json { "j" } else { "t" },
            Sha256::digest(
                format!(
                    "{}:{now}:{}:{}:{query}",
                    self.path.display(),
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                )
                .as_bytes()
            )
        );
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        tx.execute(
            "INSERT INTO cli_queries(token,canonical_path,query,created_us) VALUES (?1,?2,?3,?4)",
            rusqlite::params![token, self.path.as_os_str().as_encoded_bytes(), query, now],
        )
        .map_err(store::db_error)?;
        // Query records are tiny metadata, never a second source snapshot cache.
        // Bound both bytes (16 MiB) and count, with a 30-day maximum lifetime.
        tx.execute("DELETE FROM cli_queries WHERE created_us < ?1 OR token IN (
            SELECT token FROM (SELECT token, ROW_NUMBER() OVER (ORDER BY created_us DESC, token DESC) AS n,
            SUM(length(query)) OVER (ORDER BY created_us DESC, token DESC) AS bytes FROM cli_queries)
            WHERE n > 4096 OR bytes > 16777216)", [now - 30*24*60*60*1_000_000_i64]).map_err(store::db_error)?;
        tx.commit().map_err(store::db_error).map_err(map)?;
        Ok(token)
    }
    pub fn resume<T: serde::de::DeserializeOwned>(&self, token: &str) -> Result<T> {
        let mut db = Store::open(&self.database).map_err(map)?;
        let tx = db.begin().map_err(map)?;
        let query: Option<String> = tx.query_row("SELECT query FROM cli_queries WHERE token=?1 AND canonical_path=?2 AND created_us>=?3",
            rusqlite::params![token,self.path.as_os_str().as_encoded_bytes(),Utc::now().timestamp_micros()-30*24*60*60*1_000_000_i64], |r| r.get(0)).optional().map_err(store::db_error)?;
        let query = query.ok_or_else(|| {
            Error::new(
                "revision_unavailable",
                "Continuation is no longer retained for this source",
            )
        })?;
        serde_json::from_str(&query).map_err(|_| Error::new("io", "Invalid retained continuation"))
    }
}
