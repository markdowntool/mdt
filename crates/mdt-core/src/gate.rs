//! Cooperative writer gates keyed by canonical destination path.
//! Existing hashes and lock filenames remain stable across tool versions.
//! Writers that ignore this gate can still race an atomic replacement.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The sidecar that stands for `note_path`, whether or not the note exists.
pub fn gate_path(state_dir: &Path, note_path: &Path) -> io::Result<PathBuf> {
    let key = canonical(note_path)?;
    let hash = fnv1a64(key.as_os_str().as_encoded_bytes());
    Ok(state_dir.join("locks").join(format!("{hash:016x}.lock")))
}

/// Hold the gate for `note_path`, or learn that another hand holds it.
///
/// `Some(file)` is the gate, held until the file is dropped. `None` means
/// held elsewhere — another process, or another open in this one — and the
/// caller must not write. `Err` is a sidecar that could not be made or
/// opened, which is a write that must not go ahead either.
pub fn try_lock(state_dir: &Path, note_path: &Path) -> io::Result<Option<File>> {
    let path = gate_path(state_dir, note_path)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// The path the key is made from: the note as the disk spells it, or — for a
/// note not written yet — its parent as the disk spells it plus the name.
fn canonical(note_path: &Path) -> io::Result<PathBuf> {
    match note_path.canonicalize() {
        Ok(path) => Ok(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = note_path.file_name().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} has no file name to gate", note_path.display()),
                )
            })?;
            let parent = note_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Ok(parent.canonicalize()?.join(name))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::save::atomic_write;
    use std::sync::mpsc;
    use std::thread;

    /// A vault with one note in it, and a state directory beside it.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        let note = vault.join("Note.md");
        fs::write(&note, "base\n").unwrap();
        let state = dir.path().join("state");
        (dir, state, note)
    }

    #[test]
    fn the_key_is_the_same_for_every_spelling_of_the_path() {
        let (dir, state, note) = fixture();
        let vault = note.parent().unwrap().to_path_buf();
        let straight = gate_path(&state, &note).unwrap();

        // Through a symlink to the folder — `/tmp` is one on every Mac.
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&vault, &link).unwrap();
        assert_eq!(gate_path(&state, &link.join("Note.md")).unwrap(), straight);
        // The long way round.
        assert_eq!(
            gate_path(&state, &vault.join("./Note.md")).unwrap(),
            straight
        );
        // On a case-insensitive volume the disk's own spelling wins.
        let shouted = vault.join("NOTE.MD");
        if shouted.exists() {
            assert_eq!(gate_path(&state, &shouted).unwrap(), straight);
        }
        // And a different note is a different gate.
        assert_ne!(
            gate_path(&state, &vault.join("Other.md")).unwrap(),
            straight
        );
    }

    /// The gate of a note about to be created is the gate it will have.
    #[test]
    fn a_note_not_yet_on_disk_is_keyed_where_it_will_be() {
        let (dir, state, note) = fixture();
        let vault = note.parent().unwrap().to_path_buf();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&vault, &link).unwrap();
        let fresh = vault.join("Fresh.md");

        let before = gate_path(&state, &fresh).unwrap();
        assert_eq!(gate_path(&state, &link.join("Fresh.md")).unwrap(), before);
        fs::write(&fresh, "").unwrap();
        assert_eq!(gate_path(&state, &fresh).unwrap(), before);
    }

    #[test]
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let (_dir, state, note) = fixture();
        let held = try_lock(&state, &note)
            .unwrap()
            .expect("nobody holds it yet");
        assert!(
            try_lock(&state, &note).unwrap().is_none(),
            "held twice at once"
        );
        drop(held);
        assert!(
            try_lock(&state, &note).unwrap().is_some(),
            "not released by the drop"
        );
    }

    #[test]
    fn the_gate_survives_the_notes_atomic_replacement() {
        let (_dir, state, note) = fixture();
        let sidecar = gate_path(&state, &note).unwrap();
        let held = try_lock(&state, &note).unwrap().unwrap();
        atomic_write(&note, "two\n").unwrap();
        assert_eq!(fs::read_to_string(&note).unwrap(), "two\n");
        assert!(
            try_lock(&state, &note).unwrap().is_none(),
            "the rename let a second holder in"
        );
        assert_eq!(
            gate_path(&state, &note).unwrap(),
            sidecar,
            "a new inode is not a new gate"
        );
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn the_sidecar_is_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, state, note) = fixture();
        let _held = try_lock(&state, &note).unwrap().unwrap();
        let mode = fs::metadata(gate_path(&state, &note).unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
    }

    /// No gate is an error, never a pass: a caller that could not take it has
    /// to know, so it can refuse the write instead of making it unguarded.
    #[test]
    fn a_state_directory_that_cannot_hold_a_lock_is_an_error() {
        let (dir, _state, note) = fixture();
        let not_a_dir = dir.path().join("state-file");
        fs::write(&not_a_dir, "").unwrap();
        assert!(try_lock(&not_a_dir, &note).is_err());
    }

    /// The race the gate exists for. A holds the gate from its read to its
    /// rename; B, scheduled between the two, is refused, and when it does get
    /// in it reads what A wrote. Nothing is lost.
    #[test]
    fn a_write_scheduled_between_the_other_hands_read_and_rename_waits_its_turn() {
        let (_dir, state, note) = fixture();
        let (held, in_the_window) = mpsc::channel();
        let (go_on, may_write) = mpsc::channel();
        let a = {
            let (state, note) = (state.clone(), note.clone());
            thread::spawn(move || {
                let gate = try_lock(&state, &note)
                    .unwrap()
                    .expect("A takes the gate first");
                let text = fs::read_to_string(&note).unwrap();
                held.send(()).unwrap();
                // B tries now, between A's read and A's write.
                may_write.recv().unwrap();
                atomic_write(&note, &format!("{text}A\n")).unwrap();
                drop(gate);
            })
        };
        in_the_window.recv().unwrap();
        assert!(
            try_lock(&state, &note).unwrap().is_none(),
            "B must be refused while A is between its read and its rename"
        );
        go_on.send(()).unwrap();
        a.join().unwrap();

        let gate = try_lock(&state, &note).unwrap().expect("A has let go");
        let text = fs::read_to_string(&note).unwrap();
        atomic_write(&note, &format!("{text}B\n")).unwrap();
        drop(gate);
        assert_eq!(fs::read_to_string(&note).unwrap(), "base\nA\nB\n");
    }

    /// The same schedule with no gate: the defect the gate closes. Recorded so
    /// the test above is known to be testing something.
    #[test]
    fn without_the_gate_the_same_schedule_loses_the_first_write() {
        let (_dir, _state, note) = fixture();
        let (a_read, after_a_read) = mpsc::channel();
        let (b_wrote, after_b_wrote) = mpsc::channel();
        let a = {
            let note = note.clone();
            thread::spawn(move || {
                let text = fs::read_to_string(&note).unwrap();
                a_read.send(()).unwrap();
                after_b_wrote.recv().unwrap();
                atomic_write(&note, &format!("{text}A\n")).unwrap();
            })
        };
        after_a_read.recv().unwrap();
        let text = fs::read_to_string(&note).unwrap();
        atomic_write(&note, &format!("{text}B\n")).unwrap();
        b_wrote.send(()).unwrap();
        a.join().unwrap();
        assert_eq!(
            fs::read_to_string(&note).unwrap(),
            "base\nA\n",
            "B's line is gone"
        );
    }
}
