//! Human-facing recent history. Current reads keep their existing contract.

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::Utc;
use serde::Serialize;

use super::{format_time, history_store, store::Store, tree::Tree};
use crate::{
    changes,
    error::{Error, Result},
};

/// Scope and pagination for retained changes. Timestamps are UTC microseconds.
#[derive(Clone, Debug)]
pub struct HistoryOptions {
    pub labels: Vec<String>,
    pub since: Option<i64>,
    pub before: Option<i64>,
    pub limit: usize,
}

impl Default for HistoryOptions {
    fn default() -> Self {
        Self {
            labels: Vec::new(),
            since: None,
            before: None,
            limit: 10,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HistoryChange {
    pub id: i64,
    pub observed_at: Option<String>,
    pub revision: String,
    pub previous_id: Option<i64>,
    pub kind: &'static str,
    pub before_path: Option<Vec<String>>,
    pub after_path: Option<Vec<String>>,
    pub diff: String,
}

#[derive(Debug, Serialize)]
pub struct RecentHistory {
    pub path: PathBuf,
    pub status: &'static str,
    pub time_basis: &'static str,
    pub earlier_history: &'static str,
    pub retained_from: Option<String>,
    pub entries: Vec<HistoryChange>,
    pub next_before: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct HistoricalText {
    pub path: PathBuf,
    pub id: i64,
    pub observed_at: Option<String>,
    pub revision: String,
    pub node_path: Vec<String>,
    pub content: String,
}

/// Query already-recorded history without observing or modifying the live file.
/// The GUI can use this same API for a future history view.
pub fn recent_history(
    path: &Path,
    database: &Path,
    options: &HistoryOptions,
) -> Result<RecentHistory> {
    recent_history_impl(path, database, options, || {})
}

// The callback exercises another observer after fetching bytes but before
// decoding them. Production queries have no hook or timing override.
fn recent_history_impl(
    path: &Path,
    database: &Path,
    options: &HistoryOptions,
    after_fetch: impl FnMut(),
) -> Result<RecentHistory> {
    validate_options(options)?;
    let path = identity(path)?;
    let mut store = Store::open(database)?;
    let transaction = store.begin()?;
    history_store::prune(&transaction, Utc::now().timestamp_micros())?;
    let index = history_store::index(&transaction, &path)?;
    transaction.commit().map_err(super::store::db_error)?;
    query(&mut store, &path, options, index, after_fetch)
}

/// Read one retained observation. This neither reads today's note nor advances
/// a reader checkpoint. An expired ID is never redirected to another version.
pub fn historical_text(
    path: &Path,
    database: &Path,
    id: i64,
    labels: &[String],
) -> Result<HistoricalText> {
    historical_text_impl(path, database, id, labels, || {})
}

fn historical_text_impl(
    path: &Path,
    database: &Path,
    id: i64,
    labels: &[String],
    after_fetch: impl FnOnce(),
) -> Result<HistoricalText> {
    if id <= 0 {
        return Err(Error::new("usage", "--at needs a positive observation ID"));
    }
    let path = identity(path)?;
    let mut store = Store::open(database)?;
    let transaction = store.begin()?;
    history_store::prune(&transaction, Utc::now().timestamp_micros())?;
    let index = history_store::index(&transaction, &path)?;
    let entry = index
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| {
            Error::new(
                "history_unavailable",
                "Observation is not retained for this file; run mdt history FILE",
            )
        })?;
    let payload = history_store::fetch(&transaction, &path, entry)?;
    transaction.commit().map_err(super::store::db_error)?;
    after_fetch();
    let source = payload.decode(&path)?;
    let tree = Tree::new(&source);
    let selected = tree.select(labels)?;
    let result = HistoricalText {
        path,
        id,
        observed_at: entry.observed_at.map(format_time).transpose()?,
        revision: entry.revision.clone(),
        node_path: tree.path(selected).into_iter().map(str::to_owned).collect(),
        content: tree.content(selected).into(),
    };
    Ok(result)
}

fn validate_options(options: &HistoryOptions) -> Result<()> {
    if options.limit == 0 || options.before.is_some_and(|id| id <= 0) {
        return Err(Error::new(
            "usage",
            "--limit and --before need positive integers",
        ));
    }
    Ok(())
}

fn selected<'a>(source: &'a str, labels: &[String]) -> Result<Option<(Vec<String>, &'a str)>> {
    let tree = Tree::new(source);
    match tree.select(labels) {
        Ok(node) => Ok(Some((
            tree.path(node).into_iter().map(str::to_owned).collect(),
            tree.content(node),
        ))),
        Err(error) if error.code == "not_found" => Ok(None),
        Err(error) => Err(error),
    }
}

fn query(
    store: &mut Store,
    path: &Path,
    options: &HistoryOptions,
    index: history_store::Index,
    mut after_fetch: impl FnMut(),
) -> Result<RecentHistory> {
    let retained_from = index
        .entries
        .last()
        .and_then(|entry| entry.observed_at)
        .map(format_time)
        .transpose()?;
    let mut output = RecentHistory {
        path: path.to_owned(),
        status: if index.entries.is_empty() {
            "unavailable"
        } else {
            "history"
        },
        time_basis: "observed",
        earlier_history: if index.pruned {
            "unavailable"
        } else {
            "unknown"
        },
        retained_from,
        entries: Vec::new(),
        next_before: None,
    };
    let mut carry: Option<(i64, String)> = None;
    let mut found_scope = options.labels.is_empty();
    for entry in &index.entries {
        if options.before.is_some_and(|before| entry.id >= before) {
            continue;
        }
        if options
            .since
            .is_some_and(|since| entry.observed_at.is_none_or(|time| time < since))
        {
            continue;
        }
        let cached = match carry.take() {
            Some((id, source)) if id == entry.id => Some(source),
            _ => None,
        };
        let previous = entry
            .previous_id
            .and_then(|id| index.entries.iter().find(|entry| entry.id == id));
        // Events and payloads are immutable while retained. Copy at most one
        // before/after pair, then release the writer lock before decompressing,
        // selecting trees or diffing. If pruning removed an uncopied version,
        // fetch fails explicitly; never reinterpret it as a new baseline.
        let transaction = store.begin()?;
        let after_payload = cached
            .is_none()
            .then(|| history_store::fetch(&transaction, path, entry))
            .transpose()?;
        let before_payload = previous
            .map(|entry| history_store::fetch(&transaction, path, entry))
            .transpose()?;
        transaction.commit().map_err(super::store::db_error)?;
        after_fetch();
        let after = match cached {
            Some(source) => source,
            None => after_payload.unwrap().decode(path)?,
        };
        let before = before_payload
            .map(|payload| payload.decode(path))
            .transpose()?;
        let old = before
            .as_deref()
            .map(|source| selected(source, &options.labels))
            .transpose()?
            .flatten();
        let new = selected(&after, &options.labels)?;
        found_scope |= old.is_some() || new.is_some();
        if old != new && (old.is_some() || new.is_some()) || previous.is_none() && new.is_some() {
            let baseline = previous.is_none();
            output.entries.push(HistoryChange {
                id: entry.id,
                observed_at: entry.observed_at.map(format_time).transpose()?,
                revision: entry.revision.clone(),
                previous_id: previous.map(|entry| entry.id),
                kind: if baseline { "baseline" } else { "change" },
                before_path: old.as_ref().map(|(path, _)| path.clone()),
                after_path: new.as_ref().map(|(path, _)| path.clone()),
                diff: if baseline {
                    String::new()
                } else {
                    changes::diff(
                        old.as_ref().map_or("", |(_, source)| source),
                        new.as_ref().map_or("", |(_, source)| source),
                    )
                },
            });
            if output.entries.len() > options.limit {
                output.entries.pop();
                output.next_before = output.entries.last().map(|entry| entry.id);
                break;
            }
        }
        if let (Some(previous), Some(source)) = (previous, before) {
            carry = Some((previous.id, source));
        }
    }
    // A timed/paged query can legitimately contain no occurrence of a selected
    // label. Do not confuse that with an invalid present-day selector.
    if !found_scope
        && !index.entries.is_empty()
        && options.since.is_none()
        && options.before.is_none()
    {
        return Err(Error::new(
            "not_found",
            format!("No retained node matches {:?}", options.labels),
        ));
    }
    if output.entries.is_empty() && output.status == "history" {
        output.status = "no_retained_changes";
    }
    Ok(output)
}

/// Resolve symlinks in the surviving prefix, including after the containing
/// directory was deleted. History receipts always expose this canonical path.
fn identity(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path).map_err(Error::io)?;
    let mut prefix = absolute.as_path();
    loop {
        match fs::canonicalize(prefix) {
            Ok(mut resolved) => {
                for component in absolute.strip_prefix(prefix).unwrap().components() {
                    match component {
                        Component::Normal(name) => resolved.push(name),
                        Component::ParentDir => {
                            resolved.pop();
                        }
                        Component::CurDir => {}
                        _ => return Err(Error::new("usage", "Expected a document path")),
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                prefix = prefix.parent().ok_or_else(|| Error::io(error))?;
            }
            Err(error) => return Err(Error::io(error)),
        }
    }
}
