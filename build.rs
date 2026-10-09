use std::{env, path::PathBuf, process::Command};
fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    for input in [
        "src",
        "crates/mdt-core",
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    let git = |args: &[&str]| -> Option<String> {
        // Never infer provenance from an unrelated parent repository.
        if !root.join(".git").exists() {
            return None;
        }
        let output = Command::new("git")
            .current_dir(&root)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    for reference in ["HEAD", "refs/heads/main", "packed-refs"] {
        let path = root.join(".git").join(reference);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    let revision = git(&["rev-parse", "--short=10", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let suffix = match git(&["status", "--porcelain", "--untracked-files=normal"]) {
        Some(s) if s.is_empty() => "",
        Some(_) => "-dirty",
        None => "",
    };
    println!("cargo:rustc-env=QM_CLI_COMMIT={revision}");
    println!("cargo:rustc-env=QM_CLI_BUILD={revision}{suffix}");
}
