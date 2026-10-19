use super::{
    query::{self, bounded_path, matches, resolve_target},
    Operation, Request, Response, Source,
};
use crate::{
    error::{Error, Result},
    snapshots,
    updates::{retained::Retained, tree::Tree},
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, ops::Range, path::Path};

pub(super) fn execute(
    source: &mut impl Source,
    database: &Path,
    request: Request,
) -> Result<Response> {
    let identity = source.identity().to_path_buf();
    let store = Retained::new(database, &identity);
    if request.operation == Operation::Forget {
        let count = store.forget(true)?;
        let value = json!({"forgotten":if request.dry_run {0} else {count},"would_forget":count,"dry_run":request.dry_run});
        let output = receipt(
            &request,
            &value,
            vec![],
            format!(
                "{}; retained observations {count}\n",
                if request.dry_run {
                    "dry_run; forgotten 0"
                } else {
                    "forgotten"
                }
            ),
        )?;
        if !request.dry_run {
            store.forget(false)?;
        }
        return Ok(output);
    }
    let base_revision = request.revision.as_ref().unwrap();
    let base = store.load(base_revision)?;
    let live = source.read()?;
    // A conflict receipt names this exact live version; retain it before any
    // return so the caller can immediately inspect --rev current_revision.
    let previous = store.observe(&live)?;
    let before = Tree::new(&base);
    let current = Tree::new(&live);
    let literal = request.old.as_deref();
    let (base_range, node) = if let Some(old) = literal {
        let selected = before.select(&request.nodes)?;
        (
            unique(matches(&base, before.exact_range(selected), old))?,
            None,
        )
    } else if request.operation == Operation::Insert {
        resolve_target(
            &before,
            base_revision,
            request.before.as_ref().or(request.after.as_ref()).unwrap(),
        )?
    } else if let Some(target) = &request.target {
        resolve_target(&before, base_revision, target)?
    } else {
        let node = before.select(&request.nodes)?;
        (before.exact_range(node), Some(node))
    };
    let same = base == live;
    // Only failures resolving an already-valid baseline intention carry
    // changed-source context. A bad baseline request is still just a bad request.
    let live_range = (|| {
        Ok(if same {
            base_range.clone()
        } else if let Some(old) = literal {
            let selected = current.select(&request.nodes)?;
            let found = unique(matches(&live, current.exact_range(selected), old))?;
            // The prepared intention is the baseline occurrence, not any new
            // globally unique occurrence in an unrelated section.
            let owner = before.containing(&base_range);
            let labels: Vec<_> = before.path(owner).into_iter().map(str::to_owned).collect();
            let owner = current.exact_range(current.select(&labels)?);
            if found.start < owner.start || found.end > owner.end {
                return Err(conflict(&previous));
            }
            found
        } else if let Some(node) = node {
            if node == 0 {
                0..live.len()
            } else {
                let labels: Vec<String> =
                    before.path(node).into_iter().map(str::to_owned).collect();
                current.exact_range(current.select(&labels)?)
            }
        } else {
            relocate_range(&before, &current, &base_range, request.force, &previous)?
        })
    })()
    .map_err(|mut error| {
        error.message = format!(
            "Source changed since supplied revision; changed 0. {}",
            error.message
        );
        error.current_revision = Some(previous.clone());
        error
    })?;
    let changed_target = base[base_range.clone()] != live[live_range.clone()];
    if changed_target && !request.force {
        return Err(conflict(&previous));
    }
    let insertion = request.operation == Operation::Insert;
    let replacement = if insertion || literal.is_none() {
        request.body.as_deref().unwrap()
    } else {
        request.new.as_deref().unwrap()
    };
    let selected_live = live_range.clone();
    let splice = if insertion {
        let at = if request.before.is_some() {
            live_range.start
        } else {
            live_range.end
        };
        at..at
    } else {
        live_range
    };
    let mut candidate = String::with_capacity(live.len() - splice.len() + replacement.len());
    candidate.push_str(&live[..splice.start]);
    candidate.push_str(replacement);
    candidate.push_str(&live[splice.end..]);
    let changed = candidate != live;
    let revision = snapshots::revision(&identity, &candidate);
    let target_path = bounded_path(&current.path(current.containing(&selected_live)));
    let other = if same {
        vec![]
    } else {
        outside_paths(&before, &current, &base_range, &selected_live)
    };
    let mut notices = vec![];
    if changed_target && request.force {
        notices.push(format!(
            "Forced {} of changed target {}.",
            if insertion {
                "insertion beside"
            } else {
                "replacement"
            },
            query::path_text(&json!(target_path))
        ));
    }
    if !other.is_empty() {
        let shown: Vec<_> = other
            .iter()
            .take(2)
            .map(|path| query::path_text(&json!(path)))
            .collect();
        notices.push(format!(
            "Changes since {base_revision} outside this edit: {}{}.",
            shown.join(", "),
            if other.len() > 2 {
                format!(", and {} other sections", other.len() - 2)
            } else {
                String::new()
            }
        ));
    }
    let mut value = json!({"base":base_revision,"previous":previous,"revision":revision,
        "dry_run":request.dry_run,"forced":changed_target && request.force,
        "other_changes":{"count":other.len(),"paths":other.iter().take(2).collect::<Vec<_>>(),"remaining":other.len().saturating_sub(2)}});
    if insertion {
        value["anchors"] = json!(1);
        value["inserted"] = json!(usize::from(changed && !request.dry_run));
    } else {
        value["matched"] = json!(1);
        value["changed"] = json!(usize::from(changed && !request.dry_run));
    }
    let status = if insertion {
        format!(
            "anchors 1; inserted {}",
            usize::from(changed && !request.dry_run)
        )
    } else {
        format!(
            "matched 1; changed {}",
            usize::from(changed && !request.dry_run)
        )
    };
    let text = if request.dry_run {
        value["revision"] = json!(previous);
        value["current"] = json!(previous);
        value["candidate_revision"] = json!(revision);
        value["would_change"] = json!(usize::from(changed));
        format!(
            "dry_run; current {previous}; {status}; would_change {}\n",
            usize::from(changed)
        )
    } else {
        format!("rev {revision}; previous {previous}; {status}\n")
    };
    const WARNING: &str = "Source saved successfully; resulting revision could not be retained. Read current source before another write.";
    // Check the worst possible receipt before any source change, including the
    // warning needed to distinguish a committed save from a failed operation.
    let mut worst = value.clone();
    worst["retention_warning"] = json!(WARNING);
    let mut worst_notices = notices.clone();
    worst_notices.push(WARNING.into());
    receipt(&request, &worst, worst_notices, text.clone())?;
    if request.dry_run {
        return receipt(&request, &value, notices, text);
    }
    // The actual pre-write source must be retained before CAS, even for force.
    // Do not mutate if source retention fails or has exhausted its bounds.
    store.observe(&live)?;
    if let Some(observed) = source.compare_and_swap(&live, &candidate)? {
        let mut error = Error::new("locked", "Source changed during this operation; changed 0");
        match store.observe(&observed) {
            Ok(revision) => error.current_revision = Some(revision),
            Err(cause) => {
                error.message.push_str(&format!(
                    ". Observed version could not be retained: {cause}. Read current source before retrying."
                ));
            }
        }
        return Err(error);
    }
    if changed && store.observe(&candidate).is_err() {
        value["retention_warning"] = json!(WARNING);
        notices.push(WARNING.into());
    }
    receipt(&request, &value, notices, text)
}

