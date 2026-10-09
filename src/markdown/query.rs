use super::{Operation, Request, Response, Source};
use crate::{
    error::{Error, Result},
    updates::{
        retained::{Entry, Retained},
        tree::Tree,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};
use std::{ops::Range, path::Path, time::Duration};

#[derive(Clone, Serialize, Deserialize)]
struct Query {
    request: Request,
    revision: String,
    history: Option<Vec<Entry>>,
    index: usize,
    offset: usize,
}

pub(super) fn execute(
    source: &mut impl Source,
    database: &Path,
    request: Request,
) -> Result<Response> {
    let store = Retained::new(database, source.identity());
    let query = if let Some(cursor) = &request.cursor {
        let query: Query = store.resume(cursor)?;
        if query.request.operation != request.operation {
            return Err(Error::new("usage", "Cursor belongs to another operation"));
        }
        query
    } else if request.operation == Operation::History {
        let history = store.history()?;
        Query {
            revision: history
                .first()
                .map_or(String::new(), |e| e.revision.clone()),
            request,
            history: Some(history),
            index: 0,
            offset: 0,
        }
    } else {
        let revision = if let Some(revision) = &request.revision {
            store.load(revision)?;
            revision.clone()
        } else {
            store.observe(&source.read()?)?
        };
        let history = (request.operation == Operation::History)
            .then(|| store.history())
            .transpose()?;
        Query {
            request,
            revision,
            history,
            index: 0,
            offset: 0,
        }
    };
    let text = if query.request.operation == Operation::History {
        String::new()
    } else {
        store.load(&query.revision)?
    };
    let tree = Tree::new(&text);
    let selected = if let Some(target) = &query.request.target {
        resolve_target(&tree, &query.revision, target)?.0
    } else if matches!(
        query.request.operation,
        Operation::Diff | Operation::History
    ) {
        0..text.len()
    } else {
        tree.exact_range(tree.select(&query.request.nodes)?)
    };
    let mut base = json!({"revision":query.revision});
    if query.request.operation == Operation::History {
        base.as_object_mut().unwrap().remove("revision");
    }
    let items = match query.request.operation {
        Operation::Read => {
            let node = tree.containing(&selected);
            base["scope"] = json!(bounded_path(&tree.path(node)));
            base["target"] = json!(query
                .request
                .target
                .clone()
                .unwrap_or_else(|| node_target(node, &query.revision)));
            vec![json!({"content":text[selected].to_owned()})]
        }
        Operation::Outline => {
            let indices: Vec<_> = tree
                .outline_nodes()
                .filter(|&i| {
                    let range = tree.exact_range(i);
                    i == 0 || (selected.start <= range.start && range.end <= selected.end)
                })
                .collect();
            base["total"] = json!(indices.len());
            indices.into_iter().map(|i| json!({"path":bounded_path(&tree.path(i)),"target":node_target(i,&query.revision)})).collect()
        }
        Operation::Find => {
            let needle = query.request.text.as_ref().unwrap();
            let ranges = matches(&text, selected, needle);
            base["match_count"] = json!(ranges.len());
            ranges.into_iter().map(|range| {
                let context = context_range(&text, &range, query.request.context_words.unwrap_or(12), 512);
                json!({"target":range_target(&range,&query.revision),"path":bounded_path(&tree.path(tree.containing(&range))),
                    "content":&text[range.clone()],"context_before":&text[context.start..range.start],
                    "context_after":&text[range.end..context.end]})
            }).collect()
        }
        Operation::Diff => {
            let since = query.request.since.as_ref().unwrap();
            let before = store.load(since)?;
            base["from"] = json!(since);
            let old_tree = Tree::new(&before);
            let old_scope = optional_scope(&old_tree, &query.request.nodes)?;
            let new_scope = optional_scope(&tree, &query.request.nodes)?;
            if old_scope.is_none() && new_scope.is_none() {
                return Err(Error::new(
                    "not_found",
                    "Named scope is absent from both versions",
                ));
            }
            let old_scope = old_scope.unwrap_or(0..0);
            let new_scope = new_scope.unwrap_or(0..0);
            let changes = diff_items(
                &before,
                &text,
                old_scope,
                new_scope,
                query.request.context_words,
            );
            base["total"] = json!(changes.len());
            changes
        }
        Operation::History => {
            let current = store.history()?;
            let entries = query.history.as_ref().unwrap();
            if entries.iter().any(|e| {
                !current
                    .iter()
                    .any(|c| c.id == e.id && c.revision == e.revision)
            }) {
                return Err(Error::new(
                    "revision_unavailable",
                    "A pinned history observation is no longer retained",
                ));
            }
            base["time_basis"] = json!("observed");
            base["total"] = json!(entries.len());
            entries
                .iter()
                .map(|e| serde_json::to_value(e).unwrap())
                .collect()
        }
        _ => unreachable!(),
    };
    page(&store, query, base, items)
}

fn optional_scope(tree: &Tree<'_>, nodes: &[String]) -> Result<Option<Range<usize>>> {
    match tree.select(nodes) {
        Ok(i) => Ok(Some(tree.exact_range(i))),
        Err(e) if e.code == "not_found" => Ok(None),
        Err(e) => Err(e),
    }
}

/// Opaque revision-bound addresses. Source bytes remain in the unified store.
pub(super) fn node_target(index: usize, revision: &str) -> String {
    if index == 0 {
        "D".into()
    } else {
        format!("N{index:x}-{}", &revision[1..33])
    }
}
pub(super) fn range_target(range: &Range<usize>, revision: &str) -> String {
    format!("T{:x}-{:x}-{}", range.start, range.end, &revision[1..33])
}
pub(super) fn resolve_target(
    tree: &Tree<'_>,
    revision: &str,
    target: &str,
) -> Result<(Range<usize>, Option<usize>)> {
    if target == "D" {
        return Ok((0..tree.source.len(), Some(0)));
    }
    let unavailable = || {
        Error::new(
            "revision_unavailable",
            "Target is unavailable at this revision",
        )
    };
    let parts: Vec<_> = target.split('-').collect();
    if target.starts_with('N') && parts.len() == 2 && revision.get(1..33) == Some(parts[1]) {
        let i = usize::from_str_radix(&parts[0][1..], 16).map_err(|_| unavailable())?;
        if tree.outline_nodes().any(|node| node == i) {
            return Ok((tree.exact_range(i), Some(i)));
        }
    }
    if target.starts_with('T') && parts.len() == 3 && revision.get(1..33) == Some(parts[2]) {
        let start = usize::from_str_radix(&parts[0][1..], 16).map_err(|_| unavailable())?;
        let end = usize::from_str_radix(parts[1], 16).map_err(|_| unavailable())?;
        if start <= end
            && end <= tree.source.len()
            && tree.source.is_char_boundary(start)
            && tree.source.is_char_boundary(end)
        {
            return Ok((start..end, None));
        }
    }
    Err(unavailable())
}
pub(super) fn matches(source: &str, range: Range<usize>, needle: &str) -> Vec<Range<usize>> {
    let mut result = vec![];
    if needle.is_empty() {
        return result;
    }
    let mut at = range.start;
    while at <= range.end {
        let Some(offset) = source[at..range.end].find(needle) else {
            break;
        };
        let start = at + offset;
        result.push(start..start + needle.len());
        at = start + source[start..].chars().next().unwrap().len_utf8();
    }
    result
}

/// Metadata labels cannot bypass the response cap. Targets still address full
/// exact source. A clipped path is explicitly marked, never a guessed selector.
pub(super) fn bounded_path(path: &[&str]) -> Vec<String> {
    let mut result: Vec<_> = path
        .iter()
        .take(6)
        .map(|label| {
            let at = floor_boundary(label, label.len().min(96));
            if at < label.len() {
                format!("{}… [label truncated]", &label[..at])
            } else {
                (*label).into()
            }
        })
        .collect();
    if path.len() > 6 {
        result.push(format!("… [{} more ancestors]", path.len() - 6));
    }
    result
}
pub(super) fn floor_boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[derive(Clone, Debug)]
pub(super) struct Span {
    pub before: Range<usize>,
    pub current: Range<usize>,
}
pub(super) fn changed_spans(before: &str, current: &str) -> Vec<Span> {
    let diff = TextDiff::configure()
        .timeout(Duration::from_millis(300))
        .diff_words(before, current);
    let mut spans: Vec<Span> = vec![];
    let (mut old, mut new) = (0, 0);
    let mut pending = None;
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                if let Some((a, b)) = pending.take() {
                    spans.push(Span {
                        before: a..old,
                        current: b..new,
                    });
                }
                old += len;
                new += len;
            }
            ChangeTag::Delete => {
                pending.get_or_insert((old, new));
                old += len;
            }
            ChangeTag::Insert => {
                pending.get_or_insert((old, new));
                new += len;
            }
        }
    }
    if let Some((a, b)) = pending {
        spans.push(Span {
            before: a..old,
            current: b..new,
        });
    }
    spans
}

