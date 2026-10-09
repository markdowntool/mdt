//! A comment is a child block of its actual Markdown list item. The parser
//! supplies ownership; indentation alone never turns fence text into items.

use super::*;

struct Item {
    range: Range<usize>,
    indent: usize,
}

pub(super) struct Placement {
    pub at: usize,
    pub indent: String,
    pub allowed: bool,
    /// Missing indentation before an existing lazy continuation line.
    pub continuation: String,
}

pub(super) fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map_or(0, |newline| newline + 1)
}

/// End of the last nonblank line, keeping its own trailing spaces. Parser
/// container ranges can include blank lines and the next item's indentation.
pub(super) fn content_end(text: &str, end: usize) -> usize {
    let ink = text[..end].trim_end().len();
    let end = text[ink..end]
        .find('\n')
        .map_or(end, |newline| ink + newline);
    if end > ink && text.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// The next source newline belongs to the selected hard line, including CRLF.
/// With no next newline the boundary is EOF. No visual wrapping is involved.
pub(super) fn line_end(text: &str, at: usize) -> usize {
    text[at..]
        .find('\n')
        .map_or(text.len(), |newline| at + newline + 1)
}

fn columns(text: &str, mut column: usize) -> usize {
    for byte in text.bytes() {
        column += if byte == b'\t' { 4 - column % 4 } else { 1 };
    }
    column
}

/// CommonMark's list continuation column: marker width plus its whitespace
/// padding (1–4 columns; more means one). A task checkbox is content, not part
/// of the marker, and therefore never adds four columns to the quote indent.
fn content_column(text: &str, marker: usize) -> usize {
    let start = line_start(text, marker);
    let line = text[marker..].split('\n').next().unwrap_or("");
    let digits = line.bytes().take_while(u8::is_ascii_digit).count();
    let width = if digits == 0 { 1 } else { digits + 1 };
    let column = columns(&text[start..marker], 0) + width;
    let tail = line.get(width..).unwrap_or("");
    let whitespace = tail.len() - tail.trim_start_matches([' ', '\t']).len();
    let padding = columns(&tail[..whitespace], column) - column;
    column
        + if (1..=4).contains(&padding) {
            padding
        } else {
            1
        }
}

pub(super) fn placement(text: &str, list: &Range<usize>, at: usize) -> Option<Placement> {
    let mut items: Vec<Item> = Vec::new();
    let mut stack: Vec<Option<usize>> = Vec::new();
    let mut quote_depth = 0;
    let mut fence: Option<Range<usize>> = None;
    let mut code_content = Vec::new();
    let mut containers = Vec::new();
    let mut comments: Vec<(usize, Range<usize>)> = Vec::new();
    for (event, local) in Parser::new_ext(&text[list.clone()], parser_options()).into_offset_iter()
    {
        let range = list.start + local.start..list.start + local.end;
        match event {
            Event::Start(Tag::CodeBlock(pulldown_cmark::CodeBlockKind::Fenced(_)))
                if quote_depth == 0 =>
            {
                fence = Some(range.start..line_end(text, range.start));
            }
            Event::Text(_) if fence.is_some() => {
                fence.as_mut().unwrap().end = range.end;
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(fence) = fence.take() {
                    code_content.push(fence);
                }
            }
            Event::Start(Tag::Table(_) | Tag::HtmlBlock) if quote_depth == 0 => {
                containers.push(range)
            }
            Event::Start(Tag::Item) => {
                let item = if quote_depth == 0 {
                    let marker = range.start;
                    let line = line_start(text, marker);
                    let start = if text[line..marker].trim().is_empty() {
                        line
                    } else {
                        marker
                    };
                    let index = items.len();
                    items.push(Item {
                        range: start..range.end,
                        indent: content_column(text, marker),
                    });
                    Some(index)
                } else {
                    None
                };
                stack.push(item);
            }
            Event::End(TagEnd::Item) => {
                stack.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if quote_depth == 0 {
                    let start = line_start(text, range.start);
                    if let Some(index) = stack
                        .last()
                        .copied()
                        .flatten()
                        .filter(|_| header(&text[start..range.end]).is_some())
                    {
                        comments.push((index, start..range.end));
                    } else {
                        containers.push(range);
                    }
                }
                quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            _ => {}
        }
    }
    let (owner, item) =
        items.iter().enumerate().rev().find(|(_, item)| {
            item.range.contains(&at) || (at == text.len() && item.range.end == at)
        })?;
    // Existing comments stay whole and append to their immediate flat thread.
    // A later comment after another content line belongs to that other line.
    let mut boundary = comments
        .iter()
        .find(|(_, range)| range.contains(&at))
        .map_or_else(
            || line_end(text, at),
            |(_, range)| line_end(text, content_end(text, range.end)),
        );
    for (_, range) in comments.iter().filter(|(index, _)| *index == owner) {
        if boundary <= range.start && text[boundary..range.start].trim().is_empty() {
            boundary = line_end(text, content_end(text, range.end));
        }
    }
    // A closing fence line is safe; its code-content span ends before it.
    // Tables, HTML and ordinary quotes can only be followed, never split.
    let allowed = !code_content
        .iter()
        .any(|range| range.start < boundary && boundary <= range.end)
        && !containers
            .iter()
            .any(|range| range.start < boundary && boundary < content_end(text, range.end));
    let mut continuation = String::new();
    if item.range.contains(&boundary) {
        let next = text[boundary..].split('\n').next().unwrap_or("");
        if !next.trim().is_empty() {
            let whitespace = next.len() - next.trim_start_matches([' ', '\t']).len();
            let padding = &next[..whitespace];
            while columns(padding, continuation.len()) < item.indent {
                continuation.push(' ');
            }
        }
    }
    Some(Placement {
        at: boundary,
        indent: " ".repeat(item.indent),
        allowed,
        continuation,
    })
}