fn receipt(
    request: &Request,
    value: &Value,
    notices: Vec<String>,
    text: String,
) -> Result<Response> {
    let stdout = if request.json {
        format!("{}\n", serde_json::to_string(value).unwrap())
    } else {
        text
    };
    let size = stdout.len()
        + if request.json {
            0
        } else {
            notices.iter().map(|s| s.len() + 1).sum::<usize>()
        };
    if size > request.max_bytes {
        return Err(Error::new(
            "budget_too_small",
            "Budget cannot fit required write receipt; changed 0",
        ));
    }
    Ok(Response {
        stdout,
        notices: if request.json { vec![] } else { notices },
    })
}
fn unique(ranges: Vec<Range<usize>>) -> Result<Range<usize>> {
    match ranges.len() {
        0 => Err(Error::new(
            "not_found",
            "Literal match not found; matched 0; changed 0",
        )),
        1 => Ok(ranges.into_iter().next().unwrap()),
        n => Err(Error::new(
            "ambiguous",
            format!("Literal has multiple matches; matched {n}; changed 0"),
        )),
    }
}
fn conflict(revision: &str) -> Error {
    let mut error = Error::new("conflict", "Selected target changed; changed 0");
    error.current_revision = Some(revision.into());
    error
}
fn relocate_range(
    old: &Tree<'_>,
    new: &Tree<'_>,
    range: &Range<usize>,
    force: bool,
    revision: &str,
) -> Result<Range<usize>> {
    let node = old.containing(range);
    let labels: Vec<_> = old.path(node).into_iter().map(str::to_owned).collect();
    let scope = new.exact_range(new.select(&labels)?);
    let text = &old.source[range.clone()];
    let found = matches(new.source, scope.clone(), text);
    if found.len() > 1 {
        return Err(Error::new(
            "ambiguous",
            "Exact target has multiple current matches; changed 0",
        ));
    }
    if let Some(found) = found.into_iter().next() {
        return Ok(found);
    }
    if !force {
        return Err(conflict(revision));
    }
    // Force can locate a changed exact range only through unchanged, unique
    // boundaries inside its unique named scope. No fuzzy text or ordinal guess.
    let old_scope = old.exact_range(node);
    let left = &old.source[old_scope.start..range.start];
    let right = &old.source[range.end..old_scope.end];
    let left_at = query::floor_boundary(left, left.len().saturating_sub(128));
    let left = &left[left_at..];
    let right = &right[..query::floor_boundary(right, right.len().min(128))];
    let start = if left.is_empty() {
        scope.start
    } else {
        unique(matches(new.source, scope.clone(), left))?.end
    };
    let end = if right.is_empty() {
        scope.end
    } else {
        unique(matches(new.source, scope, right))?.start
    };
    if start > end {
        return Err(Error::new(
            "ambiguous",
            "Changed range boundaries no longer identify one target; changed 0",
        ));
    }
    Ok(start..end)
}
fn outside_paths(
    old: &Tree<'_>,
    new: &Tree<'_>,
    old_target: &Range<usize>,
    new_target: &Range<usize>,
) -> Vec<Vec<String>> {
    let mut paths = BTreeSet::new();
    for span in query::changed_spans(old.source, new.source) {
        if old_target.start <= span.before.start
            && span.before.end <= old_target.end
            && new_target.start <= span.current.start
            && span.current.end <= new_target.end
        {
            continue;
        }
        let (tree, range) = if span.current.is_empty() {
            (old, &span.before)
        } else {
            (new, &span.current)
        };
        paths.insert(bounded_path(&tree.path(tree.containing(range))));
    }
    paths.into_iter().collect()
}
