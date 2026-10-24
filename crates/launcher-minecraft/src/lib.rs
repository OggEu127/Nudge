//! Minecraft metadata, installation, and launch functionality.

mod error;
pub mod fabric;
pub mod java;
pub mod launch;
pub mod manifest;
pub mod metadata;
pub mod natives;
mod process;
pub mod resolver;
pub mod rules;

pub use error::Error;
pub use manifest::{
    LatestVersions, VersionEntry, VersionKind, VersionManifest, fetch_version_manifest,
    fetch_version_manifest_from, parse_version_manifest,
};
pub use metadata::{VersionMetadata, parse_version_metadata};
