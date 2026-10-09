//! Quote lines: which hard lines of a text stand inside
//! a block quote, how deep each stands, and how many bytes of `>` markers it
//! begins with.
//!
//! A quote is drawn as prose at an indent — a bar and a gap per level — and
//! edited with the same geometry: its markers hidden and every row of the
//! line at its level's column (`Workspace::editor_decorations`,
//! `Workspace::render_quote_block`). Both sides ask here, so they cannot
//! disagree about which lines are quoted or how deep.
//!
//! Only a quote of **prose** answers: paragraphs, and quotes inside quotes.
//! One holding a list, a fence, a table, a heading, a rule, raw HTML, or
//! inline markup that crosses a line is left exactly as it was — rendered
//! whole by the markdown view and edited as source — because a line of it
//! drawn alone would not be the line the rendered quote shows.
//!
//! Depth is the parser's, not the marker count's. CommonMark lets a paragraph
//! continue onto a line with fewer markers than its quote is deep, or none:
//! in `> a`, `>> b`, `> c`, the `c` is the second paragraph's third word,
//! two deep, though its line carries one marker. So the markers hidden are
//! the ones the line has, and the column is the depth the parser gives it.

use std::ops::Range;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

use crate::blocks::{lines_render_independently, parser_options};

/// One hard line inside a quote of prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuoteLine {
    /// The line without its newline, as a byte range of the text.
    pub range: Range<usize>,
    /// The bytes at the line's start that are quote syntax and nothing a
    /// reader sees: each `>` with up to three spaces before it, and the
    /// whitespace after the last one — which the paragraph drops, so it is
    /// hidden with the markers. Zero on a lazy continuation line.
    pub prefix: usize,
    /// How many quotes deep the line stands: 1 for `> a`, 3 for `>>> a` and
    /// for a line lazily continuing the paragraph under it.
    pub depth: usize,
}

/// The lines of every quote of prose in `text`, in order. A line outside
/// every quote, or inside a quote that holds more than prose, is absent.
pub fn quote_lines(text: &str) -> Vec<QuoteLine> {
    // A text with no line that starts with a marker holds no quote: the
    // parse below is skipped for the ordinary line of prose.
    if !text
        .split('\n')
        .any(|l| l.trim_start_matches(' ').starts_with('>'))
    {
        return Vec::new();
    }
    // Every top-level quote of prose: its own range, and the range and depth
    // of every quote in it, itself included.
    let mut quotes: Vec<(Range<usize>, Vec<(usize, Range<usize>)>)> = Vec::new();
    let mut open = 0usize;
    let mut deep = 0usize;
    let mut current: Option<(Range<usize>, Vec<(usize, Range<usize>)>, bool)> = None;
    for (event, range) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::BlockQuote(_)) => {
                if open == 0 {
                    current = Some((range.clone(), Vec::new(), true));
                }
                deep += 1;
                if let Some(c) = current.as_mut() {
                    c.1.push((deep, range.clone()));
                }
                open += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                open -= 1;
                deep -= 1;
                if open == 0 {
                    if let Some((r, nested, prose)) = current.take() {
                        if prose && lines_render_independently(&text[r.clone()]) {
                            quotes.push((r, nested));
                        }
                    }
                }
            }
            Event::Start(tag) => {
                open += 1;
                let inline = matches!(
                    tag,
                    Tag::Paragraph
                        | Tag::Emphasis
                        | Tag::Strong
                        | Tag::Strikethrough
                        | Tag::Link { .. }
                        | Tag::Image { .. }
                );
                if !inline {
                    if let Some(c) = current.as_mut() {
                        c.2 = false;
                    }
                }
            }
            Event::End(_) => open -= 1,
            Event::Rule => {
                if let Some(c) = current.as_mut() {
                    c.2 = false;
                }
            }
            _ => {}
        }
    }
    if quotes.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    for line in text.split('\n') {
        let range = at..at + line.len();
        at = range.end + 1;
        if line.is_empty() {
            continue;
        }
        // The line's last byte decides: a nested quote's range starts at
        // its own marker, past the outer one, so the line's start would
        // undercount it.
        let last = range.end - 1;
        let inside = |r: &Range<usize>| r.start <= last && last < r.end;
        let Some((_, nested)) = quotes.iter().find(|(r, _)| inside(r)) else {
            continue;
        };
        let depth = nested
            .iter()
            .filter(|(_, r)| inside(r))
            .map(|(d, _)| *d)
            .max()
            .unwrap_or(1);
        out.push(QuoteLine {
            range,
            prefix: marker_prefix(line, depth),
            depth,
        });
    }
    out
}

