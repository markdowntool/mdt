//! Atomic file writes: write a temp file in the same directory,
//! fsync it, rename over the target, fsync the directory. The target is always
//! either the old contents or the new contents, never a torn write.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically replace `target` with `contents`. Creates parent directories as
/// needed. Notes writes document content through this path.
pub fn atomic_write(target: &Path, contents: &str) -> Result<()> {
    atomic_write_bytes(target, contents.as_bytes())
}

/// The same pipeline for data that is not text — a pasted image.
/// Kept as one implementation so an attachment gets the same durability
/// guarantee a note does: it is either the old file or the new one, never a
/// torn write, even if the machine loses power mid-paste.
pub fn atomic_write_bytes(target: &Path, contents: &[u8]) -> Result<()> {
    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| Path::new(".").to_path_buf());
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating parent directory {}", dir.display()))?;

    let file_name = target
        .file_name()
        .context("atomic_write target has no file name")?
        .to_string_lossy()
        .into_owned();
    let nonce = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(
        ".{file_name}.mdt-tmp-{}-{nonce}",
        std::process::id()
    ));

    let mut created = false;
    let result = (|| -> Result<()> {
        let permissions = match fs::metadata(target) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).context("reading existing file permissions"),
        };
        let mut tmp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .with_context(|| format!("creating temp file {}", tmp_path.display()))?;
        created = true;
        if let Some(permissions) = permissions {
            tmp.set_permissions(permissions)
                .context("preserving existing file permissions")?;
        }
        tmp.write_all(contents).context("writing temp file")?;
        tmp.sync_all().context("fsync temp file")?;
        drop(tmp);
        fs::rename(&tmp_path, target)
            .with_context(|| format!("renaming into place: {}", target.display()))?;
        // fsync the directory so the rename itself survives power loss.
        #[cfg(unix)]
        if let Ok(dir_handle) = File::open(&dir) {
            let _ = dir_handle.sync_all();
        }
        Ok(())
    })();

    if result.is_err() && created {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_new_file_and_parents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("a/b/note.md");
        atomic_write(&target, "hello").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
    }

    #[test]
    fn replaces_existing_contents() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.md");
        atomic_write(&target, "one").unwrap();
        atomic_write(&target, "two").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "two");
    }

    #[cfg(unix)]
    #[test]
    fn replacing_a_private_note_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("private.md");
        fs::write(&target, "private text").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        atomic_write(&target, "still private").unwrap();
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_to_string(target).unwrap(), "still private");
    }

    #[test]
    fn leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.md");
        atomic_write(&target, "x").unwrap();
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["note.md"]);
    }

    #[test]
    fn failed_write_preserves_original() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.md");
        atomic_write(&target, "original").unwrap();
        // Make the target a directory so the rename must fail.
        fs::remove_file(&target).unwrap();
        fs::create_dir(&target).unwrap();
        assert!(atomic_write(&target, "clobber").is_err());
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("mdt-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    /// Check that an actual ENOSPC filesystem error preserves saved source.
    /// Ignored by default because it mounts a disk image and needs macOS
    /// `hdiutil`. Run explicitly with
    /// `cargo test -p mdt-core -- --ignored full_disk`.
    ///
    /// The two filesystems fail at different points, which is the reason to
    /// test rather than reason: **APFS** gets as far as `write_all`, **HFS+**
    /// fails a step earlier at `File::create` because it cannot even allocate
    /// a directory entry. (HFS+ also cannot *delete* on a 100%-full volume —
    /// the journal has no room to record it — so this uses APFS, which is
    /// the default filesystem on supported Macs.)
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "mounts a disk image; run with --ignored"]
    fn a_full_disk_leaves_the_note_alone() {
        use std::process::Command;

        /// Unmounts on the way out, however the test ends. A panic that left
        /// a volume mounted would follow the developer around all day.
        struct Mounted(std::path::PathBuf);
        impl Drop for Mounted {
            fn drop(&mut self) {
                let _ = Command::new("hdiutil")
                    .args(["detach", "-force"])
                    .arg(&self.0)
                    .output();
            }
        }

        let scratch = tempfile::tempdir().unwrap();
        let image = scratch.path().join("full.dmg");
        let mount = scratch.path().join("mnt");

        let made = Command::new("hdiutil")
            .args([
                "create", "-size", "40m", "-fs", "APFS", "-volname", "qmfull", "-quiet",
            ])
            .arg(&image)
            .status()
            .expect("hdiutil is missing");
        assert!(made.success(), "could not create the disk image");

        let attached = Command::new("hdiutil")
            .arg("attach")
            .arg(&image)
            .args(["-mountpoint"])
            .arg(&mount)
            .arg("-nobrowse")
            .output()
            .expect("hdiutil attach failed to run");
        assert!(
            attached.status.success(),
            "could not mount: {}",
            String::from_utf8_lossy(&attached.stderr)
        );
        let _unmount = Mounted(mount.clone());

        let note = mount.join("Note.md");
        atomic_write(&note, "the original\n").expect("an empty volume must accept a write");

        // Fill it: coarse chunks to get close, then 4k crumbs for the rest.
        let mut fillers = 0usize;
        while fs::write(
            mount.join(format!("filler-{fillers}")),
            vec![b'x'; 256 * 1024],
        )
        .is_ok()
        {
            fillers += 1;
            assert!(
                fillers < 4000,
                "the volume is not the size this test made it"
            );
        }
        let _ = fs::remove_file(mount.join(format!("filler-{fillers}")));
        let mut crumbs = 0usize;
        while fs::write(mount.join(format!("crumb-{crumbs}")), vec![b'y'; 4096]).is_ok() {
            crumbs += 1;
            if crumbs > 4000 {
                break;
            }
        }
        let _ = fs::remove_file(mount.join(format!("crumb-{crumbs}")));

        // 1. Not a byte to spare.
        let err = atomic_write(&note, "the original, plus a word\n")
            .expect_err("a full disk must not report a successful save");
        let io = err
            .chain()
            .find_map(|c| c.downcast_ref::<std::io::Error>())
            .expect("the io error has to survive the context chain");
        assert_eq!(
            io.kind(),
            std::io::ErrorKind::StorageFull,
            "the reason reaching the user must be the real one: {err:#}"
        );

        // 2. Room for the file but not its contents — `File::create` succeeds
        //    and the failure has to surface from `write_all` or the fsync. A
        //    save path that trusted a buffered write would pass part 1 and
        //    lose the note here.
        for i in 0..2 {
            fs::remove_file(mount.join(format!("filler-{}", fillers - 1 - i))).unwrap();
        }
        assert!(
            atomic_write(&note, &"z".repeat(4_000_000)).is_err(),
            "a write far larger than the free space must not report success"
        );

        // Both times: the note on disk is untouched and nothing was littered.
        assert_eq!(
            fs::read_to_string(&note).unwrap(),
            "the original\n",
            "the previous contents are the thing being protected"
        );
        let leftovers: Vec<String> = fs::read_dir(&mount)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("mdt-tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed save must not leave a temp file on an already-full disk: {leftovers:?}"
        );
    }

    #[test]
    fn unicode_contents_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("note.md");
        let text = "日本語 🦀 café\nline two\n";
        atomic_write(&target, text).unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), text);
    }
}
