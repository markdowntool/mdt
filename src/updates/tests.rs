use super::*;
use serde_json::{json, Value};

const A: &str = include_str!("../../tests/fixtures/updates/a.md");
const B: &str = include_str!("../../tests/fixtures/updates/b.md");
const C: &str = include_str!("../../tests/fixtures/updates/c.md");
const D: &str = include_str!("../../tests/fixtures/updates/d.md");

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    database: PathBuf,
    gates: PathBuf,
}
impl Fixture {
    fn new(source: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        fs::write(&path, source).unwrap();
        Self {
            path,
            database: dir.path().join("state/updates.sqlite3"),
            gates: dir.path().join("gates"),
            _dir: dir,
        }
    }
    fn request(&self, args: &[&str]) -> Request {
        Request::parse(
            std::iter::once(self.path.clone().into_os_string())
                .chain(args.iter().map(OsString::from)),
        )
        .unwrap()
        .unwrap()
    }
    fn read(&self, time: &str, args: &[&str]) -> Value {
        let mut request = self.request(args);
        request.json = true;
        let output = observe(
            &request,
            &self.database,
            &self.gates,
            || parse_time(time).unwrap(),
            || {},
        )
        .unwrap();
        serde_json::from_str(&output).unwrap()
    }
    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.database).unwrap()
    }
    fn row(&self) -> (i64, String, String, i64, String) {
        self.connection()
            .query_row(
                "SELECT tracked_from_us, source, node_times, last_us, subtree_times FROM documents, clock",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap()
    }
}

