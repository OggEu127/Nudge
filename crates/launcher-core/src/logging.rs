//! Process-wide structured logging setup.

use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::{Error, Result};

/// Console log encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogFormat {
    /// Human-readable terminal output.
    Pretty,
    /// Newline-delimited JSON for automation and diagnostics.
    Json,
}

/// Installs the global tracing subscriber.
///
/// `RUST_LOG` overrides `default_directive` when present and valid.
///
/// # Errors
///
/// Returns an error when the filter is invalid or another global subscriber
/// has already been installed.
pub fn init(default_directive: &str, format: LogFormat) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_directive))
        .map_err(|error| Error::Logging(error.to_string()))?;
    let registry = tracing_subscriber::registry().with(filter);

    match format {
        LogFormat::Pretty => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .with_writer(std::io::stderr),
            )
            .try_init(),
        LogFormat::Json => registry
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(std::io::stderr),
            )
            .try_init(),
    }
    .map_err(|error| Error::Logging(error.to_string()))
}
