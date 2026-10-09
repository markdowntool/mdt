use super::*;

fn insert_answer(source: &str, at: usize, author: &str) -> (String, Insertion) {
    let blocks = editor_block_map(source);
    assert!(can_insert_at(source, &blocks, at), "{source}");
    let insertion = insertion(source, &blocks, at, &Author::named(author), STAMP);
    let mut text = source.to_owned();
    text.insert_str(insertion.at, &insertion.text);
    // One contiguous edit owns separators and any lazy continuation indent.
    let mut undone = text.clone();
    undone.replace_range(insertion.at..insertion.at + insertion.text.len(), "");
    assert_eq!(undone, source);
    assert_eq!(&text[insertion.caret - 2..insertion.caret], "> ");
    text.insert_str(insertion.caret, "Answer.");
    (text, insertion)
}

#[test]
fn comments_follow_source_newlines_not_paragraph_ends_or_visual_wraps() {
    let source = "First hard line αβ.\nSecond hard line.\nThird hard line.\n\nNext paragraph.";
    for at in [0, source.find('α').unwrap(), source.find('\n').unwrap()] {
        let (text, insert) = insert_answer(source, at, "One");
        assert_eq!(insert.at, source.find('\n').unwrap() + 1);
        assert!(text.starts_with("First hard line αβ.\n\n"));
        assert!(text.find("One").unwrap() < text.find("Second hard line").unwrap());
        assert!(text.ends_with("Second hard line.\nThird hard line.\n\nNext paragraph."));
    }
    let one_long_line = "A source line that may wrap visually still has no source newline.";
    assert_eq!(
        insertion_offset(one_long_line, &editor_block_map(one_long_line), 12),
        one_long_line.len()
    );
}

#[test]
fn line_threads_stop_at_the_next_content_line() {
    let source = "First line\nSecond line\nThird line";
    let (first, _) = insert_answer(source, 0, "One");
    let (second, _) = insert_answer(&first, first.find("Second line").unwrap(), "Two");
    for at in [
        0,
        second.find("One").unwrap(),
        second.find("Answer.").unwrap(),
    ] {
        let (third, _) = insert_answer(&second, at, "Three");
        assert!(third.find("One").unwrap() < third.find("Three").unwrap());
        assert!(third.find("Three").unwrap() < third.find("Second line").unwrap());
        assert!(third.find("Second line").unwrap() < third.find("Two").unwrap());
        assert!(third.find("Two").unwrap() < third.find("Third line").unwrap());
    }
}

#[test]
fn crlf_eof_and_empty_source_use_one_reversible_insert() {
    for source in [
        "",
        "Single α line",
        "First\r\nSecond",
        "First\r\nSecond\r\n",
        "First\r\n\r\nSecond",
    ] {
        let (text, insert) = insert_answer(source, 0, "One");
        let expected = source
            .find('\n')
            .map_or(source.len(), |newline| newline + 1);
        assert_eq!(insert.at, expected, "{source:?}");
        if source.contains("\r\n") {
            assert!(!text.replace("\r\n", "").contains('\n'), "{text:?}");
            assert!(text.contains("Answer.\r\n\r\nSecond"), "{text:?}");
        }
    }
    let (text, _) = insert_answer("First\r\nSecond", "First\r\nSecond".len(), "One");
    assert!(text.starts_with("First\r\nSecond\r\n\r\n"));
    assert!(!text.replace("\r\n", "").contains('\n'));
    let blocks = editor_block_map(&text);
    let comment = blocks.iter().find(|block| is_reply(&text, block)).unwrap();
    let exit = exit_after(&text, comment.range.end);
    let mut after = text.clone();
    after.insert_str(exit.at, &exit.text);
    after.insert_str(exit.caret, "Outside.");
    assert!(after.ends_with("Answer.\r\n\r\nOutside."), "{after:?}");
    assert_eq!(
        editor_block_map(&after).last().unwrap().kind,
        BlockKind::Paragraph
    );
}

#[test]
fn list_hard_lines_keep_lazy_continuations_inside_the_same_item() {
    for (source, continuation) in [
        ("- First\nlazy next\n- Last\n", "  lazy next"),
        ("- [ ] First\n lazy next\n- Last\n", "  lazy next"),
        ("10. First\nlazy next\n11. Last\n", "    lazy next"),
        (
            "1. Parent\n   - [ ] First\nlazy next\n   - Sibling\n2. Last\n",
            "     lazy next",
        ),
        ("- First\r\nlazy next\r\n- Last\r\n", "  lazy next"),
    ] {
        let (text, _) = insert_answer(source, source.find("First").unwrap(), "One");
        assert!(text.contains(continuation), "{text}");
        assert!(text.find("One").unwrap() < text.find("lazy next").unwrap());
        assert_eq!(
            super::list_tests::item_faces(&text),
            super::list_tests::item_faces(source)
        );
        assert_eq!(block_map(&text).len(), 1, "{text}");
        let comments = editor_block_map(&text);
        assert_eq!(
            comments
                .iter()
                .filter(|block| is_reply(&text, block))
                .count(),
            1
        );
    }
}

#[test]
fn list_continuation_comments_stay_at_that_line_and_append_only_its_thread() {
    let source =
        "1. Parent\n   - [ ] First\n     second line\n     third line\n   - Sibling\n2. Last\n";
    let (first, _) = insert_answer(source, source.find("First").unwrap(), "One");
    let (second, _) = insert_answer(&first, first.find("second line").unwrap(), "Two");
    for at in [
        second.find("First").unwrap(),
        second.find("One").unwrap(),
        second.find("Answer.").unwrap(),
    ] {
        let (third, _) = insert_answer(&second, at, "Three");
        assert!(third.find("One").unwrap() < third.find("Three").unwrap());
        assert!(third.find("Three").unwrap() < third.find("second line").unwrap());
        assert!(third.find("second line").unwrap() < third.find("Two").unwrap());
        assert!(third.find("Two").unwrap() < third.find("third line").unwrap());
        assert_eq!(
            super::list_tests::item_faces(&third),
            super::list_tests::item_faces(source)
        );
        assert!(editor_block_map(&third)
            .iter()
            .filter(|block| is_reply(&third, block))
            .all(|block| header(&third[block.range.clone()]).unwrap().indent == "     "));
    }
}

#[test]
fn nested_structures_refuse_interior_lines_and_allow_their_last_line() {
    for source in [
        "- Item\n\n  | a | b |\n  | - | - |\n  | c | d |\n- Next\n",
        "- Item\n\n  > ordinary\n  > quote\n- Next\n",
        "- Item\n\n  <div>\n  html\n  </div>\n\n- Next\n",
    ] {
        let blocks = editor_block_map(source);
        let interior = source.find("\n\n").unwrap() + 2;
        assert!(!can_insert_at(source, &blocks, interior), "{source}");
        let end = source.find("\n- Next").unwrap();
        let last_line = source[..end].trim_end().rfind('\n').unwrap() + 1;
        assert!(can_insert_at(source, &blocks, last_line), "{source}");
        let (text, _) = insert_answer(source, last_line, "One");
        assert_eq!(
            super::list_tests::item_faces(&text),
            super::list_tests::item_faces(source)
        );
    }
}
