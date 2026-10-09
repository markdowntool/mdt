//! Run the real helper against an isolated note and store, without changing HOME.

use serde_json::Value;
use std::{fs, process::Command};

#[test]
fn an_explicit_state_directory_is_shared_by_read_write_and_snapshot_commands() {
    let scratch = tempfile::tempdir().unwrap();
    let note = scratch.path().join("note.md");
    let state = scratch.path().join("state");
    fs::write(&note, "# Plan\nOriginal.\n").unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_mdt"))
            .env("EMM_HOME", scratch.path().join("emm-unused"))
            .env("QM_STATE_DIR", &state)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let path = note.to_str().unwrap();
    let first = run(&["read", path, "--json"]);
    assert!(state.join("updates.sqlite3").is_file());
    let written = run(&[
        "replace",
        path,
        "--node",
        "Plan",
        "--rev",
        first["revision"].as_str().unwrap(),
        "--old",
        "Original.",
        "--new",
        "Updated.",
        "--json",
    ]);
    assert_eq!(written["changed"], 1);
    let after = run(&[
        "diff",
        path,
        "--since",
        first["revision"].as_str().unwrap(),
        "--json",
    ]);
    assert_eq!(after["complete"], true);
    assert!(after["changes"][0]["current"]
        .as_str()
        .unwrap()
        .contains("Updated."));
    assert!(
        !state.join("cli-cache").exists(),
        "all retained source lives in SQLite"
    );
    assert_eq!(fs::read_to_string(note).unwrap(), "# Plan\nUpdated.\n");
    assert!(
        !scratch.path().join("emm-unused").exists(),
        "QM_STATE_DIR overrides EMM_HOME"
    );
}