fn context_range(text: &str, changed: &Range<usize>, words: usize, cap: usize) -> Range<usize> {
    let mut start = changed.start;
    let mut end = changed.end;
    let (mut count, mut in_word) = (0, false);
    for (at, c) in text[..changed.start].char_indices().rev() {
        if changed.start - at > cap {
            break;
        }
        if !c.is_whitespace() && !in_word {
            count += 1;
        }
        if count > words {
            break;
        }
        in_word = !c.is_whitespace();
        start = at;
    }
    let (mut count, mut in_word) = (0, false);
    for (at, c) in text[changed.end..].char_indices() {
        if at + c.len_utf8() > cap {
            break;
        }
        if !c.is_whitespace() && !in_word {
            count += 1;
        }
        if count > words {
            break;
        }
        in_word = !c.is_whitespace();
        end = changed.end + at + c.len_utf8();
    }
    start..end
}
fn paragraph(blocks: &[mdt_core::blocks::Block], range: &Range<usize>) -> Range<usize> {
    blocks
        .iter()
        .find(|b| b.range.start <= range.start && range.end <= b.range.end)
        .map_or_else(|| range.clone(), |b| b.range.clone())
}
fn comment_stamp(text: &str, blocks: &[mdt_core::blocks::Block], at: usize) -> Option<Value> {
    use mdt_core::{blocks::BlockKind, replies};
    let mut stamp = None;
    for (index, block) in blocks.iter().enumerate() {
        if block.range.start > at {
            break;
        }
        if matches!(block.kind, BlockKind::Rule | BlockKind::Heading(_)) {
            stamp = None;
        }
        let source = &text[block.range.clone()];
        let quote = matches!(block.kind, BlockKind::BlockQuote);
        let header = if quote {
            replies::header(source)
        } else {
            replies::entry_header(text, blocks, index)
        };
        if let Some(header) = header {
            if !quote || at <= block.range.end {
                let line = source.lines().next().unwrap_or("");
                let end = floor_boundary(line, line.len().min(512));
                stamp = Some(
                    json!({"source":&line[..end],"truncated":end<line.len(),"time":header.stamp}),
                );
            }
        } else if block.kind == BlockKind::Paragraph
            && index > 0
            && blocks[index - 1].kind == BlockKind::Rule
        {
            // `emm issue note`'s explicit UTC attribution is also source data.
            let line = source.lines().next().unwrap_or("");
            if let Some((time, author)) = line.split_once(" UTC · ") {
                if !author.trim().is_empty()
                    && chrono::NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M").is_ok()
                {
                    let end = floor_boundary(line, line.len().min(512));
                    stamp = Some(
                        json!({"source":&line[..end],"truncated":end<line.len(),"time":format!("{time} UTC")}),
                    );
                }
            }
        }
        if block.range.end >= at {
            break;
        }
    }
    stamp
}
fn diff_items(
    before: &str,
    current: &str,
    old_scope: Range<usize>,
    new_scope: Range<usize>,
    context: Option<usize>,
) -> Vec<Value> {
    let old = &before[old_scope.clone()];
    let new = &current[new_scope.clone()];
    let old_tree = Tree::new(before);
    let new_tree = Tree::new(current);
    let old_blocks = mdt_core::blocks::block_map(old);
    let new_blocks = mdt_core::blocks::block_map(new);
    let old_comment_blocks = mdt_core::replies::editor_block_map(before);
    let new_comment_blocks = mdt_core::replies::editor_block_map(current);
    let mut groups: Vec<(Span, Span)> = vec![];
    for core in changed_spans(old, new) {
        let a = paragraph(&old_blocks, &core.before);
        let b = paragraph(&new_blocks, &core.current);
        let full = context.is_none() && a.len() + b.len() <= 1024;
        let excerpt = if full {
            Span {
                before: a,
                current: b,
            }
        } else {
            Span {
                before: context_range(old, &core.before, context.unwrap_or(12), 256),
                current: context_range(new, &core.current, context.unwrap_or(12), 256),
            }
        };
        if let Some((prior, prior_core)) = groups.last_mut() {
            if excerpt.before.start <= prior.before.end
                || excerpt.current.start <= prior.current.end
            {
                prior.before.end = prior.before.end.max(excerpt.before.end);
                prior.current.end = prior.current.end.max(excerpt.current.end);
                prior_core.before.end = core.before.end;
                prior_core.current.end = core.current.end;
                continue;
            }
        }
        groups.push((excerpt, core));
    }
    groups.into_iter().map(|(excerpt,core)| {
        let old_range = old_scope.start+core.before.start..old_scope.start+core.before.end;
        let new_range = new_scope.start+core.current.start..new_scope.start+core.current.end;
        let path = if !core.current.is_empty() { new_tree.path(new_tree.containing(&new_range)) } else { old_tree.path(old_tree.containing(&old_range)) };
        let mut value=json!({"path":bounded_path(&path),"before":&old[excerpt.before.clone()],"current":&new[excerpt.current.clone()],
            "before_start":old_scope.start+excerpt.before.start,"current_start":new_scope.start+excerpt.current.start,
            "context_omitted_before":excerpt.before.start > 0 || excerpt.current.start > 0,
            "context_omitted_after":excerpt.before.end < old.len() || excerpt.current.end < new.len()});
        if let Some(comment)=comment_stamp(before,&old_comment_blocks,old_range.start) {value["before_comment"]=comment;}
        if let Some(comment)=comment_stamp(current,&new_comment_blocks,new_range.start) {value["current_comment"]=comment;}
        value
    }).collect()
}