/// How many bytes of `line` are quote markers and the whitespace around
/// them: up to `depth` markers, each up to three spaces then `>` then one
/// optional space or tab (CommonMark's block quote marker), then whatever
/// spaces and tabs follow the last one — or begin a line with no marker.
fn marker_prefix(line: &str, depth: usize) -> usize {
    let b = line.as_bytes();
    let mut pos = 0usize;
    for _ in 0..depth {
        let mut p = pos;
        let mut spaces = 0;
        while p < b.len() && b[p] == b' ' && spaces < 3 {
            p += 1;
            spaces += 1;
        }
        if p < b.len() && b[p] == b'>' {
            p += 1;
            if p < b.len() && matches!(b[p], b' ' | b'\t') {
                p += 1;
            }
            pos = p;
        } else {
            break;
        }
    }
    while pos < b.len() && matches!(b[pos], b' ' | b'\t') {
        pos += 1;
    }
    pos
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(text of the line, prefix, depth)` per quoted line.
    fn lines(text: &str) -> Vec<(&str, usize, usize)> {
        quote_lines(text)
            .into_iter()
            .map(|l| (&text[l.range.clone()], l.prefix, l.depth))
            .collect()
    }

    #[test]
    fn a_quote_line_is_one_deep_and_its_marker_is_the_prefix() {
        assert_eq!(lines("> a\n"), vec![("> a", 2, 1)]);
        assert_eq!(
            lines(">a\n"),
            vec![(">a", 1, 1)],
            "the space after `>` is optional"
        );
        assert_eq!(
            lines("  > a\n"),
            vec![("  > a", 4, 1)],
            "up to three spaces may precede it"
        );
        assert_eq!(
            lines(">   a\n"),
            vec![(">   a", 4, 1)],
            "the paragraph drops the spaces after it"
        );
        assert_eq!(lines(">\n"), vec![(">", 1, 1)], "a blank quote line");
    }

    #[test]
    fn nesting_is_counted_by_the_parser_and_a_blank_quote_line_closes_the_inner_ones() {
        assert_eq!(
            lines("> one\n>> two\n>>> three\n>\n> back\n"),
            vec![
                ("> one", 2, 1),
                (">> two", 3, 2),
                (">>> three", 4, 3),
                (">", 1, 1),
                ("> back", 2, 1)
            ]
        );
        assert_eq!(lines("> > > a\n"), vec![("> > > a", 6, 3)]);
    }

    #[test]
    fn a_lazy_continuation_keeps_its_depth_with_no_marker_to_hide() {
        assert_eq!(lines("> a\nlazy\n"), vec![("> a", 2, 1), ("lazy", 0, 1)]);
        assert_eq!(
            lines(">>> a\nlazy\n"),
            vec![(">>> a", 4, 3), ("lazy", 0, 3)],
            "three deep, the lazy line is three deep too"
        );
        assert_eq!(
            lines("> a\n   lazy\n"),
            vec![("> a", 2, 1), ("   lazy", 3, 1)],
            "leading whitespace on a lazy line is the paragraph's to drop"
        );
    }

    #[test]
    fn fewer_markers_than_the_depth_is_still_a_continuation() {
        // CommonMark: `c` continues the paragraph `b`, two deep, though its
        // line carries one marker. The one marker is hidden; the column is
        // the second level's.
        assert_eq!(
            lines("> a\n>> b\n> c\n"),
            vec![("> a", 2, 1), (">> b", 3, 2), ("> c", 2, 2)]
        );
    }

    #[test]
    fn a_quote_of_prose_may_carry_inline_markup_on_a_line() {
        assert_eq!(
            lines("> **bold** and a [link](x)\n> `code`\n"),
            vec![("> **bold** and a [link](x)", 2, 1), ("> `code`", 2, 1)]
        );
    }

    #[test]
    fn a_quote_holding_more_than_prose_answers_nothing() {
        assert!(lines("> - item\n> - item\n").is_empty(), "a list");
        assert!(lines("> ```\n> code\n> ```\n").is_empty(), "a fence");
        assert!(
            lines("> a\n>\n>     code\n").is_empty(),
            "an indented fence"
        );
        assert!(
            lines("> | a | b |\n> | - | - |\n> | 1 | 2 |\n").is_empty(),
            "a table"
        );
        assert!(lines("> # heading\n").is_empty(), "a heading");
        assert!(lines("> a\n>\n> ---\n").is_empty(), "a rule");
        assert!(lines("> <div>\n> </div>\n").is_empty(), "html");
        assert!(
            lines("> **bold\n> text**\n").is_empty(),
            "inline markup crossing a line"
        );
        assert!(
            lines("> [[wiki\n> link]]\n").is_empty(),
            "a wikilink crossing a line"
        );
        assert!(
            lines("> a\n>> - nested list\n").is_empty(),
            "a list inside the inner quote"
        );
    }

    #[test]
    fn a_marker_that_is_not_a_top_level_quote_is_not_one() {
        assert!(
            lines("- item\n  > quoted inside the item\n").is_empty(),
            "a quote inside a list"
        );
        assert!(
            lines("```\n> not a quote\n```\n").is_empty(),
            "a marker inside a fence"
        );
        assert!(lines("plain text with > in it\n").is_empty());
        assert!(lines("").is_empty());
    }

    #[test]
    fn two_quotes_in_one_text_each_answer_for_their_own_lines() {
        assert_eq!(
            lines("> one\n\npara\n\n>> two\ntwo lazy\n"),
            vec![("> one", 2, 1), (">> two", 3, 2), ("two lazy", 0, 2)]
        );
    }

    #[test]
    fn a_quote_that_interrupts_a_paragraph_still_answers() {
        assert_eq!(lines("para\n> a\n"), vec![("> a", 2, 1)]);
    }
}
