//! Markdown block map: top-level blocks with byte-span provenance, built with
//! pulldown-cmark. This is the foundation the block-swap live preview (task
//! 0030) stands on; v1 uses it for heading lookup and code-fence awareness.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    CodeBlock,
    BlockQuote,
    List,
    Rule,
    Html,
    /// A GFM pipe table: a grid while rendered, its whole raw
    /// source while edited.
    Table,
    /// Anything else (footnotes, metadata...). Rendered as plain text —
    /// never a crash.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// Byte range of the block's source, including markers.
    pub range: Range<usize>,
    pub kind: BlockKind,
    /// An unfinished fenced block owns its final empty source row. Computed
    /// while parsing so rendering never scans the fence for a delimiter.
    pub unclosed_fence: bool,
    /// Editor-only fragments share the original list's Markdown context.
    /// Ordinary block_map/export never splits a list and leaves this absent.
    pub source_list: Option<Arc<ListSource>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSource {
    pub range: Range<usize>,
    /// Display replacements keyed by absolute document offsets. Computed
    /// once per split list, never once per visible fragment or frame.
    pub numbers: HashMap<usize, String>,
}

/// The extensions Notes reads everywhere it parses markdown — the block map,
/// and the PDF export that follows it block by block.
pub(crate) fn parser_options() -> Options {
    Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES
}

/// Split `text` into top-level blocks. Every block's range is a valid byte
/// range into `text`; blocks are ordered and non-overlapping. Gaps (blank
/// lines) belong to no block.
pub fn block_map(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut depth = 0usize;
    let mut open: Option<(usize, BlockKind, bool)> = None;
    for (event, range) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    let kind = kind_for(&tag);
                    let mut start = range.start;
                    // A paragraph's leading spaces are its own: the
                    // parser's range begins at the first character of text,
                    // and the line begins at the whitespace before it. A
                    // table's likewise — its range begins at the
                    // first `|`, and spaces owned by no block are the orphaned
                    // bytes the 2026-08-24 phantom rows grew from.
                    if matches!(kind, BlockKind::Paragraph | BlockKind::Table) {
                        let bytes = text.as_bytes();
                        while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
                            start -= 1;
                        }
                    }
                    open = Some((
                        start,
                        kind,
                        matches!(tag, Tag::CodeBlock(CodeBlockKind::Fenced(_))),
                    ));
                }
                depth += 1;
            }
            Event::End(_tag_end) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some((start, kind, fenced)) = open.take() {
                        // Blank lines inside a fence are code, including an
                        // unfinished fence's final empty line.
                        let end = if kind == BlockKind::CodeBlock {
                            range.end
                        } else {
                            shed_swallowed_whitespace(text, start, range.end)
                        };
                        let unclosed_fence =
                            fenced && end == text.len() && is_unclosed_fence(&text[start..end]);
                        blocks.push(Block {
                            range: start..end,
                            kind,
                            unclosed_fence,
                            source_list: None,
                        });
                    }
                }
            }
            Event::Rule if depth == 0 => {
                let end = shed_swallowed_whitespace(text, range.start, range.end);
                blocks.push(Block {
                    range: range.start..end,
                    kind: BlockKind::Rule,
                    unclosed_fence: false,
                    source_list: None,
                });
            }
            _ => {}
        }
    }
    split_setext_underlines(text, blocks)
}

/// Fenced code and HTML spans at every nesting depth, including inside replies
/// and list items. Structural insertion must not split these containers.
pub fn code_and_html_ranges(text: &str) -> Vec<Range<usize>> {
    Parser::new_ext(text, parser_options())
        .into_offset_iter()
        .filter_map(|(event, range)| match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_)) | Tag::HtmlBlock) => Some(range),
            _ => None,
        })
        .collect()
}

/// Called only while building the block map, for a parsed fence at EOF.
/// Match the parser's closing delimiter: at most three leading spaces,
/// enough identical markers, then ASCII spaces and an optional CRLF.
fn is_unclosed_fence(source: &str) -> bool {
    let Some((opening, rest)) = source.split_once('\n') else {
        return true;
    };
    let opening = opening.trim_start_matches(' ');
    let Some(&marker) = opening.as_bytes().first() else {
        return true;
    };
    let count = opening.bytes().take_while(|b| *b == marker).count();
    let last = rest
        .strip_suffix('\n')
        .unwrap_or(rest)
        .rsplit('\n')
        .next()
        .unwrap_or("");
    let last = last.strip_suffix('\r').unwrap_or(last);
    let closing = last.trim_start_matches(' ');
    let indent = last.len() - closing.len();
    let closing = closing.trim_end_matches(' ');
    !(indent <= 3 && closing.len() >= count && closing.bytes().all(|b| b == marker))
}

/// Marker lines of an already parsed fenced block, without changing its
/// source geometry. Preview hides their ink, keeping the same rows, spaces
/// and wrapping as editing. An unfinished fence has no closing marker.
pub fn code_fence_marker_ranges(source: &str) -> Vec<Range<usize>> {
    let opening_end = source.find('\n').unwrap_or(source.len());
    let mut ranges = Vec::with_capacity(2);
    ranges.push(0..opening_end);
    if !is_unclosed_fence(source) {
        let end = source.strip_suffix('\n').map_or(source.len(), str::len);
        let start = source[..end].rfind('\n').map_or(0, |at| at + 1);
        ranges.push(start..end);
    }
    ranges
}

/// CommonMark reads a run of `-` or `=` directly under a paragraph as a
/// setext heading underline: the `-` that starts a list under a line of text
/// grows that line into a heading, and the keystroke after — `- i`, a list
/// interrupting a paragraph — shrinks it back (the owner's report,
/// 2026-08-26). the source-preservation contract removes setext headings: writing a list without
/// flicker matters more than the spec, and `#` is the only heading syntax.
/// The text above the underline stays the paragraph it already was, and the
/// underline's own line is read as if it stood alone — `-` the empty list
/// item it is anywhere else, `---` a rule, `===` plain text.
fn split_setext_underlines(text: &str, blocks: Vec<Block>) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !matches!(block.kind, BlockKind::Heading(_)) {
            out.push(block);
            continue;
        }
        // An ATX heading is one line; a setext heading is at least two, the
        // underline last. The line count decides, not a leading `#`: `#tag`
        // is a paragraph line (no space after the run), so `#tag\n---` is a
        // setext shape too.
        let src = text[block.range.clone()].trim_end_matches('\n');
        let Some(newline) = src.rfind('\n') else {
            out.push(block);
            continue;
        };
        let underline = block.range.start + newline + 1;
        out.push(Block {
            range: block.range.start..underline,
            kind: BlockKind::Paragraph,
            unclosed_fence: false,
            source_list: None,
        });
        // The underline, read as if it stood alone. A single line can never
        // be a setext heading, so this recursion ends here.
        for sub in block_map(&text[underline..block.range.end]) {
            out.push(Block {
                range: underline + sub.range.start..underline + sub.range.end,
                kind: sub.kind,
                unclosed_fence: sub.unclosed_fence,
                source_list: None,
            });
        }
    }
    out
}