fn source_fields(operation: Operation) -> &'static [&'static str] {
    match operation {
        Operation::Read => &["content"],
        Operation::Diff => &["before", "current"],
        Operation::Find => &["context_before", "content", "context_after"],
        _ => &[],
    }
}
fn source_size(item: &Value, fields: &[&str]) -> usize {
    fields
        .iter()
        .map(|field| item[*field].as_str().unwrap().len())
        .sum()
}
fn fragment(item: &Value, fields: &[&str], offset: usize, count: usize) -> (Value, usize) {
    let mut result = item.clone();
    let mut position = 0;
    let mut used = 0;
    for field in fields {
        let source = item[*field].as_str().unwrap();
        let start = offset.saturating_sub(position).min(source.len());
        let end = floor_boundary(
            source,
            offset
                .saturating_add(count)
                .saturating_sub(position)
                .min(source.len()),
        );
        result[*field] = json!(&source[start..end]);
        if fields.len() > 1 {
            result[format!("{field}_offset")] = json!(start);
        }
        used += end - start;
        position += source.len();
    }
    (result, used)
}
fn page(store: &Retained, mut query: Query, base: Value, items: Vec<Value>) -> Result<Response> {
    if query.index > items.len() {
        return Err(Error::new(
            "revision_unavailable",
            "Invalid continuation position",
        ));
    }
    let fields = source_fields(query.request.operation);
    let mut shown = vec![];
    let placeholder = format!("Cj{}", "0".repeat(64));
    let render = |shown: &[Value], complete, cursor: Option<&str>| {
        let mut value = base.clone();
        value["complete"] = json!(complete);
        value["next_cursor"] = json!(cursor);
        if query.request.operation == Operation::Read {
            value["content"] = shown
                .first()
                .map_or(json!(""), |item| item["content"].clone());
        } else {
            let key = match query.request.operation {
                Operation::Diff => "changes",
                Operation::Find => "matches",
                Operation::Outline => "nodes",
                Operation::History => "entries",
                _ => unreachable!(),
            };
            value[key] = json!(shown);
        }
        render_value(&query.request, &value)
    };
    while query.index < items.len() {
        let item = &items[query.index];
        let total = source_size(item, fields);
        if query.offset > total {
            return Err(Error::new(
                "revision_unavailable",
                "Invalid continuation offset",
            ));
        }
        let remaining = total - query.offset;
        let make = |count: usize| {
            let (mut part, used) = fragment(item, fields, query.offset, count);
            if matches!(query.request.operation, Operation::Diff | Operation::Find) {
                part["item_index"] = json!(query.index);
                part["change_complete"] = json!(used == remaining);
            }
            let mut trial = shown.clone();
            trial.push(part);
            let complete = used == remaining && query.index + 1 == items.len();
            let output = render(
                &trial,
                complete,
                (!complete).then_some(placeholder.as_str()),
            );
            (trial, used, output.len())
        };
        let chunk = if query.request.operation == Operation::Diff {
            remaining.min(1024)
        } else {
            remaining
        };
        let (mut trial, mut used, len) = make(chunk);
        if len > query.request.max_bytes {
            let (mut low, mut high) = (0, chunk);
            while low < high {
                let middle = low + (high - low).div_ceil(2);
                if make(middle).2 <= query.request.max_bytes {
                    low = middle;
                } else {
                    high = middle - 1;
                }
            }
            (trial, used, _) = make(low);
            if fields.is_empty() || used == 0 {
                if shown.is_empty() {
                    return Err(Error::new(
                        "budget_too_small",
                        "Budget cannot fit the result and continuation envelope",
                    ));
                }
                break;
            }
        }
        shown = trial;
        query.offset += used;
        if query.offset == total {
            query.index += 1;
            query.offset = 0;
        } else if used < chunk || query.request.operation != Operation::Diff {
            break;
        }
        if query.request.operation == Operation::Read {
            break;
        }
    }
    let complete = query.index == items.len();
    let cursor = (!complete)
        .then(|| store.cursor(&query, query.request.json))
        .transpose()?;
    let output = render(&shown, complete, cursor.as_deref());
    if output.len() > query.request.max_bytes {
        return Err(Error::new(
            "budget_too_small",
            "Budget cannot fit the result envelope",
        ));
    }
    Ok(Response {
        stdout: output,
        notices: vec![],
    })
}

