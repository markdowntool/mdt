//! Saved Markdown operations shared by `mdt` and issue-description adapters.
//!
//! Revisions identify exact bytes at a stable source identity. Implementors must
//! make compare-and-swap atomic with all other writers of their source.
mod edit;
mod query;
mod request;

use crate::error::{Error, Result};
pub use request::{help, Operation, Request};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub trait Source {
    /// Stable absolute identity, including the store and object for virtual text.
    fn identity(&self) -> &Path;
    fn read(&mut self) -> Result<String>;
    /// Replace only if the source still equals `expected`. Return `locked` on
    /// contention; on error no replacement may have committed.
    fn compare_and_swap(&mut self, expected: &str, replacement: &str) -> Result<()>;
    /// Release any operation-wide gate after result or error. Database adapters
    /// may use the default; CAS must still be atomic.
    fn finish(&mut self) {}
}

#[derive(Debug)]
pub struct Response {
    pub stdout: String,
    /// Plain-text notices for stderr. Empty in JSON mode (notices are fields).
    pub notices: Vec<String>,
}

pub fn execute(source: &mut impl Source, database: &Path, request: Request) -> Result<Response> {
    request.validate()?;
    let result = if request.operation.is_write() {
        edit::execute(source, database, request)
    } else {
        query::execute(source, database, request)
    };
    source.finish();
    result
}

/// The same state database selected by Emm Notes, or isolated QM_STATE_DIR.
pub fn state_database() -> Result<PathBuf> {
    Ok(crate::state::paths()?.database)
}

/// Retain current saved bytes and return the description/note comparison
/// baseline without producing content or a continuation.
pub fn observe(source: &mut impl Source, database: &Path) -> Result<String> {
    let result = (|| {
        let store = crate::updates::retained::Retained::new(database, source.identity());
        store.observe(&source.read()?)
    })();
    source.finish();
    result
}

/// Retain exactly the already-fetched row being returned by another command.
/// Identity must name the same source as the adapter used by description edits.
pub fn observe_text(identity: &Path, database: &Path, text: &str) -> Result<String> {
    crate::updates::retained::Retained::new(database, identity).observe(text)
}

pub struct FileSource {
    identity: PathBuf,
    gates: PathBuf,
    gate: Option<fs::File>,
}
impl FileSource {
    pub fn new(path: &Path) -> Result<Self> {
        Ok(Self {
            identity: crate::snapshots::identity(path)?,
            gates: crate::state::paths()?.gates,
            gate: None,
        })
    }
}
impl Source for FileSource {
    fn identity(&self) -> &Path {
        &self.identity
    }
    fn read(&mut self) -> Result<String> {
        if self.gate.is_none() {
            self.gate = Some(
                mdt_core::gate::try_lock(&self.gates, &self.identity)
                    .map_err(Error::io)?
                    .ok_or_else(|| Error::new("locked", "Source is being written; changed 0"))?,
            );
        }
        fs::read_to_string(&self.identity).map_err(Error::io)
    }
    fn compare_and_swap(&mut self, expected: &str, replacement: &str) -> Result<()> {
        let current = self.read()?;
        if current != expected {
            return Err(Error::new(
                "locked",
                "Source changed during this operation; changed 0",
            ));
        }
        if current != replacement {
            mdt_core::save::atomic_write(&self.identity, replacement)
                .map_err(|e| Error::new("io", format!("Source was not replaced: {e:#}")))?;
        }
        Ok(())
    }
    fn finish(&mut self) {
        self.gate = None;
    }
}
