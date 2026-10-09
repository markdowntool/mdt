//! The outline of a note: every unit at its level, what stands
//! beneath each, and the walk between siblings.
//!
//! A note is a sequence of **units** — a heading, a paragraph, a list item at
//! any depth, a quote, a fence — and each has a **level**; smaller is higher.
//! A unit's **face** is the line that stands for it: where the fold's triangle
//! goes and where the caret lands. What a fold on a unit hides is its own
//! tail (a list item's continuation lines and nested content, as the markdown
//! parser assigns them) and every unit after it that stands deeper, up to the
//! first that does not. The blank lines among the hidden lines hide with them;
//! the ones after the last hidden line — the gap before the next sibling —
//! stay.
//!
//! Everything here is a pure function of the text and its block map, so the
//! shell can keep a set of folded faces and ask. Nothing here knows what a
//! row is.

use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::blocks::{parser_options, Block, BlockKind};
use crate::lists::indent_depth;

/// The level of body text at the column: a paragraph with no indent. Headings
/// are above it (1–6); an indented paragraph is one level deeper per indent;
/// a list item or a quote is one deeper again ("lists are a hierarchy below
/// normal").
pub const BODY: u8 = 7;
/// The level of a top-level list item, and of a quote at the column.
pub const ITEM: u8 = BODY + 1;

/// One unit of the outline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The block the unit is in.
    pub block: usize,
    /// The unit's own territory: its face and its tail, nested units
    /// included. Ends at the end of its last line's text — never on a
    /// newline, never in the blank lines after it.
    pub range: Range<usize>,
    /// The line that stands for it: the first line of `range`, without its
    /// newline.
    pub face: Range<usize>,
    /// Smaller is higher.
    pub level: u8,
    /// Whether a fold may stand on this unit at all. A quote, a fence, a
    /// table, a rule are only ever hidden, never owners.
    pub owner: bool,
    /// Whether the unit's own lines past the face hide under its fold — true
    /// for a list item (its continuation lines and nested content are what
    /// is beneath it), false for a paragraph (its lines are all its face).
    tail_folds: bool,
}

/// The units of `text`, in order. Nested list items follow their parent and
/// precede its next sibling, as they do in the file.
pub fn units(text: &str, blocks: &[Block]) -> Vec<Unit> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut source_list = None;
    for (i, block) in blocks.iter().enumerate() {
        if let Some(source) = block.source_list.as_ref() {
            if source_list.as_ref() != Some(&source.range) {
                list_fragment_units(text, blocks, i, &source.range, &mut out);
                source_list = Some(source.range.clone());
            }
            continue;
        }
        let range = trimmed(text, block.range.clone());
        if range.is_empty() {
            continue;
        }
        match block.kind {
            BlockKind::Heading(level) => out.push(Unit {
                block: i,
                face: first_line(text, &range),
                range,
                level,
                owner: true,
                tail_folds: false,
            }),
            BlockKind::Paragraph => {
                let face = first_line(text, &range);
                let level = BODY.saturating_add(indent_depth(&text[face.clone()]).min(200) as u8);
                out.push(Unit {
                    block: i,
                    face,
                    range,
                    level,
                    owner: true,
                    tail_folds: false,
                });
            }
            BlockKind::List => items_of(text, i, &range, &mut out),
            BlockKind::BlockQuote => {
                let face = first_line(text, &range);
                let level = ITEM.saturating_add(indent_depth(&text[face.clone()]).min(200) as u8);
                out.push(Unit {
                    block: i,
                    face,
                    range,
                    level,
                    owner: false,
                    tail_folds: false,
                });
            }
            BlockKind::CodeBlock
            | BlockKind::Rule
            | BlockKind::Html
            | BlockKind::Table
            | BlockKind::Other => out.push(Unit {
                block: i,
                face: first_line(text, &range),
                range,
                level: BODY,
                owner: false,
                tail_folds: false,
            }),
        }
    }
    out
}

