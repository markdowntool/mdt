//! Current source ownership and conservative correspondence with one prior tree.
//! Matching stays within matched parents: a move is a removal and insertion.

use std::{collections::HashMap, ops::Range};

use mdt_core::{
    blocks::{self, BlockKind},
    outline,
};

use crate::error::{Error, Result};

// Bump when parsing, ownership or matching changes invalidate stored times.
pub(super) const PROFILE: &str = "qm-updates-v1";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Kind {
    Root,
    Preamble,
    Heading,
    Item,
}

#[derive(Debug)]
pub(crate) struct Node {
    kind: Kind,
    level: u8,
    label: String,
    face: Range<usize>,
    own: Range<usize>,
    parent: usize,
    children: Vec<usize>,
    end: usize,
}

pub(crate) struct Tree<'a> {
    pub source: &'a str,
    pub nodes: Vec<Node>,
}

pub(crate) struct ReadNode {
    pub index: usize,
    pub complete: bool,
}

impl<'a> Tree<'a> {
    pub fn outline_nodes(&self) -> impl Iterator<Item = usize> + '_ {
        self.nodes.iter().enumerate().filter_map(|(i, n)| {
            matches!(n.kind, Kind::Root | Kind::Heading | Kind::Item).then_some(i)
        })
    }

    pub fn exact_range(&self, i: usize) -> Range<usize> {
        let boundary = |at| {
            if at == self.source.len() {
                at
            } else {
                self.source[..at].rfind('\n').map_or(0, |line| line + 1)
            }
        };
        let range = self.range(i);
        boundary(range.start)..boundary(range.end)
    }

    pub fn containing(&self, range: &Range<usize>) -> usize {
        self.outline_nodes()
            .filter(|&i| {
                let r = self.exact_range(i);
                r.start <= range.start && range.end <= r.end
            })
            .last()
            .unwrap_or(0)
    }

    pub fn new(source: &'a str) -> Self {
        let blocks = blocks::block_map(source);
        let units = outline::units(source, &blocks);
        let faces: Vec<_> = units
            .iter()
            .filter(|u| {
                matches!(
                    blocks[u.block].kind,
                    BlockKind::Heading(_) | BlockKind::List
                )
            })
            .collect();
        let preamble = faces.first().is_none_or(|u| u.face.start > 0);
        let count = 1 + usize::from(preamble) + faces.len();
        let mut nodes = Vec::with_capacity(count);
        nodes.push(Node {
            kind: Kind::Root,
            level: 0,
            label: String::new(),
            face: 0..0,
            own: 0..0,
            parent: 0,
            children: Vec::new(),
            end: count,
        });
        if preamble {
            nodes[0].children.push(1);
            nodes.push(Node {
                kind: Kind::Preamble,
                level: 0,
                label: "Preamble".into(),
                face: 0..0,
                own: 0..faces.first().map_or(source.len(), |u| u.face.start),
                parent: 0,
                children: Vec::new(),
                end: 2,
            });
        }
        let mut stack = vec![0];
        for (position, unit) in faces.iter().enumerate() {
            let i = nodes.len();
            while nodes[*stack.last().unwrap()].level >= unit.level {
                nodes[stack.pop().unwrap()].end = i;
            }
            let parent = *stack.last().unwrap();
            let kind = if matches!(blocks[unit.block].kind, BlockKind::Heading(_)) {
                Kind::Heading
            } else {
                Kind::Item
            };
            // The fold parser trims an item's final whitespace. Its face start
            // is authoritative; recover the complete first line for literal labels.
            let face_end = source[unit.face.start..]
                .find('\n')
                .map_or(source.len(), |end| unit.face.start + end);
            let face_range = unit.face.start..face_end;
            let face = &source[face_range.clone()];
            let label = match kind {
                Kind::Heading => blocks::heading_inline_text(face),
                _ => item_label(face),
            }
            .to_owned();
            nodes[parent].children.push(i);
            nodes.push(Node {
                kind,
                level: unit.level,
                label,
                face: face_range,
                own: unit.face.start
                    ..faces
                        .get(position + 1)
                        .map_or(source.len(), |u| u.face.start),
                parent,
                children: Vec::new(),
                end: count,
            });
            stack.push(i);
        }
        Self { source, nodes }
    }

    pub fn content(&self, i: usize) -> &'a str {
        &self.source[self.range(i)]
    }

    pub fn range(&self, i: usize) -> Range<usize> {
        let start = self.nodes[i].own.start;
        let end = self
            .nodes
            .get(self.nodes[i].end)
            .map_or(self.source.len(), |n| n.own.start);
        start..end
    }

    pub fn own_content(&self, i: usize) -> Option<&'a str> {
        let node = &self.nodes[i];
        let line_boundary = |at: usize| {
            at == 0 || at == self.source.len() || self.source.as_bytes()[at - 1] == b'\n'
        };
        // A fragment must not cut a shared source line or omit indentation.
        // Leaves already have complete content, so need no second representation.
        (!node.children.is_empty()
            && node.own.start < node.own.end
            && line_boundary(node.own.start)
            && line_boundary(node.own.end))
        .then(|| &self.source[node.own.clone()])
    }

    pub fn path(&self, mut i: usize) -> Vec<&str> {
        let mut path = Vec::new();
        while i != 0 {
            path.push(self.nodes[i].label.as_str());
            i = self.nodes[i].parent;
        }
        path.reverse();
        path
    }

    pub fn select(&self, labels: &[String]) -> Result<usize> {
        if labels.is_empty() {
            return Ok(0);
        }
        let mut selected = None;
        for i in 1..self.nodes.len() {
            // Synthetic preamble/root labels are display context, not source labels.
            if !matches!(self.nodes[i].kind, Kind::Heading | Kind::Item) {
                continue;
            }
            if !self.matches_path(i, labels) {
                continue;
            }
            if selected.replace(i).is_some() {
                return Err(self.selection_error(labels, true));
            }
        }
        selected.ok_or_else(|| self.selection_error(labels, false))
    }

    /// Find literal source text without inventing a second ownership model.
    /// All occurrences must resolve to the same smallest containing subtree.
    pub fn find(&self, selected: usize, text: &str) -> Result<usize> {
        if text.is_empty() {
            return Err(Error::new("usage", "--find needs nonempty literal text"));
        }
        let scope = self.range(selected);
        let mut cursor = scope.start;
        let mut candidates = Vec::new();
        while let Some(offset) = self.source[cursor..scope.end].find(text) {
            let start = cursor + offset;
            let end = start + text.len();
            let mut node = self.nodes.partition_point(|node| node.own.start <= start) - 1;
            while self.range(node).end < end {
                node = self.nodes[node].parent;
            }
            if !candidates.contains(&node) {
                candidates.push(node);
                // Six distinct scopes suffice to report ambiguity and truncation.
                if candidates.len() == 6 {
                    break;
                }
            }
            // Include overlapping occurrences; advance along a UTF-8 boundary.
            cursor = start + self.source[start..].chars().next().unwrap().len_utf8();
        }
        match candidates.as_slice() {
            [] => Err(Error::new(
                "not_found",
                "No literal text match in the selected scope. Check exact wording and case, or use a shorter phrase; omit --node to search the document.",
            )),
            [node] => {
                // A read receipt must name the bytes it actually contains.
                // Duplicate labels and synthetic preambles need a containing
                // scope; the document's empty path always selects node zero.
                let mut node = *node;
                loop {
                    let labels: Vec<_> = self.path(node).into_iter().map(str::to_owned).collect();
                    if self.select(&labels).ok() == Some(node) {
                        return Ok(node);
                    }
                    if node == selected {
                        return Err(Error::new(
                            "ambiguous",
                            "The matching scope has duplicate labels. Read the selected scope without --find, or select a containing scope explicitly.",
                        ));
                    }
                    node = self.nodes[node].parent;
                }
            }
            _ => {
                let mut message = String::from(
                    "Literal text matches more than one subtree. Narrow the phrase or repeat --node with parent labels.\nExact paths:",
                );
                for (position, &node) in candidates.iter().enumerate() {
                    let path = format!("\n  {:?}", self.path(node));
                    if position == 5 || message.len() + path.len() > 8_192 {
                        message.push_str("\nMore matching paths omitted; use qm doc outline FILE.");
                        break;
                    }
                    message.push_str(&path);
                }
                Err(Error::new("ambiguous", message))
            }
        }
    }

    fn matches_path(&self, mut at: usize, labels: &[String]) -> bool {
        labels.iter().rev().all(|label| {
            if at == 0 || !self.matches_label(at, label) {
                return false;
            }
            at = self.nodes[at].parent;
            true
        })
    }

    fn selection_error(&self, labels: &[String], ambiguous: bool) -> Error {
        // Suggestions never select or authorize a write. Preserve literal matching,
        // while making long headings and typographic quotation marks discoverable.
        fn words(label: &str) -> String {
            label
                .split(|c: char| !c.is_alphanumeric())
                .filter(|word| !word.is_empty())
                .map(str::to_lowercase)
                .collect::<Vec<_>>()
                .join(" ")
        }
        let query = labels.last().map(|label| words(label)).unwrap_or_default();
        let query = format!(" {query} ");
        let mut message = if ambiguous {
            format!("More than one node matches {labels:?}")
        } else {
            format!("No current node matches {labels:?}")
        };
        let mut shown = 0;
        let mut shown_paths = Vec::new();
        let mut duplicate_path = false;
        for i in 1..self.nodes.len() {
            if !matches!(self.nodes[i].kind, Kind::Heading | Kind::Item) {
                continue;
            }
            let related = if ambiguous {
                self.matches_path(i, labels)
            } else {
                query != "  " && format!(" {} ", words(&self.nodes[i].label)).contains(&query)
            };
            if !related {
                continue;
            }
            let path = format!("\n  {:?}", self.path(i));
            if shown == 5 || message.len() + path.len() > 8_192 {
                message.push_str("\nMore candidates omitted; use qm doc outline FILE.");
                break;
            }
            if shown == 0 {
                message.push_str("\nExact paths:");
            }
            message.push_str(&path);
            duplicate_path |= shown_paths.contains(&path);
            shown_paths.push(path);
            shown += 1;
        }
        if shown > 0 {
            message.push_str(if duplicate_path {
                "\nIdentical paths cannot select separate nodes. Read a containing scope, or use qm read FILE --find TEXT with a unique phrase."
            } else {
                "\nRepeat --node for successive labels."
            });
        }
        if shown == 0 && ambiguous {
            message.push_str("; repeat --node with its parent labels");
        }
        Error::new(if ambiguous { "ambiguous" } else { "not_found" }, message)
    }

    fn matches_label(&self, i: usize, label: &str) -> bool {
        let node = &self.nodes[i];
        if node.label.trim_end() == label.trim_end() {
            return true;
        }
        let face = self.source[node.face.clone()].trim();
        let label = label.trim_end();
        let digits = face.bytes().take_while(u8::is_ascii_digit).count();
        node.kind == Kind::Item
            && digits > 0
            && matches!(face.as_bytes().get(digits), Some(b'.' | b')'))
            && (label == &face[..digits] || label == &face[..digits + 1])
    }

    pub fn results(
        &self,
        times: &[i64],
        subtree_times: &[i64],
        selected: usize,
        since: Option<i64>,
    ) -> Vec<ReadNode> {
        let Some(since) = since else {
            return vec![ReadNode {
                index: selected,
                complete: true,
            }];
        };
        let mut results = Vec::new();
        let mut i = selected;
        while i < self.nodes[selected].end {
            if times[i] >= since {
                let complete = subtree_times[i] >= since || self.own_content(i).is_none();
                results.push(ReadNode { index: i, complete });
                // Short parent text does not cover changed descendants. Visit
                // those independently instead of treating the parent as read.
                i = if complete { self.nodes[i].end } else { i + 1 };
            } else {
                i += 1;
            }
        }
        // A headingless, list-free document (including empty) is one root result.
        if selected == 0
            && self.nodes.len() == 2
            && self.nodes[1].kind == Kind::Preamble
            && !results.is_empty()
        {
            return vec![ReadNode {
                index: 0,
                complete: true,
            }];
        }
        results
    }

    fn key(&self, i: usize) -> (Kind, &'a str) {
        (self.nodes[i].kind, &self.source[self.nodes[i].face.clone()])
    }
}

