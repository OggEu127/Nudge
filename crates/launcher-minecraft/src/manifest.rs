//! Mojang version-manifest retrieval and validation.

use std::collections::HashSet;

use chrono::{DateTime, FixedOffset};
use launcher_net::HttpClient;
use serde::{Deserialize, Serialize};
use tracing::instrument;
use url::Url;

use crate::Error;

/// Mojang's official v2 version manifest endpoint.
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// IDs of the current release and snapshot versions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LatestVersions {
    /// Current release version ID.
    pub release: String,
    /// Current snapshot version ID.
    pub snapshot: String,
}

/// Forward-compatible Minecraft version category.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VersionKind(String);

impl VersionKind {
    /// Returns the category as supplied by Mojang.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One version entry in Mojang's version manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionEntry {
    /// Minecraft version identifier.
    pub id: String,
    /// Mojang version category (`release`, `snapshot`, or a historical kind).
    #[serde(rename = "type")]
    pub kind: VersionKind,
    /// URL of the version-specific metadata document.
    pub url: Url,
    /// Last metadata update time.
    pub time: DateTime<FixedOffset>,
    /// Original release time.
    pub release_time: DateTime<FixedOffset>,
    /// SHA-1 of the version-specific metadata when provided by the v2 API.
    #[serde(default)]
    pub sha1: Option<String>,
    /// Mojang's metadata compliance level when provided.
    #[serde(default)]
    pub compliance_level: Option<u8>,
}

/// Mojang v2 version manifest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionManifest {
    /// Current release and snapshot IDs.
    pub latest: LatestVersions,
    /// Versions ordered by Mojang, newest first.
    pub versions: Vec<VersionEntry>,
}

impl VersionManifest {
    /// Finds a version by its exact Mojang ID.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&VersionEntry> {
        self.versions.iter().find(|version| version.id == id)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.versions.is_empty() {
            return Err(Error::InvalidMetadata("version list is empty".to_owned()));
        }

        let mut ids = HashSet::with_capacity(self.versions.len());
        for version in &self.versions {
            if version.id.trim().is_empty() {
                return Err(Error::InvalidMetadata(
                    "a version has an empty ID".to_owned(),
                ));
            }
            if !ids.insert(version.id.as_str()) {
                return Err(Error::InvalidMetadata(format!(
                    "duplicate version ID `{}`",
                    version.id
                )));
            }
            if version.url.scheme() != "https" {
                return Err(Error::InvalidMetadata(format!(
                    "metadata URL for `{}` is not HTTPS",
                    version.id
                )));
            }
            if let Some(sha1) = &version.sha1
                && (sha1.len() != 40 || !sha1.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                return Err(Error::InvalidMetadata(format!(
                    "metadata SHA-1 for `{}` is malformed",
                    version.id
                )));
            }
        }

        for (label, id) in [
            ("latest.release", self.latest.release.as_str()),
            ("latest.snapshot", self.latest.snapshot.as_str()),
        ] {
            if !ids.contains(id) {
                return Err(Error::InvalidMetadata(format!(
                    "{label} refers to missing version `{id}`"
                )));
            }
        }

        Ok(())
    }
}

/// Parses and validates a Mojang v2 version manifest.
///
/// # Errors
///
/// Returns [`Error::Json`] for malformed JSON or [`Error::InvalidMetadata`]
/// when required semantic invariants are not satisfied.
pub fn parse_version_manifest(bytes: &[u8]) -> Result<VersionManifest, Error> {
    let manifest: VersionManifest = serde_json::from_slice(bytes)?;
    manifest.validate()?;
    Ok(manifest)
}

/// Fetches the manifest from Mojang's official endpoint.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be requested, parsed, or
/// semantically validated.
#[instrument(skip(client))]
pub async fn fetch_version_manifest(client: &impl HttpClient) -> Result<VersionManifest, Error> {
    let url = Url::parse(VERSION_MANIFEST_URL).map_err(Error::InvalidEndpoint)?;
    fetch_version_manifest_from(client, &url).await
}

/// Fetches a manifest from an explicit endpoint.
///
/// This seam exists for controlled mirrors and black-box HTTP tests. The HTTP
/// implementation still applies its outbound URL policy.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be requested, parsed, or
/// semantically validated.
#[instrument(skip(client), fields(url = %url))]
pub async fn fetch_version_manifest_from(
    client: &impl HttpClient,
    url: &Url,
) -> Result<VersionManifest, Error> {
    let body = client.get_bytes(url).await?;
    parse_version_manifest(&body)
}

#[cfg(test)]
mod tests {
    use launcher_net::ReqwestHttpClient;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    use super::{Error, fetch_version_manifest_from, parse_version_manifest};

    const VALID_MANIFEST: &str = r#"{
        "latest": {"release": "1.21.5", "snapshot": "25w10a"},
        "versions": [
            {
                "id": "1.21.5",
                "type": "release",
                "url": "https://piston-meta.mojang.com/v1/packages/release.json",
                "time": "2025-03-25T12:00:00+00:00",
                "releaseTime": "2025-03-25T12:00:00+00:00",
                "sha1": "0123456789abcdef0123456789abcdef01234567",
                "complianceLevel": 1
            },
            {
                "id": "25w10a",
                "type": "snapshot",
                "url": "https://piston-meta.mojang.com/v1/packages/snapshot.json",
                "time": "2025-03-05T12:00:00+00:00",
                "releaseTime": "2025-03-05T12:00:00+00:00",
                "sha1": "89abcdef0123456789abcdef0123456789abcdef",
                "complianceLevel": 1
            }
        ]
    }"#;

    #[test]
    fn parses_valid_manifest() {
        let manifest =
            parse_version_manifest(VALID_MANIFEST.as_bytes()).expect("valid fixture should parse");

        assert_eq!(manifest.latest.release, "1.21.5");
        assert_eq!(manifest.versions.len(), 2);
        assert_eq!(manifest.versions[0].kind.as_str(), "release");
        assert!(manifest.find("25w10a").is_some());
    }

    #[test]
    fn rejects_missing_latest_version() {
        let invalid = VALID_MANIFEST.replace("25w10a\"}", "missing\"}");
        let error = parse_version_manifest(invalid.as_bytes())
            .expect_err("missing latest reference must fail validation");

        assert!(matches!(error, Error::InvalidMetadata(_)));
        assert!(error.to_string().contains("missing version"));
    }

    #[tokio::test]
    async fn fetches_manifest_through_mock_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version_manifest_v2.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(VALID_MANIFEST),
            )
            .expect(1)
            .mount(&server)
            .await;

        let endpoint = Url::parse(&format!("{}/version_manifest_v2.json", server.uri()))
            .expect("mock URL should parse");
        let client = ReqwestHttpClient::new_with_policy(
            "Nudge/test",
            1024 * 1024,
            launcher_net::policy::NetworkPolicy::LoopbackOnly,
        )
        .expect("test client should build");

        let manifest = fetch_version_manifest_from(&client, &endpoint)
            .await
            .expect("mock request should succeed");

        assert_eq!(manifest.latest.release, "1.21.5");
    }
}
