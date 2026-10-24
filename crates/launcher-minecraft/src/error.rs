//! Errors produced by Minecraft metadata, installation, and launch operations.

use std::{io, path::PathBuf};

use thiserror::Error;

/// Minecraft domain error.
#[derive(Debug, Error)]
pub enum Error {
    /// A built-in or supplied endpoint is not a valid URL.
    #[error("invalid metadata endpoint")]
    InvalidEndpoint(#[source] url::ParseError),

    /// An HTTP operation failed.
    #[error("Minecraft network operation failed")]
    Network(#[from] launcher_net::Error),

    /// JSON deserialization failed.
    #[error("failed to parse Minecraft metadata JSON")]
    Json(#[from] serde_json::Error),

    /// Metadata is syntactically valid but internally inconsistent.
    #[error("invalid Minecraft metadata: {0}")]
    InvalidMetadata(String),

    /// A rule contains an invalid regular expression.
    #[error("invalid operating-system rule expression `{expression}`")]
    InvalidRuleExpression {
        /// Rejected expression.
        expression: String,
        /// Regex parser error.
        #[source]
        source: regex::Error,
    },

    /// An argument references a placeholder unavailable in the launch context.
    #[error("launch argument contains unresolved placeholder `{0}`")]
    UnresolvedPlaceholder(String),

    /// A filesystem operation failed.
    #[error("failed to {operation} `{path}`")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Relevant filesystem path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// A ZIP archive could not be read.
    #[error("failed to read ZIP archive `{path}`")]
    Zip {
        /// Archive path.
        path: PathBuf,
        /// ZIP parser error.
        #[source]
        source: zip::result::ZipError,
    },

    /// Native archive extraction violated a safety constraint.
    #[error("unsafe native archive `{path}`: {reason}")]
    UnsafeArchive {
        /// Archive path.
        path: PathBuf,
        /// Rejection reason.
        reason: String,
    },

    /// The native output directory already exists and cannot be atomically committed.
    #[error("native output directory `{0}` already exists")]
    NativeDestinationExists(PathBuf),

    /// Java could not be executed or inspected.
    #[error("failed to inspect Java executable `{path}`")]
    JavaInspection {
        /// Java executable path.
        path: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },

    /// Java output did not contain a usable version.
    #[error("Java executable `{path}` did not report a valid version")]
    InvalidJavaVersion {
        /// Java executable path.
        path: PathBuf,
    },

    /// No compatible Java runtime was found.
    #[error("no Java {required_major} runtime was found")]
    JavaNotFound {
        /// Required major version.
        required_major: u32,
    },

    /// A Minecraft child process could not be started.
    #[error("failed to start Minecraft process with `{path}`")]
    ProcessStart {
        /// Java executable path.
        path: PathBuf,
        /// Underlying process error.
        #[source]
        source: io::Error,
    },
}