/// Parse an original list once even when the editor exposes many comments.
/// Item ownership includes their comment tails; each face points at the
/// editor fragment that actually draws it. Lists inside a comment stay body
/// text rather than growing a second set of outline items.
fn list_fragment_units(
    text: &str,
    blocks: &[Block],
    first: usize,
    range: &Range<usize>,
    out: &mut Vec<Unit>,
) {
    let mut units = Vec::new();
    items_of(text, first, range, &mut units);
    units.retain_mut(|unit| {
        let Some(index) = blocks
            .partition_point(|block| block.range.start <= unit.face.start)
            .checked_sub(1)
        else {
            return false;
        };
        unit.block = index;
        blocks[index].kind == BlockKind::List
    });
    for (offset, block) in blocks[first..]
        .iter()
        .take_while(|block| {
            block
                .source_list
                .as_ref()
                .is_some_and(|source| source.range == *range)
        })
        .enumerate()
    {
        if block.kind == BlockKind::BlockQuote {
            let range = trimmed(text, block.range.clone());
            units.push(Unit {
                block: first + offset,
                face: first_line(text, &range),
                range,
                level: ITEM,
                owner: false,
                tail_folds: false,
            });
        }
    }
    units.sort_by_key(|unit| unit.face.start);
    let mut parents: Vec<(usize, u8)> = Vec::new();
    for mut unit in units {
        while parents
            .last()
            .is_some_and(|(end, _)| *end <= unit.face.start)
        {
            parents.pop();
        }
        if unit.owner {
            parents.push((unit.range.end, unit.level));
        } else if let Some((_, level)) = parents.last() {
            unit.level = level.saturating_add(1);
        }
        out.push(unit);
    }
}

/// A list block's items at every depth, as the parser nests them: an item's
/// range runs from its marker to the end of its last own line, nested items
/// and continuation lines included — lazy continuations too, which is the
/// parser's call and not the indent's.
fn items_of(text: &str, block: usize, range: &Range<usize>, out: &mut Vec<Unit>) {
    let src = &text[range.clone()];
    let mut depth = 0usize;
    let mut items: Vec<(usize, Range<usize>)> = Vec::new();
    for (event, r) in Parser::new_ext(src, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Item) => depth += 1,
            Event::End(TagEnd::Item) => {
                depth = depth.saturating_sub(1);
                items.push((depth, range.start + r.start..range.start + r.end));
            }
            _ => {}
        }
    }
    // Ends come innermost first; the file's order is by start.
    items.sort_by_key(|(_, r)| r.start);
    for (depth, item) in items {
        // The parser's range begins at the marker; the face is the whole
        // line, indent included, because that is the line the renderer
        // draws and the caret's line starts at.
        let line_start = text[range.start..item.start]
            .rfind('\n')
            .map(|i| range.start + i + 1)
            .unwrap_or(range.start);
        let item = trimmed(text, line_start..item.end);
        if item.is_empty() {
            continue;
        }
        out.push(Unit {
            block,
            face: first_line(text, &item),
            range: item,
            level: ITEM.saturating_add(depth.min(200) as u8),
            owner: true,
            tail_folds: true,
        });
    }
}

/// `range` without the whitespace and newlines it ends in.
fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let end = range.start + text[range.clone()].trim_end().len();
    range.start..end
}

/// The first line of `range`, without its newline.
fn first_line(text: &str, range: &Range<usize>) -> Range<usize> {
    let end = text[range.clone()]
        .find('\n')
        .map(|i| range.start + i)
        .unwrap_or(range.end);
    range.start..end
}

