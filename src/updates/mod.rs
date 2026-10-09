//! Observed updates and the shared node selection used by reads and replies.

pub(crate) mod history;
mod history_store;
pub(crate) mod retained;
mod store;
pub(crate) mod tree;

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
#[cfg(test)]
use std::{collections::HashSet, ffi::OsString};

use crate::error::{Error, Result};
pub use store::database_path;
use store::{Observation, Store};
use tree::Tree;

struct Request {
    path: PathBuf,
    since: Option<i64>,
    labels: Vec<String>,
    find: Option<String>,
    list: bool,
    json: bool,
    max_bytes: usize,
}

impl Request {
    #[cfg(test)]
    fn parse(mut args: impl Iterator<Item = OsString>) -> Result<Option<Self>> {
        let mut path = None;
        let mut since = None;
        let mut labels = Vec::new();
        let mut find = None;
        let mut list = false;
        let mut json = false;
        let mut help = false;
        let mut max_bytes = 256 * 1024;
        let mut seen = HashSet::new();
        let mut positional = false;
        while let Some(arg) = args.next() {
            if !positional && arg == "--" {
                positional = true;
                continue;
            }
            if positional || !arg.to_str().is_some_and(|s| s.starts_with('-')) {
                if path.replace(PathBuf::from(arg)).is_some() {
                    return Err(Error::new("usage", "read takes one FILE; use --help"));
                }
                continue;
            }
            let key = arg.to_str().unwrap();
            if key != "--node" && !seen.insert(key.to_owned()) {
                return Err(Error::new("usage", format!("Repeated option {key}")));
            }
            match key {
                "--help" | "-h" => help = true,
                "--list" => list = true,
                "--json" => json = true,
                "--since" | "--node" | "--find" | "--max-bytes" => {
                    let value = args
                        .next()
                        .and_then(|s| s.into_string().ok())
                        .ok_or_else(|| Error::new("usage", format!("{key} needs a UTF-8 value")))?;
                    match key {
                        "--since" => since = Some(parse_time(&value)?),
                        "--node" => labels.push(value),
                        "--find" => {
                            if value.is_empty() {
                                return Err(Error::new(
                                    "usage",
                                    "--find needs nonempty literal text",
                                ));
                            }
                            find = Some(value);
                        }
                        _ => {
                            max_bytes =
                                value
                                    .parse::<usize>()
                                    .ok()
                                    .filter(|n| *n > 0)
                                    .ok_or_else(|| {
                                        Error::new("usage", "--max-bytes needs a positive integer")
                                    })?
                        }
                    }
                }
                _ => {
                    return Err(Error::new(
                        "usage",
                        format!("Unknown option {key}; use mdt read --help"),
                    ))
                }
            }
        }
        if help {
            return Ok(None);
        }
        if find.is_some() && (since.is_some() || list) {
            return Err(Error::new(
                "usage",
                "--find reads discovered content; do not combine it with --since or --list",
            ));
        }
        Ok(Some(Self {
            path: path.ok_or_else(|| Error::new("usage", "Expected mdt read FILE; use --help"))?,
            since,
            labels,
            find,
            list,
            json,
            max_bytes,
        }))
    }

