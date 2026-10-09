//! Portable, flat inline feedback. The existing block map supplies boundaries;
//! only explicit reply headers are special. Ordinary quotes remain ordinary.
use crate::blocks::{parser_options, Block, BlockKind, ListSource};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use std::ops::Range;
use std::sync::Arc;

mod list_items;

pub const STAMP_FORMAT: &str = "%Y-%m-%d %I:%M %p %:z";
pub const HEADER_PREFIX: &str = "> ";
const LEGACY_COMMENT_PREFIX: &str = "> **Comment — ";
const LEGACY_HEADER_PREFIX: &str = "> **Reply — ";

/// A readable attribution, with the host's stable project identity when known.
/// Standalone editors do not invent an identity from a machine/display name.
#[derive(Clone, Debug)]
pub struct Author {
    pub name: String,
    pub user_id: Option<uuid::Uuid>,
}

impl Author {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            user_id: None,
        }
    }
    pub fn identified(name: impl Into<String>, user_id: uuid::Uuid) -> Self {
        Self {
            name: name.into(),
            user_id: Some(user_id),
        }
    }
}

impl From<String> for Author {
    fn from(name: String) -> Self {
        Self::named(name)
    }
}
impl From<&str> for Author {
    fn from(name: &str) -> Self {
        Self::named(name)
    }
}

/// New replies use the first eight UUID digits; existing full IDs still read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserId {
    Full(uuid::Uuid),
    Short(u32),
}

pub struct Header<'a> {
    /// List-container whitespace, retained exactly in source and previews.
    pub indent: &'a str,
    pub author: &'a str,
    pub user_id: Option<UserId>,
    pub stamp: &'a str,
    pub time: chrono::DateTime<chrono::FixedOffset>,
}

impl Header<'_> {
    pub fn is_own(&self, author: &Author) -> bool {
        let Some(id) = author.user_id else {
            return false;
        };
        match self.user_id {
            Some(UserId::Full(found)) => found == id,
            Some(UserId::Short(prefix)) => {
                prefix == id.as_fields().0 && self.author == author_label(&author.name)
            }
            None => false,
        }
    }
}

/// Read one short first line, never parse the document on the render path.
pub fn header(source: &str) -> Option<Header<'_>> {
    let line = source.lines().next()?;
    let content = line.trim_start_matches([' ', '\t']);
    let indent = &line[..line.len() - content.len()];
    let line = content;
    let legacy = [LEGACY_COMMENT_PREFIX, LEGACY_HEADER_PREFIX]
        .into_iter()
        .find(|prefix| line.starts_with(*prefix));
    let content = if let Some(prefix) = legacy {
        line.strip_prefix(prefix)?.strip_suffix("**")?
    } else {
        line.strip_prefix(HEADER_PREFIX)?
    };
    parse_attribution(content, indent, legacy.is_some())
}

fn parse_attribution<'a>(content: &'a str, indent: &'a str, legacy: bool) -> Option<Header<'a>> {
    let (author, stamp) = content.rsplit_once(" · ")?;
    let (author, user_id) = if let Some((name, id)) = author.rsplit_once(" · user:") {
        let id = if id == "unlinked" {
            None
        } else if id.len() == 8 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Some(UserId::Short(u32::from_str_radix(id, 16).ok()?))
        } else {
            Some(UserId::Full(uuid::Uuid::parse_str(id).ok()?))
        };
        (name, id)
    } else if legacy {
        (author, None)
    } else {
        return None;
    };
    if author.trim().is_empty() || author.starts_with('>') {
        return None;
    }
    let time = chrono::DateTime::parse_from_str(stamp, STAMP_FORMAT).ok()?;
    if time.format(STAMP_FORMAT).to_string() != stamp {
        return None;
    }
    Some(Header {
        indent,
        author,
        user_id,
        stamp,
        time,
    })
}