fn paths(response: &Value) -> Vec<Value> {
    response["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["path"].clone())
        .collect()
}

#[test]
fn a_background_observation_keeps_changes_available_to_independent_readers() {
    let original = "# Plan\n\n## Scope\n\nHuman draft.\n\n## Budget\n\nCost: 100.\n";
    let f = Fixture::new(original);
    let first = f.read("1664-01-01T13:00:00Z", &[]);
    let checkpoint = first["as_of"].as_str().unwrap();
    let changed = original.replace("Cost: 100.", "Cost: 125.");
    fs::write(&f.path, &changed).unwrap();

    observe_saved_file(&f.path, &f.database, &f.gates).unwrap();
    let now = format_time(Utc::now().timestamp_micros() + 1_000_000).unwrap();
    let reader_one = f.read(&now, &["--since", checkpoint]);
    let reader_two = f.read(&now, &["--since", checkpoint]);
    assert_eq!(paths(&reader_one), vec![json!(["Plan", "Budget"])]);
    assert_eq!(reader_one["nodes"], reader_two["nodes"]);
    assert!(reader_one["nodes"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Cost: 125."));
    assert_eq!(f.row().1, changed);
}

#[test]
fn a_delayed_background_trigger_never_restores_an_older_source() {
    let f = Fixture::new("# Note\nOriginal.\n");
    let first = f.read("1664-01-01T13:00:00Z", &[]);
    // The GUI queued this path while the file was still Original. Another
    // reader sees the replacement before the background callback executes.
    fs::write(&f.path, "# Note\nNewest saved text.\n").unwrap();
    f.read("1664-01-01T14:00:00Z", &[]);
    observe_saved_file(&f.path, &f.database, &f.gates).unwrap();
    assert_eq!(f.row().1, "# Note\nNewest saved text.\n");
    let now = format_time(Utc::now().timestamp_micros() + 1_000_000).unwrap();
    let read = f.read(&now, &["--since", first["as_of"].as_str().unwrap()]);
    assert_eq!(read["status"], "changes");
    assert!(read["nodes"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Newest saved text."));
}

#[test]
fn the_writer_can_observe_without_relocking_its_held_gate() {
    let f = Fixture::new("# Note\nOriginal.\n");
    let first = f.read("1664-01-01T13:00:00Z", &[]);
    let gate = mdt_core::gate::try_lock(&f.gates, &f.path)
        .unwrap()
        .unwrap();
    fs::write(&f.path, "# Note\nChanged.\n").unwrap();
    assert_eq!(
        observe_saved_file(&f.path, &f.database, &f.gates)
            .unwrap_err()
            .code,
        "locked"
    );
    observe_saved_file_under_gate(&f.path, &f.database).unwrap();
    fs::write(&f.path, "# Note\nOriginal.\n").unwrap();
    observe_saved_file_under_gate(&f.path, &f.database).unwrap();
    drop(gate);
    let now = format_time(Utc::now().timestamp_micros() + 1_000_000).unwrap();
    let read = f.read(&now, &["--since", first["as_of"].as_str().unwrap()]);
    assert_eq!(
        read["status"], "changes",
        "both observed edits survive a return to old bytes"
    );
}

#[test]
fn logbook_text_matches_the_worked_example() {
    let f = Fixture::new(A);
    f.read("1664-01-01T13:00:00Z", &[]);
    fs::write(&f.path, B).unwrap();
    let mut ticks = ["1664-01-01T13:15:00Z", "1664-01-01T13:16:00Z"].into_iter();
    let text = observe(
        &f.request(&["--since", "1664-01-01T13:01:00Z"]),
        &f.database,
        &f.gates,
        || parse_time(ticks.next().unwrap()).unwrap(),
        || {},
    )
    .unwrap();
    let revision = crate::snapshots::revision(&fs::canonicalize(&f.path).unwrap(), B);
    assert!(text.contains(&format!("Revision {revision}\n")));
    let text = text.replace(&format!("Revision {revision}\n"), "");
    assert_eq!(
        text,
        "As of 1664-01-01T13:16:00Z\n\n\
[Baker's logbook > Little Red]\n\
## Little Red\n\
We gave Little Red sweets for Granny.\n\
- Two olive baguettes\n\
- Three cookies\n  - Peanut butter\n  - Chocalote\n  - Sugar\n\n\
[Baker's logbook > Witch's visit > White cow]\n\
- White cow\n\n\
[Baker's logbook > Witch's visit > Golden slipper]\n\
- Golden slipper\n"
    );
}

#[test]
fn logbook_observations_keep_other_readers_history_and_select_exact_subtrees() {
    let f = Fixture::new(A);
    let first = f.read("1664-01-01T13:00:00Z", &[]);
    assert_eq!(
        first,
        json!({"status": "first_observation", "hint": "Earlier history unknown. Keep as_of for this scope; use --since next.", "as_of": "1664-01-01T13:00:00.000001Z", "revision": crate::snapshots::revision(&fs::canonicalize(&f.path).unwrap(), A), "nodes": [{"path": [], "content": A}]})
    );
    fs::write(&f.path, B).unwrap();
    let b = f.read("1664-01-01T13:15:00Z", &["--since", "1664-01-01T13:01:00Z"]);
    assert_eq!(
        paths(&b),
        vec![
            json!(["Baker's logbook", "Little Red"]),
            json!(["Baker's logbook", "Witch's visit", "White cow"]),
            json!(["Baker's logbook", "Witch's visit", "Golden slipper"]),
        ]
    );
    assert_eq!(b["nodes"][0]["content"], "## Little Red\nWe gave Little Red sweets for Granny.\n- Two olive baguettes\n- Three cookies\n  - Peanut butter\n  - Chocalote\n  - Sugar\n\n");
    assert_eq!(b["nodes"][1]["content"], "- White cow\n");
    assert_eq!(b["nodes"][2]["content"], "- Golden slipper\n");
    assert_eq!(b["status"], "changes");
    let list = f.read(
        "1664-01-01T13:15:30Z",
        &["--since", "1664-01-01T13:01:00Z", "--list"],
    );
    assert_eq!(paths(&list), paths(&b));
    assert!(list["nodes"][0].get("content").is_none());
    fs::write(&f.path, C).unwrap();
    let c = f.read("1664-01-01T13:30:00Z", &["--since", "1664-01-01T13:16:00Z"]);
    assert_eq!(
        paths(&c),
        vec![
            json!(["Baker's logbook", "Witch's demands"]),
            json!(["Baker's logbook", "Jack"])
        ]
    );
    assert_eq!(c["nodes"][0]["content"], "## Witch's demands\nTo lift our curse:\n- White cow\n- Red cape\n- Yellow hair\n- Golden slipper\n\n");
    fs::write(&f.path, D).unwrap();
    let d = f.read("1664-01-01T13:45:00Z", &["--since", "1664-01-01T13:31:00Z"]);
    assert_eq!(
        paths(&d),
        vec![
            json!([
                "Baker's logbook",
                "Little Red",
                "Three cookies",
                "Chocolate"
            ]),
            json!(["Baker's logbook", "Witch's demands", "Red cape (received)"])
        ]
    );
    assert_eq!(d["nodes"][0]["content"], "  - Chocolate\n");
    assert_eq!(d["nodes"][1]["content"], "- Red cape (received)\n");
    let scoped = f.read(
        "1664-01-01T13:46:00Z",
        &["--since", "1664-01-01T13:31:00Z", "--node", "Little Red"],
    );
    assert_eq!(scoped["nodes"], json!([d["nodes"][0]]));
    let empty = f.read(
        "1664-01-01T13:46:01Z",
        &["--since", "1664-01-01T13:31:00Z", "--node", "Jack"],
    );
    assert_eq!(empty["nodes"], json!([]));
    let old_reader = f.read(
        "1664-01-01T13:47:00Z",
        &["--since", first["as_of"].as_str().unwrap()],
    );
    assert_eq!(
        paths(&old_reader),
        vec![
            json!(["Baker's logbook", "Little Red"]),
            json!(["Baker's logbook", "Witch's demands"]),
            json!(["Baker's logbook", "Jack"])
        ]
    );
    assert_eq!(f.row().0, parse_time("1664-01-01T13:00:00Z").unwrap());
    assert_eq!(f.row().1, D);
}

#[test]
fn first_timed_query_and_failed_scope_or_size_do_not_establish_a_baseline() {
    let f = Fixture::new("# Note\n- item\n");
    for (args, code) in [
        (
            vec!["--since", "1664-01-01T13:00:00Z"],
            "tracking_unavailable",
        ),
        (vec!["--node", "missing"], "not_found"),
        (vec!["--max-bytes", "1"], "too_large"),
    ] {
        let error = observe(&f.request(&args), &f.database, &f.gates, || 0, || {}).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            f.connection()
                .query_row("SELECT count(*) FROM documents", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            f.connection()
                .query_row("SELECT count(*) FROM clock", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let first = f.read("1664-01-01T13:00:00Z", &["--list", "--node", "item"]);
    assert_eq!(first["status"], "first_observation");
    assert_eq!(f.row().1, "# Note\n- item\n");
    let baseline = f.row();
    fs::write(&f.path, "# Note\n- changed\n").unwrap();
    for (args, code) in [
        (
            vec!["--since", "1664-01-01T12:59:00Z"],
            "tracking_unavailable",
        ),
        (vec!["--since", "2664-01-01T13:00:00Z"], "usage"),
        (vec!["--node", "item"], "not_found"),
        (vec!["--max-bytes", "1"], "too_large"),
    ] {
        assert_eq!(
            observe(&f.request(&args), &f.database, &f.gates, || 0, || {})
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(f.row(), baseline);
    }
}

#[test]
fn persisted_clock_handles_equal_ticks_rollback_inclusive_cutoffs_and_lost_delivery() {
    let f = Fixture::new("# Note\nold\n");
    let first = f.read("1664-01-01T13:00:00Z", &[]);
    let since = first["as_of"].as_str().unwrap();
    let unchanged = f.read("1664-01-01T13:00:00Z", &["--since", since]);
    assert_eq!(unchanged["nodes"], json!([]));
    fs::write(&f.path, "# Note\nnew\n").unwrap();
    let update = f.read(
        "1660-01-01T00:00:00Z",
        &["--since", unchanged["as_of"].as_str().unwrap()],
    );
    assert_eq!(paths(&update), vec![json!(["Note"])]);
    // Pretend the response was lost. The same older cutoff still gets the edit.
    let retry = f.read(
        "1660-01-01T00:00:00Z",
        &["--since", unchanged["as_of"].as_str().unwrap()],
    );
    assert_eq!(retry["nodes"], update["nodes"]);
    let next = f.read(
        "1660-01-01T00:00:00Z",
        &["--since", update["as_of"].as_str().unwrap()],
    );
    assert_eq!(next["nodes"], json!([]));
    assert!(parse_time(next["as_of"].as_str().unwrap()).unwrap() < 0);
}

#[test]
fn failed_recheck_and_failed_database_write_leave_observation_and_clock_together() {
    let f = Fixture::new("# A\nold\n");
    f.read("1664-01-01T13:00:00Z", &[]);
    let baseline = f.row();
    fs::write(&f.path, "# A\nnew\n").unwrap();
    let error = observe(
        &f.request(&[]),
        &f.database,
        &f.gates,
        || 0,
        || fs::write(&f.path, "third version").unwrap(),
    )
    .unwrap_err();
    assert_eq!(error.code, "changed_during_read");
    assert_eq!(f.row(), baseline);
    // A different SQLite reader allows writes under BEGIN IMMEDIATE but blocks
    // the final exclusive commit. The entire document/clock update must roll back.
    let reader = f.connection();
    let error = observe(
        &f.request(&[]),
        &f.database,
        &f.gates,
        || 0,
        || {
            reader
                .execute_batch("BEGIN; SELECT * FROM documents;")
                .unwrap()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "database_busy");
    reader.execute_batch("ROLLBACK").unwrap();
    assert_eq!(f.row(), baseline);
    f.connection().execute_batch("CREATE TRIGGER fail_clock BEFORE UPDATE ON clock BEGIN SELECT RAISE(ABORT, 'injected clock failure'); END;").unwrap();
    assert!(observe(&f.request(&[]), &f.database, &f.gates, || 0, || {}).is_err());
    assert_eq!(f.row(), baseline);
}

#[test]
fn corrupt_arrays_profiles_schema_and_files_are_never_reset() {
    for sql in [
        "UPDATE documents SET node_times = '[]'",
        "UPDATE documents SET node_times = '[\"bad\"]'",
        "UPDATE documents SET node_times = '[9223372036854775807, 0]'",
        "UPDATE documents SET subtree_times = '[]'",
        "UPDATE documents SET subtree_times = NULL",
        "UPDATE documents SET subtree_times = '[9223372036854775807, 0]'",
        "UPDATE documents SET tree_profile = 'unknown'",
        "DELETE FROM clock",
        "PRAGMA user_version = 99",
    ] {
        let f = Fixture::new("# A\nold\n");
        f.read("1664-01-01T13:00:00Z", &[]);
        f.connection().execute_batch(sql).unwrap();
        let bytes = fs::read(&f.database).unwrap();
        assert_eq!(
            observe(&f.request(&[]), &f.database, &f.gates, || 0, || {})
                .unwrap_err()
                .code,
            "tracking_invalid",
            "{sql}"
        );
        assert_eq!(fs::read(&f.database).unwrap(), bytes, "{sql}");
    }
    let f = Fixture::new("# A\n");
    fs::create_dir_all(f.database.parent().unwrap()).unwrap();
    fs::write(&f.database, "not SQLite").unwrap();
    assert_eq!(
        observe(&f.request(&[]), &f.database, &f.gates, || 0, || {})
            .unwrap_err()
            .code,
        "tracking_invalid"
    );
    assert_eq!(fs::read(&f.database).unwrap(), b"not SQLite");
}

#[test]
fn timezone_fraction_and_negative_date_contract() {
    let utc = parse_time("1664-01-01T13:00:00.000001Z").unwrap();
    assert!(utc < 0);
    assert_eq!(utc, parse_time("1664-01-01T06:00:00.000001-07:00").unwrap());
    assert_eq!(format_time(utc).unwrap(), "1664-01-01T13:00:00.000001Z");
    for invalid in [
        "today",
        "1664-01-01",
        "1664-01-01T13:00:00",
        "1664-01-01T13:00:00.0000001Z",
        "1664-01-01T13:00:00.1234560Z",
        "2016-12-31T23:59:60Z",
    ] {
        assert_eq!(parse_time(invalid).unwrap_err().code, "usage", "{invalid}");
    }
}

fn changed(old: &str, new: &str) -> Vec<(Vec<String>, String)> {
    let before = Tree::new(old);
    let after = Tree::new(new);
    let old_times = vec![0; before.nodes.len()];
    let (times, subtree_times) = tree::compare(&before, &after, &old_times, &old_times, 1);
    after
        .results(&times, &subtree_times, 0, Some(1))
        .into_iter()
        .map(|node| {
            (
                after
                    .path(node.index)
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                if node.complete {
                    after.content(node.index)
                } else {
                    after.own_content(node.index).unwrap()
                }
                .into(),
            )
        })
        .collect()
}

#[test]
fn removals_reorders_moves_and_empty_documents_return_current_surviving_content() {
    for (old, new, expected_path, content) in [
        ("# A\n- last\n", "# A\n", vec!["A"], "# A\n"),
        (
            "# A\n- one\n- two\n",
            "# A\n- two\n- one\n",
            vec!["A"],
            "# A\n- two\n- one\n",
        ),
        ("# A\n", "", vec![], ""),
        ("plain\r\ntext", "", vec![], ""),
        ("", "plain\r\ntext", vec![], "plain\r\ntext"),
        (
            "old intro\n# A\n",
            "new intro\n# A\n",
            vec!["Preamble"],
            "new intro\n",
        ),
    ] {
        assert_eq!(
            changed(old, new),
            vec![(
                expected_path.iter().map(|s| s.to_string()).collect(),
                content.into()
            )]
        );
    }
    assert_eq!(
        changed("# A\n- task\n# B\n", "# A\n# B\n- task\n"),
        vec![
            (vec!["A".into()], "# A\n".into()),
            (vec!["B".into(), "task".into()], "- task\n".into()),
        ]
    );
    assert!(changed("", "").is_empty());
}

#[test]
fn duplicates_preserve_exact_runs_but_do_not_inherit_uncertain_history() {
    let old = "# A\n- same\n- same\n# B\nold\n";
    let new = "# A\n- same\n- same\n# B\nnew\n";
    assert_eq!(
        changed(old, new),
        vec![(vec!["B".into()], "# B\nnew\n".into())]
    );
    let new = "# A\n- edited\n- same\n# B\nold\n";
    assert_eq!(changed(old, new)[0].0, vec!["A"]);
    let before = Tree::new(old);
    let after = Tree::new(new);
    let old_times = vec![0; before.nodes.len()];
    let (times, subtree_times) = tree::compare(&before, &after, &old_times, &old_times, 1);
    let remaining = after.select(&["same".into()]).unwrap();
    let result = after.results(&times, &subtree_times, remaining, Some(1));
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].index, remaining);
    assert!(result[0].complete);
    assert_eq!(changed(old, "# A\n- same\n# B\nold\n")[0].0, vec!["A"]);
}

#[test]
fn selectors_and_source_ownership_follow_qm_parser() {
    let source = "Preamble.\r\n# Outer\r\n## A > B\r\n- [ ] Task  \r\n  - 日本語\r\n> reply\r\n```md\r\n# Hidden\r\n```\r\n## Other\r\n- [ ] Task  \r\n";
    let tree = Tree::new(source);
    let task = tree.select(&["A > B".into(), "[ ] Task  ".into()]).unwrap();
    assert_eq!(tree.path(task), vec!["Outer", "A > B", "[ ] Task  "]);
    assert_eq!(
        tree.select(&["A > B".into(), "[ ] Task".into()]).unwrap(),
        task
    );
    let leaf = tree.select(&["日本語".into()]).unwrap();
    assert_eq!(
        tree.content(leaf),
        "  - 日本語\r\n> reply\r\n```md\r\n# Hidden\r\n```\r\n"
    );
    assert_eq!(
        tree.select(&["[ ] Task  ".into()]).unwrap_err().code,
        "ambiguous"
    );
    assert_eq!(
        tree.select(&["[ ] Task".into()]).unwrap_err().code,
        "ambiguous"
    );
    let duplicates = Tree::new("# Plan\n- Task\n- Task  \n");
    assert_eq!(
        duplicates.select(&["Task".into()]).unwrap_err().code,
        "ambiguous"
    );
    for label in ["Hidden", "A", "a > b"] {
        assert_eq!(tree.select(&[label.into()]).unwrap_err().code, "not_found");
    }
    assert_eq!(tree.content(0), source);
    let nested = Tree::new("- - x\n");
    assert_eq!(nested.nodes.len(), 3);
    // Same-line nesting owns the shared line once, without duplicating bytes.
    assert_eq!(nested.content(0), "- - x\n");
    assert_eq!(changed("# A\r\n- old", "# A\r\n- new")[0].1, "- new");
}

#[test]
fn written_task_numbers_select_current_items_without_guessing_positions() {
    let source = "# Work\n## Done\n70. [x] Long task title  \n        >>> Keep the labels.\n    1. Nested\n## Later\n70) Other task\n";
    let tree = Tree::new(source);
    let task = tree.select(&["Done".into(), "70".into()]).unwrap();
    assert_eq!(tree.path(task), ["Work", "Done", "[x] Long task title  "]);
    assert_eq!(
        tree.content(task),
        "70. [x] Long task title  \n        >>> Keep the labels.\n    1. Nested\n"
    );
    for label in ["70.", "70.  ", "[x] Long task title"] {
        assert_eq!(tree.select(&["Done".into(), label.into()]).unwrap(), task);
    }
    assert_eq!(tree.select(&["70".into()]).unwrap_err().code, "ambiguous");
    assert_eq!(
        tree.content(tree.select(&["70)".into()]).unwrap()),
        "70) Other task\n"
    );
    assert_eq!(
        tree.content(
            tree.select(&["Done".into(), "70".into(), "1".into()])
                .unwrap()
        ),
        "    1. Nested\n"
    );
    for label in ["2", "070", "70th"] {
        assert_eq!(tree.select(&[label.into()]).unwrap_err().code, "not_found");
    }
    let collision = Tree::new("# 70\n70. task\n");
    assert_eq!(
        collision.select(&["70".into()]).unwrap_err().code,
        "ambiguous"
    );
    assert_eq!(
        collision.content(collision.select(&["70.".into()]).unwrap()),
        "70. task\n"
    );

    let f = Fixture::new(source);
    let baseline = f.read("1664-01-01T13:00:00Z", &[]);
    let since = baseline["as_of"].as_str().unwrap();
    assert!(f.read(
        "1664-01-01T13:01:00Z",
        &["--node", "Done", "--node", "70", "--since", since]
    )["nodes"]
        .as_array()
        .unwrap()
        .is_empty());
    fs::write(
        &f.path,
        source.replace("Keep the labels.", "Keep empty labels too."),
    )
    .unwrap();
    let update = f.read(
        "1664-01-01T13:02:00Z",
        &["--node", "Done", "--node", "70", "--since", since],
    );
    assert_eq!(update["status"], "changes");
    assert_eq!(
        paths(&update),
        vec![json!(["Work", "Done", "[x] Long task title  "])]
    );
    assert!(update["nodes"][0]["own_content"]
        .as_str()
        .unwrap()
        .contains("Keep empty labels too."));
    assert_eq!(update["nodes"][0]["children_omitted"], true);
}

#[test]
fn sparse_parent_text_and_changed_children_are_separate_and_persist_for_other_readers() {
    let original = "# Plan\nOld introduction.\n\n## Budget\nCost: 100.\n\n## Scope\nKeep this.\n";
    let f = Fixture::new(original);
    let baseline = f.read("1664-01-01T13:00:00Z", &[]);
    let since = baseline["as_of"].as_str().unwrap();
    let changed = original
        .replace("Old introduction.", "New introduction.")
        .replace("100", "125");
    fs::write(&f.path, &changed).unwrap();
    // A background/full observer must not consume the reasons for a later catch-up.
    let full = f.read("1664-01-01T14:00:00Z", &[]);
    assert_eq!(full["nodes"][0]["content"], changed);
    let read = f.read("1664-01-01T15:00:00Z", &["--since", since]);
    assert_eq!(
        read["nodes"],
        json!([
            {"path": ["Plan"], "own_content": "# Plan\nNew introduction.\n\n", "children_omitted": true},
            {"path": ["Plan", "Budget"], "content": "## Budget\nCost: 125.\n\n"}
        ])
    );
    let again = f.read("1664-01-01T16:00:00Z", &["--since", since]);
    assert_eq!(again["nodes"], read["nodes"]);
    let text = observe(
        &f.request(&["--since", since]),
        &f.database,
        &f.gates,
        || parse_time("1664-01-01T17:00:00Z").unwrap(),
        || {},
    )
    .unwrap();
    assert!(text.contains("[Plan; children omitted]\n# Plan\nNew introduction."));
    assert!(text.contains("[Plan > Budget]\n## Budget\nCost: 125."));
    assert!(!text.contains("Keep this."));
}

#[test]
fn a_later_text_edit_never_hides_an_earlier_removal_from_an_older_reader() {
    let f = Fixture::new("# Plan\nDraft.\n## Removed\nOld.\n## Kept\nCurrent.\n");
    let baseline = f.read("1664-01-01T13:00:00Z", &[]);
    fs::write(&f.path, "# Plan\nDraft.\n## Kept\nCurrent.\n").unwrap();
    let after_removal = f.read("1664-01-01T14:00:00Z", &[]);
    let current = "# Plan\nReviewed.\n## Kept\nCurrent.\n";
    fs::write(&f.path, current).unwrap();
    f.read("1664-01-01T15:00:00Z", &[]);
    let older = f.read(
        "1664-01-01T16:00:00Z",
        &["--since", baseline["as_of"].as_str().unwrap()],
    );
    assert_eq!(
        older["nodes"],
        json!([{"path": ["Plan"], "content": current}])
    );
    let newer = f.read(
        "1664-01-01T17:00:00Z",
        &["--since", after_removal["as_of"].as_str().unwrap()],
    );
    assert_eq!(
        newer["nodes"],
        json!([{
            "path": ["Plan"], "own_content": "# Plan\nReviewed.\n", "children_omitted": true
        }])
    );
}

#[test]
fn a_root_rename_does_not_replay_a_large_unchanged_child() {
    let original = format!(
        "# Book\nPreface.\n## Chapter\n{}",
        "Paragraph.\n".repeat(30_000)
    );
    let f = Fixture::new(&original);
    let baseline = f.read("1664-01-01T13:00:00Z", &["--max-bytes", "1000000"]);
    let renamed = original.replacen("# Book", "# Book draft", 1);
    fs::write(&f.path, &renamed).unwrap();
    let read = f.read(
        "1664-01-01T14:00:00Z",
        &[
            "--since",
            baseline["as_of"].as_str().unwrap(),
            "--max-bytes",
            "1000",
        ],
    );
    assert_eq!(
        read["nodes"],
        json!([{
            "path": ["Book draft"], "own_content": "# Book draft\nPreface.\n", "children_omitted": true
        }])
    );
    let full = f.read("1664-01-01T15:00:00Z", &["--max-bytes", "1000000"]);
    assert_eq!(full["nodes"][0]["content"], renamed);
}

#[test]
fn version_two_migration_retains_complete_output_for_preexisting_change_times() {
    let f = Fixture::new("# Plan\nDraft.\n## Child\nKeep.\n");
    let baseline = f.read("1664-01-01T13:00:00Z", &[]);
    let changed = "# Plan\nReviewed.\n## Child\nKeep.\n";
    fs::write(&f.path, changed).unwrap();
    f.read("1664-01-01T14:00:00Z", &[]);
    let history_count: i64 = f
        .connection()
        .query_row("SELECT count(*) FROM history_events", [], |r| r.get(0))
        .unwrap();
    f.connection()
        .execute_batch("ALTER TABLE documents DROP COLUMN subtree_times; PRAGMA user_version = 2;")
        .unwrap();
    let read = f.read(
        "1664-01-01T15:00:00Z",
        &["--since", baseline["as_of"].as_str().unwrap()],
    );
    assert_eq!(
        read["nodes"],
        json!([{"path": ["Plan"], "content": changed}])
    );
    assert_eq!(
        f.connection()
            .query_row("SELECT count(*) FROM history_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        history_count
    );
    assert_eq!(
        f.connection()
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
