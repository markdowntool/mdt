use super::*;

fn comment(text: &str, at: usize, name: &str) -> String {
    let blocks = editor_block_map(text);
    assert!(can_insert_at(text, &blocks, at), "{text}");
    let insert = insertion(text, &blocks, at, &Author::named(name), STAMP);
    let mut next = text.to_owned();
    next.insert_str(insert.at, &insert.text);
    next.insert_str(insert.caret, "Answer.");
    assert_eq!(&next[..insert.at], &text[..insert.at]);
    assert_eq!(
        &next[insert.at + insert.text.len() + "Answer.".len()..],
        &text[insert.at..]
    );
    next
}

/// Markdown's real hierarchy, excluding any list-looking content quoted in
/// the comment. This guards portability, not merely editor classification.
pub(super) fn item_faces(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut depth: usize = 0;
    let mut quotes = 0;
    for (event, range) in Parser::new_ext(text, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::BlockQuote(_)) => quotes += 1,
            Event::End(TagEnd::BlockQuote(_)) => quotes -= 1,
            Event::Start(Tag::Item) if quotes == 0 => {
                out.push((
                    depth,
                    text[range.clone()].lines().next().unwrap().to_owned(),
                ));
                depth += 1;
            }
            Event::End(TagEnd::Item) if quotes == 0 => depth -= 1,
            _ => {}
        }
    }
    out
}

#[test]
fn nested_task_comments_belong_to_the_selected_item() {
    let original = "1. Parent\n   - [ ] Child A\n   - Child B\n2. Next\n";
    let first = comment(original, original.find("Child A").unwrap(), "Alex");
    assert!(first.contains(&format!("Child A\n\n     {HEADER_PREFIX}Alex · user:unlinked · {STAMP}\n     > Answer.\n\n   - Child B")), "{first}");
    assert_eq!(item_faces(&first), item_faces(original));
    assert_eq!(block_map(&first).len(), 1);
    assert_eq!(block_map(&first)[0].kind, BlockKind::List);
    let second = comment(&first, first.find("Answer.").unwrap(), "Morgan");
    assert_eq!(item_faces(&second), item_faces(original));
    assert!(second.find("Alex").unwrap() < second.find("Morgan").unwrap());
    assert!(second.find("Morgan").unwrap() < second.find("Child B").unwrap());
    assert_eq!(
        editor_block_map(&second)
            .iter()
            .filter(|block| is_reply(&second, block))
            .count(),
        2
    );
    assert!(!second.contains("> > "));
    assert!(editor_block_map(&second)
        .iter()
        .filter(|b| is_reply(&second, b))
        .all(|b| header(&second[b.range.clone()]).unwrap().indent == "     "));
}

#[test]
fn parent_comments_precede_children_and_child_comments_stay_with_children() {
    let original = "- Parent\n  - Child\n- Sibling\n";
    let child = comment(original, original.find("Child").unwrap(), "Child author");
    let parent = comment(&child, 2, "Parent author");
    assert!(
        parent.find("Parent author").unwrap() < parent.find("- Child").unwrap(),
        "{parent}"
    );
    assert!(parent.contains(&format!("\n  {HEADER_PREFIX}Parent author")));
    assert!(parent.contains(&format!("\n    {HEADER_PREFIX}Child author")));
    let again = comment(&parent, parent.find("Parent author").unwrap(), "Again");
    assert!(again.find("Again").unwrap() < again.find("- Child").unwrap());
    assert_eq!(item_faces(&again), item_faces(original));
}

#[test]
fn marker_width_and_tabs_determine_quote_indent_without_checkbox_width() {
    for (source, indent) in [
        ("- [x] Task\n- Next\n", 2),
        ("10. [ ] Task\n11. Next\n", 4),
        ("8) Task\n9) Next\n", 3),
        ("-\tTask\n- Next\n", 4),
        ("-   Task\n- Next\n", 4),
    ] {
        let result = comment(source, source.find("Task").unwrap(), "Alex");
        assert!(
            result.contains(&format!("\n{}{HEADER_PREFIX}", " ".repeat(indent))),
            "{result}"
        );
        assert_eq!(item_faces(&result), item_faces(source));
        assert_eq!(block_map(&result).len(), 1);
    }
}