/// An unquoted, single-line attribution following a Markdown divider. Check
/// only this cached block and its predecessor, never scan the document to draw.
pub fn entry_header<'a>(text: &'a str, blocks: &[Block], index: usize) -> Option<Header<'a>> {
    let block = blocks.get(index)?;
    let previous = index.checked_sub(1).and_then(|i| blocks.get(i))?;
    if block.kind != BlockKind::Paragraph
        || previous.kind != BlockKind::Rule
        || block.range.len() > 4096
    {
        return None;
    }
    let source = text[block.range.clone()].trim_end_matches(['\r', '\n']);
    if source.contains(['\r', '\n']) {
        return None;
    }
    parse_attribution(source, "", false)
}

/// The end-of-document command applies only to spaces and line endings;
/// a tab or any other content after the caret retains the inline operation.
pub fn at_document_end(text: &str, at: usize) -> bool {
    text.get(at..)
        .is_some_and(|tail| tail.bytes().all(|b| matches!(b, b' ' | b'\r' | b'\n')))
}

/// Visible content of an already-recognized header, after its quote marker.
/// Legacy role/bold syntax stays hidden in both modes; source is unchanged.
pub fn header_text_range(line: &str) -> Range<usize> {
    let line = line.trim_end_matches('\r');
    for prefix in ["**Comment — ", "**Reply — "] {
        if line.starts_with(prefix) && line.ends_with("**") {
            return prefix.len()..line.len() - 2;
        }
    }
    0..line.len()
}

/// Bytes belonging to indentation and one outer quote marker on this line.
/// A nested `>` remains body text. Lazy continuation lines have no marker.
pub fn quote_prefix(line: &str) -> usize {
    let quoted = line.trim_start_matches([' ', '\t']);
    let indent = line.len() - quoted.len();
    if quoted.starts_with("> ") {
        indent + 2
    } else if quoted.starts_with('>') {
        indent + 1
    } else {
        0
    }
}

/// Source hard lines with only the outer comment quote marker hidden. Nested
/// quotes, lists, fences and lazy continuations retain their source geometry.
pub fn lines(source: &str) -> Vec<crate::quotes::QuoteLine> {
    let mut start = 0;
    source
        .split('\n')
        .map(|line| {
            let range = start..start + line.len();
            start += line.len() + 1;
            crate::quotes::QuoteLine {
                range,
                prefix: quote_prefix(line),
                depth: 1,
            }
        })
        .collect()
}

pub fn is_reply(text: &str, block: &Block) -> bool {
    block.kind == BlockKind::BlockQuote && header(&text[block.range.clone()]).is_some()
}

/// The editor exposes comments inside lists as individual blocks; the stored
/// Markdown and the ordinary block map remain one complete list. Parsing and
/// numbering happen only when this cached map is rebuilt after an edit.
pub fn editor_block_map(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    for block in crate::blocks::block_map(text) {
        if block.kind != BlockKind::List
            || (!text[block.range.clone()].contains(" · user:")
                && !text[block.range.clone()].contains(LEGACY_COMMENT_PREFIX)
                && !text[block.range.clone()].contains(LEGACY_HEADER_PREFIX))
        {
            out.push(block);
            continue;
        }
        let mut quotes = Vec::new();
        let mut quote_depth = 0;
        let mut numbers = std::collections::HashMap::new();
        let mut counters: Vec<Option<u64>> = Vec::new();
        for (event, range) in
            Parser::new_ext(&text[block.range.clone()], parser_options()).into_offset_iter()
        {
            match event {
                Event::Start(Tag::List(start)) => counters.push(start),
                Event::End(TagEnd::List(_)) => {
                    counters.pop();
                }
                Event::Start(Tag::Item) if quote_depth == 0 => {
                    if let Some(Some(next)) = counters.last_mut() {
                        let marker = block.range.start + range.start;
                        let digits = text[marker..]
                            .bytes()
                            .take_while(u8::is_ascii_digit)
                            .count();
                        if text[marker..marker + digits].parse::<u64>().ok() != Some(*next) {
                            numbers.insert(marker, next.to_string());
                        }
                        *next = (*next + 1).min(999_999_999);
                    }
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    let marker = block.range.start + range.start;
                    let start = list_items::line_start(text, marker);
                    if quote_depth == 0
                        && header(&text[start..block.range.start + range.end]).is_some()
                    {
                        let end = crate::blocks::shed_swallowed_whitespace(
                            text,
                            start,
                            block.range.start + range.end,
                        );
                        quotes.push(start..end);
                    }
                    quote_depth += 1;
                }
                Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
                _ => {}
            }
        }
        if quotes.is_empty() {
            out.push(block);
            continue;
        }
        let source = Arc::new(ListSource {
            numbers,
            range: block.range.clone(),
        });
        let mut start = block.range.start;
        for quote in quotes {
            push_list_fragment(text, start..quote.start, &source, &mut out);
            start = quote.end;
            out.push(Block {
                range: quote,
                kind: BlockKind::BlockQuote,
                unclosed_fence: false,
                source_list: Some(source.clone()),
            });
        }
        push_list_fragment(text, start..block.range.end, &source, &mut out);
    }
    out
}