/// The bytes a fold on `units[i]` hides: from the start of the line after its
/// face to the end of the last hidden line's text. `None` when nothing stands
/// beneath it — such a unit has no triangle.
///
/// Every unit after it that is deeper is hidden, up to the first that is
/// not; blank lines never end an extent (they are not units), and the note's
/// end does. The blank lines after the last hidden line are outside the
/// extent on purpose: they are the gap before the next sibling.
pub fn extent(units: &[Unit], i: usize) -> Option<Range<usize>> {
    let u = units.get(i)?;
    if !u.owner {
        return None;
    }
    // What stays: the face alone for an item (its tail is what is beneath
    // it), every one of its own lines for anything else.
    let shown_end = if u.tail_folds {
        u.face.end
    } else {
        u.range.end
    };
    let mut end = u.range.end;
    for v in &units[i + 1..] {
        if v.level <= u.level {
            break;
        }
        end = end.max(v.range.end);
    }
    (end > shown_end).then(|| shown_end + 1..end)
}

/// Every fold extent in one linear pass. The stack carries completed
/// subtrees; a unit absorbs only the following units deeper than itself.
/// This is the cached counterpart of [`extent`] for a document renderer.
pub fn extents(units: &[Unit]) -> Vec<Option<Range<usize>>> {
    let mut result = vec![None; units.len()];
    let mut following: Vec<(u8, usize)> = Vec::new();
    for (i, unit) in units.iter().enumerate().rev() {
        let mut end = unit.range.end;
        while following
            .last()
            .is_some_and(|(level, _)| *level > unit.level)
        {
            end = end.max(following.pop().unwrap().1);
        }
        following.push((unit.level, end));
        let shown_end = if unit.tail_folds {
            unit.face.end
        } else {
            unit.range.end
        };
        if unit.owner && end > shown_end {
            result[i] = Some(shown_end + 1..end);
        }
    }
    result
}

/// The hidden bytes of a note whose folded units are the ones `folded` says,
/// as sorted, disjoint ranges. A line is hidden when it starts inside one
/// ([`is_hidden`]).
pub fn hidden(units: &[Unit], folded: impl Fn(&Unit) -> bool) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (i, u) in units.iter().enumerate() {
        if !folded(u) {
            continue;
        }
        let Some(r) = extent(units, i) else {
            continue;
        };
        match ranges.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => ranges.push(r),
        }
    }
    ranges
}

/// Whether the line starting at `line_start` is hidden.
pub fn is_hidden(hidden: &[Range<usize>], line_start: usize) -> bool {
    let i = hidden.partition_point(|r| r.start <= line_start);
    i > 0 && line_start < hidden[i - 1].end
}

/// The unit whose face is the line starting at `line_start`, if any.
pub fn face_at(units: &[Unit], line_start: usize) -> Option<usize> {
    let i = units.partition_point(|u| u.face.start < line_start);
    (i < units.len() && units[i].face.start == line_start).then_some(i)
}

/// The innermost unit whose territory holds `offset`; a nested item before
/// the item it is nested in. `None` on a blank line or past the note.
pub fn unit_at(units: &[Unit], offset: usize) -> Option<usize> {
    let i = units.partition_point(|u| u.range.start <= offset);
    units[..i].iter().rposition(|u| offset <= u.range.end)
}

/// The unit a position belongs to for the purposes of the walk: the one
/// holding it, or — on a blank line — the last one before it.
pub fn origin_at(units: &[Unit], offset: usize) -> Option<usize> {
    unit_at(units, offset).or_else(|| {
        units
            .partition_point(|u| u.range.start <= offset)
            .checked_sub(1)
    })
}

/// The walk (⌥⌘↑ / ⌥⌘↓, ⌃↑ / ⌃↓): from unit `i`, the nearest unit before or
/// after it whose level is `i`'s or smaller — the sibling or the parent,
/// never a child — whose face is not hidden.
pub fn sibling(units: &[Unit], i: usize, forward: bool, hidden: &[Range<usize>]) -> Option<usize> {
    let level = units.get(i)?.level;
    let qualifies = |u: &Unit| u.level <= level && !is_hidden(hidden, u.face.start);
    if forward {
        units[i + 1..].iter().position(qualifies).map(|p| i + 1 + p)
    } else {
        units[..i].iter().rposition(qualifies)
    }
}