pub(super) fn render_value(request: &Request, value: &Value) -> String {
    if request.json {
        return format!("{}\n", serde_json::to_string(value).unwrap());
    }
    let revision = value["revision"].as_str().unwrap_or("");
    let mut output = String::new();
    if let Some(from) = value["from"].as_str() {
        output += &format!("from {from}; ");
    }
    if revision.is_empty() {
        output += &format!("history; complete {}\n", value["complete"]);
    } else {
        output += &format!("rev {revision}; complete {}\n", value["complete"]);
    }
    if let Some(content) = value["content"].as_str() {
        output += &format!(
            "scope {}; target {}\n\n{content}",
            path_text(&value["scope"]),
            value["target"].as_str().unwrap_or("D")
        );
    }
    for (key, kind) in [("changes", 0), ("matches", 1), ("nodes", 2), ("entries", 3)] {
        if let Some(items) = value[key].as_array() {
            if kind <= 1 {
                output += &format!(
                    "{key} {}\n",
                    value[if kind == 0 { "total" } else { "match_count" }]
                );
            }
            for item in items {
                if kind < 3 {
                    output += &format!("\n{}\n", path_text(&item["path"]));
                }
                match kind {
                    0 => {
                        for key in ["before_comment", "current_comment"] {
                            if let Some(stamp) = item[key]["source"].as_str() {
                                output += &format!("{key}: {stamp}\n");
                            }
                        }
                        if item["context_omitted_before"] == true {
                            output += "context omitted before\n";
                        }
                        output += &format!(
                            "BEFORE: {}\nCURRENT: {}\n",
                            item["before"].as_str().unwrap(),
                            item["current"].as_str().unwrap()
                        );
                        if item["context_omitted_after"] == true {
                            output += "context omitted after\n";
                        }
                        if item["change_complete"] == false {
                            output += "changed source continues\n";
                        }
                    }
                    1 => {
                        output += &format!("target {}; matched source: {}\ncontext before: {}\ncontext after: {}\n",item["target"].as_str().unwrap(),item["content"].as_str().unwrap(),item["context_before"].as_str().unwrap(),item["context_after"].as_str().unwrap());
                    }
                    2 => output += &format!("target {}\n", item["target"].as_str().unwrap()),
                    _ => {
                        output += &format!(
                            "{}; observed {}\n",
                            item["revision"].as_str().unwrap(),
                            item["observed_at"].as_str().unwrap_or("unknown")
                        )
                    }
                }
            }
        }
    }
    if let Some(cursor) = value["next_cursor"].as_str() {
        output += &format!("\npending source or results\nnext_cursor {cursor}\n");
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output
}
pub(super) fn path_text(value: &Value) -> String {
    let labels: Vec<_> = value
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    format!(
        "[{}]",
        if labels.is_empty() {
            "document".into()
        } else {
            labels.join(" > ")
        }
    )
}