/// A reply may add nodes, but it cannot hide or reparent an existing one.
/// Compare exact shifted source positions, so duplicate labels need no matching.
fn item_label(face: &str) -> &str {
    let face = face.trim_start_matches([' ', '\t']).trim_end_matches('\r');
    if let Some(rest) = face.strip_prefix(['-', '+', '*']) {
        return rest.trim_start_matches([' ', '\t']);
    }
    let digits = face.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && matches!(face.as_bytes().get(digits), Some(b'.' | b')')) {
        return face[digits + 1..].trim_start_matches([' ', '\t']);
    }
    face
}

/// Each mapped parent compares its direct children once. Unique faces anchor
/// correspondence; an isolated replacement between ordered anchors is an edit.
/// Uncertain runs get fresh times and stamp their enclosing parent. We never
/// match descendants through an uncertain parent or guess duplicate occurrences.
pub(super) fn compare(
    old: &Tree<'_>,
    new: &Tree<'_>,
    old_times: &[i64],
    old_subtree_times: &[i64],
    now: i64,
) -> (Vec<i64>, Vec<i64>) {
    if old.source == new.source {
        return (old_times.to_vec(), old_subtree_times.to_vec());
    }
    let hints = parent_hints(old, new);
    let mut times = vec![now; new.nodes.len()];
    let mut subtree_times = vec![now; new.nodes.len()];
    let mut pending = vec![(0, 0)];
    while let Some((a, b)) = pending.pop() {
        let before = &old.nodes[a];
        let after = &new.nodes[b];
        subtree_times[b] = old_subtree_times[a];
        if old.source[before.own.clone()] == new.source[after.own.clone()] {
            times[b] = old_times[a];
        }
        let left = &before.children;
        let right = &after.children;
        let mut counts = HashMap::new();
        for (position, &i) in left.iter().enumerate() {
            let entry = counts.entry(old.key(i)).or_insert((0, 0, position));
            entry.0 += 1;
        }
        for &i in right {
            counts.entry(new.key(i)).or_insert((0, 0, 0)).1 += 1;
        }
        let mut matches: Vec<Option<usize>> = right
            .iter()
            .map(|&i| {
                let &(old_count, new_count, position) = &counts[&new.key(i)];
                (old_count == 1 && new_count == 1).then_some(position)
            })
            .collect();
        // Unchanged children can identify a renamed heading even when a new
        // sibling was also added. Conflicting parent hints are discarded.
        let positions: HashMap<_, _> = left.iter().enumerate().map(|(p, &i)| (i, p)).collect();
        let mut used = vec![false; left.len()];
        for &position in matches.iter().flatten() {
            used[position] = true;
        }
        for (position, &i) in right.iter().enumerate() {
            if matches[position].is_some() {
                continue;
            }
            if let Some(previous) = hints[i].and_then(|p| positions.get(&p).copied()) {
                if !used[previous] && old.nodes[left[previous]].kind == new.nodes[i].kind {
                    matches[position] = Some(previous);
                    used[previous] = true;
                }
            }
        }
        let mut copied = vec![false; right.len()];
        let ordered = matches
            .iter()
            .flatten()
            .copied()
            .try_fold(None, |last, i| {
                if last.is_some_and(|last| i < last) {
                    Err(())
                } else {
                    Ok(Some(i))
                }
            })
            .is_ok();
        if ordered {
            let mut old_start = 0;
            let mut new_start = 0;
            // End sentinel lets an isolated final leaf be renamed, too.
            for new_end in 0..=right.len() {
                let old_end = if new_end == right.len() {
                    left.len()
                } else if let Some(i) = matches[new_end] {
                    i
                } else {
                    continue;
                };
                let old_gap = &left[old_start..old_end];
                let new_gap = &right[new_start..new_end];
                if old_gap.len() == 1
                    && new_gap.len() == 1
                    && old.nodes[old_gap[0]].kind == new.nodes[new_gap[0]].kind
                {
                    matches[new_start] = Some(old_start);
                } else if !old_gap.is_empty()
                    && old_gap.len() == new_gap.len()
                    && old_gap.iter().zip(new_gap).all(|(&x, &y)| {
                        old.key(x) == new.key(y)
                            && old.content(x) == new.content(y)
                            && old.nodes[x].end - x == new.nodes[y].end - y
                    })
                {
                    // Entire unchanged runs, including their subtrees, are safe.
                    // Copy once and do not descend into their duplicate children.
                    for (&x, &y) in old_gap.iter().zip(new_gap) {
                        times[y..new.nodes[y].end].copy_from_slice(&old_times[x..old.nodes[x].end]);
                        subtree_times[y..new.nodes[y].end]
                            .copy_from_slice(&old_subtree_times[x..old.nodes[x].end]);
                    }
                    for (offset, slot) in matches[new_start..new_end].iter_mut().enumerate() {
                        *slot = Some(old_start + offset);
                    }
                    copied[new_start..new_end].fill(true);
                }
                old_start = old_end + 1;
                new_start = new_end + 1;
            }
        }
        let mut retained = vec![false; left.len()];
        for (position, matched) in matches.into_iter().enumerate() {
            if let Some(previous) = matched {
                retained[previous] = true;
                if !copied[position] {
                    pending.push((left[previous], right[position]));
                }
            }
        }
        if !ordered || retained.contains(&false) {
            times[b] = now;
            subtree_times[b] = now;
        }
    }
    (times, subtree_times)
}

