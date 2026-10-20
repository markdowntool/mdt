//! Additional operation and shared-observation behavior, using real processes.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};
struct Note {
    temp: tempfile::TempDir,
    file: PathBuf,
}
impl Note {
    fn new(text: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("note.md");
        fs::write(&file, text).unwrap();
        Self { temp, file }
    }
    fn run(&self, op: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mdt"))
            .arg(op)
            .arg(&self.file)
            .args(args)
            .env("QM_STATE_DIR", self.temp.path().join("state"))
            .env("EMM_HOME", self.temp.path().join("home"))
            .output()
            .unwrap()
    }
    fn json(&self, op: &str, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let out = self.run(op, &args);
        assert!(out.status.success(), "{out:?}");
        assert!(out.stderr.is_empty(), "{out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn rev(&self) -> String {
        self.json("read", &[])["revision"].as_str().unwrap().into()
    }
    fn body(&self, text: &str) -> String {
        let path = self.temp.path().join("body");
        fs::write(&path, text).unwrap();
        path.to_str().unwrap().into()
    }
}
#[test]
fn insertion_is_exact_and_after_a_whole_subtree() {
    let note = Note::new("# Parent\r\nbody\r\n## Child\r\nchild\r\n# Next\r\nlast");
    let selected = note.json("read", &["--node", "Parent"]);
    let base = selected["revision"].as_str().unwrap();
    let target = selected["target"].as_str().unwrap();
    let body = note.body("\tliteral  \r\n");
    let receipt = note.json(
        "insert",
        &["--rev", base, "--after", target, "--body", &body],
    );
    assert_eq!(receipt["anchors"], 1);
    assert_eq!(receipt["inserted"], 1);
    assert_eq!(
        fs::read_to_string(&note.file).unwrap(),
        "# Parent\r\nbody\r\n## Child\r\nchild\r\n\tliteral  \r\n# Next\r\nlast"
    );
    let base = receipt["revision"].as_str().unwrap();
    let empty = note.body("");
    let noop = note.json(
        "insert",
        &["--rev", base, "--before", "D", "--body", &empty],
    );
    assert_eq!(noop["revision"], base);
    assert_eq!(noop["inserted"], 0);
    let body = note.body("START\n");
    note.json("insert", &["--rev", base, "--before", "D", "--body", &body]);
    assert!(fs::read_to_string(&note.file)
        .unwrap()
        .starts_with("START\n# Parent"));
}
#[test]
fn find_lists_overlap_and_empty_results_and_literal_noop_keeps_revision() {
    let note = Note::new("# Plan\nbanana.\n");
    let found = note.json("find", &["ana"]);
    assert_eq!(found["match_count"], 2);
    let absent = note.json("find", &["missing"]);
    assert_eq!(absent["match_count"], 0);
    assert_eq!(absent["matches"], serde_json::json!([]));
    let base = note.rev();
    let noop = note.json(
        "replace",
        &["--rev", &base, "--old", "banana", "--new", "banana"],
    );
    assert_eq!(noop["revision"], base);
    assert_eq!(noop["matched"], 1);
    assert_eq!(noop["changed"], 0);
    note.json("replace", &["--rev", &base, "--old", "banana", "--new", ""]);
    assert_eq!(fs::read_to_string(&note.file).unwrap(), "# Plan\n.\n");
}
#[test]
fn outline_targets_read_exact_indented_list_source() {
    let note = Note::new("## Plan\n  4. Parent\n     - Child\n  9. Next\n");
    let outline = note.json("outline", &[]);
    let base = outline["revision"].as_str().unwrap();
    assert!(outline["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["target"] == "D"));
    let list = outline["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| {
            n["path"]
                .as_array()
                .unwrap()
                .last()
                .is_some_and(|label| label == "Parent")
        })
        .unwrap();
    let read = note.json(
        "read",
        &["--rev", base, "--target", list["target"].as_str().unwrap()],
    );
    assert_eq!(read["content"], "  4. Parent\n     - Child\n");
}
#[test]
fn emitted_conflict_revision_is_immediately_readable() {
    let note = Note::new("# Plan\nOld\n");
    let base = note.rev();
    let live = "# Plan\nUnobserved human save\n";
    fs::write(&note.file, live).unwrap();
    let body = note.body("# Plan\nAgent replacement\n");
    let out = note.run(
        "replace",
        &["--rev", &base, "--node", "Plan", "--body", &body, "--json"],
    );
    assert_eq!(out.status.code(), Some(5));
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    let current = error["error"]["current_revision"].as_str().unwrap();
    let plain = note.run(
        "replace",
        &["--rev", &base, "--node", "Plan", "--body", &body],
    );
    assert_eq!(plain.status.code(), Some(5));
    assert!(plain.stdout.is_empty());
    assert!(String::from_utf8(plain.stderr)
        .unwrap()
        .contains(&format!("current_revision {current}")));
    assert_eq!(note.json("read", &["--rev", current])["content"], live);
    assert_eq!(
        note.json("diff", &["--since", &base, "--rev", current])["revision"],
        current
    );
}

#[test]
fn changed_source_selection_errors_report_a_retained_revision() {
    for (live, selection, code, exit) in [
        (
            "# Plan\nGone\n",
            vec!["--old", "Old", "--new", "New"],
            "not_found",
            3,
        ),
        (
            "# Plan\nOld Old\n",
            vec!["--old", "Old", "--new", "New"],
            "ambiguous",
            4,
        ),
        ("# Renamed\nOld\n", vec!["--node", "Plan"], "not_found", 3),
        (
            "# Plan\nOld\n# Plan\nOther\n",
            vec!["--node", "Plan"],
            "ambiguous",
            4,
        ),
    ] {
        let note = Note::new("# Plan\nOld\n");
        let base = note.rev();
        fs::write(&note.file, live).unwrap();
        let body = note.body("# Plan\nReplacement\n");
        let mut args = vec!["--rev", &base];
        args.extend(selection);
        if args.contains(&"--node") {
            args.extend(["--body", &body]);
        }
        let plain = note.run("replace", &args);
        assert_eq!(plain.status.code(), Some(exit));
        assert!(plain.stdout.is_empty());
        args.push("--json");
        let out = note.run("replace", &args);
        assert_eq!(out.status.code(), Some(exit));
        assert!(out.stdout.is_empty());
        let error: Value = serde_json::from_slice(&out.stderr).unwrap();
        let error = &error["error"];
        assert_eq!(error["code"], code);
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("Source changed since supplied revision"));
        let current = error["current_revision"].as_str().unwrap();
        let plain = String::from_utf8(plain.stderr).unwrap();
        assert!(plain.contains("Source changed since supplied revision"));
        assert!(plain.contains(&format!("current_revision {current}")));
        assert_eq!(fs::read_to_string(&note.file).unwrap(), live);
        // The receipt refers to observed bytes, even if another editor saves again.
        fs::write(&note.file, "# Later\nAnother save\n").unwrap();
        assert_eq!(note.json("read", &["--rev", current])["content"], live);
    }
}

#[test]
fn invalid_baseline_selections_are_not_mislabeled_as_concurrent_changes() {
    for (base_text, old, exit) in [
        ("# Plan\nOld\n", "Absent", 3),
        ("# Plan\nOld Old\n", "Old", 4),
    ] {
        let note = Note::new(base_text);
        let base = note.rev();
        let live = "# Changed\nOther editor's save\n";
        fs::write(&note.file, live).unwrap();
        let out = note.run(
            "replace",
            &["--rev", &base, "--old", old, "--new", "New", "--json"],
        );
        assert_eq!(out.status.code(), Some(exit));
        let error: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert!(error["error"]["current_revision"].is_null());
        assert!(!error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Source changed"));
        assert_eq!(fs::read_to_string(&note.file).unwrap(), live);
    }
}

#[test]
fn changed_source_errors_keep_revision_and_context_with_a_small_budget() {
    let label = "長い名前".repeat(100);
    let note = Note::new(&format!("# {label}\nOld\n"));
    let base = note.rev();
    fs::write(&note.file, "# Renamed\nOld\n").unwrap();
    let body = note.body("Replacement\n");
    let mut args = vec![
        "--rev",
        &base,
        "--node",
        &label,
        "--body",
        &body,
        "--max-bytes",
        "256",
    ];
    for json in [false, true] {
        if json {
            args.push("--json");
        }
        let out = note.run("replace", &args);
        assert_eq!(out.status.code(), Some(3));
        assert!(out.stdout.is_empty());
        assert!(out.stderr.len() <= 256);
        let text = String::from_utf8(out.stderr).unwrap();
        assert!(text.contains("Source changed since supplied revision"));
        assert!(text.contains("current_revision"));
        if json {
            let error: Value = serde_json::from_str(&text).unwrap();
            let current = error["error"]["current_revision"].as_str().unwrap();
            assert_eq!(
                note.json("read", &["--rev", current])["content"],
                "# Renamed\nOld\n"
            );
        }
    }
}
#[test]
fn saved_emm_observations_are_the_same_revisions_and_aba_history_is_visible() {
    let note = Note::new("# Plan\nA\n");
    let path = fs::canonicalize(&note.file).unwrap();
    let gates = note.temp.path().join("state");
    let db = gates.join("updates.sqlite3");
    // Emm's observer is deliberately nonblocking and asks callers to retry a
    // busy gate. This test checks shared history, not absence of contention
    // while other tests launch processes concurrently.
    let observe = || {
        for _ in 0..100 {
            match mdt::observe_saved_file(&path, &db, &gates) {
                Ok(()) => return,
                Err(error) if error.code == "locked" => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(error) => panic!("{error}"),
            }
        }
        panic!("observer remained locked");
    };
    assert_eq!(note.json("history", &[])["entries"], serde_json::json!([]));
    observe();
    let a = note.rev();
    fs::write(&note.file, "# Plan\nB\n").unwrap();
    observe();
    fs::write(&note.file, "# Plan\nA\n").unwrap();
    observe();
    let history = note.json("history", &[]);
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["revision"], a);
    assert_eq!(entries[2]["revision"], a);
    assert_ne!(entries[1]["revision"], a);
    let diff = note.json("diff", &["--since", &a]);
    assert_eq!(diff["total"], 0);
    assert_eq!(
        note.json("read", &["--rev", entries[1]["revision"].as_str().unwrap()])["content"],
        "# Plan\nB\n"
    );
    assert!(!gates.join("cli-cache").exists());
    fs::remove_file(&note.file).unwrap();
    assert_eq!(
        note.json("history", &[])["entries"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let connection = rusqlite::Connection::open(db).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        3
    );
}
#[test]
fn recognized_comment_stamp_survives_a_long_prose_excerpt() {
    for stamp in [
        "> Alex · user:unlinked · 2026-10-09 02:30 PM +00:00",
        "2026-10-09 14:30 UTC · Alex",
    ] {
        let note = Note::new(&format!(
            "# Notes\n\n---\n\n{stamp}\n{} angry {}\n",
            "unchanged ".repeat(100),
            "trailing ".repeat(100)
        ));
        let base = note.rev();
        let old = fs::read_to_string(&note.file).unwrap();
        fs::write(&note.file, old.replace("angry", "calm")).unwrap();
        let diff = note.json("diff", &["--since", &base, "--context-words", "3"]);
        let change = &diff["changes"][0];
        assert_eq!(change["current_comment"]["source"], stamp, "{diff}");
        assert!(change["current"].as_str().unwrap().len() < 1024);
    }
}
#[test]
fn help_and_version_have_no_state_side_effect_and_old_modes_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state");
    for command in [
        "read", "diff", "find", "outline", "replace", "insert", "history", "forget", "feedback",
    ] {
        for flag in ["-h", "--help"] {
            let output = Command::new(env!("CARGO_BIN_EXE_mdt"))
                .args([command, flag])
                .env("QM_STATE_DIR", &state)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(!state.exists());
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_mdt"))
        .arg("--version")
        .env("QM_STATE_DIR", &state)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stdout).starts_with(concat!(
        "mdt ",
        env!("CARGO_PKG_VERSION"),
        " "
    )));
    assert!(!state.exists());
    let note = Note::new("source");
    for args in [
        &["--since", "2026-10-09T00:00:00Z"][..],
        &["--find", "source"],
        &["--raw"],
        &["--cache-dir", "ignored"],
    ] {
        let output = note.run("read", args);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}