/// pulldown-cmark's range for a block that only the *next* line could end — a
/// list above all — includes the whitespace it read while deciding the block
/// was over: the newlines under the last item, any whitespace-only lines
/// among them, and the leading spaces of whatever comes next. None of those
/// bytes are the block's. The spaces before a paragraph are that paragraph's
/// own indent, so a range that keeps them
/// overlaps the paragraph's — the map's "ordered and non-overlapping" broken
/// — and `block_lines` hands the list a phantom `"  "` line: drawn as an
/// extra row whenever the caret is elsewhere (the owner's lines shifting
/// down the page, 2026-08-24), a dead stop for Up on the paragraph's first
/// row, and a two-byte span that swallowed the whole paragraph off the
/// screen.
///
/// The shed rule: a block keeps its trailing bare newlines — the shape every
/// consumer already handles — and loses everything from the first trailing
/// line that holds a space or tab. The last text line's own trailing spaces
/// (`"1. a  \n"`) sit before the first newline of the tail and stay.
pub(crate) fn shed_swallowed_whitespace(text: &str, start: usize, end: usize) -> usize {
    let s = &text[start..end];
    let ink = s.trim_end().len();
    let own = ink + s[ink..].len() - s[ink..].trim_start_matches([' ', '\t']).len();
    match s[own..].find([' ', '\t']) {
        None => end,
        Some(space) => {
            let line_start = s[own..own + space].rfind('\n').map(|i| i + 1).unwrap_or(0);
            start + own + line_start
        }
    }
}

fn kind_for(tag: &Tag) -> BlockKind {
    match tag {
        Tag::Heading { level, .. } => BlockKind::Heading(*level as u8),
        Tag::Paragraph => BlockKind::Paragraph,
        // Four leading spaces are an indent, not a code block: a
        // block of code is fenced and nothing else. The indented kind is read
        // as the paragraph it looks like.
        Tag::CodeBlock(CodeBlockKind::Indented) => BlockKind::Paragraph,
        Tag::CodeBlock(CodeBlockKind::Fenced(_)) => BlockKind::CodeBlock,
        Tag::BlockQuote(_) => BlockKind::BlockQuote,
        Tag::List(_) => BlockKind::List,
        Tag::HtmlBlock => BlockKind::Html,
        Tag::Table(_) => BlockKind::Table,
        _ => BlockKind::Other,
    }
}

/// Index of the block containing byte `offset`, if any.
///
/// A block's range runs through its trailing newline, so an offset equal to
/// `range.end` is the first byte of the *next* line — and when the next block
/// starts there, with no blank line between (a list straight under a
/// paragraph, a heading straight over one), the offset is inside that next
/// block and this answers with it. Only an offset no block contains — the
/// end of the note, or the byte after a block's newline where a blank line
/// follows — falls back to the block that ends there, which is where a caret
/// sits after a block's last character. Before 2026-08-22 the tie went the
/// other way, to the earlier block, and a grown window asked which block a
/// list's first line belonged to, was told "the paragraph", and drew the line
/// raw: the owner's screenshot of the first item's indent changing under a
/// selection.
pub fn block_at(blocks: &[Block], offset: usize) -> Option<usize> {
    blocks
        .iter()
        .position(|b| b.range.contains(&offset))
        .or_else(|| blocks.iter().position(|b| b.range.end == offset))
}

/// The block `offset` belongs to, or — when it sits in a blank-line gap, which
/// belongs to no block — the nearest one, ties going to the earlier block.
/// `None` only when the note has no blocks at all.
pub fn nearest_block(blocks: &[Block], offset: usize) -> Option<usize> {
    if let Some(i) = block_at(blocks, offset) {
        return Some(i);
    }
    blocks
        .iter()
        .enumerate()
        .min_by_key(|(_, b)| {
            if offset < b.range.start {
                b.range.start - offset
            } else {
                offset.saturating_sub(b.range.end)
            }
        })
        .map(|(i, _)| i)
}

/// The hard line holding `offset`, as a byte range that excludes its newline.
/// Every offset is on some line, including the blank ones between blocks that
/// belong to no block at all — which is what makes this the fallback edit span
/// when the caret leaves every block's text.
pub fn line_span(text: &str, offset: usize) -> Range<usize> {
    let (start, end) = line_bounds(text, offset);
    start..end
}

/// The line holding `offset`, as a byte range that excludes its newline.
fn line_bounds(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while offset > 0 && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let end = text[offset..]
        .find('\n')
        .map(|i| offset + i)
        .unwrap_or(text.len());
    (start, end)
}

/// How many characters into its own line `offset` sits. Characters, not bytes,
/// so the column survives a line with an accent or an emoji in front of it.
pub fn caret_column(text: &str, offset: usize) -> usize {
    let (start, _) = line_bounds(text, offset);
    let mut offset = offset.min(text.len());
    while offset > start && !text.is_char_boundary(offset) {
        offset -= 1;
    }
    text[start..offset].chars().count()
}

/// The offset `column` characters into the line that holds `line_anchor`,
/// clamped to that line's end. The other half of `caret_column`: together they
/// carry a caret from one line to another at the same column, which is what
/// Up and Down owe the user.
pub fn offset_at_column(text: &str, line_anchor: usize, column: usize) -> usize {
    let (start, end) = line_bounds(text, line_anchor);
    match text[start..end].char_indices().nth(column) {
        Some((i, _)) => start + i,
        None => end,
    }
}