#[test]
fn hard_line_comment_keeps_literal_inline_text_and_following_fence() {
    let original = "- **A thought\n  across lines**\n\n  ```rust\n  let x = 1;\n  ```\n- Next\n";
    let result = comment(original, original.find("thought").unwrap(), "Alex");
    assert!(result.starts_with("- **A thought\n\n"), "{result}");
    assert!(
        result.contains("\n\n  across lines**\n\n  ```rust\n  let x = 1;\n  ```\n- Next\n"),
        "{result}"
    );
    assert_eq!(item_faces(&result), item_faces(original));
    // Inserting a block necessarily separates inline emphasis across this
    // newline. It preserves the literal source instead of rewriting markup.
    let fenced = format!("- Task\n  ```md\n  {HEADER_PREFIX}Fake · user:unlinked · {STAMP}\n  > body\n  ```\n- Next\n");
    assert_eq!(
        editor_block_map(&fenced),
        block_map(&fenced),
        "fenced header-looking text is not extracted"
    );
}

#[test]
fn nested_unclosed_fences_refuse_comments_without_blocking_other_items() {
    for source in [
        "- First\n- Code\n  ```rust",
        "- First\n- Code\n  ```rust\n",
        "- First\n- Code\n  ```rust\n  let x = 1;",
        "- First\n- Code\n  ```rust\n  let x = 1;\n- Next\n",
        "1. Parent\n   - Code\n     ```rust\n     let x = 1;",
    ] {
        let blocks = editor_block_map(source);
        let opening = source.find("```").unwrap();
        assert!(!can_insert_at(source, &blocks, opening), "{source}");
        if let Some(code) = source.find("let x") {
            assert!(!can_insert_at(source, &blocks, code), "{source}");
        }
        assert!(
            can_insert_at(source, &blocks, source.find("Code").unwrap()),
            "item text precedes the fence: {source}"
        );
        if let Some(first) = source.find("First") {
            assert!(can_insert_at(source, &blocks, first));
        }
    }
    for source in [
        "- Code\n  ```\n  ```",
        "- Code\n  ```rust\n  let x = 1;\n  ```\n- Next\n",
        "1. Parent\n   - Code\n     ```rust\n     let x = 1;\n     ```\n   - Next\n",
    ] {
        let at = source.find("Code").unwrap();
        assert!(
            can_insert_at(source, &editor_block_map(source), at),
            "{source}"
        );
        let after = comment(source, at, "Alex");
        assert_eq!(item_faces(&after), item_faces(source));
        assert!(after.find("Alex").unwrap() < after.find("```").unwrap());
        let closing = source.rfind("```").unwrap();
        assert!(
            can_insert_at(source, &editor_block_map(source), closing),
            "{source}"
        );
        let after_fence = comment(source, closing, "Alex");
        assert!(after_fence.find("Alex").unwrap() > after_fence.rfind("```").unwrap());
    }
}

#[test]
fn editor_fragments_share_source_and_numbering_without_changing_export() {
    let original = "# Start\n\n1. First\n1. Second\n1. Third\n\nAfter.\n";
    let first = comment(original, original.find("First").unwrap(), "One");
    let text = comment(&first, first.find("Second").unwrap(), "Two");
    let plain = block_map(&text);
    assert_eq!(
        plain.iter().filter(|b| b.kind == BlockKind::List).count(),
        1
    );
    assert!(plain.iter().all(|b| b.source_list.is_none()));
    let editor = editor_block_map(&text);
    assert_eq!(editor.iter().filter(|b| is_reply(&text, b)).count(), 2);
    let fragments: Vec<_> = editor
        .iter()
        .filter_map(|block| block.source_list.as_ref())
        .collect();
    assert_eq!(fragments.len(), 5);
    assert!(fragments
        .iter()
        .all(|source| Arc::ptr_eq(source, fragments[0])));
    assert_eq!(
        fragments[0].range,
        plain
            .iter()
            .find(|b| b.kind == BlockKind::List)
            .unwrap()
            .range
    );
    assert_eq!(
        fragments[0]
            .numbers
            .get(&text.find("1. Second").unwrap())
            .map(String::as_str),
        Some("2")
    );
    assert_eq!(
        fragments[0]
            .numbers
            .get(&text.find("1. Third").unwrap())
            .map(String::as_str),
        Some("3")
    );
    for block in &editor {
        assert!(!block.range.is_empty());
        assert!(text.is_char_boundary(block.range.start) && text.is_char_boundary(block.range.end));
    }
    for pair in editor.windows(2) {
        assert!(pair[0].range.end <= pair[1].range.start);
        assert!(text[pair[0].range.end..pair[1].range.start]
            .trim()
            .is_empty());
    }
    let at = text.find("Answer.").unwrap();
    assert!(containing(&text, &editor, at).is_some());
}