fn push_list_fragment(
    text: &str,
    mut range: Range<usize>,
    source: &Arc<ListSource>,
    out: &mut Vec<Block>,
) {
    for line in text[range.clone()].split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        range.start += line.len();
    }
    if range.start < range.end {
        out.push(Block {
            range,
            kind: BlockKind::List,
            unclosed_fence: false,
            source_list: Some(source.clone()),
        });
    }
}

/// Comment after the next source newline (or EOF), never a visual wrap or
/// an entire paragraph/item. Existing comments retain their flat thread order.
pub fn insertion_offset(text: &str, blocks: &[Block], at: usize) -> usize {
    placement(text, blocks, at).at
}

fn placement(text: &str, blocks: &[Block], at: usize) -> list_items::Placement {
    let mut boundary = list_items::line_end(text, at);
    let mut indent = String::new();
    let mut allowed = true;
    if let Some(mut index) = blocks
        .partition_point(|block| block.range.start <= at)
        .checked_sub(1)
    {
        let block = &blocks[index];
        let list = block
            .source_list
            .as_ref()
            .map(|source| &source.range)
            .or_else(|| (block.kind == BlockKind::List).then_some(&block.range));
        if let Some(position) = list.and_then(|list| list_items::placement(text, list, at)) {
            return position;
        }
        let comment = is_reply(text, block);
        if comment {
            boundary = list_items::line_end(text, list_items::content_end(text, block.range.end));
        }
        allowed = !block.unclosed_fence
            && (comment
                || matches!(
                    block.kind,
                    BlockKind::Paragraph | BlockKind::Heading(_) | BlockKind::Rule
                )
                || boundary >= list_items::content_end(text, block.range.end));
        // Only a final hard line can own the following thread. Inserting on
        // an earlier line must not skip the rest of a multi-line paragraph.
        if boundary >= list_items::content_end(text, block.range.end) {
            while blocks.get(index + 1).is_some_and(|next| {
                is_reply(text, next) && text[boundary..next.range.start].trim().is_empty()
            }) {
                index += 1;
                boundary = list_items::line_end(
                    text,
                    list_items::content_end(text, blocks[index].range.end),
                );
            }
            indent = header(&text[blocks[index].range.clone()])
                .map_or("", |header| header.indent)
                .to_owned();
        }
    }
    list_items::Placement {
        at: boundary,
        indent,
        allowed,
        continuation: String::new(),
    }
}

/// Cheap cached guard for rendering. The command uses the source-aware guard
/// below to refuse an interior structural line without relocating the comment.
pub fn can_insert(blocks: &[Block], at: usize) -> bool {
    blocks
        .partition_point(|block| block.range.start <= at)
        .checked_sub(1)
        .and_then(|index| blocks.get(index))
        .or_else(|| blocks.first())
        .is_none_or(|block| !block.unclosed_fence)
}

/// Fences, tables, HTML and ordinary quoted blocks stay intact. On a command,
/// only the relevant list is parsed to recover item/continuation ownership.
pub fn can_insert_at(text: &str, blocks: &[Block], at: usize) -> bool {
    placement(text, blocks, at).allowed
}