/// Whether `span` is still the edit span the editor owns after it wrote
/// `editor_text` at `editing_start` with the caret at document offset `caret`,
/// or whether a split moved a different block/item under the caret and the
/// editor must be re-seeded.
///
/// Same start is the ordinary case. A start shifted forward by exactly the
/// editor text's leading blank lines, with the caret sitting at the span's
/// start, is Enter at span start: blank lines belong to no block, so the block
/// slid forward whole and the caret is just past the newline it opened.
/// Re-seeding there would drop the newline — block ranges exclude leading
/// blanks — and make Enter look like a no-op.
///
/// The same shape with the caret *past* the span's start is not that. The
/// caret has typed its way beyond the blanks — a letter on the blank line
/// under a paragraph makes a block of its own — and the blank lines are stale
/// text the editor holds no claim to: their rows are the neighbouring blocks'
/// to draw, and an editor that keeps them paints the active-line
/// background over a stack of empty rows (the owner's report, 2026-08-19).
pub fn span_unchanged(
    span: &Range<usize>,
    editing_start: usize,
    editor_text: &str,
    caret: usize,
) -> bool {
    if span.start == editing_start {
        return true;
    }
    let leading_blanks = editor_text.len() - editor_text.trim_start_matches('\n').len();
    span.start == editing_start + leading_blanks && caret <= span.start
}

/// Whether the editor's span reaches into a block other than `active`.
///
/// The editor holds exactly one block's source; every other block is rendered
/// from the document. So a span that overlaps a sibling puts that sibling's
/// text on screen twice — once as raw source inside the editor, once as its own
/// rendered block. It happens when a keystroke turns the span into two blocks
/// without moving its start: typing on the first of two blank lines opened
/// above a paragraph splits the span, but leaves it starting exactly where
/// `span_unchanged` expects.
pub fn span_covers_other_blocks(blocks: &[Block], active: usize, span: &Range<usize>) -> bool {
    blocks
        .iter()
        .enumerate()
        .any(|(i, b)| i != active && b.range.start < span.end && span.start < b.range.end)
}

/// Byte ranges of the top-level items of a list block. `list_range` must be
/// the range of a `BlockKind::List` block in `text`. Ranges are in document
/// coordinates, ordered and non-overlapping; each item's range covers its
/// marker and any nested content (sublists, continuation lines).
pub fn list_items(text: &str, list_range: Range<usize>) -> Vec<Range<usize>> {
    let src = &text[list_range.clone()];
    let mut items = Vec::new();
    let mut item_depth = 0usize;
    let mut open_start = 0usize;
    for (event, range) in Parser::new_ext(src, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Item) => {
                if item_depth == 0 {
                    open_start = range.start;
                }
                item_depth += 1;
            }
            Event::End(TagEnd::Item) => {
                item_depth = item_depth.saturating_sub(1);
                if item_depth == 0 {
                    items.push(list_range.start + open_start..list_range.start + range.end);
                }
            }
            _ => {}
        }
    }
    items
}

/// Byte ranges of the hard lines of a block's text, in document coordinates,
/// excluding both the newlines that separate them and the trailing newlines
/// pulldown-cmark includes in a block's span. Never empty.
///
/// The line counterpart of `list_items`: a CommonMark paragraph is any run of
/// consecutive non-blank lines, so a ten-line stanza is one block, and editing
/// it as one unit drops all ten lines into source mode together.
pub fn block_lines(text: &str, block_range: Range<usize>) -> Vec<Range<usize>> {
    let src = text[block_range.clone()].trim_end_matches('\n');
    let base = block_range.start;
    let mut lines = Vec::new();
    let mut start = 0usize;
    for (i, _) in src.match_indices('\n') {
        lines.push(base + start..base + i);
        start = i + 1;
    }
    lines.push(base + start..base + src.len());
    lines
}

/// Whether each hard line of `src` renders alone to what it renders as part of
/// the whole block — i.e. no inline construct straddles a newline.
///
/// Line-granular editing draws one row per hard line, and a `**bold` split over
/// two rows would show its markers instead of its emphasis. Splitting is a
/// choice, so the answer here is a veto: a block that would render worse in
/// pieces stays whole, and line granularity is never a downgrade.
pub fn lines_render_independently(src: &str) -> bool {
    if !src.contains('\n') {
        return true;
    }
    for (event, range) in Parser::new_ext(src, parser_options()).into_offset_iter() {
        let inline = matches!(
            event,
            Event::Start(
                Tag::Emphasis
                    | Tag::Strong
                    | Tag::Strikethrough
                    | Tag::Link { .. }
                    | Tag::Image { .. }
            ) | Event::Code(_)
        );
        if inline && src[range].contains('\n') {
            return false;
        }
    }
    // Wikilinks are Notes' own syntax and invisible to pulldown-cmark, so they
    // need their own look: a `[[` whose `]]` is on a later line. An *unclosed*
    // `[[` is not that — it is what every wikilink looks like while it is being
    // typed, and vetoing there would swallow the whole paragraph back into the
    // editor at exactly the keystroke the `[[` popup opens on.
    let mut i = 0;
    while let Some(open) = src[i..].find("[[").map(|o| i + o) {
        let Some(close) = src[open + 2..].find("]]").map(|c| open + 2 + c) else {
            break;
        };
        if src[open..close].contains('\n') {
            return false;
        }
        i = close + 2;
    }
    true
}

/// Split an ordered-list item's source into its marker (e.g. "3." or "12)")
/// and the content after it. `None` for bullet items and non-items.
pub fn split_ordered_marker(item_src: &str) -> Option<(&str, &str)> {
    let src = item_src.trim_start_matches([' ', '\t']);
    let digits = src.len() - src.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 || !matches!(src.as_bytes().get(digits), Some(b'.') | Some(b')')) {
        return None;
    }
    let (marker, rest) = src.split_at(digits + 1);
    Some((marker, rest.strip_prefix(' ').unwrap_or(rest)))
}

