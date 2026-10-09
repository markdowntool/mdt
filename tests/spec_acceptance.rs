//! End-to-end data-preservation checks for the 2026-10-09 Markdown CLI contract.
//! These deliberately use only public commands and receipts, with isolated state.

use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    process::{Command, Output},
};

struct Note {
    directory: tempfile::TempDir,
    file: PathBuf,
}

impl Note {
    fn new(source: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("café note.md");
        fs::write(&file, source).unwrap();
        Self { directory, file }
    }

    fn save(&self, source: &str) {
        fs::write(&self.file, source).unwrap();
    }

    fn source(&self) -> String {
        fs::read_to_string(&self.file).unwrap()
    }

    fn body(&self, source: &str) -> String {
        let path = self.directory.path().join("replacement.md");
        fs::write(&path, source).unwrap();
        path.to_str().unwrap().to_owned()
    }

    fn run(&self, operation: &str, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mdt"))
            .arg(operation)
            .arg(&self.file)
            .args(arguments)
            .env("EMM_HOME", self.directory.path().join("home"))
            .env("QM_STATE_DIR", self.directory.path().join("state"))
            .current_dir(self.directory.path())
            .output()
            .unwrap()
    }

    fn json(&self, operation: &str, arguments: &[&str]) -> Value {
        let mut arguments = arguments.to_vec();
        arguments.push("--json");
        success(self.run(operation, &arguments))
    }

    fn read(&self) -> Value {
        self.json("read", &[])
    }
}

fn success(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stderr.is_empty(),
        "JSON notices belong in the receipt: {output:?}"
    );
    serde_json::from_slice(&output.stdout).expect("successful JSON response")
}

fn failure(output: Output, status: i32, code: &str) {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "failed operation returned success data: {output:?}"
    );
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error response");
    assert_eq!(error["error"]["code"], code, "{error}");
}

fn string(value: &Value, field: &str) -> String {
    value[field]
        .as_str()
        .unwrap_or_else(|| panic!("missing {field}: {value}"))
        .to_owned()
}

fn changed_source(page: &Value, side: &str) -> String {
    page["changes"]
        .as_array()
        .unwrap_or_else(|| panic!("missing changes: {page}"))
        .iter()
        .map(|change| {
            change[side]
                .as_str()
                .unwrap_or_else(|| panic!("missing {side}: {change}"))
        })
        .collect()
}

#[test]
fn bounded_read_pages_reconstruct_exact_utf8_and_remain_pinned() {
    const BUDGET: usize = 1024;
    let source = format!(
        "# Résumé\r\n\t{}\r\nhttps://example.test/{}\r\nlast line has no newline",
        "日本語 🦉 e\u{301} \\\"quoted\\\"  ".repeat(100),
        "長い🌱".repeat(500)
    );
    let note = Note::new(&source);
    let mut output = note.run("read", &["--json", "--max-bytes", "1024"]);
    let mut revision = None;
    let mut delivered = String::new();
    let mut cursors = HashSet::new();
    for page_number in 0..100 {
        assert!(
            output.stdout.len() <= BUDGET,
            "response exceeds the complete byte cap"
        );
        let page = success(output);
        let current_revision = string(&page, "revision");
        assert_eq!(
            revision.get_or_insert(current_revision.clone()),
            &current_revision
        );
        delivered.push_str(&string(&page, "content"));
        if page["complete"] == true {
            assert!(page["next_cursor"].is_null());
            assert!(page_number > 0, "large source was not paginated");
            assert_eq!(delivered.as_bytes(), source.as_bytes());
            assert_eq!(note.source(), "# Later\nnew saved content\n");
            return;
        }
        let cursor = string(&page, "next_cursor");
        assert!(
            cursors.insert(cursor.clone()),
            "continuation repeated without progress"
        );
        if page_number == 0 {
            note.save("# Later\nnew saved content\n");
        }
        // The cursor must retain JSON, budget, selection and revision without flags.
        output = note.run("read", &["--cursor", &cursor]);
    }
    panic!("read never completed");
}