/// Stable readable attribution, escaped as literal Markdown text.
pub fn author_label(author: &str) -> String {
    let author = author
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\\', "\\\\")
        .replace('*', "\\*");
    if author.is_empty() {
        "Human".into()
    } else {
        author
    }
}

pub struct Insertion {
    pub at: usize,
    pub text: String,
    pub caret: usize,
}

/// A normal Markdown entry, with a blank line before the rule so it cannot
/// turn the preceding paragraph into a setext heading. Existing text, including
/// trailing whitespace, stays intact; the caret lands in the unquoted body.
pub fn entry_insertion(text: &str, at: usize, author: &Author, stamp: &str) -> Insertion {
    let newline = newline_at(text, at);
    let leading = if at == 0 || text[..at].ends_with(&format!("{newline}{newline}")) {
        ""
    } else if text[..at].ends_with(newline) {
        "\n"
    } else {
        "\n\n"
    };
    let name = author_label(&author.name);
    let identity = author
        .user_id
        .map(|id| format!("{:08x}", id.as_fields().0))
        .unwrap_or_else(|| "unlinked".into());
    let mut entry = format!("{leading}---\n{name} · user:{identity} · {stamp}\n\n");
    if newline == "\r\n" {
        entry = entry.replace('\n', newline);
    }
    Insertion {
        at,
        caret: at + entry.len(),
        text: entry,
    }
}

/// No hidden metadata; author and local minute timestamp are literal Markdown.
pub fn insertion(
    text: &str,
    blocks: &[Block],
    at: usize,
    author: &Author,
    stamp: &str,
) -> Insertion {
    let position = placement(text, blocks, at);
    let (at, indent) = (position.at, position.indent);
    let name = author_label(&author.name);
    let identity = author
        .user_id
        .map(|id| format!("{:08x}", id.as_fields().0))
        .unwrap_or_else(|| "unlinked".into());
    let newline = newline_at(text, at);
    let leading = if at == 0 || text[..at].ends_with(&format!("{newline}{newline}")) {
        ""
    } else if text[..at].ends_with(newline) {
        "\n"
    } else {
        "\n\n"
    };
    let mut reply =
        format!("{leading}{indent}{HEADER_PREFIX}{name} · user:{identity} · {stamp}\n{indent}> ");
    if newline == "\r\n" {
        reply = reply.replace('\n', newline);
    }
    let caret = at + reply.len();
    // Reuse source blank lines. Add indentation only when a lazy list
    // continuation would otherwise leave its original item after the quote.
    if at < text.len() {
        if !text[at..].starts_with(&format!("{newline}{newline}")) {
            reply.push_str(newline);
            if !text[at..].starts_with(newline) {
                reply.push_str(newline);
            }
        }
        reply.push_str(&position.continuation);
    }
    Insertion {
        at,
        text: reply,
        caret,
    }
}

fn newline_at(text: &str, at: usize) -> &'static str {
    let newline = if text[..at].ends_with('\n') {
        Some(at - 1)
    } else {
        text[at..]
            .find('\n')
            .map(|offset| at + offset)
            .or_else(|| text[..at].rfind('\n'))
    };
    if newline.is_some_and(|offset| offset > 0 && text.as_bytes()[offset - 1] == b'\r') {
        "\r\n"
    } else {
        "\n"
    }
}

/// Land beyond the quote's separating blank line. Typing on that separator
/// itself would become Markdown's lazy quote continuation. Existing outside
/// text/blank rows are reused; only missing line breaks are inserted at EOF.
pub fn exit_after(text: &str, end: usize) -> Insertion {
    if text.is_empty() {
        return Insertion {
            at: 0,
            text: String::new(),
            caret: 0,
        };
    }
    let at = text[..end].trim_end_matches(['\r', '\n']).len();
    let newline = newline_at(text, at);
    let separator = format!("{newline}{newline}");
    let suffix = &text[at..];
    let missing = if suffix.starts_with(&separator) {
        String::new()
    } else if suffix.starts_with(newline) {
        newline.to_owned()
    } else {
        separator
    };
    Insertion {
        at,
        text: missing,
        caret: at + 2 * newline.len(),
    }
}

