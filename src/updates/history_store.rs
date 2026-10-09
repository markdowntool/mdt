//! Bounded observed versions. Events are chronological; payloads are shared.
//!
//! A revision identifies bytes, not an event: observing A, B, A records three
//! events. Links name only the immediately preceding known version. Retention
//! can remove that predecessor; it must never turn the survivor into an edit
//! from some other retained version.

use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
};

use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use rusqlite::{OptionalExtension, Transaction};
use serde::Serialize;

use super::store::db_error;
use crate::{
    error::{Error, Result},
    snapshots,
};

const DAY_US: i64 = 24 * 60 * 60 * 1_000_000;
// A small compressed blob must not authorize an unbounded allocation on read.
// Larger live notes can still use the tracker; only their history is skipped.
const MAX_SOURCE_BYTES: usize = 256 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct Limits {
    pub age_us: i64,
    pub per_document: usize,
    pub total: usize,
    pub bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            age_us: 30 * DAY_US,
            per_document: 2_048,
            total: 16_384,
            bytes: MAX_PAYLOAD_BYTES,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(super) struct Entry {
    pub id: i64,
    pub observed_at: Option<i64>,
    pub revision: String,
    pub previous_id: Option<i64>,
}

#[derive(Debug)]
pub(super) struct Index {
    pub entries: Vec<Entry>,
    pub pruned: bool,
}

/// Called inside the store's schema transaction, for both a new and a v1 DB.
/// Existing latest sources are seeded lazily by `record`, never assigned an
/// invented observation date during migration.
pub(super) fn migrate(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute_batch(
            "CREATE TABLE history_documents (
                canonical_path BLOB PRIMARY KEY NOT NULL,
                last_revision TEXT,
                last_event_id INTEGER,
                pruned INTEGER NOT NULL DEFAULT 0 CHECK (pruned IN (0, 1))
            );
            CREATE TABLE history_payloads (
                revision TEXT PRIMARY KEY NOT NULL,
                source_bytes INTEGER NOT NULL CHECK (source_bytes >= 0),
                compressed_source BLOB NOT NULL
            );
            CREATE TABLE history_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                canonical_path BLOB NOT NULL,
                observed_at_us INTEGER,
                revision TEXT NOT NULL,
                previous_id INTEGER CHECK (previous_id IS NULL OR previous_id < id)
            );
            CREATE INDEX history_events_document ON history_events (canonical_path, id);
            CREATE INDEX history_events_payload ON history_events (revision);
            PRAGMA user_version = 2;",
        )
        .map_err(db_error)
}

struct Head {
    revision: Option<String>,
    event: Option<i64>,
}

pub(super) fn record(
    transaction: &Transaction<'_>,
    path: &Path,
    previous_source: Option<&str>,
    source: &str,
    observed_at: i64,
) -> Result<()> {
    record_with_limits(
        transaction,
        path,
        previous_source,
        source,
        observed_at,
        Limits::default(),
    )
}