fn parent_hints(old: &Tree<'_>, new: &Tree<'_>) -> Vec<Option<usize>> {
    let mut unique = HashMap::new();
    for i in 1..old.nodes.len() {
        let entry = unique.entry(old.key(i)).or_insert((0, 0, i));
        entry.0 += 1;
    }
    for i in 1..new.nodes.len() {
        unique.entry(new.key(i)).or_insert((0, 0, 0)).1 += 1;
    }
    // Two different candidate partners invalidate a hint in both directions.
    fn suggest(hint: &mut Option<usize>, candidate: usize) {
        match hint {
            None => *hint = Some(candidate),
            Some(i) if *i != candidate => *i = usize::MAX,
            _ => {}
        }
    }
    let mut forward = vec![None; old.nodes.len()];
    let mut backward = vec![None; new.nodes.len()];
    for i in 1..new.nodes.len() {
        let &(a, b, previous) = &unique[&new.key(i)];
        if a == 1 && b == 1 {
            let x = old.nodes[previous].parent;
            let y = new.nodes[i].parent;
            suggest(&mut forward[x], y);
            suggest(&mut backward[y], x);
        }
    }
    backward
        .into_iter()
        .enumerate()
        .map(|(i, candidate)| candidate.filter(|&x| x != usize::MAX && forward[x] == Some(i)))
        .collect()
}