/// Every pasted line stays quoted. A pasted author header becomes nested
/// quoted content, not a new attribution at the outer level.
pub fn quoted_paste(text: &str) -> String {
    quoted_paste_with_indent(text, "")
}

pub fn quoted_paste_with_indent(text: &str, indent: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', &format!("\n{indent}> "))
}

/// A cheap label for already-visible headers. No timers; refresh on redraw.
/// Future timestamps retain their absolute label without a misleading age.
pub fn relative(
    time: chrono::DateTime<chrono::FixedOffset>,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> Option<String> {
    if time > now {
        return None;
    }
    let minutes = now.signed_duration_since(time).num_minutes();
    let plural = |n, singular, plural| format!("{n} {}", if n == 1 { singular } else { plural });
    Some(if minutes == 0 {
        "just now".into()
    } else if minutes < 60 {
        format!("{} ago", plural(minutes, "minute", "minutes"))
    } else if minutes < 24 * 60 && minutes % 60 != 0 {
        format!(
            "{} and {} ago",
            plural(minutes / 60, "hour", "hours"),
            plural(minutes % 60, "minute", "minutes")
        )
    } else if minutes < 48 * 60 {
        format!("{} ago", plural(minutes / 60, "hour", "hours"))
    } else {
        format!("{} ago", plural(minutes / (24 * 60), "day", "days"))
    })
}

/// Decorate only the visible reply's first line; source bytes stay untouched.
pub fn with_relative_label(source: &str, now: chrono::DateTime<chrono::FixedOffset>) -> String {
    let (header, prefix) = if let Some(header) = header(source) {
        (header, HEADER_PREFIX)
    } else if let Some(header) = source
        .lines()
        .next()
        .and_then(|line| parse_attribution(line, "", false))
    {
        (header, "")
    } else {
        return source.to_owned();
    };
    let label = relative(header.time, now).unwrap_or_else(|| header.stamp.to_owned());
    let end = source.find('\n').unwrap_or(source.len());
    format!(
        "{}{prefix}{} · {label}{}",
        header.indent,
        header.author,
        &source[end..]
    )
}

/// The body range of a recognized reply; useful for keeping Enter/paste in it.
pub fn containing<'a>(text: &str, blocks: &'a [Block], caret: usize) -> Option<&'a Block> {
    let index = blocks
        .partition_point(|block| block.range.start <= caret)
        .checked_sub(1)?;
    let block = &blocks[index];
    (caret <= block.range.end && is_reply(text, block)).then_some(block)
}

