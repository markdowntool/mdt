use super::*;

struct Fixture {
    _directory: tempfile::TempDir,
    note: PathBuf,
    other: PathBuf,
    database: PathBuf,
    gates: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let note = directory.path().join("note.md");
        fs::write(&note, "").unwrap();
        let fixture = Self {
            note: fs::canonicalize(note).unwrap(),
            other: directory.path().join("other.md"),
            database: directory.path().join("state/updates.sqlite3"),
            gates: directory.path().join("gates"),
            _directory: directory,
        };
        for source in ["# Plan\nFirst.\n", "# Plan\nSecond.\n", "# Plan\nThird.\n"] {
            fixture.observe(&fixture.note, source);
        }
        fixture
    }

    fn observe(&self, path: &Path, source: &str) {
        fs::write(path, source).unwrap();
        crate::updates::observe_saved_file(path, &self.database, &self.gates).unwrap();
    }

    fn clear(&self) {
        let mut store = Store::open(&self.database).unwrap();
        let transaction = store.begin().unwrap();
        history_store::clear(&transaction, &self.note).unwrap();
        transaction.commit().unwrap();
    }
}

#[test]
fn history_processing_allows_other_observers_and_keeps_its_original_index() {
    let f = Fixture::new();
    let options = HistoryOptions::default();
    let original = recent_history(&f.note, &f.database, &options).unwrap();
    let mut fetches = 0;
    let history = recent_history_impl(&f.note, &f.database, &options, || {
        fetches += 1;
        if fetches == 1 {
            // Both operations open a separate connection and commit while the
            // history reader owns its first pair, before any decoding or diff.
            // A retained SQLite transaction would make these fail as busy.
            f.observe(&f.other, "# Other\nAn unrelated save.\n");
            f.observe(&f.note, "# Plan\nFourth.\n");
        }
    })
    .unwrap();
    assert_eq!(fetches, 3);
    assert_eq!(
        serde_json::to_value(&history).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    let later = recent_history(&f.note, &f.database, &options).unwrap();
    assert_eq!(later.entries.len(), original.entries.len() + 1);
    assert!(later.entries[0].diff.contains("+Fourth."));
}

#[test]
fn pruning_an_unfetched_predecessor_is_an_error_not_a_baseline() {
    let f = Fixture::new();
    let mut fetches = 0;
    let error = recent_history_impl(&f.note, &f.database, &HistoryOptions::default(), || {
        fetches += 1;
        if fetches == 1 {
            // The current and immediately prior versions have been copied.
            // Their older predecessor is still required by the captured index.
            f.clear();
        }
    })
    .unwrap_err();
    assert_eq!(fetches, 1);
    assert_eq!(error.code, "history_unavailable");
    assert!(error.message.contains("retry"));
    assert_eq!(fs::read_to_string(&f.note).unwrap(), "# Plan\nThird.\n");
}

#[test]
fn historical_text_releases_sqlite_before_decoding_and_survives_later_eviction() {
    let f = Fixture::new();
    let history = recent_history(&f.note, &f.database, &HistoryOptions::default()).unwrap();
    let middle = &history.entries[1];
    let text = historical_text_impl(&f.note, &f.database, middle.id, &["Plan".into()], || {
        f.observe(&f.other, "# Other\nSaved during history browsing.\n");
        f.clear();
    })
    .unwrap();
    assert_eq!(text.content, "# Plan\nSecond.\n");
    assert_eq!(text.revision, middle.revision);
    assert_eq!(text.observed_at, middle.observed_at);
    assert_eq!(
        historical_text(&f.note, &f.database, middle.id, &[])
            .unwrap_err()
            .code,
        "history_unavailable"
    );
}
