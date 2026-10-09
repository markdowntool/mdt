//! Stable source identities and content revisions.
use crate::{
    document::DIALECT,
    error::{Error, Result},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub fn identity(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .ok_or_else(|| Error::new("usage", "Expected a document file name"))?;
            Ok(fs::canonicalize(parent).map_err(Error::io)?.join(name))
        }
        Err(e) => Err(Error::io(e)),
    }
}

pub fn revision(path: &Path, source: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(DIALECT.as_bytes());
    hash.update([0]);
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.update([0]);
    hash.update(source.as_bytes());
    format!("R{:x}", hash.finalize())
}

fn is_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

pub fn is_revision(text: &str) -> bool {
    text.strip_prefix('R').is_some_and(is_hash)
}
