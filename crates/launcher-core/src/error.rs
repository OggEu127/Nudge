//! Unified application-layer error type.

use std::{io, path::PathBuf};

use thiserror::Error;

/// Result type returned by launcher-core operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that cross the application coordination boundary.
#[derive(Debug, Error)]
pub enum Error {
    /// Official Microsoft account, entitlement or protected-credential failure.
    #[error(transparent)]
    Authentication(#[from] crate::auth::Error),
    /// A filesystem operation failed outside configuration handling.
    #[error("failed to {operation} `{path}`")]
    Io {
        /// Attempted operation.
        operation: &'static str,
        /// Relevant file path.
        path: PathBuf,
        /// Underlying cause.
        #[source]
        source: io::Error,
    },
    /// Invalid user input or unsupported instance state.
    #[error("{0}")]
    InvalidInput(String),
    /// Instance metadata could not be serialized or parsed.
    #[error("instance metadata JSON operation failed")]
    InstanceJson(#[from] serde_json::Error),
    /// Modpack inspection or extraction failure.
    #[error(transparent)]
    Modpack(#[from] launcher_modpack::Error),
    /// No platform configuration directory could be determined.
    #[error("the operating system did not provide an application configuration directory")]
    ConfigDirectoryUnavailable,

    /// A directory could not be created.
    #[error("failed to create directory `{path}`")]
    CreateDirectory {
        /// Directory path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// A configuration file could not be read.
    #[error("failed to read configuration file `{path}`")]
    ReadConfig {
        /// Configuration file path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// A configuration file contained invalid JSON or values.
    #[error("failed to parse configuration file `{path}`")]
    ParseConfig {
        /// Configuration file path.
        path: PathBuf,
        /// JSON parsing error.
        #[source]
        source: serde_json::Error,
    },

    /// A configuration schema is newer than this binary understands.
    #[error("configuration schema {found} is unsupported; maximum supported schema is {supported}")]
    UnsupportedConfigSchema {
        /// Schema read from disk.
        found: u32,
        /// Latest schema supported by the application.
        supported: u32,
    },

    /// Configuration serialization failed.
    #[error("failed to serialize configuration")]
    SerializeConfig(#[source] serde_json::Error),

    /// A temporary configuration file could not be created or written.
    #[error("failed to write temporary configuration file in `{directory}`")]
    WriteConfig {
        /// Parent directory used for the temporary file.
        directory: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// The completed temporary file could not replace the configuration.
    #[error("failed to atomically replace configuration file `{path}`")]
    PersistConfig {
        /// Destination configuration path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },

    /// Logging configuration is invalid or a global subscriber already exists.
    #[error("failed to initialize logging: {0}")]
    Logging(String),

    /// Network boundary failure.
    #[error(transparent)]
    Network(#[from] launcher_net::Error),

    /// Minecraft metadata or runtime failure.
    #[error(transparent)]
    Minecraft(#[from] launcher_minecraft::Error),
}
