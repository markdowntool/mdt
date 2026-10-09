//! Latest observations and bounded history, committed with their clock floor.

use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::error::{Error, Result};

pub(super) struct Observation {
    pub tracked_from: i64,
    pub profile: String,
    pub source: String,
    pub times: Vec<i64>,
    pub subtree_times: Vec<i64>,
}

pub(super) struct Store {
    connection: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| Error::new("io", "Updates database needs a directory"))?;
        let mut directory = fs::DirBuilder::new();
        directory.recursive(true);
        let mut file = OpenOptions::new();
        file.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
            directory.mode(0o700);
            file.mode(0o600);
        }
        directory.create(parent).map_err(Error::io)?;
        // Create privately before SQLite opens it (never truncate existing state).
        file.open(path).map_err(Error::io)?;
        let mut connection = Connection::open(path).map_err(db_error)?;
        connection
            .busy_timeout(Duration::from_millis(250))
            .map_err(db_error)?;
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(db_error)?;
        connection
            .pragma_update(None, "synchronous", "EXTRA")
            .map_err(db_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let version: i64 = transaction
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(db_error)?;
        match version {
            0 => {
                let tables: i64 = transaction
                    .query_row(
                        "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(db_error)?;
                if tables != 0 {
                    return Err(Error::new(
                        "tracking_invalid",
                        "Unversioned updates database contains tables; state was not changed",
                    ));
                }
                transaction.execute_batch("
                    CREATE TABLE clock (id INTEGER PRIMARY KEY CHECK (id = 1), last_us INTEGER NOT NULL);
                    CREATE TABLE documents (
                        canonical_path BLOB PRIMARY KEY NOT NULL,
                        tracked_from_us INTEGER NOT NULL,
                        tree_profile TEXT NOT NULL,
                        source TEXT NOT NULL,
                        node_times TEXT NOT NULL
                    );
                ").map_err(db_error)?;
                super::history_store::migrate(&transaction)?;
            }
            1 => super::history_store::migrate(&transaction)?,
            2 | 3 => {}
            _ => {
                return Err(Error::new(
                    "tracking_invalid",
                    format!(
                        "Unsupported updates database version {version}; state was not changed"
                    ),
                ))
            }
        }
        if version <= 2 {
            // Older observations do not distinguish text edits from removed or
            // rearranged children. Preserve their complete-subtree behavior.
            transaction
                .execute_batch(
                    "
                ALTER TABLE documents ADD COLUMN subtree_times TEXT;
                UPDATE documents SET subtree_times = node_times;
                PRAGMA user_version = 3;
            ",
                )
                .map_err(db_error)?;
        }
        {
            // Additive CLI metadata: old Emm builds can continue observing the
            // unchanged v3 history schema while the CLI is released separately.
            transaction
                .execute_batch(
                    "CREATE TABLE IF NOT EXISTS cli_queries (
                token TEXT PRIMARY KEY NOT NULL,
                canonical_path BLOB NOT NULL,
                query TEXT NOT NULL,
                created_us INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS cli_queries_created ON cli_queries(created_us);",
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)?;
        Ok(Self { connection })
    }

    pub fn begin(&mut self) -> Result<Transaction<'_>> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)
    }
}

pub(super) fn floor(transaction: &Transaction<'_>) -> Result<Option<i64>> {
    let floor = transaction
        .query_row("SELECT last_us FROM clock WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(db_error)?;
    if floor.is_none()
        && transaction
            .query_row("SELECT EXISTS(SELECT 1 FROM documents)", [], |row| {
                row.get::<_, bool>(0)
            })
            .map_err(db_error)?
    {
        return Err(Error::new(
            "tracking_invalid",
            "Updates database has documents but no clock; state was not changed",
        ));
    }
    Ok(floor)
}

pub(super) fn read(transaction: &Transaction<'_>, path: &Path) -> Result<Option<Observation>> {
    let row = transaction.query_row(
        "SELECT tracked_from_us, tree_profile, source, node_times, subtree_times FROM documents WHERE canonical_path = ?1",
        [path.as_os_str().as_encoded_bytes()],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, String>(4)?)),
    ).optional().map_err(db_error)?;
    row.map(|(tracked_from, profile, source, times, subtree_times)| {
        let times = serde_json::from_str(&times).map_err(|e| {
            Error::new(
                "tracking_invalid",
                format!("Invalid stored node times: {e}"),
            )
        })?;
        let subtree_times = serde_json::from_str(&subtree_times).map_err(|e| {
            Error::new(
                "tracking_invalid",
                format!("Invalid stored subtree times: {e}"),
            )
        })?;
        Ok(Observation {
            tracked_from,
            profile,
            source,
            times,
            subtree_times,
        })
    })
    .transpose()
}

pub(super) fn write(
    transaction: &Transaction<'_>,
    path: &Path,
    observation: &Observation,
) -> Result<()> {
    let times =
        serde_json::to_string(&observation.times).map_err(|e| Error::new("io", e.to_string()))?;
    let subtree_times = serde_json::to_string(&observation.subtree_times)
        .map_err(|e| Error::new("io", e.to_string()))?;
    transaction.execute(
        "INSERT INTO documents (canonical_path, tracked_from_us, tree_profile, source, node_times, subtree_times)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT(canonical_path) DO UPDATE SET
         tracked_from_us = excluded.tracked_from_us, tree_profile = excluded.tree_profile,
         source = excluded.source, node_times = excluded.node_times, subtree_times = excluded.subtree_times",
        rusqlite::params![path.as_os_str().as_encoded_bytes(), observation.tracked_from, observation.profile, observation.source, times, subtree_times],
    ).map_err(db_error)?;
    Ok(())
}

pub(super) fn advance_clock(transaction: &Transaction<'_>, as_of: i64) -> Result<()> {
    transaction.execute(
        "INSERT INTO clock (id, last_us) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET last_us = excluded.last_us",
        [as_of],
    ).map_err(db_error)?;
    Ok(())
}

pub(super) fn db_error(error: rusqlite::Error) -> Error {
    let code = match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            "database_busy"
        }
        _ => "tracking_invalid",
    };
    Error::new(code, format!("Updates database: {error}"))
}

/// The default tracker location shared with Emm Notes. EMM_HOME overrides
/// the app-state directory; QM_STATE_DIR is handled by the CLI state resolver.
pub fn database_path(home: &Path, emm_home: Option<&Path>) -> PathBuf {
    emm_home
        .map_or_else(|| home.join(".emm"), Path::to_path_buf)
        .join("notes/updates.sqlite3")
}