#[test]
fn bounded_diff_delivers_all_changed_utf8_source_at_pinned_versions() {
    const REPEATS: usize = 600;
    const BUDGET: usize = 1024;
    let before = format!("# Large\n{}\n", "旧🦉".repeat(REPEATS));
    let current = format!("# Large\n{}\n", "新🌿".repeat(REPEATS));
    let note = Note::new(&before);
    let base = string(&note.read(), "revision");
    note.save(&current);
    let mut output = note.run(
        "diff",
        &[
            "--since",
            &base,
            "--context-words",
            "0",
            "--max-bytes",
            "1024",
            "--json",
        ],
    );
    let mut endpoint = None;
    let mut before_delivered = String::new();
    let mut current_delivered = String::new();
    let mut cursors = HashSet::new();
    for page_number in 0..100 {
        assert!(
            output.stdout.len() <= BUDGET,
            "diff response exceeds byte cap"
        );
        let page = success(output);
        assert_eq!(page["from"], base);
        let revision = string(&page, "revision");
        assert_eq!(endpoint.get_or_insert(revision.clone()), &revision);
        before_delivered.push_str(&changed_source(&page, "before"));
        current_delivered.push_str(&changed_source(&page, "current"));
        if page["complete"] == true {
            assert!(page_number > 0, "oversized changes were not paginated");
            assert!(page["next_cursor"].is_null());
            assert_eq!(before_delivered.matches("旧🦉").count(), REPEATS);
            assert_eq!(current_delivered.matches("新🌿").count(), REPEATS);
            assert!(!current_delivered.contains("Later"));
            return;
        }
        let cursor = string(&page, "next_cursor");
        assert!(
            cursors.insert(cursor.clone()),
            "diff continuation did not progress"
        );
        if page_number == 0 {
            note.save("# Later\nA third version.\n");
        }
        output = note.run("diff", &["--cursor", &cursor]);
    }
    panic!("diff never completed");
}

#[test]
fn diff_returns_removed_scopes_and_changed_text_at_a_retained_endpoint() {
    let before = "# Keep\nThe witch is angry.\n# Gone\nDeleted words 🦉  \n";
    let current = "# Keep\nThe witch is calm.\n";
    let note = Note::new(before);
    let base = string(&note.read(), "revision");
    note.save(current);
    let endpoint = string(&note.read(), "revision");
    note.save("# Later\nThis version must not leak into the comparison.\n");

    let removed = note.json(
        "diff",
        &["--since", &base, "--rev", &endpoint, "--node", "Gone"],
    );
    assert_eq!(removed["from"], base);
    assert_eq!(removed["revision"], endpoint);
    assert_eq!(removed["complete"], true);
    assert!(changed_source(&removed, "before").contains("Deleted words 🦉  \n"));
    assert!(changed_source(&removed, "current").is_empty());

    let changed = note.json(
        "diff",
        &["--since", &base, "--rev", &endpoint, "--node", "Keep"],
    );
    assert_eq!(changed["complete"], true);
    assert!(changed_source(&changed, "before").contains("angry"));
    assert!(changed_source(&changed, "current").contains("calm"));
    assert!(!changed.to_string().contains("must not leak"));
}

#[test]
fn stale_literal_and_exact_range_edits_preserve_other_changes_in_the_same_section() {
    for use_exact_range in [false, true] {
        let note = Note::new(
            "# Plan\r\nCap as white as milk.\r\nThe witch is angry.\r\n# Other\r\nKeep me.\r\n",
        );
        let found = note.json("find", &["Cap"]);
        let base = string(&found, "revision");
        let target = string(&found["matches"][0], "target");
        let intervening = "# Plan\r\nCap as white as milk.\r\nThe witch is calm.\r\n# Other\r\nHuman additions 🦉.\r\n";
        note.save(intervening);
        let prior = string(&note.read(), "revision");
        let receipt = if use_exact_range {
            let body = note.body("Cape");
            note.json(
                "replace",
                &["--rev", &base, "--target", &target, "--body", &body],
            )
        } else {
            note.json(
                "replace",
                &[
                    "--rev", &base, "--node", "Plan", "--old", "Cap", "--new", "Cape",
                ],
            )
        };
        assert_eq!(receipt["base"], base);
        assert_eq!(receipt["previous"], prior);
        assert_eq!(receipt["matched"], 1);
        assert_eq!(receipt["changed"], 1);
        assert!(receipt["other_changes"]["count"].as_u64().unwrap() > 0);
        assert_eq!(note.source(), intervening.replacen("Cap", "Cape", 1));
    }
}