/// A heading block's own text, without the syntax that made it a heading:
/// the leading `#` run, and a closing `#` run if the author wrote one. A
/// `#` run is the only heading syntax there is — the source-preservation contract removed the
/// setext underline this also used to shed.
///
/// The renderer needs this because it draws a heading as its inline content at
/// the heading's size rather than handing the whole block to the markdown
/// heading node, which does not wrap. Inline markup inside the
/// heading — `**bold**`, `[[links]]` — is left exactly as it was, because it
/// still goes through the inline renderer.
pub fn heading_inline_text(block: &str) -> &str {
    let block = block.trim_end_matches('\n');
    let Some(rest) = block.trim_start().strip_prefix('#') else {
        // Not a heading's source; nothing to shed.
        return block;
    };
    // The `#` run, then the text, then an optional closing run.
    let text = rest.trim_start_matches('#');
    let text = text.strip_prefix(' ').unwrap_or(text);
    let trimmed = text.trim_end();
    let closed = trimmed.trim_end_matches('#');
    // Only a *space-separated* trailing run closes a heading: `# C#` keeps
    // its sharp, `# C #` does not.
    if closed.len() < trimmed.len() && closed.ends_with(' ') {
        closed.trim_end()
    } else {
        trimmed
    }
}

/// Byte offset of the heading whose text equals `heading`
/// (case-insensitive, trimmed), for `[[note#Heading]]` navigation.
pub fn heading_offset(text: &str, heading: &str) -> Option<usize> {
    let want = heading.trim().to_lowercase();
    find_heading(text, |title| title.trim().to_lowercase() == want)
}

/// Markdown fragment links use slugs; wikilinks continue to use heading text.
pub fn heading_anchor_offset(text: &str, anchor: &str) -> Option<usize> {
    let want = anchor.trim().to_lowercase();
    let mut used = std::collections::HashSet::new();
    find_heading(text, |title| {
        let base: String = title
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| {
                if c.is_whitespace() {
                    Some('-')
                } else if c.is_alphanumeric() || c == '-' || c == '_' {
                    Some(c)
                } else {
                    None
                }
            })
            .collect();
        let mut slug = base.clone();
        let mut suffix = 1;
        while !used.insert(slug.clone()) {
            slug = format!("{base}-{suffix}");
            suffix += 1;
        }
        slug == want
    })
    .or_else(|| heading_offset(text, anchor))
}