#[test]
fn ordered_task_comments_keep_numbering_at_the_marker_content_column() {
    let source = "1. [ ] First\n1. [x] Second\n1. [ ] Third\n";
    let text = comment(source, source.find("First").unwrap(), "Alex");
    let blocks = editor_block_map(&text);
    let list = blocks
        .iter()
        .find_map(|block| block.source_list.as_ref())
        .unwrap();
    assert_eq!(
        list.numbers
            .get(&text.find("1. [x] Second").unwrap())
            .map(String::as_str),
        Some("2")
    );
    assert_eq!(
        list.numbers
            .get(&text.find("1. [ ] Third").unwrap())
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(item_faces(&text), item_faces(source));
}

#[test]
fn indented_legacy_header_and_paste_keep_their_container_prefix() {
    let text = format!("   {LEGACY_HEADER_PREFIX}Alex · {STAMP}**\n   > Body\n   > > Nested");
    let header = header(&text).unwrap();
    assert_eq!(header.indent, "   ");
    assert_eq!(
        with_relative_label(&text, header.time),
        "   > Alex · just now\n   > Body\n   > > Nested"
    );
    assert_eq!(quote_prefix("\t > Body"), 4);
    assert_eq!(quote_prefix("   lazy"), 0);
    assert_eq!(
        quoted_paste_with_indent("first\n\n> Nested", "   "),
        "first\n   > \n   > > Nested"
    );
    let visible: Vec<_> = lines(&text)
        .into_iter()
        .map(|line| text[line.range.start + line.prefix..line.range.end].to_owned())
        .collect();
    assert_eq!(visible[1..], ["Body", "> Nested"]);
}

#[test]
fn outline_folds_and_faces_follow_original_item_ownership() {
    let original = "1. Parent\n   - [ ] Child A\n   - Child B\n2. Next\n";
    let child = comment(original, original.find("Child A").unwrap(), "Child author");
    let text = comment(&child, 0, "Parent author");
    let blocks = editor_block_map(&text);
    let units = crate::outline::units(&text, &blocks);
    let faces: Vec<_> = units
        .iter()
        .filter(|unit| unit.owner)
        .map(|unit| (&text[unit.face.clone()], unit.level))
        .collect();
    assert_eq!(
        faces,
        [
            ("1. Parent", 8),
            ("   - [ ] Child A", 9),
            ("   - Child B", 9),
            ("2. Next", 8)
        ]
    );
    for unit in &units {
        assert!(blocks[unit.block].range.contains(&unit.face.start));
    }
    let folded = crate::outline::hidden(&units, |unit| text[unit.face.clone()].contains("Child A"));
    let child_comment = text[..text.find("Child author").unwrap()]
        .rfind('\n')
        .unwrap()
        + 1;
    let parent_comment = text[..text.find("Parent author").unwrap()]
        .rfind('\n')
        .unwrap()
        + 1;
    assert!(crate::outline::is_hidden(&folded, child_comment));
    assert!(!crate::outline::is_hidden(&folded, parent_comment));
    assert!(!crate::outline::is_hidden(
        &folded,
        text.find("   - Child B").unwrap()
    ));
    let parent_fold =
        crate::outline::hidden(&units, |unit| &text[unit.face.clone()] == "1. Parent");
    assert!(crate::outline::is_hidden(&parent_fold, child_comment));
    assert!(crate::outline::is_hidden(&parent_fold, parent_comment));
    assert!(!crate::outline::is_hidden(
        &parent_fold,
        text.find("2. Next").unwrap()
    ));
}
