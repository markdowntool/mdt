use std::fmt;

use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<usize>,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            current_revision: None,
            bytes: None,
        }
    }

    pub fn io(error: std::io::Error) -> Self {
        let code = if error.kind() == std::io::ErrorKind::NotFound {
            "not_found"
        } else {
            "io"
        };
        Self::new(code, error.to_string())
    }

    pub fn exit_code(&self) -> u8 {
        match self.code {
            "usage" | "budget_too_small" => 2,
            "not_found" => 3,
            "ambiguous" => 4,
            "stale" | "conflict" => 5,
            "snapshot_expired" | "history_unavailable" | "revision_unavailable" => 6,
            "locked" | "database_busy" | "changed_during_read" => 9,
            _ => 10,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(revision) = &self.current_revision {
            write!(f, "; current_revision {revision}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}