    fn render(
        &self,
        tree: &Tree<'_>,
        times: &[i64],
        subtree_times: &[i64],
        selected: usize,
        initialized: bool,
        as_of: i64,
        revision: &str,
    ) -> Result<String> {
        #[derive(Serialize)]
        struct Node<'a> {
            path: Vec<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            content: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            own_content: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            children_omitted: Option<bool>,
        }
        #[derive(Serialize)]
        struct Response<'a> {
            status: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            hint: Option<&'static str>,
            as_of: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            revision: Option<&'a str>,
            nodes: Vec<Node<'a>>,
        }
        let nodes: Vec<_> = tree
            .results(times, subtree_times, selected, self.since)
            .into_iter()
            .map(|node| Node {
                path: tree.path(node.index),
                content: (!self.list && node.complete).then(|| tree.content(node.index)),
                own_content: (!self.list && !node.complete)
                    .then(|| tree.own_content(node.index).unwrap()),
                children_omitted: (!self.list && !node.complete).then_some(true),
            })
            .collect();
        let status = if initialized {
            "first_observation"
        } else if self.since.is_none() {
            "current"
        } else if nodes.is_empty() {
            "no_changes"
        } else {
            "changes"
        };
        let response = Response {
            status,
            hint: initialized.then_some(if self.list {
                "Earlier history unknown. Read the listed content before keeping as_of."
            } else {
                "Earlier history unknown. Keep as_of for this scope; use --since next."
            }),
            as_of: format_time(as_of)?,
            revision: (!self.list).then_some(revision),
            nodes,
        };
        let output = if self.json {
            serde_json::to_string_pretty(&response).map_err(|e| Error::new("io", e.to_string()))?
                + "\n"
        } else {
            let mut output = String::new();
            if let Some(hint) = response.hint {
                output.push_str(&format!("First observation. {hint}\n"));
            }
            output.push_str(&format!("As of {}\n", response.as_of));
            if let Some(revision) = response.revision {
                output.push_str(&format!("Revision {revision}\n"));
            }
            output.push('\n');
            if response.nodes.is_empty() {
                output.push_str("No updates.\n");
            }
            for (i, node) in response.nodes.iter().enumerate() {
                let path = if node.path.is_empty() {
                    "Document".into()
                } else {
                    node.path.join(" > ")
                };
                if self.list {
                    output.push_str(&format!("{path}\n"));
                } else {
                    if i > 0 && !output.ends_with("\n\n") {
                        output.push('\n');
                    }
                    let omitted = if node.children_omitted.is_some() {
                        "; children omitted"
                    } else {
                        ""
                    };
                    output.push_str(&format!(
                        "[{path}{omitted}]\n{}",
                        node.content.or(node.own_content).unwrap()
                    ));
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            output
        };
        if output.len() > self.max_bytes {
            let mut error = Error::new(
                "too_large",
                format!(
                    "Complete response is {} bytes; raise --max-bytes or select a smaller subtree",
                    output.len()
                ),
            );
            error.bytes = Some(output.len());
            return Err(error);
        }
        Ok(output)
    }
}

#[cfg(test)]
fn parse_time(value: &str) -> Result<i64> {
    let invalid = || {
        Error::new("usage", "--since needs a datetime with a timezone and at most six fractional digits, e.g. 1664-01-01T13:00:00.000001Z")
    };
    if let Some((_, fraction)) = value.split_once('.') {
        let precision = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if precision == 0 || precision > 6 {
            return Err(invalid());
        }
    }
    let time = DateTime::parse_from_rfc3339(value).map_err(|_| invalid())?;
    // Chrono permits leap seconds, which have no unique POSIX microsecond value.
    if time.timestamp_subsec_nanos() >= 1_000_000_000 {
        return Err(invalid());
    }
    Ok(time.timestamp_micros())
}

fn format_time(time: i64) -> Result<String> {
    DateTime::<Utc>::from_timestamp_micros(time)
        .map(|t| t.to_rfc3339_opts(SecondsFormat::AutoSi, true))
        .ok_or_else(|| {
            Error::new(
                "tracking_invalid",
                "Observation time is outside the supported datetime range",
            )
        })
}

/// Record the current saved bytes using the same tracking state as `mdt read`.
///
/// The GUI calls this off its UI thread after opening, saving or reloading a
/// note. Only a path is queued: this function rereads the file under the shared
/// note gate, so a delayed observation cannot replace newer state with an old
/// editor buffer. It records no reader checkpoint and does not modify the note.
/// Callers must not hold the note gate, and a tracking error must not turn an
/// already successful document save into a failed save.
pub fn observe_saved_file(path: &Path, database: &Path, gates: &Path) -> Result<()> {
    record_saved_file(path, database, Some(gates))
}

#[cfg(test)]
fn observe_saved_file_under_gate(path: &Path, database: &Path) -> Result<()> {
    record_saved_file(path, database, None)
}

fn record_saved_file(path: &Path, database: &Path, gates: Option<&Path>) -> Result<()> {
    let request = Request {
        path: path.to_path_buf(),
        since: None,
        labels: Vec::new(),
        find: None,
        list: true,
        json: false,
        max_bytes: usize::MAX,
    };
    observe_impl(
        &request,
        database,
        gates,
        || Utc::now().timestamp_micros(),
        || {},
        false,
    )
    .map(|_| ())
}

/// The callback is a test seam for a non-cooperating edit during the read. There
/// are no hooks or environment clock overrides in the shipped command.
#[cfg(test)]
fn observe(
    request: &Request,
    database: &Path,
    gates: &Path,
    clock: impl FnMut() -> i64,
    before_recheck: impl FnOnce(),
) -> Result<String> {
    observe_impl(request, database, Some(gates), clock, before_recheck, true)
}

