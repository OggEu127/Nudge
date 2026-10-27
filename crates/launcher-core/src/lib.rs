//! Top-level application coordination for Nudge.

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Nudge supports Windows x86-64 only");

pub mod auth;
pub mod config;
pub mod error;
pub mod instances;
pub mod logging;
pub mod modpack;

use launcher_minecraft::VersionManifest;
use launcher_net::{HttpClient, ReqwestHttpClient};

pub use error::{Error, Result};

/// Top-level coordinator used by the CLI and GUI frontends.
#[derive(Debug)]
pub struct Launcher<C = ReqwestHttpClient> {
    http: C,
}

impl Launcher<ReqwestHttpClient> {
    /// Constructs the production launcher coordinator.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform HTTP client cannot be constructed.
    pub fn new() -> Result<Self> {
        let http = ReqwestHttpClient::new(
            concat!("Nudge/", env!("CARGO_PKG_VERSION")),
            launcher_net::DEFAULT_MAX_RESPONSE_BYTES,
        )?;
        Ok(Self { http })
    }
}

impl<C> Launcher<C>
where
    C: HttpClient,
{
    /// Constructs a coordinator with an injected HTTP implementation.
    pub fn with_http(http: C) -> Self {
        Self { http }
    }

    /// Fetches and validates Mojang's official version manifest.
    ///
    /// # Errors
    ///
    /// Returns an error when the official metadata cannot be requested,
    /// parsed, or validated.
    pub async fn version_manifest(&self) -> Result<VersionManifest> {
        Ok(launcher_minecraft::fetch_version_manifest(&self.http).await?)
    }
}
