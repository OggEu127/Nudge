//! Fabric's official metadata adapter; installation remains in launcher-core.

use launcher_net::HttpClient;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, VersionMetadata, metadata::DownloadArtifact, resolver};

/// Fabric Meta's official API root.
pub const META_URL: &str = "https://meta.fabricmc.net/v2/";

/// One Fabric Loader version returned by the official API.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoaderVersion {
    /// Exact loader version string.
    pub version: String,
    /// Whether Fabric marks this version stable.
    pub stable: bool,
}

#[derive(Deserialize)]
struct LoaderEntry {
    loader: LoaderVersion,
}

#[derive(Deserialize)]
struct ProfileIntegrity {
    libraries: Vec<LibraryIntegrity>,
}

#[derive(Deserialize)]
struct LibraryIntegrity {
    name: String,
    #[serde(default)]
    sha1: Option<String>,
    #[serde(default)]
    size: Option<u64>,
}

fn endpoint(base: &Url, segments: &[&str]) -> Result<Url, Error> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| Error::InvalidMetadata("Fabric API URL must be hierarchical".to_owned()))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

/// Lists compatible loaders in the order supplied by Fabric, newest first.
///
/// # Errors
/// Propagates network or metadata errors.
pub async fn list_loaders(http: &impl HttpClient, game: &str) -> Result<Vec<LoaderVersion>, Error> {
    let base = Url::parse(META_URL).map_err(Error::InvalidEndpoint)?;
    let bytes = http
        .get_bytes(&endpoint(&base, &["versions", "loader", game])?)
        .await?;
    let entries: Vec<LoaderEntry> = serde_json::from_slice(&bytes)?;
    Ok(entries.into_iter().map(|entry| entry.loader).collect())
}

/// Fetches and merges an exact Fabric Loader profile with an official base game.
///
/// # Errors
/// Rejects incompatible inheritance, invalid coordinates, and missing/malformed
/// Maven checksums; propagates network errors.
pub async fn resolve_profile(
    http: &impl HttpClient,
    base: &VersionMetadata,
    loader: &str,
) -> Result<VersionMetadata, Error> {
    let origin = Url::parse(META_URL).map_err(Error::InvalidEndpoint)?;
    resolve_profile_from(http, &origin, base, loader).await
}

/// Resolves a profile using an injected metadata endpoint for offline HTTP tests.
///
/// # Errors
/// Has the same validation behavior as [`resolve_profile`].
pub async fn resolve_profile_from(
    http: &impl HttpClient,
    origin: &Url,
    base: &VersionMetadata,
    loader: &str,
) -> Result<VersionMetadata, Error> {
    if loader.trim().is_empty() || loader.len() > 128 || loader.contains(['/', '\\']) {
        return Err(Error::InvalidMetadata(
            "invalid Fabric Loader version".to_owned(),
        ));
    }
    let url = endpoint(
        origin,
        &["versions", "loader", &base.id, loader, "profile", "json"],
    )?;
    let bytes = http.get_bytes(&url).await?;
    let mut child = crate::parse_version_metadata(&bytes)?;
    if child.inherits_from.as_deref() != Some(&base.id) {
        return Err(Error::InvalidMetadata(
            "Fabric profile inherits from another Minecraft version".to_owned(),
        ));
    }
    let integrity: ProfileIntegrity = serde_json::from_slice(&bytes)?;
    for (library, checksum) in child.libraries.iter_mut().zip(integrity.libraries) {
        if library.name != checksum.name {
            return Err(Error::InvalidMetadata(
                "inconsistent Fabric library order".to_owned(),
            ));
        }
        let path = resolver::maven_path(&library.name)?
            .to_string_lossy()
            .replace('\\', "/");
        let repository = library.url.as_ref().ok_or_else(|| {
            Error::InvalidMetadata(format!(
                "Fabric library `{}` has no repository",
                library.name
            ))
        })?;
        if !repository.path().ends_with('/')
            || repository.query().is_some()
            || repository.fragment().is_some()
        {
            return Err(Error::InvalidMetadata(
                "invalid Fabric Maven repository URL".to_owned(),
            ));
        }
        let artifact_url = endpoint(repository, &path.split('/').collect::<Vec<_>>())?;
        let hash = if let Some(hash) = checksum.sha1 {
            hash
        } else {
            let mut checksum_url = artifact_url.clone();
            checksum_url.set_path(&format!("{}.sha1", artifact_url.path()));
            let bytes = http.get_bytes(&checksum_url).await?;
            String::from_utf8(bytes)
                .map_err(|_| Error::InvalidMetadata("Fabric checksum is not UTF-8".to_owned()))?
                .trim()
                .to_owned()
        };
        if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::InvalidMetadata(format!(
                "invalid published checksum for Fabric library `{}`",
                library.name
            )));
        }
        library.downloads.artifact = Some(DownloadArtifact {
            id: None,
            path: Some(path),
            sha1: Some(hash.to_ascii_lowercase()),
            size: checksum.size,
            url: artifact_url,
        });
    }
    resolver::merge(base, &child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_net::{ReqwestHttpClient, policy::NetworkPolicy};
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn resolves_sidecar_checksums_and_preserves_base_jar_and_arguments() {
        let server = MockServer::start().await;
        let base = crate::parse_version_metadata(br#"{"id":"1.21.5","mainClass":"Original","arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]}}"#).expect("base metadata");
        let profile = serde_json::json!({"id":"fabric-loader-test-1.21.5","inheritsFrom":"1.21.5","mainClass":"KnotClient","arguments":{"jvm":["-DFabricMcEmu= net.minecraft.client.main.Main "]},"libraries":[{"name":"net.fabricmc:loader:test","url":format!("{}/maven/",server.uri())}]});
        Mock::given(method("GET"))
            .and(path("/v2/versions/loader/1.21.5/test/profile/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(profile))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/maven/net/fabricmc/loader/test/loader-test.jar.sha1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("0123456789abcdef0123456789abcdef01234567\n"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let http =
            ReqwestHttpClient::new_with_policy("Nudge/test", 8192, NetworkPolicy::LoopbackOnly)
                .expect("client");
        let merged = resolve_profile_from(
            &http,
            &Url::parse(&format!("{}/v2/", server.uri())).expect("origin"),
            &base,
            "test",
        )
        .await
        .expect("Fabric profile");
        assert_eq!(merged.main_class.as_deref(), Some("KnotClient"));
        assert_eq!(merged.jar.as_deref(), Some("1.21.5"));
        assert!(
            merged.libraries[0]
                .downloads
                .artifact
                .as_ref()
                .expect("artifact")
                .sha1
                .is_some()
        );
        assert_eq!(merged.arguments.expect("merged arguments").jvm.len(), 3);
    }
}