/// Remove only the untouched placeholder just created by this view. External
/// edits and user content are never guessed at or removed.
pub fn untouched(text: &str, draft: &(Range<usize>, String)) -> bool {
    text.get(draft.0.clone()) == Some(draft.1.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blocks::block_map;
    const STAMP: &str = "2026-09-08 12:23 PM -07:00";
    #[test]
    fn entry_headers_require_a_divider_identity_and_exact_timestamp() {
        let author = Author::identified(
            "Alex Smith",
            uuid::Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap(),
        );
        for source in ["Context.", "", "- Final item", "> Last quote"] {
            let insert = entry_insertion(source, source.len(), &author, STAMP);
            let text = format!("{source}{}", insert.text);
            assert_eq!(insert.caret, text.len());
            let blocks = editor_block_map(&text);
            let index = blocks.len() - 1;
            let header = entry_header(&text, &blocks, index).unwrap();
            assert_eq!(header.author, "Alex Smith");
            assert!(header.is_own(&author));
            assert_eq!(blocks[index - 1].kind, BlockKind::Rule);
            assert!(
                !blocks
                    .iter()
                    .any(|b| matches!(b.kind, BlockKind::Heading(_))),
                "rule must not create a setext heading"
            );
            let now = chrono::DateTime::parse_from_str("2026-09-08 02:23 PM -07:00", STAMP_FORMAT)
                .unwrap();
            assert_eq!(
                with_relative_label(&text[blocks[index].range.clone()], now),
                "Alex Smith · 2 hours ago\n"
            );
        }
        for source in [
            "Alex · user:12345678 · 2026-09-08 12:23 PM -07:00\n",
            "---\nAlex · 2026-09-08 12:23 PM -07:00\n",
            "---\nAlex · user:12345678 · yesterday\n",
            "---\nAlex · user:12345678 · 2026-09-08 12:23 PM -07:00\nordinary continuation\n",
        ] {
            let blocks = editor_block_map(source);
            assert!(
                (0..blocks.len()).all(|i| entry_header(source, &blocks, i).is_none()),
                "{source}"
            );
        }
        let large = format!("---\n{} · user:unlinked · {STAMP}\n", "Name".repeat(2000));
        let blocks = editor_block_map(&large);
        assert!(
            entry_header(&large, &blocks, 1).is_none(),
            "oversized paragraphs use the ordinary bounded renderer"
        );
    }

    #[test]
    fn end_entry_preserves_suffix_and_newline_convention() {
        for source in ["Context.\n \n", "Context.\r\n \r\n"] {
            let insert = entry_insertion(source, 8, &Author::named("Alex"), STAMP);
            let mut text = source.to_owned();
            text.insert_str(insert.at, &insert.text);
            assert_eq!(&text[insert.caret..], &source[8..]);
            assert!(at_document_end(source, 8));
            if source.contains('\r') {
                assert!(!insert.text.replace("\r\n", "").contains('\n'));
            }
            text.replace_range(insert.at..insert.at + insert.text.len(), "");
            assert_eq!(text, source);
        }
        assert!(!at_document_end("Context.\t", 8));
        assert!(!at_document_end("Context.\nLater", 8));
        assert!(!at_document_end("é", 1));
    }
    mod list_tests;
    mod placement_tests;
    fn append(text: &str, at: usize, name: &str) -> String {
        let insert = insertion(text, &block_map(text), at, &Author::named(name), STAMP);
        let mut result = text.to_owned();
        result.insert_str(insert.at, &insert.text);
        result.insert_str(insert.caret, "A reply.");
        result
    }
    #[test]
    fn replies_are_flat_and_append_from_context_or_an_earlier_reply() {
        let text = append("Question?\n\nNext paragraph.\n", 3, "Morgan");
        let text = append(&text, 2, "Codex");
        let from_first = text.find("Morgan").unwrap();
        let text = append(&text, from_first, "Alex");
        let blocks = block_map(&text);
        let replies: Vec<_> = blocks
            .iter()
            .filter(|block| is_reply(&text, block))
            .collect();
        assert_eq!(replies.len(), 3, "{text}\n{blocks:?}");
        assert!(text.find("Morgan").unwrap() < text.find("Codex").unwrap());
        assert!(text.find("Codex").unwrap() < text.find("Alex").unwrap());
        assert!(text.find("Alex").unwrap() < text.find("Next paragraph").unwrap());
        assert!(!text.contains(">>"));
    }
    #[test]
    fn comments_append_to_legacy_and_mixed_threads_without_rewriting_them() {
        let legacy = format!(
            "{LEGACY_HEADER_PREFIX}Morgan · user:12345678 · {STAMP}**\n>\n> Existing answer."
        );
        let original = format!("Question?\n\n{legacy}\n\nNext paragraph.\n");
        let once = append(&original, 3, "Codex");
        for at in [3, once.find("Morgan").unwrap(), once.find("Codex").unwrap()] {
            let twice = append(&once, at, "Alex");
            assert!(
                twice.contains(&legacy),
                "existing source is byte-for-byte intact"
            );
            assert_eq!(twice.matches(LEGACY_HEADER_PREFIX).count(), 1);
            assert_eq!(twice.matches(" · user:unlinked · ").count(), 2);
            assert!(twice.find("Codex").unwrap() < twice.find("Alex").unwrap());
            assert!(twice.find("Alex").unwrap() < twice.find("Next paragraph").unwrap());
            let blocks = block_map(&twice);
            assert_eq!(
                blocks
                    .iter()
                    .filter(|block| is_reply(&twice, block))
                    .count(),
                3
            );
            assert!(containing(&twice, &blocks, twice.find("Existing answer").unwrap()).is_some());
        }
    }

    #[test]
    fn plain_and_legacy_headers_share_the_same_relative_label() {
        let time = chrono::DateTime::parse_from_str(STAMP, STAMP_FORMAT).unwrap();
        for (prefix, suffix) in [
            (LEGACY_HEADER_PREFIX, "**"),
            (LEGACY_COMMENT_PREFIX, "**"),
            (HEADER_PREFIX, ""),
        ] {
            let source = format!(
                "{prefix}Alex · user:12345678 · {STAMP}{suffix}\n>\n> **Body**\n> > Nested quote."
            );
            assert_eq!(header(&source).unwrap().author, "Alex");
            assert_eq!(
                with_relative_label(&source, time + chrono::Duration::minutes(5)),
                "> Alex · 5 minutes ago\n>\n> **Body**\n> > Nested quote."
            );
        }
        assert!(header(&format!("> **Discussion — Alex · {STAMP}**")).is_none());
        assert!(header(&format!("> > **Comment — Alex · {STAMP}**")).is_none());
    }

    #[test]
    fn plain_headers_require_an_explicit_identity_marker_and_exact_timestamp() {
        let source = insertion("", &[], 0, &Author::named("Morgan"), STAMP).text;
        assert_eq!(source, format!("> Morgan · user:unlinked · {STAMP}\n> "));
        let found = header(&source).unwrap();
        assert_eq!(found.author, "Morgan");
        assert_eq!(found.user_id, None);
        for line in [
            format!("> Morgan · {STAMP}"),
            format!("> Morgan · user: · {STAMP}"),
            format!(">  · user:unlinked · {STAMP}"),
            format!("> > Morgan · user:unlinked · {STAMP}"),
            "> Morgan · user:unlinked · 2026-9-8 12:23 PM -07:00".into(),
            "> Morgan · user:unlinked · 2026-09-08 12:23 pm -07:00".into(),
        ] {
            assert!(header(&line).is_none(), "ordinary/malformed quote: {line}");
        }
        assert_eq!(
            header_text_range("**Comment — Alex · just now**"),
            "**Comment — ".len().."**Comment — Alex · just now".len()
        );
        assert_eq!(
            header_text_range("Alex · just now"),
            0.."Alex · just now".len()
        );
        assert_eq!(
            header_text_range("**Reply — Alex · just now**\r"),
            "**Reply — ".len().."**Reply — Alex · just now".len()
        );
    }

    #[test]
    fn comment_lines_hide_only_the_outer_marker_and_keep_byte_offsets() {
        let source = "> héllo\n>\n>> Nested\n> - List\n> ```rust\n>\tcode\nlazy continuation\n";
        let lines = lines(source);
        let visible: Vec<_> = lines
            .iter()
            .map(|line| {
                assert_eq!(line.depth, 1);
                &source[line.range.start + line.prefix..line.range.end]
            })
            .collect();
        assert_eq!(
            visible,
            [
                "héllo",
                "",
                "> Nested",
                "- List",
                "```rust",
                "\tcode",
                "lazy continuation",
                ""
            ]
        );
        for pair in lines.windows(2) {
            assert_eq!(pair[0].range.end + 1, pair[1].range.start);
        }
    }

    #[test]
    fn complete_containers_are_preserved() {
        for source in [
            "```rust\none\ntwo\n```",
            "| a | b |\n| - | - |\n| c | d |",
            "> ordinary\n> quote",
            "<div>\nhtml\n</div>",
        ] {
            let blocks = editor_block_map(source);
            assert!(!can_insert_at(source, &blocks, 1), "{source}");
            let last_line = source.rfind('\n').unwrap() + 1;
            assert!(can_insert_at(source, &blocks, last_line), "{source}");
            let result = append(source, last_line, "Human");
            assert!(
                result.starts_with(&format!("{source}\n\n{HEADER_PREFIX}")),
                "{result}"
            );
        }
    }
    #[test]
    fn arbitrary_paste_stays_inside_reply() {
        assert_eq!(
            quoted_paste("first\r\n\r\n> **Reply — Fake · 2026-09-08 12:23 PM -07:00**\nlast"),
            "first\n> \n> > **Reply — Fake · 2026-09-08 12:23 PM -07:00**\n> last"
        );
        assert!(header("> ordinary quote").is_none());
        assert!(header("> **Reply — Human · tomorrow**").is_none());
    }
    #[test]
    fn relative_labels_are_minute_precision_and_bounded_arithmetic() {
        let time = chrono::DateTime::parse_from_str(STAMP, STAMP_FORMAT).unwrap();
        for (minutes, label) in [
            (5, "5 minutes ago"),
            (75, "1 hour and 15 minutes ago"),
            (135, "2 hours and 15 minutes ago"),
            (2160, "36 hours ago"),
            (11520, "8 days ago"),
        ] {
            assert_eq!(
                relative(time, time + chrono::Duration::minutes(minutes)).as_deref(),
                Some(label)
            );
        }
        assert_eq!(relative(time, time - chrono::Duration::seconds(30)), None);
        let utc = time.with_timezone(&chrono::FixedOffset::east_opt(0).unwrap());
        assert_eq!(
            relative(time, utc + chrono::Duration::minutes(5)).as_deref(),
            Some("5 minutes ago")
        );
    }

    #[test]
    fn identity_uses_uuid_and_preview_replaces_metadata_with_age() {
        let one = uuid::Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap();
        let two = uuid::Uuid::parse_str("87654321-4321-4321-8321-cba987654321").unwrap();
        let author = Author::identified("Morgan", one);
        let source = insertion("", &[], 0, &author, STAMP).text;
        assert_eq!(
            source,
            format!("{HEADER_PREFIX}Morgan · user:12345678 · {STAMP}\n> ")
        );
        let header = header(&source).unwrap();
        assert_eq!(header.author, "Morgan");
        assert!(header.is_own(&author));
        assert!(!header.is_own(&Author::identified("Morgan", two)));
        assert!(
            !header.is_own(&Author::identified("Someone else", one)),
            "a short ID is paired with the readable name"
        );
        assert!(!header.is_own(&Author::named("Morgan")));
        let rendered = with_relative_label(&source, header.time + chrono::Duration::minutes(5));
        assert!(rendered.starts_with("> Morgan · 5 minutes ago"));
        assert!(!rendered.contains("user:") && !rendered.contains(STAMP));
        let future = with_relative_label(&source, header.time - chrono::Duration::minutes(5));
        assert!(future.contains(STAMP) && !future.contains("ago") && !future.contains("user:"));
        let legacy = format!("{LEGACY_HEADER_PREFIX}Morgan · {STAMP}**");
        assert!(!super::header(&legacy).unwrap().is_own(&author));
        let full =
            format!("{LEGACY_HEADER_PREFIX}Morgan · user:{one} · {STAMP}**\n>\n> Existing answer.");
        assert!(
            super::header(&full)
                .unwrap()
                .is_own(&Author::identified("Renamed", one)),
            "existing full IDs retain exact identity"
        );
        assert!(
            with_relative_label(&full, header.time).ends_with("\n>\n> Existing answer."),
            "display does not rewrite existing blank rows"
        );
        for invalid in ["0437003", "123456780", "0437003z"] {
            assert!(
                super::header(&format!("{HEADER_PREFIX}Morgan · user:{invalid} · {STAMP}"))
                    .is_none()
            );
        }
    }

    #[test]
    fn exit_keeps_a_quote_separator_and_reuses_outside_rows() {
        for suffix in ["", "\n", "\n\n", "\n\n\n", "\n\nNext."] {
            let mut source =
                format!("{HEADER_PREFIX}Morgan · user:unlinked · {STAMP}\n>\n> Answer.{suffix}");
            let block = block_map(&source).remove(0);
            let exit = exit_after(&source, block.range.end);
            source.insert_str(exit.at, &exit.text);
            source.insert_str(exit.caret, "Normal.");
            let blocks = block_map(&source);
            assert_eq!(blocks[1].kind, BlockKind::Paragraph, "{source}");
            assert!(
                source[blocks[1].range.clone()].starts_with("Normal."),
                "{source}"
            );
        }
    }
}