#[test]
fn forced_replacement_keeps_the_overwritten_before_write_version_inspectable() {
    let note = Note::new("# Notes\n## Witch\nThe witch is angry.\n# Travel\nOld route.\n");
    let base = string(&note.read(), "revision");
    let intervening =
        "# Notes\n## Witch\nThe witch is angry. Do not enter the tower.\n# Travel\nNew route 🦉.\n";
    note.save(intervening);
    let body = note.body("## Witch\nThe witch agrees to help.\n");
    failure(
        note.run(
            "replace",
            &[
                "--rev", &base, "--node", "Notes", "--node", "Witch", "--body", &body, "--json",
            ],
        ),
        5,
        "conflict",
    );
    assert_eq!(note.source(), intervening);
    // This additional save is not read before the forced write. Its retained
    // presence must come from the write itself, not the earlier conflict query.
    let intervening = intervening.replace(
        "Do not enter the tower.",
        "Do not enter the tower. Bring silver.",
    );
    note.save(&intervening);
    let receipt = note.json(
        "replace",
        &[
            "--rev", &base, "--node", "Notes", "--node", "Witch", "--body", &body, "--force",
        ],
    );
    let previous = string(&receipt, "previous");
    let resulting = string(&receipt, "revision");
    assert_ne!(previous, base);
    assert_ne!(previous, resulting);
    assert_eq!(receipt["matched"], 1);
    assert_eq!(receipt["changed"], 1);
    assert_eq!(
        note.source(),
        "# Notes\n## Witch\nThe witch agrees to help.\n# Travel\nNew route 🦉.\n"
    );
    let historical = note.json("read", &["--rev", &previous]);
    assert_eq!(historical["content"], intervening);
    let overwritten = note.json("diff", &["--since", &base, "--rev", &previous]);
    assert!(changed_source(&overwritten, "current").contains("Do not enter the tower."));
    let final_diff = note.json("diff", &["--since", &base, "--rev", &resulting]);
    assert!(!changed_source(&final_diff, "current").contains("Do not enter the tower."));
}

#[test]
fn force_never_selects_a_duplicated_or_deleted_heading() {
    for (current, exit, code) in [
        (
            "# Plan\nOriginal.\n# Plan\nDuplicate.\n# Other\nKeep.\n",
            4,
            "ambiguous",
        ),
        ("# Other\nKeep.\n", 3, "not_found"),
    ] {
        for use_exact_target in [false, true] {
            let note = Note::new("# Plan\nOriginal.\n# Other\nKeep.\n");
            let selected = note.json("read", &["--node", "Plan"]);
            let base = string(&selected, "revision");
            let target = string(&selected, "target");
            note.save(current);
            let body = note.body("# Plan\nReplacement.\n");
            let selection = if use_exact_target {
                ["--target", target.as_str()]
            } else {
                ["--node", "Plan"]
            };
            failure(
                note.run(
                    "replace",
                    &[
                        "--rev",
                        &base,
                        selection[0],
                        selection[1],
                        "--body",
                        &body,
                        "--force",
                        "--json",
                    ],
                ),
                exit,
                code,
            );
            assert_eq!(note.source(), current);
            failure(
                note.run(
                    "insert",
                    &[
                        "--rev", &base, "--before", &target, "--body", &body, "--force", "--json",
                    ],
                ),
                exit,
                code,
            );
            assert_eq!(note.source(), current);
        }
    }
}

#[test]
fn force_never_guesses_a_missing_or_duplicated_literal() {
    for (current, exit, code) in [
        ("# Plan\nCap and Cap.\n", 4, "ambiguous"),
        ("# Plan\nThe cap was removed.\n", 3, "not_found"),
    ] {
        let note = Note::new("# Plan\nCap.\n");
        let base = string(&note.read(), "revision");
        note.save(current);
        failure(
            note.run(
                "replace",
                &[
                    "--rev", &base, "--old", "Cap", "--new", "Cape", "--force", "--json",
                ],
            ),
            exit,
            code,
        );
        assert_eq!(note.source(), current);
    }
}

