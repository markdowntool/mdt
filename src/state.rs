//! Paths for the headless CLI. An explicit directory isolates research copies
//! without changing HOME or accessing the user's ordinary tracking database.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(crate) struct Paths {
    pub gates: PathBuf,
    pub database: PathBuf,
}

pub(crate) fn paths() -> Result<Paths> {
    let state = std::env::var_os("QM_STATE_DIR").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let emm_home = std::env::var_os("EMM_HOME").map(PathBuf::from);
    resolve(state.as_deref(), home.as_deref(), emm_home.as_deref())
}

fn resolve(state: Option<&Path>, home: Option<&Path>, emm_home: Option<&Path>) -> Result<Paths> {
    if let Some(state) = state {
        if !state.is_absolute() {
            return Err(Error::new(
                "usage",
                "QM_STATE_DIR must be an absolute directory",
            ));
        }
        return Ok(Paths {
            gates: state.to_path_buf(),
            database: state.join("updates.sqlite3"),
        });
    }
    // Preserve shared-state compatibility: even a relative or
    // empty EMM_HOME is explicit, and is resolved from the current directory.
    let gates = match (emm_home, home) {
        (Some(dir), _) => dir.join("notes"),
        (None, Some(home)) => home.join(".emm/notes"),
        (None, None) => {
            return Err(Error::new(
                "io",
                "HOME, EMM_HOME or QM_STATE_DIR is required for local mdt state",
            ))
        }
    };
    Ok(Paths {
        database: gates.join("updates.sqlite3"),
        gates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_state_is_absolute_and_relocates_every_cli_store() {
        for path in ["", "relative"] {
            assert_eq!(
                resolve(Some(Path::new(path)), None, None)
                    .err()
                    .unwrap()
                    .code,
                "usage"
            );
        }
        let paths = resolve(Some(Path::new("/isolated/qm")), None, None).unwrap();
        assert_eq!(paths.gates, Path::new("/isolated/qm"));
        assert_eq!(paths.database, Path::new("/isolated/qm/updates.sqlite3"));
    }

    #[test]
    fn ordinary_paths_match_hosted_emm_notes_including_relative_overrides() {
        let home = Path::new("/users/example");
        for emm_home in [
            None,
            Some(Path::new("/isolated/emm")),
            Some(Path::new("relative")),
            Some(Path::new("")),
        ] {
            let paths = resolve(None, Some(home), emm_home).unwrap();
            let expected =
                emm_home.map_or_else(|| home.join(".emm/notes"), |dir| dir.join("notes"));
            assert_eq!(paths.gates, expected);
            assert_eq!(paths.database, expected.join("updates.sqlite3"));
        }
        let paths = resolve(None, None, Some(Path::new("/emm"))).unwrap();
        assert_eq!(paths.gates, Path::new("/emm/notes"));
    }
}
