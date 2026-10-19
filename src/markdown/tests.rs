use super::*;
use crate::updates::retained::Retained;

const BASE: &str = "# Plan\nBaseline\n";
const OBSERVED: &str = "# Plan\nExternal save before the final check\n";
const LATER: &str = "# Plan\nExternal save after the final check\n";

// Interleave a writer that ignores the gate with the actual FileSource check.
// A second save makes it impossible to pass by rereading instead of retaining
// the exact version that caused the refusal. No timing or process-wide state.
struct RacingWriter {
    file: FileSource,
    break_retention: Option<PathBuf>,
}
impl Source for RacingWriter {
    fn identity(&self) -> &Path {
        self.file.identity()
    }
    fn read(&mut self) -> Result<String> {
        self.file.read()
    }
    fn compare_and_swap(&mut self, expected: &str, replacement: &str) -> Result<Option<String>> {
        fs::write(self.identity(), OBSERVED).unwrap();
        let result = self.file.compare_and_swap(expected, replacement);
        fs::write(self.identity(), LATER).unwrap();
        if let Some(database) = &self.break_retention {
            fs::remove_file(database).unwrap();
            fs::create_dir(database).unwrap();
        }
        result
    }
    fn finish(&mut self) {
        self.file.finish();
    }
}

#[test]
fn final_check_retains_the_exact_observed_version_or_reports_retention_failure() {
    for break_retention in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("note.md");
        let database = temp.path().join("updates.sqlite3");
        fs::write(&path, BASE).unwrap();
        let mut file = FileSource {
            identity: fs::canonicalize(&path).unwrap(),
            gates: temp.path().join("gates"),
            gate: None,
        };
        let baseline = observe(&mut file, &database).unwrap();
        let mut source = RacingWriter {
            file,
            break_retention: break_retention.then(|| database.clone()),
        };
        let mut request = Request::new(Operation::Replace);
        request.revision = Some(baseline);
        request.old = Some("Baseline".into());
        request.new = Some("Agent replacement".into());
        let error = execute(&mut source, &database, request).unwrap_err();
        assert_eq!(error.code, "locked");
        assert_eq!(error.exit_code(), 9);
        assert!(error
            .message
            .contains("Source changed during this operation; changed 0"));
        assert_eq!(fs::read_to_string(&path).unwrap(), LATER);
        assert!(
            source.file.gate.is_none(),
            "the failed operation must release its gate"
        );
        if break_retention {
            assert!(error.current_revision.is_none());
            assert!(error
                .message
                .contains("Observed version could not be retained"));
        } else {
            let revision = error.current_revision.as_ref().unwrap();
            let store = Retained::new(&database, source.identity());
            assert_eq!(store.load(revision).unwrap(), OBSERVED);
            assert_eq!(store.history().unwrap()[0].revision, *revision);
        }
    }
}