#[test]
fn stale_edit_never_rebinds_to_matching_text_in_another_section() {
    let original = "# A\nphrase\n# B\nother\n";
    let current = "# A\nchanged\n# B\nphrase\n";
    for force in [false, true] {
        let note = Note::new(original);
        let base = string(&note.read(), "revision");
        note.save(current);
        let mut arguments = vec!["--rev", &base, "--old", "phrase", "--new", "X", "--json"];
        if force {
            arguments.push("--force");
        }
        let result = note.run("replace", &arguments);
        assert!(
            !result.status.success(),
            "literal edit rebound to a different section: {result:?}"
        );
        assert!(result.stdout.is_empty());
        assert!(matches!(result.status.code(), Some(3 | 5)), "{result:?}");
        assert_eq!(note.source(), current);
    }
    for force in [false, true] {
        let note = Note::new(original);
        let found = note.json("find", &["phrase"]);
        let base = string(&found, "revision");
        let target = string(&found["matches"][0], "target");
        note.save(current);
        let body = note.body("X");
        let mut arguments = vec![
            "--rev", &base, "--target", &target, "--body", &body, "--json",
        ];
        if force {
            arguments.push("--force");
        }
        let result = note.run("replace", &arguments);
        if force && result.status.success() {
            // Force may replace A's changed range when its unchanged boundaries
            // still identify it. B's newly matching source is never that target.
            assert_eq!(success(result)["changed"], 1);
            assert_eq!(note.source(), "# A\nX\n# B\nphrase\n");
        } else {
            assert!(
                !result.status.success(),
                "changed exact target was not refused: {result:?}"
            );
            assert!(matches!(result.status.code(), Some(3..=5)), "{result:?}");
            assert!(result.stdout.is_empty());
            assert_eq!(note.source(), current);
        }
    }
}

#[test]
fn dry_runs_and_unrepresentable_receipts_never_write_source() {
    let source = "# Plan\r\nCap.\r\nUntouched 🦉.\r\n";
    let note = Note::new(source);
    let base = string(&note.read(), "revision");
    let preview = note.json(
        "replace",
        &["--rev", &base, "--old", "Cap", "--new", "Cape", "--dry-run"],
    );
    assert_eq!(preview["changed"], 0);
    assert_eq!(preview["would_change"], 1);
    assert_eq!(note.source(), source);
    let body = note.body("New introduction.\n");
    // 1 is too small even for an envelope; 256 passes argument validation but
    // cannot hold the required revision-bearing write receipt.
    for budget in ["1", "256"] {
        for dry_run in [false, true] {
            for operation in ["replace", "insert"] {
                let mut args = if operation == "replace" {
                    vec!["--rev", &base, "--old", "Cap", "--new", "Cape"]
                } else {
                    vec!["--rev", &base, "--before", "D", "--body", &body]
                };
                args.extend(["--max-bytes", budget, "--json"]);
                if dry_run {
                    args.push("--dry-run");
                }
                failure(note.run(operation, &args), 2, "budget_too_small");
                assert_eq!(note.source(), source);
            }
        }
    }
    assert_eq!(note.read()["revision"], base);
}

#[test]
fn forget_dry_run_keeps_references_and_forget_expires_them_without_changing_source() {
    let source = format!("# Plan\n{}\n", "Exact source 🦉. ".repeat(200));
    let note = Note::new(&source);
    let first = note.json("read", &["--node", "Plan", "--max-bytes", "1024"]);
    assert_eq!(first["complete"], false);
    let base = string(&first, "revision");
    let target = string(&first, "target");
    let cursor = string(&first, "next_cursor");
    note.save("# Plan\nA later version.\n");
    note.json("forget", &["--dry-run"]);
    let retained = note.json("read", &["--rev", &base, "--target", &target]);
    assert_eq!(retained["content"], source);
    assert_eq!(
        success(note.run("read", &["--cursor", &cursor]))["revision"],
        base
    );

    note.json("forget", &[]);
    assert_eq!(note.source(), "# Plan\nA later version.\n");
    failure(
        note.run("read", &["--rev", &base, "--target", &target, "--json"]),
        6,
        "revision_unavailable",
    );
    failure(
        note.run("diff", &["--since", &base, "--json"]),
        6,
        "revision_unavailable",
    );
    let expired_cursor = note.run("read", &["--cursor", &cursor]);
    assert_eq!(expired_cursor.status.code(), Some(6), "{expired_cursor:?}");
    assert!(expired_cursor.stdout.is_empty());
    assert!(String::from_utf8_lossy(&expired_cursor.stderr).contains("revision_unavailable"));
    assert_eq!(note.read()["content"], "# Plan\nA later version.\n");
}
