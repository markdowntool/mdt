//! Run the shipped CLI against the state directory selected by the Emm host.
use serde_json::Value;
use std::{fs, path::Path, process::Command};

#[test]
fn emm_and_mdt_share_the_gate_and_history_for_default_and_explicit_homes() {
    for override_path in [None, Some("absolute"), Some("relative-state"), Some("")] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let note = root.path().join("note.md");
        fs::write(&note, "# Plan\nOriginal.\n").unwrap();
        let emm_home = override_path.map(|value| {
            if value == "absolute" {
                root.path().join("custom-emm")
            } else {
                Path::new(value).to_path_buf()
            }
        });
        let host_state = emm_home
            .as_ref()
            .map_or_else(|| home.join(".emm/notes"), |dir| dir.join("notes"));
        let host_state = if host_state.is_absolute() {
            host_state
        } else {
            root.path().join(host_state)
        };
        let run = |args: &[&str]| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_mdt"));
            command
                .current_dir(root.path())
                .env("HOME", &home)
                .env_remove("QM_STATE_DIR")
                .env_remove("EMM_HOME");
            if let Some(value) = &emm_home {
                command.env("EMM_HOME", value);
            }
            command.args(args).output().unwrap()
        };
        let file = note.to_str().unwrap();
        let read = run(&["read", file, "--json"]);
        assert!(read.status.success(), "{read:?}");
        let read: Value = serde_json::from_slice(&read.stdout).unwrap();
        let gate = mdt_core::gate::try_lock(&host_state, &note)
            .unwrap()
            .unwrap();
        let args = [
            "replace",
            file,
            "--node",
            "Plan",
            "--rev",
            read["revision"].as_str().unwrap(),
            "--old",
            "Original.",
            "--new",
            "Updated.",
            "--json",
        ];
        let blocked = run(&args);
        assert_eq!(blocked.status.code(), Some(9), "{blocked:?}");
        assert_eq!(fs::read_to_string(&note).unwrap(), "# Plan\nOriginal.\n");
        drop(gate);
        let written = run(&args);
        assert!(written.status.success(), "{written:?}");
        assert_eq!(fs::read_to_string(&note).unwrap(), "# Plan\nUpdated.\n");
        assert!(host_state.join("updates.sqlite3").is_file());
        assert!(
            !host_state.join("cli-cache").exists(),
            "all versions share SQLite"
        );
        assert!(
            !home.join("Library/Application Support/dev.qm.qm").exists(),
            "no legacy fallback"
        );
    }
}