fn observe_impl(
    request: &Request,
    database: &Path,
    gates: Option<&Path>,
    mut clock: impl FnMut() -> i64,
    before_recheck: impl FnOnce(),
    render_output: bool,
) -> Result<String> {
    let path = fs::canonicalize(&request.path).map_err(|cause| {
        if cause.kind() == std::io::ErrorKind::NotFound {
            Error::new(
                "not_found",
                format!(
                    "File {:?} not found. Usage: mdt read FILE [--node LABEL]...",
                    request.path
                ),
            )
        } else {
            Error::io(cause)
        }
    })?;
    let mut store = Store::open(database)?;
    let transaction = store.begin()?;
    let _gate = gates
        .map(|gates| {
            mdt_core::gate::try_lock(gates, &path)
                .map_err(Error::io)?
                .ok_or_else(|| {
                    Error::new(
                        "locked",
                        "Another Markdown operation holds this note; retry the query",
                    )
                })
        })
        .transpose()?;
    let source = fs::read_to_string(&path).map_err(Error::io)?;
    let previous = store::read(&transaction, &path)?;
    let floor = store::floor(&transaction)?;
    let now = clock();
    let observed_at = floor.map_or(now, |floor| now.max(floor));
    if let Some(since) = request.since {
        let previous = previous.as_ref().ok_or_else(|| {
            Error::new(
                "tracking_unavailable",
                "Earlier history unknown. Rerun this command without --since.",
            )
        })?;
        if since < previous.tracked_from {
            return Err(Error::new(
                "tracking_unavailable",
                format!(
                    "Tracking began at {}. Rerun this command without --since.",
                    format_time(previous.tracked_from)?
                ),
            ));
        }
        if since > observed_at {
            return Err(Error::new("usage", "--since is in the future"));
        }
    }
    let tree = Tree::new(&source);
    let selected = tree.select(&request.labels)?;
    let selected = request
        .find
        .as_deref()
        .map(|text| tree.find(selected, text))
        .transpose()?
        .unwrap_or(selected);
    let (times, subtree_times) = if let Some(previous) = &previous {
        if previous.profile != tree::PROFILE {
            return Err(Error::new(
                "tracking_invalid",
                format!(
                    "Unsupported stored tree profile {:?}; state was not changed",
                    previous.profile
                ),
            ));
        }
        let old = (previous.source != source).then(|| Tree::new(&previous.source));
        let old_count = old.as_ref().map_or(tree.nodes.len(), |old| old.nodes.len());
        if old_count != previous.times.len()
            || old_count != previous.subtree_times.len()
            || floor.is_none_or(|floor| previous.tracked_from >= floor)
            || previous.times.iter().any(|&time| {
                time < previous.tracked_from || floor.is_none_or(|floor| time >= floor)
            })
            || previous
                .subtree_times
                .iter()
                .zip(&previous.times)
                .any(|(&full, &changed)| full < previous.tracked_from || full > changed)
        {
            return Err(Error::new(
                "tracking_invalid",
                "Stored node times do not match the document and clock; state was not changed",
            ));
        }
        old.as_ref().map_or_else(
            || (previous.times.clone(), previous.subtree_times.clone()),
            |old| {
                tree::compare(
                    old,
                    &tree,
                    &previous.times,
                    &previous.subtree_times,
                    observed_at,
                )
            },
        )
    } else {
        (
            vec![observed_at; tree.nodes.len()],
            vec![observed_at; tree.nodes.len()],
        )
    };
    let as_of = observed_at
        .checked_add(1)
        .ok_or_else(|| Error::new("tracking_invalid", "Observation clock overflow"))?
        .max(clock());
    let initialized = previous.is_none();
    let output = if render_output {
        let revision = crate::snapshots::revision(&path, &source);
        request.render(
            &tree,
            &times,
            &subtree_times,
            selected,
            initialized,
            as_of,
            &revision,
        )?
    } else {
        String::new()
    };
    before_recheck();
    if fs::read(&path).map_err(Error::io)? != source.as_bytes() {
        return Err(Error::new(
            "changed_during_read",
            "Saved bytes changed during the read; retry the query",
        ));
    }
    history_store::record(
        &transaction,
        &path,
        previous
            .as_ref()
            .map(|observation| observation.source.as_str()),
        &source,
        observed_at,
    )?;
    history_store::prune(&transaction, observed_at)?;
    if previous
        .as_ref()
        .is_none_or(|previous| previous.source != source)
    {
        store::write(
            &transaction,
            &path,
            &Observation {
                tracked_from: previous.map_or(observed_at, |previous| previous.tracked_from),
                profile: tree::PROFILE.into(),
                source,
                times,
                subtree_times,
            },
        )?;
    }
    store::advance_clock(&transaction, as_of)?;
    transaction.commit().map_err(store::db_error)?;
    Ok(output)
}

#[cfg(test)]
mod tests;