/// The nearest unit before `i` that `i` stands beneath — the one a fold on
/// which would hide `i`. `None` at the top of the note's outline.
pub fn parent(units: &[Unit], i: usize) -> Option<usize> {
    let level = units.get(i)?.level;
    units[..i].iter().rposition(|u| u.level < level && u.owner)
}

/// The edit units of a block: what the editor holds at a time, and what the
/// renderer draws a row for. One hard line of a list or
/// of a paragraph whose lines render alone; the whole block otherwise —
/// a quote line rendered by itself is its own quote, half a fence is not a
/// fence, and a `**bold` split over two rows would show its markers.
pub fn edit_units(text: &str, block: &Block) -> Vec<Range<usize>> {
    use crate::blocks::{block_lines, lines_render_independently, list_items};
    let range = block.range.clone();
    let src = &text[range.clone()];
    match block.kind {
        BlockKind::List => {
            if lines_render_independently(src) {
                block_lines(text, range)
            } else {
                let items = list_items(text, range.clone());
                if items.is_empty() {
                    vec![range]
                } else {
                    items
                }
            }
        }
        BlockKind::Paragraph => {
            if src.contains('\n') && lines_render_independently(src) {
                block_lines(text, range)
            } else {
                vec![range]
            }
        }
        _ => vec![range],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::block_map;

    fn outline(text: &str) -> Vec<Unit> {
        units(text, &block_map(text))
    }

    fn faces(text: &str) -> Vec<(&str, u8)> {
        outline(text)
            .iter()
            .map(|u| (&text[u.face.clone()], u.level))
            .collect()
    }

    /// The lines left on screen once `face` is folded.
    fn visible_after_folding<'a>(text: &'a str, face: &str) -> Vec<&'a str> {
        let units = outline(text);
        let hidden = hidden(&units, |u| &text[u.face.clone()] == face);
        let mut lines = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            if !is_hidden(&hidden, start) {
                lines.push(line.trim_end_matches('\n'));
            }
            start += line.len();
        }
        if text.ends_with('\n') {
            // The empty "line" after the final newline is not a line.
        } else if lines.is_empty() {
            lines.push("");
        }
        lines
    }

    #[test]
    fn cached_extents_match_individual_extents_for_every_outline_shape() {
        // All level arrangements through six units include sibling boundaries,
        // nested owners, non-owning blocks and multiline item tails.
        for encoded in 0usize..4usize.pow(6) {
            let mut code = encoded;
            let units: Vec<_> = (0..6)
                .map(|i| {
                    let level = 1 + (code % 4) as u8;
                    code /= 4;
                    Unit {
                        block: i,
                        range: i * 10..i * 10 + 8,
                        face: i * 10..i * 10 + 3,
                        level,
                        owner: i % 3 != 0,
                        tail_folds: i % 2 == 0,
                    }
                })
                .collect();
            assert_eq!(
                extents(&units),
                (0..units.len())
                    .map(|i| extent(&units, i))
                    .collect::<Vec<_>>()
            );
        }
        let text = "# Heading\n\n- parent\n  continuation\n  - child\n    - grandchild\n- sibling\n\n> quote\n\n## Next\n\nBody.\n";
        let units = outline(text);
        assert_eq!(
            extents(&units),
            (0..units.len())
                .map(|i| extent(&units, i))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_heading_stands_over_body_text_and_a_paragraph_over_its_list() {
        let text = "# T\n\npara\n\n- a\n- b\n\n## S\n";
        assert_eq!(
            faces(text),
            vec![("# T", 1), ("para", 7), ("- a", 8), ("- b", 8), ("## S", 2)]
        );
    }

    #[test]
    fn a_heading_straight_onto_a_list_owns_it() {
        let text = "## H\n- a\n- b\n## I\n";
        assert_eq!(visible_after_folding(text, "## H"), vec!["## H", "## I"]);
    }

    #[test]
    fn a_paragraph_folds_the_list_the_quote_and_the_indented_paragraph_under_it() {
        let text = "para\n\n- a\n  - b\n\n> quoted\n\n  indented\n\nnext\n";
        assert_eq!(
            visible_after_folding(text, "para"),
            vec!["para", "", "next"]
        );
    }

    #[test]
    fn the_gap_before_the_next_sibling_stays_and_the_ones_inside_hide() {
        let text = "## H\n\npara one\n\npara two\n\n\n## I\n";
        assert_eq!(
            visible_after_folding(text, "## H"),
            vec!["## H", "", "", "## I"]
        );
    }

    #[test]
    fn a_heading_with_nothing_under_it_has_no_extent() {
        let text = "## H\n\n## I\n";
        let units = outline(text);
        assert_eq!(extent(&units, 0), None);
        assert!(extent(&units, 1).is_none());
    }

    #[test]
    fn an_indented_paragraph_is_beside_a_top_level_item_not_over_it() {
        let text = "  indented\n\n- item\n";
        assert_eq!(faces(text), vec![("  indented", 8), ("- item", 8)]);
        assert_eq!(extent(&outline(text), 0), None);
    }

    #[test]
    fn a_table_under_a_paragraph_ends_its_fold() {
        let text = "para\n\n- a\n\n| x | y |\n| - | - |\n| 1 | 2 |\n\n- b\n";
        assert_eq!(
            visible_after_folding(text, "para"),
            vec!["para", "", "| x | y |", "| - | - |", "| 1 | 2 |", "", "- b"]
        );
    }

    #[test]
    fn the_boards_reply_lines_are_inside_their_item() {
        // The research note's shape: an item, then three-space-indented
        // quote lines and an ACTION line, all the item's own by the parser.
        let text = "- [ ] #20 the ask\n   > Claude: honest answer\n   > Claude: THE WALK-THROUGH\n   ACTION: step one\n- [ ] #21 next\n";
        assert_eq!(
            faces(text),
            vec![("- [ ] #20 the ask", 8), ("- [ ] #21 next", 8)]
        );
        assert_eq!(
            visible_after_folding(text, "- [ ] #20 the ask"),
            vec!["- [ ] #20 the ask", "- [ ] #21 next"]
        );
    }

    #[test]
    fn a_lazy_continuation_belongs_to_its_item_whatever_its_indent() {
        let text = "1. foo\n bar\n2. baz\n";
        assert_eq!(faces(text), vec![("1. foo", 8), ("2. baz", 8)]);
        assert_eq!(
            visible_after_folding(text, "1. foo"),
            vec!["1. foo", "2. baz"]
        );
    }

    #[test]
    fn nested_items_are_units_a_level_down_and_fold_with_their_parent() {
        let text = "- a\n  - b\n    - c\n  - d\n- e\n";
        assert_eq!(
            faces(text),
            vec![
                ("- a", 8),
                ("  - b", 9),
                ("    - c", 10),
                ("  - d", 9),
                ("- e", 8)
            ]
        );
        assert_eq!(visible_after_folding(text, "- a"), vec!["- a", "- e"]);
        assert_eq!(
            visible_after_folding(text, "  - b"),
            vec!["- a", "  - b", "  - d", "- e"]
        );
    }

    #[test]
    fn a_loose_list_stays_loose_around_a_collapsed_item() {
        let text = "- a\n  - b\n\n- c\n";
        assert_eq!(visible_after_folding(text, "- a"), vec!["- a", "", "- c"]);
    }

    #[test]
    fn two_lists_parted_by_a_blank_line_are_one_block_and_fold_by_item() {
        let text = "- a\n\n- b\n  - c\n";
        assert_eq!(visible_after_folding(text, "- b"), vec!["- a", "", "- b"]);
        assert_eq!(extent(&outline(text), 0), None, "a has nothing nested");
    }

    #[test]
    fn a_fence_inside_an_item_is_the_items_tail_and_a_fence_at_the_column_is_a_leaf() {
        let text = "- a\n  ```\n  - not an item\n  ```\n- b\n\n```\ncode\n```\n";
        assert_eq!(faces(text), vec![("- a", 8), ("- b", 8), ("```", 7)]);
        assert_eq!(
            visible_after_folding(text, "- a"),
            vec!["- a", "- b", "", "```", "code", "```"]
        );
        assert!(!outline(text)[2].owner);
    }

    #[test]
    fn a_quote_is_hidden_never_an_owner_and_nested_quotes_are_one_unit() {
        let text = "para\n\n> a\n> > b\n\nnext\n";
        let units = outline(text);
        assert_eq!(faces(text), vec![("para", 7), ("> a", 8), ("next", 7)]);
        assert!(!units[1].owner);
        assert_eq!(
            visible_after_folding(text, "para"),
            vec!["para", "", "next"]
        );
    }

    #[test]
    fn a_paragraphs_own_lines_never_fold_away_from_it() {
        let text = "The plan is:\n   as follows\n\n- a\n";
        let units = outline(text);
        assert_eq!(faces(text), vec![("The plan is:", 7), ("- a", 8)]);
        assert_eq!(
            visible_after_folding(text, "The plan is:"),
            vec!["The plan is:", "   as follows"]
        );
        assert_eq!(units[0].range, 0..text.find("\n\n").unwrap());
    }

    #[test]
    fn a_note_that_starts_with_a_list_folds_from_its_first_item() {
        let text = "- a\n  - b\n- c\n";
        assert_eq!(visible_after_folding(text, "- a"), vec!["- a", "- c"]);
    }

    #[test]
    fn a_ticked_item_over_unticked_children_folds_them_all_the_same() {
        let text = "- [x] done\n  - [ ] open\n- [ ] other\n";
        assert_eq!(
            visible_after_folding(text, "- [x] done"),
            vec!["- [x] done", "- [ ] other"]
        );
    }

    #[test]
    fn a_fold_at_the_end_of_the_note_keeps_the_trailing_blank_lines() {
        let text = "## H\n\npara\n\n\n";
        assert_eq!(visible_after_folding(text, "## H"), vec!["## H", "", ""]);
    }

    #[test]
    fn nested_folds_merge_into_one_hidden_range() {
        let text = "# T\n\n## S\n\npara\n\n# U\n";
        let units = outline(text);
        let h = hidden(&units, |u| matches!(&text[u.face.clone()], "# T" | "## S"));
        assert_eq!(h.len(), 1);
        assert_eq!(&text[h[0].clone()], "\n## S\n\npara");
    }

    #[test]
    fn is_hidden_answers_by_line_start() {
        let text = "## H\n\npara\n\n## I\n";
        let units = outline(text);
        let h = hidden(&units, |u| &text[u.face.clone()] == "## H");
        assert!(!is_hidden(&h, 0));
        assert!(is_hidden(&h, 5), "the blank line under the heading");
        assert!(is_hidden(&h, 6), "para");
        assert!(!is_hidden(&h, 11), "the gap before I");
        assert!(!is_hidden(&h, 12), "I");
    }

    #[test]
    fn the_walk_goes_to_the_sibling_or_the_parent_never_a_child() {
        let text = "# T\n\n## A\n\npara a\n\n### A1\n\n- x\n  - y\n- z\n\n## B\n";
        let units = outline(text);
        let at = |face: &str| {
            units
                .iter()
                .position(|u| &text[u.face.clone()] == face)
                .unwrap()
        };
        let go = |face: &str, forward: bool| {
            sibling(&units, at(face), forward, &[]).map(|i| &text[units[i].face.clone()])
        };
        assert_eq!(go("## B", false), Some("## A"), "past A's whole subtree");
        assert_eq!(
            go("### A1", false),
            Some("## A"),
            "the parent, when no sibling"
        );
        assert_eq!(go("### A1", true), Some("## B"), "up and out");
        assert_eq!(go("para a", false), Some("## A"));
        assert_eq!(
            go("para a", true),
            Some("### A1"),
            "a heading is above a paragraph"
        );
        assert_eq!(go("  - y", false), Some("- x"));
        assert_eq!(go("  - y", true), Some("- z"));
        assert_eq!(go("- z", true), Some("## B"));
        assert_eq!(go("- z", false), Some("- x"), "over y, which is a child");
        assert_eq!(go("# T", false), None);
        assert_eq!(go("## B", true), None);
    }

    #[test]
    fn the_walk_never_lands_on_a_hidden_face() {
        let text = "## A\n\npara\n\n## B\n\npara b\n\n## C\n";
        let units = outline(text);
        let h = hidden(&units, |u| &text[u.face.clone()] == "## A");
        // From B's paragraph backwards: B, then A — never A's hidden paragraph.
        let at = |face: &str| {
            units
                .iter()
                .position(|u| &text[u.face.clone()] == face)
                .unwrap()
        };
        let from = sibling(&units, at("para b"), false, &h).unwrap();
        assert_eq!(&text[units[from].face.clone()], "## B");
        let from = sibling(&units, at("## B"), false, &h).unwrap();
        assert_eq!(&text[units[from].face.clone()], "## A");
    }

    #[test]
    fn origin_on_a_blank_line_is_the_unit_above() {
        let text = "para\n\n- a\n";
        let units = outline(text);
        assert_eq!(unit_at(&units, 5), None, "the blank line");
        assert_eq!(origin_at(&units, 5), Some(0));
        assert_eq!(unit_at(&units, 7), Some(1));
        assert_eq!(face_at(&units, 6), Some(1));
        assert_eq!(face_at(&units, 5), None);
    }

    #[test]
    fn unit_at_is_the_innermost() {
        let text = "- a\n  - b\n- c\n";
        let units = outline(text);
        assert_eq!(unit_at(&units, 0), Some(0));
        assert_eq!(unit_at(&units, 7), Some(1), "inside b, which is inside a");
        assert_eq!(parent(&units, 1), Some(0));
        assert_eq!(parent(&units, 0), None);
    }

    /// A book-sized outline should take one cheap pass. The bound allows a
    /// debug build on a loaded machine while catching a quadratic walk over
    /// a synthetic 3,000-paragraph note and its nested lists.
    #[test]
    fn a_book_sized_outline_is_one_cheap_pass() {
        let mut text = String::from("# Title\n\n");
        for i in 0..3_000 {
            text.push_str(&format!(
                "Paragraph {i} of the book, long enough to be a line of prose.\n\n"
            ));
            if i % 50 == 0 {
                text.push_str("- an item\n  - nested\n- another\n\n");
            }
        }
        let blocks = block_map(&text);
        let started = std::time::Instant::now();
        for _ in 0..10 {
            let units = units(&text, &blocks);
            let h = hidden(&units, |u| u.level == 1);
            assert_eq!(h.len(), 1, "the title folds the whole book");
            assert!(
                is_hidden(&h, text.rfind("Paragraph").unwrap()),
                "to the last paragraph"
            );
        }
        let per_pass = started.elapsed() / 10;
        assert!(
            per_pass < std::time::Duration::from_millis(100),
            "one pass over 3,000 blocks took {per_pass:?}"
        );
    }

    #[test]
    fn edit_units_split_lists_and_independent_paragraphs_by_line() {
        let text = "- a\n- b\n\nline one\nline two\n\n**bold\nacross**\n";
        let blocks = block_map(text);
        let lines = |i: usize| {
            edit_units(text, &blocks[i])
                .into_iter()
                .map(|r| &text[r])
                .collect::<Vec<_>>()
        };
        assert_eq!(lines(0), vec!["- a", "- b"]);
        assert_eq!(lines(1), vec!["line one", "line two"]);
        assert_eq!(
            lines(2).len(),
            1,
            "a straddling marker keeps the block whole"
        );
    }
}