fn find_heading(text: &str, mut matches: impl FnMut(&str) -> bool) -> Option<usize> {
    let mut in_heading_since: Option<usize> = None;
    let mut collected = String::new();
    for (event, range) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                // A setext shape — text with an underline, two lines at
                // least — is not a heading in Notes, so a link
                // cannot navigate to one. One line means ATX.
                if !text[range.clone()].trim_end_matches('\n').contains('\n') {
                    in_heading_since = Some(range.start);
                    collected.clear();
                }
            }
            Event::Text(t) | Event::Code(t) if in_heading_since.is_some() => {
                collected.push_str(&t);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(start) = in_heading_since.take() {
                    if matches(&collected) {
                        return Some(start);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unclosed_fences_own_their_trailing_blank_code_lines() {
        for (text, open) in [
            ("```\n", true),
            ("```\nx\n```\t\n", true),
            ("```\nx\n```\u{00a0}\n", true),
            ("```\r\nx\r\n```  \r\n", false),
            ("```rust\n  \n\n", true),
            ("````\n```\n", true),
            ("```\nx\n    ```\n", true),
            ("```\nx\n```\n", false),
            ("~~~rust\nx\n  ~~~~\n", false),
            ("```\nx\n```\n\nAfter.\n", false),
        ] {
            let blocks = block_map(text);
            let block = &blocks[0];
            assert_eq!(block.unclosed_fence, open, "{text:?}");
            if open {
                assert_eq!(
                    block.range.end,
                    text.len(),
                    "trailing whitespace is still inside code"
                );
            }
        }
    }

    #[test]
    fn markdown_anchors_resolve_formatted_unicode_and_duplicate_headings() {
        let text = "# **Next** step!\n\n# Next step!\n\n# Café\n";
        assert_eq!(heading_anchor_offset(text, "next-step"), Some(0));
        assert_eq!(
            heading_anchor_offset(text, "next-step-1"),
            text.find("# Next")
        );
        assert_eq!(heading_anchor_offset(text, "café"), text.find("# Café"));
        assert_eq!(heading_offset(text, "Next step!"), Some(0));
        assert_eq!(heading_anchor_offset(text, "absent"), None);
    }

    #[test]
    fn a_heading_keeps_its_text_and_loses_its_syntax() {
        // The renderer draws this text at the heading's size, so anything left
        // in it shows up on screen.
        assert_eq!(heading_inline_text("# Title\n"), "Title");
        assert_eq!(heading_inline_text("### Deeper still"), "Deeper still");
        // No space after the hashes is not a heading to CommonMark, but the
        // block map can still hand one over mid-keystroke.
        assert_eq!(heading_inline_text("#Title"), "Title");
        // A closed ATX heading: the trailing run goes, and only when it is a
        // run of its own.
        assert_eq!(heading_inline_text("## Title ##"), "Title");
        assert_eq!(heading_inline_text("# C#"), "C#");
        assert_eq!(heading_inline_text("# Sharp C# still"), "Sharp C# still");
        // Setext headings are gone: no heading block ever holds
        // an underline, so there is no underline here to shed.
        // Inline markup is left alone — it goes through the inline renderer.
        assert_eq!(
            heading_inline_text("# A **bold** [[link]]"),
            "A **bold** [[link]]"
        );
        // An empty heading is empty, not a panic.
        assert_eq!(heading_inline_text("#"), "");
        assert_eq!(heading_inline_text("## "), "");
    }

    /// The owner's report of 2026-08-26: with "aoeu" on one line, typing `-`
    /// on the next grew "aoeu" into a heading — CommonMark's setext
    /// underline — and the `i` of `- i` shrank it back, a flicker at the
    /// first keystroke of every list written under text. the source-preservation contract removes
    /// setext headings: the run never reaches the line above, which stays
    /// the paragraph it already was, and the run's own line is read as if
    /// it stood alone — `-` the empty list item it is anywhere else, `---`
    /// a rule, `===` plain text.
    #[test]
    fn an_underline_never_grows_the_line_above_it() {
        for (text, want) in [
            // Every keystroke of writing "- item" under a paragraph...
            ("aoeu\n-", vec![BlockKind::Paragraph, BlockKind::List]),
            ("aoeu\n- ", vec![BlockKind::Paragraph, BlockKind::List]),
            ("aoeu\n- item", vec![BlockKind::Paragraph, BlockKind::List]),
            // ...and the other runs CommonMark reads as underlines.
            ("aoeu\n---\n", vec![BlockKind::Paragraph, BlockKind::Rule]),
            ("aoeu\n=", vec![BlockKind::Paragraph, BlockKind::Paragraph]),
            (
                "aoeu\n===\n",
                vec![BlockKind::Paragraph, BlockKind::Paragraph],
            ),
            // `#tag` is a paragraph line (no space after the run), so this
            // is a setext shape too — not an ATX heading over a rule.
            ("#tag\n---\n", vec![BlockKind::Paragraph, BlockKind::Rule]),
        ] {
            let blocks = block_map(text);
            let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
            assert_eq!(kinds, want, "{text:?}: {blocks:?}");
            assert_eq!(
                blocks[0].range,
                0..5,
                "{text:?}: the first line and its newline are the paragraph's"
            );
            assert_eq!(
                blocks[1].range.start, 5,
                "{text:?}: the run's line is a block of its own"
            );
            for pair in blocks.windows(2) {
                assert!(
                    pair[0].range.end <= pair[1].range.start,
                    "{text:?} maps to overlapping blocks: {blocks:?}"
                );
            }
        }
    }

    /// A setext text of several lines is one paragraph after the split —
    /// exactly the block those lines were before the underline landed.
    #[test]
    fn the_lines_above_an_underline_stay_one_paragraph() {
        let text = "Foo\nBar\n---\n";
        let blocks = block_map(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![BlockKind::Paragraph, BlockKind::Rule],
            "{blocks:?}"
        );
        assert_eq!(&text[blocks[0].range.clone()], "Foo\nBar\n");
        let lines = block_lines(text, blocks[0].range.clone());
        let shown: Vec<&str> = lines.iter().map(|l| &text[l.clone()]).collect();
        assert_eq!(shown, vec!["Foo", "Bar"]);
    }

    /// `[[note#Heading]]` answers only for `#` headings: a setext shape is
    /// not a heading anywhere in Notes, link navigation included.
    #[test]
    fn link_navigation_does_not_see_a_setext_shape_as_a_heading() {
        let text = "Intro\n=====\n\n# Real\n";
        assert_eq!(heading_offset(text, "intro"), None);
        assert!(heading_offset(text, "real").is_some());
    }

    /// the source-preservation contract: leading spaces are an indent. A paragraph's range starts
    /// at its line's start, whitespace included, so the editor is handed the
    /// spaces the rendered row draws; and four of them make an indented
    /// paragraph, not a code block — fences are the only code blocks.
    #[test]
    fn leading_spaces_belong_to_the_paragraph_and_never_make_code() {
        let text = "  two in\n\n    four in\n\n```\nfenced\n```\n";
        let blocks = block_map(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Paragraph,
                BlockKind::Paragraph,
                BlockKind::CodeBlock
            ],
            "{blocks:?}"
        );
        assert_eq!(&text[blocks[0].range.clone()], "  two in\n");
        assert_eq!(&text[blocks[1].range.clone()], "    four in\n");
        // A paragraph without any is unchanged.
        let plain = block_map("flush\n");
        assert_eq!(&"flush\n"[plain[0].range.clone()], "flush\n");
    }

    /// The owner's note of 2026-08-24, the shape kept exactly: an ordered
    /// list, a blank line, then a paragraph indented two spaces, with a
    /// whitespace-only line under it. pulldown-cmark's range for the *list*
    /// runs through the blank line and over the paragraph's leading spaces —
    /// the whitespace it read while deciding the list was over — and the
    /// extension above gives those same spaces to the paragraph. Left alone
    /// the two ranges overlap, and everything that trusts "ordered and
    /// non-overlapping" breaks at once: `block_lines` hands the list a
    /// phantom `"  "` line (drawn as an extra row whenever the caret is
    /// elsewhere — the owner's lines shifting down the page), `blank_lines_
    /// after` finds no blank line above the paragraph (Up stops dead), and
    /// the caret at the paragraph's start is seeded a two-byte span that
    /// swallows the whole paragraph off the screen.
    #[test]
    fn a_list_never_reaches_the_indented_paragraph_below_it() {
        let text = "3. Me, a name, I call myself\n\n  Do we still process indents properly?\n  \n";
        let blocks = block_map(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![BlockKind::List, BlockKind::Paragraph],
            "{blocks:?}"
        );
        let spaces = text.find("  Do").unwrap();
        assert_eq!(
            blocks[1].range.start, spaces,
            "the spaces are the paragraph's"
        );
        assert!(
            blocks[0].range.end <= spaces,
            "the list reaches into the paragraph: {blocks:?}"
        );
        // And the list's lines are its items — no phantom blank or `"  "` line.
        let lines = block_lines(text, blocks[0].range.clone());
        let shown: Vec<&str> = lines.iter().map(|l| &text[l.clone()]).collect();
        assert_eq!(shown, vec!["3. Me, a name, I call myself"]);
    }

    /// The same swallowing, in every shape it comes in. The rule: a block's
    /// range may keep its trailing bare newlines (the shape every consumer
    /// already handles), and sheds everything from the first trailing line
    /// that holds a space or tab — those bytes are the gap's, or the next
    /// block's indent.
    #[test]
    fn trailing_whitespace_lines_a_block_swallowed_are_shed() {
        for (text, expected_first) in [
            ("1. a\n\n  ", "1. a\n\n"), // whitespace-only line at the end of the note
            ("1. a\n\n  \n\npara", "1. a\n\n"), // before a flush paragraph
            ("1. a\n\n  \n  para", "1. a\n\n"), // before an indented one
            ("1. a\n\n\n  para", "1. a\n\n\n"), // bare blank lines stay
            ("1. a  \n\n  para", "1. a  \n\n"), // the item's own trailing spaces stay
        ] {
            let blocks = block_map(text);
            assert_eq!(
                &text[blocks[0].range.clone()],
                expected_first,
                "for {text:?}: {blocks:?}"
            );
            for pair in blocks.windows(2) {
                assert!(
                    pair[0].range.end <= pair[1].range.start,
                    "{text:?} maps to overlapping blocks: {blocks:?}"
                );
            }
        }
    }

    /// the source-preservation contract: a GFM pipe table is a block of its own kind, and its range
    /// is exact — header, delimiter row, body rows, the trailing newline,
    /// and nothing of what follows. Unlike a list (the shed above), the
    /// parser's table range stops before the blank line under it, so the
    /// indented paragraph below keeps its spaces without a fight.
    #[test]
    fn a_table_is_one_block_and_stops_where_its_rows_do() {
        let text = "before\n\n| a | b |\n| - | - |\n| 1 | 2 |\n\n  indented after\n";
        let blocks = block_map(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![BlockKind::Paragraph, BlockKind::Table, BlockKind::Paragraph],
            "{blocks:?}"
        );
        assert_eq!(
            &text[blocks[1].range.clone()],
            "| a | b |\n| - | - |\n| 1 | 2 |\n"
        );
        // The paragraph below owns its indent, untouched.
        assert_eq!(blocks[2].range.start, text.find("  indented").unwrap());
    }

    /// The the source-preservation contract extension applies to tables too: an indented table owns
    /// the spaces before its first `|`, which pulldown-cmark's range leaves
    /// out. Bytes owned by no block are how the 2026-08-24 phantom rows grew.
    #[test]
    fn an_indented_table_owns_its_leading_spaces() {
        let text = "  | a | b |\n  | - | - |\n";
        let blocks = block_map(text);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        assert_eq!(blocks[0].kind, BlockKind::Table);
        assert_eq!(blocks[0].range.start, 0, "the spaces are the table's own");
    }

    /// The delimiter row is what *makes* a table. Pipes without one — the
    /// header alone, or prose that happens to contain a `|` — stay the
    /// paragraph they always were, so typing a table's first line changes
    /// nothing until the second line lands.
    #[test]
    fn pipes_without_a_delimiter_row_are_still_a_paragraph() {
        for text in ["| a | b |\n", "a | b\n", "pay | grade\nand more prose\n"] {
            let blocks = block_map(text);
            assert_eq!(blocks.len(), 1, "{text:?}: {blocks:?}");
            assert_eq!(blocks[0].kind, BlockKind::Paragraph, "{text:?}");
        }
    }

    /// GFM reads a non-blank line straight under a table as another row —
    /// the same lookahead that makes a paragraph's lines one block. Worth
    /// pinning because it is the spec's surprise, not a bug: a blank line is
    /// what ends a table.
    #[test]
    fn the_line_under_a_table_is_a_row_until_a_blank_line_parts_them() {
        let joined = "| a | b |\n| - | - |\ntail\n";
        let blocks = block_map(joined);
        assert_eq!(blocks.len(), 1, "{blocks:?}");
        assert_eq!(blocks[0].kind, BlockKind::Table);

        let parted = "| a | b |\n| - | - |\n\ntail\n";
        let blocks = block_map(parted);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert_eq!(blocks[0].kind, BlockKind::Table);
        assert_eq!(blocks[1].kind, BlockKind::Paragraph);
    }

    #[test]
    fn maps_basic_document() {
        let text = "# Title\n\npara one\n\n- a\n- b\n\n```\ncode\n```\n\n> quote\n\n---\n";
        let blocks = block_map(text);
        let kinds: Vec<_> = blocks.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                BlockKind::Heading(1),
                BlockKind::Paragraph,
                BlockKind::List,
                BlockKind::CodeBlock,
                BlockKind::BlockQuote,
                BlockKind::Rule,
            ]
        );
        // Every range slices cleanly and covers its source.
        assert_eq!(&text[blocks[0].range.clone()], "# Title\n");
        assert!(&text[blocks[3].range.clone()].contains("code"));
    }

    #[test]
    fn ranges_are_ordered_and_disjoint() {
        let text = "# a\n\nb\n\n## c\n\nd e f\n\n- one\n- two\n";
        let blocks = block_map(text);
        for pair in blocks.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start);
        }
    }

    #[test]
    fn a_column_survives_the_trip_to_another_line() {
        let text = "1. The cow\n2. The slipper\n";
        // Caret after "cow" on line 1, carried down to line 2.
        let from = text.find("cow").unwrap() + 3;
        let column = caret_column(text, from);
        assert_eq!(column, 10);
        let to = offset_at_column(text, text.find("2.").unwrap(), column);
        assert_eq!(&text[..to], "1. The cow\n2. The sli");
    }

    #[test]
    fn a_column_past_the_end_of_the_target_line_clamps_to_it() {
        let text = "a very long line indeed\nshort\n";
        let column = caret_column(text, 20);
        let to = offset_at_column(text, 25, column);
        assert_eq!(to, text.find("short").unwrap() + "short".len());
    }

    #[test]
    fn columns_are_counted_in_characters_not_bytes() {
        let text = "héllo wörld\nplain line\n";
        let after_wo = text.find('ö').unwrap() + 'ö'.len_utf8();
        assert_eq!(caret_column(text, after_wo), 8);
        let to = offset_at_column(text, text.find("plain").unwrap(), 8);
        assert_eq!(&text[text.find("plain").unwrap()..to], "plain li");
    }

    #[test]
    fn column_zero_and_empty_lines_behave() {
        let text = "abc\n\ndef\n";
        assert_eq!(caret_column(text, 0), 0);
        assert_eq!(
            offset_at_column(text, 4, 2),
            4,
            "an empty line clamps to itself"
        );
        assert_eq!(caret_column(text, 4), 0);
    }

    #[test]
    fn block_at_finds_containing_block() {
        let text = "# Title\n\nparagraph here\n";
        let blocks = block_map(text);
        let off = text.find("here").unwrap();
        let idx = block_at(&blocks, off).unwrap();
        assert_eq!(blocks[idx].kind, BlockKind::Paragraph);
        assert_eq!(block_at(&blocks, 0), Some(0));
    }

    /// Two blocks with no blank line between share a byte: the paragraph's
    /// range ends where the list's begins. That byte is the list's first
    /// character, so the list is the answer — the earlier block only wins
    /// where nothing contains the offset (owner report, 2026-08-22).
    #[test]
    fn block_at_a_shared_boundary_is_the_block_that_starts_there() {
        let text = "Here, with blocks\n1. First line\n2. Second line\n";
        let blocks = block_map(text);
        assert_eq!(
            blocks.len(),
            2,
            "a `1.` item interrupts a paragraph: {blocks:?}"
        );
        let list_start = text.find("1. First").unwrap();
        assert_eq!(blocks[0].range.end, list_start, "the ranges are adjacent");
        assert_eq!(blocks[1].range.start, list_start);
        assert_eq!(block_at(&blocks, list_start), Some(1));
        // The paragraph's own last character, and the note's end, still
        // belong to the block that ends there.
        assert_eq!(block_at(&blocks, list_start - 1), Some(0));
        assert_eq!(block_at(&blocks, text.len()), Some(1));
        // A blank line after a block belongs to nobody, as before.
        let gapped = "Para\n\n1. Item\n";
        let blocks = block_map(gapped);
        assert_eq!(
            block_at(&blocks, 5),
            Some(0),
            "the byte after the paragraph's newline"
        );
        assert_eq!(block_at(&blocks, 6), Some(1));
    }

    /// Enter at span start: the document gains a blank line that belongs to no
    /// block, so the block's range slides forward by one byte. The editor must
    /// keep the newline it just inserted rather than re-seed from the block.
    #[test]
    fn span_unchanged_across_blank_line_opened_above() {
        for (before, editor_text) in [
            ("# Title\n\nBody\n", "\n# Title"),
            ("# Title\n\nBody\n", "\n\n# Title"), // Enter pressed twice
            ("First para.\n\nSecond.\n", "\nFirst para."),
            ("- alpha\n- beta\n", "\n- alpha"),
        ] {
            let editing_start = block_map(before)[0].range.start;
            let mut after = before.to_string();
            let overwritten = editor_text.trim_start_matches('\n').len();
            after.replace_range(editing_start..editing_start + overwritten, editor_text);
            let blocks = block_map(&after);
            let caret = editing_start + (editor_text.len() - overwritten);
            let idx = block_at(&blocks, caret).unwrap();
            let shifted = blocks[idx].range.start;
            assert!(
                shifted > editing_start,
                "{editor_text:?} should shift the block"
            );
            assert!(
                span_unchanged(&blocks[idx].range, editing_start, editor_text, caret),
                "{editor_text:?} in {after:?}: span {:?} wrongly read as a split",
                blocks[idx].range
            );
        }
    }

    /// The caret half of the blank-line rule: the same leading-blanks shape,
    /// but with the caret past the span's start because a letter was typed on
    /// the far side of the blanks. The blanks are stale and the span must
    /// re-seed — keeping them is what highlighted three empty rows above the
    /// word being typed (the owner's report, 2026-08-19).
    #[test]
    fn a_span_whose_blanks_the_caret_typed_past_is_stale() {
        // "as we know it" + Enter×3 + "E": the editor wrote "\n\nE" at 14 and
        // the caret is after the E, one past the new block's start.
        let after = "as we know it\n\n\nE";
        let blocks = block_map(after);
        let caret = after.len();
        let idx = block_at(&blocks, caret).unwrap();
        assert_eq!(blocks[idx].range.start, after.find('E').unwrap());
        assert!(!span_unchanged(&blocks[idx].range, 14, "\n\nE", caret));
    }

    /// A real split — text typed on the line Enter opened below a heading —
    /// puts a *new* block under the caret, and that must re-seed.
    #[test]
    fn span_unchanged_rejects_a_split_below() {
        let after = "# Title\nx\n\nBody\n";
        let blocks = block_map(after);
        let idx = block_at(&blocks, 9).unwrap();
        assert_eq!(blocks[idx].kind, BlockKind::Paragraph);
        assert!(!span_unchanged(&blocks[idx].range, 0, "# Title\nx", 9));
        // Trailing newlines never shift a span forward, so Enter at span end
        // keeps the editor either way.
        let after = "# Title\n\n\nBody\n";
        let blocks = block_map(after);
        let idx = block_at(&blocks, 8).unwrap();
        assert!(span_unchanged(&blocks[idx].range, 0, "# Title\n", 8));
    }

    /// The state behind the owner's "the text below `[[` gets garbled" report:
    /// two blank lines opened above a paragraph, then a character typed on the
    /// *first* of them. The span still starts where it did, so `span_unchanged`
    /// says "keep the editor" — but the typed character made the span two
    /// blocks, and the paragraph is now inside the editor *and* rendered below
    /// it.
    #[test]
    fn a_span_split_in_two_without_moving_is_still_a_leak() {
        let after = "[[\n\nI am in Center Park.\n";
        let blocks = block_map(after);
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        let editing = 0..after.trim_end_matches('\n').len();
        let idx = block_at(&blocks, 2).unwrap();
        assert_eq!(idx, 0);
        // The old check alone is happy with this...
        assert!(span_unchanged(&blocks[idx].range, editing.start, after, 2));
        // ...and this is what it misses.
        assert!(span_covers_other_blocks(&blocks, idx, &editing));
    }

    #[test]
    fn a_span_holding_one_block_covers_nothing_else() {
        // Ordinary editing: the span is the block.
        let text = "# Title\n\nBody\n";
        let blocks = block_map(text);
        for (i, b) in blocks.iter().enumerate() {
            let span = b.range.start..b.range.end - 1; // trailing newline trimmed
            assert!(!span_covers_other_blocks(&blocks, i, &span), "block {i}");
        }
        // Blank lines opened above a paragraph belong to no block, so a span
        // that starts in them still holds only its own block.
        let text = "\n\nBody\n";
        let blocks = block_map(text);
        assert_eq!(blocks.len(), 1);
        assert!(!span_covers_other_blocks(&blocks, 0, &(0..6)));
    }

    #[test]
    fn an_empty_document_has_no_siblings_to_leak_into() {
        assert!(!span_covers_other_blocks(&[], 0, &(0..0)));
        assert!(!span_covers_other_blocks(&block_map(""), 0, &(0..12)));
    }

    #[test]
    fn weird_markdown_never_panics() {
        for text in [
            "",
            "\n\n\n",
            "[[[[]]]]",
            "``` unclosed fence\nstuff",
            "|a|b|\n|-|-|\n|1|2|",
            "<div><p>html soup",
            "#\n##\n######## too deep\n",
            "1. \n2. \n",
            "> > > deep quotes\nmore",
        ] {
            let _ = block_map(text);
        }
    }

    #[test]
    fn list_items_bullets_with_nested_sublist() {
        let text = "# h\n\n- a\n- b\n  - b1\n  - b2\n- c\n";
        let blocks = block_map(text);
        let list = blocks.iter().find(|b| b.kind == BlockKind::List).unwrap();
        let items = list_items(text, list.range.clone());
        assert_eq!(items.len(), 3);
        assert_eq!(&text[items[0].clone()], "- a\n");
        // The nested sublist belongs to its parent item.
        assert!(text[items[1].clone()].contains("b1") && text[items[1].clone()].contains("b2"));
        assert_eq!(&text[items[2].clone()], "- c\n");
        for pair in items.windows(2) {
            assert!(pair[0].end <= pair[1].start);
        }
    }

    #[test]
    fn list_items_ordered_and_loose() {
        let text = "1. a\n2. b\n3. c\n";
        let blocks = block_map(text);
        let items = list_items(text, blocks[0].range.clone());
        assert_eq!(items.len(), 3);
        assert_eq!(&text[items[1].clone()], "2. b\n");

        let loose = "- a\n\n- b\n";
        let blocks = block_map(loose);
        assert_eq!(list_items(loose, blocks[0].range.clone()).len(), 2);
    }

    #[test]
    fn block_lines_splits_a_paragraph_on_its_hard_newlines() {
        let text = "# h\n\nThe cow jumped\nover the moon\nand back\n\ntail\n";
        let blocks = block_map(text);
        let para = blocks
            .iter()
            .find(|b| b.kind == BlockKind::Paragraph)
            .unwrap();
        let lines = block_lines(text, para.range.clone());
        let shown: Vec<&str> = lines.iter().map(|l| &text[l.clone()]).collect();
        assert_eq!(shown, vec!["The cow jumped", "over the moon", "and back"]);
        // Document coordinates, and no newline caught in any of them.
        assert_eq!(lines[0].start, text.find("The cow").unwrap());
        for pair in lines.windows(2) {
            assert_eq!(&text[pair[0].end..pair[1].start], "\n");
        }
    }

    #[test]
    fn block_lines_of_a_one_line_block_is_the_block() {
        let text = "# Title\n\nBody\n";
        let blocks = block_map(text);
        let lines = block_lines(text, blocks[0].range.clone());
        assert_eq!(lines.len(), 1);
        // The trailing newline pulldown-cmark includes is not part of the line.
        assert_eq!(&text[lines[0].clone()], "# Title");
    }

    #[test]
    fn block_lines_never_returns_nothing() {
        // Callers index the result; an empty vec would be a panic waiting.
        for text in ["", "\n\n\n", "a"] {
            assert!(!block_lines(text, 0..text.len()).is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_line_span_is_found_anywhere_including_the_blanks_between_blocks() {
        let text = "# Title\n\nBody\n";
        // Inside a block's text.
        assert_eq!(&text[line_span(text, 3)], "# Title");
        // The blank line between the two, which belongs to no block: an empty
        // span exactly where the caret is.
        assert_eq!(line_span(text, 8), 8..8);
        // Past the last newline.
        assert_eq!(line_span(text, text.len()), 14..14);
    }

    #[test]
    fn plain_lines_render_independently() {
        assert!(lines_render_independently("The cow jumped\nover the moon"));
        assert!(lines_render_independently("one line only"));
        // Markup that opens and closes on the same line is fine.
        assert!(lines_render_independently(
            "**bold** here\nand *italic* there"
        ));
        assert!(lines_render_independently("see [[Note]]\nand [[Other]]"));
        assert!(lines_render_independently("`code` here\nplain there"));
        // A wikilink still being typed: no `]]` yet, so nothing straddles. The
        // `[[` popup opens on this exact state, and pulling the paragraph back
        // into one span there would flicker the whole thing mid-keystroke.
        assert!(lines_render_independently("first line\nsee [[Not"));
        assert!(lines_render_independently("open [[ here\nplain there"));
    }

    #[test]
    fn markup_across_a_newline_vetoes_the_split() {
        // Each of these renders differently line-by-line than it does whole,
        // so the paragraph stays one edit unit.
        for src in [
            "**bold\ntext**",
            "*emphasis\nwrapped*",
            "~~struck\nthrough~~",
            "[link text\nsplit](http://example.com)",
            "![alt\ntext](img.png)",
            "`code\nspan`",
            "a [[Wiki\nLink]] here",
        ] {
            assert!(
                !lines_render_independently(src),
                "{src:?} should not be split into lines"
            );
        }
    }

    #[test]
    fn split_ordered_marker_variants() {
        assert_eq!(split_ordered_marker("2. b"), Some(("2.", "b")));
        assert_eq!(split_ordered_marker("12) x y"), Some(("12)", "x y")));
        assert_eq!(split_ordered_marker("3."), Some(("3.", "")));
        assert_eq!(split_ordered_marker("- b"), None);
        assert_eq!(split_ordered_marker("plain"), None);
    }

    #[test]
    fn heading_lookup_case_insensitive() {
        let text = "# Intro\n\nstuff\n\n## Setup Steps\n\nmore\n";
        let off = heading_offset(text, "setup steps").unwrap();
        assert_eq!(&text[off..off + 2], "##");
        assert_eq!(heading_offset(text, "missing"), None);
    }

    #[test]
    fn heading_with_inline_code_matches_plain_text() {
        let text = "## Run `cargo test` locally\n";
        assert!(heading_offset(text, "run cargo test locally").is_some());
    }

    #[test]
    fn nested_list_is_one_block() {
        let text = "- a\n  - a1\n  - a2\n- b\n";
        let blocks = block_map(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].kind, BlockKind::List);
        assert_eq!(&text[blocks[0].range.clone()], text);
    }
}

#[cfg(test)]
mod fence_preview_tests {
    #[test]
    fn only_actual_marker_lines_lose_ink() {
        for (source, visible) in [
            ("```", ""),
            ("```\n", "\n"),
            ("```rust\nx\n", "\nx\n"),
            ("```\nx\n```", "\nx\n"),
            ("```\n\n```\n", "\n\n\n"),
            ("````\n```\n````", "\n```\n"),
            ("~~~rs\nx\n~~~", "\nx\n"),
        ] {
            let mut shown = source.to_string();
            for range in super::code_fence_marker_ranges(source).into_iter().rev() {
                shown.replace_range(range, "");
            }
            assert_eq!(shown, visible, "{source:?}");
        }
    }
}
