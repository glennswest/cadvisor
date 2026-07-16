//! Error type for host data-plane operations.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {what}: {detail}")]
    Parse { what: &'static str, detail: String },
}

impl HostError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        HostError::Io { path: path.into(), source }
    }

    pub fn parse(what: &'static str, detail: impl Into<String>) -> Self {
        HostError::Parse { what, detail: detail.into() }
    }

    /// True when the underlying cause is a missing file — common for cgroup
    /// files that don't exist on every kernel/controller combination.
    pub fn is_not_found(&self) -> bool {
        matches!(self, HostError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}