fn record_with_limits(
    transaction: &Transaction<'_>,
    path: &Path,
    previous_source: Option<&str>,
    source: &str,
    observed_at: i64,
    limits: Limits,
) -> Result<()> {
    let key = path.as_os_str().as_encoded_bytes();
    let head = transaction
        .query_row(
            "SELECT last_revision, last_event_id FROM history_documents WHERE canonical_path = ?1",
            [key],
            |row| {
                Ok(Head {
                    revision: row.get(0)?,
                    event: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(db_error)?;
    let revision = snapshots::revision(path, source);
    let changed = previous_source.is_some_and(|previous| previous != source);
    if !changed
        && head
            .as_ref()
            .is_some_and(|head| head.revision.as_deref() == Some(&revision))
    {
        return Ok(());
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO history_documents (canonical_path) VALUES (?1)",
            [key],
        )
        .map_err(db_error)?;

    let mut predecessor = None;
    if let Some(previous) = previous_source.filter(|_| changed) {
        let previous_revision = snapshots::revision(path, previous);
        if let Some(head) = &head {
            if head.revision.as_deref() == Some(&previous_revision) {
                if let Some(id) = head.event {
                    let retained: bool = transaction
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM history_events
                             WHERE id = ?1 AND canonical_path = ?2 AND revision = ?3)",
                            rusqlite::params![id, key, previous_revision],
                            |row| row.get(0),
                        )
                        .map_err(db_error)?;
                    predecessor = retained.then_some(id);
                }
            }
        }
        if predecessor.is_none() {
            // This source is the tracker's immediately prior copy. Its date
            // is unavailable after migration, a gap or eviction, but its bytes
            // are sufficient for an honest before image of this transition.
            if retain_payload(transaction, path, previous, limits)? {
                predecessor = Some(insert_event(
                    transaction,
                    path,
                    &previous_revision,
                    None,
                    None,
                )?);
            } else {
                mark_partial(transaction, key)?;
            }
            if head.is_some() {
                mark_partial(transaction, key)?;
            }
        }
    } else if head.is_some() {
        // A disconnected tracker has no trustworthy predecessor. Keep the
        // older history, but do not silently join its head to this baseline.
        mark_partial(transaction, key)?;
    }

    let event = if retain_payload(transaction, path, source, limits)? {
        Some(insert_event(
            transaction,
            path,
            &revision,
            Some(observed_at),
            predecessor,
        )?)
    } else {
        mark_partial(transaction, key)?;
        None
    };
    transaction
        .execute(
            "UPDATE history_documents SET last_revision = ?2, last_event_id = ?3
             WHERE canonical_path = ?1",
            rusqlite::params![key, revision, event],
        )
        .map_err(db_error)?;
    Ok(())
}

fn retain_payload(
    transaction: &Transaction<'_>,
    path: &Path,
    source: &str,
    limits: Limits,
) -> Result<bool> {
    if source.len() > MAX_SOURCE_BYTES {
        return Ok(false);
    }
    let revision = snapshots::revision(path, source);
    let existing = transaction
        .query_row(
            "SELECT source_bytes, length(compressed_source) FROM history_payloads WHERE revision = ?1",
            [&revision],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    if let Some((source_bytes, compressed_bytes)) = existing {
        if source_bytes != source.len() as i64 || compressed_bytes < 0 {
            return Err(invalid("Retained history payload metadata is inconsistent"));
        }
        // Never rewrite an existing payload, even when its contents are bad.
        // `load` verifies the stream and hash and reports that corruption.
        return Ok(compressed_bytes as u64 <= limits.bytes.min(MAX_PAYLOAD_BYTES));
    }
    let referenced: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM history_events WHERE revision = ?1)",
            [&revision],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if referenced {
        return Err(invalid(
            "A retained history payload is missing; it was not replaced",
        ));
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(source.as_bytes()).map_err(Error::io)?;
    let compressed = encoder.finish().map_err(Error::io)?;
    if compressed.len() as u64 > limits.bytes.min(MAX_PAYLOAD_BYTES) {
        return Ok(false);
    }
    transaction
        .execute(
            "INSERT INTO history_payloads (revision, source_bytes, compressed_source)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![revision, source.len() as i64, compressed],
        )
        .map_err(db_error)?;
    Ok(true)
}

fn insert_event(
    transaction: &Transaction<'_>,
    path: &Path,
    revision: &str,
    observed_at: Option<i64>,
    previous_id: Option<i64>,
) -> Result<i64> {
    transaction
        .execute(
            "INSERT INTO history_events (canonical_path, observed_at_us, revision, previous_id)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                path.as_os_str().as_encoded_bytes(),
                observed_at,
                revision,
                previous_id
            ],
        )
        .map_err(db_error)?;
    Ok(transaction.last_insert_rowid())
}

fn mark_partial(transaction: &Transaction<'_>, key: &[u8]) -> Result<()> {
    transaction
        .execute(
            "UPDATE history_documents SET pruned = 1 WHERE canonical_path = ?1",
            [key],
        )
        .map_err(db_error)?;
    Ok(())
}

pub(super) fn index(transaction: &Transaction<'_>, path: &Path) -> Result<Index> {
    let key = path.as_os_str().as_encoded_bytes();
    let pruned = transaction
        .query_row(
            "SELECT pruned FROM history_documents WHERE canonical_path = ?1",
            [key],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(db_error)?;
    let mut statement = transaction
        .prepare(
            "SELECT id, observed_at_us, revision, previous_id FROM history_events
             WHERE canonical_path = ?1 ORDER BY id DESC",
        )
        .map_err(db_error)?;
    let entries = statement
        .query_map([key], |row| {
            Ok(Entry {
                id: row.get(0)?,
                observed_at: row.get(1)?,
                revision: row.get(2)?,
                previous_id: row.get(3)?,
            })
        })
        .map_err(db_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)?;
    if (!entries.is_empty() && pruned.is_none())
        || entries.iter().any(|entry| {
            entry.id <= 0
                || !snapshots::is_revision(&entry.revision)
                || entry
                    .previous_id
                    .is_some_and(|id| id <= 0 || id >= entry.id)
        })
    {
        return Err(invalid("Retained history event metadata is inconsistent"));
    }
    Ok(Index {
        entries,
        pruned: pruned.unwrap_or(false),
    })
}

/// Owned compressed bytes can be decoded after releasing SQLite's locks.
pub(super) struct Payload {
    revision: String,
    source_bytes: usize,
    compressed: Vec<u8>,
}

pub(super) fn fetch(transaction: &Transaction<'_>, path: &Path, entry: &Entry) -> Result<Payload> {
    let stored = transaction
        .query_row(
            "SELECT revision FROM history_events WHERE id = ?1 AND canonical_path = ?2",
            rusqlite::params![entry.id, path.as_os_str().as_encoded_bytes()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(unavailable)?;
    if stored != entry.revision || !snapshots::is_revision(&stored) {
        return Err(invalid(
            "History entry does not match its retained revision",
        ));
    }
    // Read lengths before the blob so corrupt metadata cannot request an
    // unbounded allocation from SQLite or from the decompressor.
    let sizes = transaction
        .query_row(
            "SELECT source_bytes, length(compressed_source) FROM history_payloads WHERE revision = ?1",
            [&stored],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(unavailable)?;
    if sizes.0 < 0
        || sizes.0 as u64 > MAX_SOURCE_BYTES as u64
        || sizes.1 < 0
        || sizes.1 as u64 > MAX_PAYLOAD_BYTES
    {
        return Err(invalid(
            "Retained history payload exceeds its validation bounds",
        ));
    }
    let compressed: Vec<u8> = transaction
        .query_row(
            "SELECT compressed_source FROM history_payloads WHERE revision = ?1",
            [&stored],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    Ok(Payload {
        revision: stored,
        source_bytes: sizes.0 as usize,
        compressed,
    })
}

impl Payload {
    pub(super) fn decode(self, path: &Path) -> Result<String> {
        let mut decoder = ZlibDecoder::new(self.compressed.as_slice());
        let mut bytes = Vec::new();
        decoder
            .by_ref()
            .take(self.source_bytes as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid("Retained history payload is not a valid zlib stream"))?;
        if bytes.len() != self.source_bytes || decoder.total_in() != self.compressed.len() as u64 {
            return Err(invalid("Retained history payload length is inconsistent"));
        }
        let source = String::from_utf8(bytes)
            .map_err(|_| invalid("Retained history source is not valid UTF-8"))?;
        if snapshots::revision(path, &source) != self.revision {
            return Err(invalid(
                "Retained history source does not match its revision",
            ));
        }
        Ok(source)
    }
}

#[cfg(test)]
fn load(transaction: &Transaction<'_>, path: &Path, entry: &Entry) -> Result<String> {
    fetch(transaction, path, entry)?.decode(path)
}

pub(super) fn clear(transaction: &Transaction<'_>, path: &Path) -> Result<usize> {
    let key = path.as_os_str().as_encoded_bytes();
    let removed = transaction
        .execute(
            "DELETE FROM history_events WHERE canonical_path = ?1",
            [key],
        )
        .map_err(db_error)?;
    transaction
        .execute(
            "DELETE FROM history_documents WHERE canonical_path = ?1",
            [key],
        )
        .map_err(db_error)?;
    collect_payloads(transaction)?;
    Ok(removed)
}

pub(super) fn prune(transaction: &Transaction<'_>, now: i64) -> Result<()> {
    prune_with_limits(transaction, now, Limits::default())
}

struct Retained {
    id: i64,
    path: Vec<u8>,
    observed_at: Option<i64>,
    revision: String,
    bytes: u64,
}

fn prune_with_limits(transaction: &Transaction<'_>, now: i64, limits: Limits) -> Result<()> {
    let mut statement = transaction
        .prepare(
            "SELECT e.id, e.canonical_path, e.observed_at_us, e.revision,
                    COALESCE(length(p.compressed_source), 0)
             FROM history_events e LEFT JOIN history_payloads p ON p.revision = e.revision
             ORDER BY e.id DESC",
        )
        .map_err(db_error)?;
    let events = statement
        .query_map([], |row| {
            Ok(Retained {
                id: row.get(0)?,
                path: row.get(1)?,
                observed_at: row.get(2)?,
                revision: row.get(3)?,
                bytes: row.get::<_, i64>(4)? as u64,
            })
        })
        .map_err(db_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    let mut by_document: HashMap<&[u8], Vec<usize>> = HashMap::new();
    for (position, event) in events.iter().enumerate() {
        by_document.entry(&event.path).or_default().push(position);
    }
    let mut keep = vec![true; events.len()];
    let cutoff = now.saturating_sub(limits.age_us.max(0));
    for positions in by_document.values() {
        // Preserve the contiguous tail through the oldest in-window event,
        // plus its predecessor. A quiet note keeps one baseline when capacity
        // permits. Dates never replace event order, including unknown dates.
        let recent = positions
            .iter()
            .rposition(|&position| events[position].observed_at.is_some_and(|at| at >= cutoff));
        let count = recent
            .map_or(1, |position| position.saturating_add(2))
            .min(positions.len())
            .min(limits.per_document);
        for &position in &positions[count..] {
            keep[position] = false;
        }
    }
    let mut count = 0;
    for retained in &mut keep {
        if *retained {
            count += 1;
            if count > limits.total {
                *retained = false;
            }
        }
    }
    // Shared payloads count once. Removing an old A event must not collect A
    // while a later A event (or another referencing event) still needs it.
    let mut references: HashMap<&str, (usize, u64)> = HashMap::new();
    for (event, &retained) in events.iter().zip(&keep) {
        if retained {
            let usage = references
                .entry(&event.revision)
                .or_insert((0, event.bytes));
            usage.0 += 1;
        }
    }
    let mut bytes = references
        .values()
        .fold(0u64, |sum, &(_, size)| sum.saturating_add(size));
    for position in (0..events.len()).rev() {
        if bytes <= limits.bytes {
            break;
        }
        if keep[position] {
            keep[position] = false;
            let usage = references
                .get_mut(events[position].revision.as_str())
                .unwrap();
            usage.0 -= 1;
            if usage.0 == 0 {
                bytes = bytes.saturating_sub(usage.1);
            }
        }
    }
    for (event, retained) in events.iter().zip(keep) {
        if !retained {
            mark_partial(transaction, &event.path)?;
            transaction
                .execute("DELETE FROM history_events WHERE id = ?1", [event.id])
                .map_err(db_error)?;
            transaction
                .execute(
                    "UPDATE history_documents SET last_event_id = NULL WHERE last_event_id = ?1",
                    [event.id],
                )
                .map_err(db_error)?;
        }
    }
    collect_payloads(transaction)
}

fn collect_payloads(transaction: &Transaction<'_>) -> Result<()> {
    transaction
        .execute(
            "DELETE FROM history_payloads WHERE NOT EXISTS
             (SELECT 1 FROM history_events WHERE history_events.revision = history_payloads.revision)",
            [],
        )
        .map_err(db_error)?;
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::new("tracking_invalid", message)
}

fn unavailable() -> Error {
    Error::new(
        "history_unavailable",
        "This history version is no longer available; retry the history query",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTE: &str = "/notes/example.md";

    fn database() -> (tempfile::TempDir, super::super::store::Store) {
        let directory = tempfile::tempdir().unwrap();
        let store =
            super::super::store::Store::open(&directory.path().join("updates.sqlite3")).unwrap();
        (directory, store)
    }

    fn count(transaction: &Transaction<'_>, table: &str) -> i64 {
        transaction
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn repeated_content_is_a_new_event_but_an_unchanged_read_is_not() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "A\r\n", 10).unwrap();
        record(&transaction, path, Some("A\r\n"), "A\r\n", 11).unwrap();
        record(&transaction, path, Some("A\r\n"), "B", 12).unwrap();
        record(&transaction, path, Some("B"), "A\r\n", 13).unwrap();
        let history = index(&transaction, path).unwrap();
        assert_eq!(history.entries.len(), 3);
        assert!(!history.pruned);
        assert_eq!(
            history
                .entries
                .iter()
                .map(|entry| entry.observed_at)
                .collect::<Vec<_>>(),
            [Some(13), Some(12), Some(10)]
        );
        assert_eq!(history.entries[0].previous_id, Some(history.entries[1].id));
        assert_eq!(history.entries[1].previous_id, Some(history.entries[2].id));
        assert_eq!(history.entries[2].previous_id, None);
        assert_eq!(count(&transaction, "history_payloads"), 2);
        assert_eq!(
            load(&transaction, path, &history.entries[0]).unwrap(),
            "A\r\n"
        );
        assert_eq!(load(&transaction, path, &history.entries[1]).unwrap(), "B");
    }

    #[test]
    fn a_pre_upgrade_copy_has_no_invented_timestamp() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, Some("older"), "current", 50).unwrap();
        let history = index(&transaction, path).unwrap();
        assert_eq!(history.entries.len(), 2);
        assert_eq!(history.entries[0].observed_at, Some(50));
        assert_eq!(history.entries[1].observed_at, None);
        assert_eq!(history.entries[0].previous_id, Some(history.entries[1].id));
        assert_eq!(
            load(&transaction, path, &history.entries[1]).unwrap(),
            "older"
        );
        let same = Path::new("/notes/unchanged.md");
        record(&transaction, same, Some("same"), "same", 60).unwrap();
        let unchanged = index(&transaction, same).unwrap();
        assert_eq!(unchanged.entries.len(), 1);
        assert_eq!(unchanged.entries[0].observed_at, Some(60));
    }

    #[test]
    fn pruning_keeps_one_age_predecessor_and_does_not_relink_missing_events() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        let mut previous = None;
        for (source, at) in [("A", 0), ("B", 5), ("C", 10), ("D", 15)] {
            record(&transaction, path, previous, source, at).unwrap();
            previous = Some(source);
        }
        let original = index(&transaction, path).unwrap();
        prune_with_limits(
            &transaction,
            15,
            Limits {
                age_us: 5,
                ..Limits::default()
            },
        )
        .unwrap();
        let recent = index(&transaction, path).unwrap();
        assert!(recent.pruned);
        assert_eq!(recent.entries.len(), 3);
        assert_eq!(recent.entries[2].observed_at, Some(5));
        assert_eq!(recent.entries[2].previous_id, Some(original.entries[3].id));
        assert_eq!(
            load(&transaction, path, &original.entries[3])
                .unwrap_err()
                .code,
            "history_unavailable"
        );
        prune_with_limits(
            &transaction,
            15,
            Limits {
                per_document: 2,
                ..Limits::default()
            },
        )
        .unwrap();
        let capped = index(&transaction, path).unwrap();
        assert_eq!(capped.entries.len(), 2);
        assert_eq!(capped.entries[1].previous_id, Some(original.entries[2].id));
        assert_eq!(load(&transaction, path, &capped.entries[0]).unwrap(), "D");
    }

    #[test]
    fn byte_retention_counts_shared_payloads_and_collects_only_unreferenced_content() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "A", 1).unwrap();
        record(&transaction, path, Some("A"), "B", 2).unwrap();
        record(&transaction, path, Some("B"), "A", 3).unwrap();
        let original = index(&transaction, path).unwrap();
        let a_bytes: i64 = transaction
            .query_row(
                "SELECT length(compressed_source) FROM history_payloads WHERE revision = ?1",
                [&original.entries[0].revision],
                |row| row.get(0),
            )
            .unwrap();
        prune_with_limits(
            &transaction,
            3,
            Limits {
                bytes: a_bytes as u64,
                ..Limits::default()
            },
        )
        .unwrap();
        let history = index(&transaction, path).unwrap();
        assert!(history.pruned);
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].id, original.entries[0].id);
        assert_eq!(history.entries[0].previous_id, Some(original.entries[1].id));
        assert_eq!(count(&transaction, "history_payloads"), 1);
        assert_eq!(load(&transaction, path, &history.entries[0]).unwrap(), "A");
    }

    #[test]
    fn oversized_history_leaves_an_explicit_gap_without_failing_observation() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "A", 1).unwrap();
        let tiny = Limits {
            bytes: 1,
            ..Limits::default()
        };
        record_with_limits(&transaction, path, Some("A"), "B", 2, tiny).unwrap();
        let skipped = index(&transaction, path).unwrap();
        assert!(skipped.pruned);
        assert_eq!(skipped.entries.len(), 1);
        // The unavailable immediately preceding B also cannot fit, so the
        // retained C is a baseline, not a false A -> C transition.
        record_with_limits(&transaction, path, Some("B"), "C", 3, tiny).unwrap();
        record(&transaction, path, Some("C"), "D", 4).unwrap();
        let restored = index(&transaction, path).unwrap();
        assert_eq!(restored.entries.len(), 3);
        assert_eq!(load(&transaction, path, &restored.entries[1]).unwrap(), "C");
        assert_eq!(restored.entries[1].observed_at, None);
        assert_eq!(restored.entries[1].previous_id, None);
        assert_eq!(
            restored.entries[0].previous_id,
            Some(restored.entries[1].id)
        );
    }

    #[test]
    fn corrupt_payloads_and_wrong_file_entries_are_never_returned_or_repaired() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "original", 1).unwrap();
        let entry = index(&transaction, path).unwrap().entries.remove(0);
        assert_eq!(
            load(&transaction, Path::new("/notes/other.md"), &entry)
                .unwrap_err()
                .code,
            "history_unavailable"
        );
        transaction
            .execute(
                "UPDATE history_payloads SET compressed_source = ?1 WHERE revision = ?2",
                rusqlite::params![b"not compressed".as_slice(), entry.revision],
            )
            .unwrap();
        assert_eq!(
            load(&transaction, path, &entry).unwrap_err().code,
            "tracking_invalid"
        );
        record(&transaction, path, Some("original"), "different", 2).unwrap();
        record(&transaction, path, Some("different"), "original", 3).unwrap();
        let repeated = index(&transaction, path).unwrap().entries.remove(0);
        assert_eq!(
            load(&transaction, path, &repeated).unwrap_err().code,
            "tracking_invalid"
        );
        transaction
            .execute(
                "UPDATE history_payloads SET source_bytes = ?1 WHERE revision = ?2",
                rusqlite::params![MAX_SOURCE_BYTES as i64 + 1, entry.revision],
            )
            .unwrap();
        assert_eq!(
            load(&transaction, path, &entry).unwrap_err().code,
            "tracking_invalid"
        );
    }

    #[test]
    fn source_hash_and_declared_length_are_checked_after_decompression() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "good", 1).unwrap();
        record(&transaction, path, Some("good"), "evil", 2).unwrap();
        let entries = index(&transaction, path).unwrap().entries;
        transaction.execute("UPDATE history_payloads SET compressed_source = (SELECT compressed_source FROM history_payloads WHERE revision = ?1) WHERE revision = ?2", rusqlite::params![entries[0].revision, entries[1].revision]).unwrap();
        assert_eq!(
            load(&transaction, path, &entries[1]).unwrap_err().code,
            "tracking_invalid"
        );
        transaction
            .execute(
                "UPDATE history_payloads SET source_bytes = 2 WHERE revision = ?1",
                [&entries[0].revision],
            )
            .unwrap();
        assert_eq!(
            load(&transaction, path, &entries[0]).unwrap_err().code,
            "tracking_invalid"
        );
    }

    #[test]
    fn a_missing_payload_is_not_recreated_by_a_later_equal_version() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let path = Path::new(NOTE);
        record(&transaction, path, None, "A", 1).unwrap();
        let first = index(&transaction, path).unwrap().entries.remove(0);
        transaction
            .execute(
                "DELETE FROM history_payloads WHERE revision = ?1",
                [&first.revision],
            )
            .unwrap();
        assert_eq!(
            load(&transaction, path, &first).unwrap_err().code,
            "history_unavailable"
        );
        record(&transaction, path, Some("A"), "B", 2).unwrap();
        assert_eq!(
            record(&transaction, path, Some("B"), "A", 3)
                .unwrap_err()
                .code,
            "tracking_invalid"
        );
        assert_eq!(count(&transaction, "history_payloads"), 1);
        assert_eq!(
            load(&transaction, path, &first).unwrap_err().code,
            "history_unavailable"
        );
    }

    #[test]
    fn global_event_cap_evicts_oldest_events_across_documents() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let first = Path::new(NOTE);
        let second = Path::new("/notes/second.md");
        record(&transaction, first, None, "A", 1).unwrap();
        record(&transaction, second, None, "B", 2).unwrap();
        record(&transaction, first, Some("A"), "C", 3).unwrap();
        let before = index(&transaction, first).unwrap();
        prune_with_limits(
            &transaction,
            3,
            Limits {
                total: 2,
                ..Limits::default()
            },
        )
        .unwrap();
        let retained = index(&transaction, first).unwrap();
        assert!(retained.pruned);
        assert_eq!(retained.entries.len(), 1);
        assert_eq!(retained.entries[0].previous_id, Some(before.entries[1].id));
        assert_eq!(index(&transaction, second).unwrap().entries.len(), 1);
        assert_eq!(count(&transaction, "history_payloads"), 2);
    }

    #[test]
    fn clearing_one_file_preserves_other_history_and_does_not_reuse_event_ids() {
        let (_directory, mut store) = database();
        let transaction = store.begin().unwrap();
        let first = Path::new(NOTE);
        let second = Path::new("/notes/second.md");
        record(&transaction, first, None, "private", 1).unwrap();
        record(&transaction, second, None, "other", 2).unwrap();
        let old = index(&transaction, first).unwrap().entries.remove(0);
        assert_eq!(clear(&transaction, first).unwrap(), 1);
        assert!(index(&transaction, first).unwrap().entries.is_empty());
        let other = index(&transaction, second).unwrap().entries;
        assert_eq!(load(&transaction, second, &other[0]).unwrap(), "other");
        assert_eq!(count(&transaction, "history_payloads"), 1);
        record(&transaction, first, Some("current"), "current", 3).unwrap();
        let restarted = index(&transaction, first).unwrap().entries;
        assert!(restarted[0].id > old.id);
        assert_eq!(restarted[0].previous_id, None);
        assert_eq!(restarted[0].observed_at, Some(3));
    }

    #[test]
    fn uncommitted_history_and_clock_changes_rollback_together() {
        let (_directory, mut store) = database();
        {
            let transaction = store.begin().unwrap();
            record(&transaction, Path::new(NOTE), None, "A", 1).unwrap();
            super::super::store::advance_clock(&transaction, 2).unwrap();
            // Simulate a later recheck/write failure by not committing.
        }
        let transaction = store.begin().unwrap();
        assert!(index(&transaction, Path::new(NOTE))
            .unwrap()
            .entries
            .is_empty());
        assert_eq!(super::super::store::floor(&transaction).unwrap(), None);
        assert_eq!(count(&transaction, "history_payloads"), 0);
    }

    #[test]
    fn a_version_one_database_migrates_without_changing_existing_tracking() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("updates.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("CREATE TABLE clock (id INTEGER PRIMARY KEY CHECK (id = 1), last_us INTEGER NOT NULL);
            CREATE TABLE documents (canonical_path BLOB PRIMARY KEY NOT NULL, tracked_from_us INTEGER NOT NULL, tree_profile TEXT NOT NULL, source TEXT NOT NULL, node_times TEXT NOT NULL);
            INSERT INTO clock VALUES (1, 11);
            PRAGMA user_version = 1;").unwrap();
        connection
            .execute(
                "INSERT INTO documents VALUES (?1, 10, 'qm-updates-v1', 'old source', '[10]')",
                [Path::new(NOTE).as_os_str().as_encoded_bytes()],
            )
            .unwrap();
        drop(connection);
        let mut store = super::super::store::Store::open(&path).unwrap();
        let transaction = store.begin().unwrap();
        assert_eq!(
            transaction
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        let prior = super::super::store::read(&transaction, Path::new(NOTE))
            .unwrap()
            .unwrap();
        assert_eq!(prior.source, "old source");
        assert_eq!(prior.tracked_from, 10);
        assert_eq!(prior.subtree_times, prior.times);
        assert_eq!(super::super::store::floor(&transaction).unwrap(), Some(11));
        assert!(index(&transaction, Path::new(NOTE))
            .unwrap()
            .entries
            .is_empty());
    }
}
