//! Content-based MRPACK inspection and immutable, executable installation plans.
//! Parsing performs no network access or destination writes.

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Nudge supports Windows x86-64 only");

mod archive;
mod model;

pub use archive::{ArchiveLimits, extract_overrides, inspect, inspect_with_limits};
pub use model::*;

use std::path::PathBuf;
use thiserror::Error;

/// Modpack format, integrity, and archive errors.
#[derive(Debug, Error)]
pub enum Error {
    /// Archive IO failed at the given path.
    #[error("failed to {operation} `{path}`")]
    Io {
        /// Attempted operation.
        operation: &'static str,
        /// Relevant path.
        path: PathBuf,
        /// Filesystem cause.
        #[source]
        source: std::io::Error,
    },
    /// ZIP format or CRC validation failed.
    #[error("invalid ZIP archive `{path}`")]
    Zip {
        /// Source archive.
        path: PathBuf,
        /// ZIP or CRC failure.
        #[source]
        source: zip::result::ZipError,
    },
    /// Manifest JSON was invalid.
    #[error("invalid modrinth.index.json")]
    Json(#[from] serde_json::Error),
    /// Download or path policy was violated.
    #[error(transparent)]
    Policy(#[from] launcher_net::Error),
    /// Archive or manifest violated an invariant.
    #[error("invalid modpack: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests;
