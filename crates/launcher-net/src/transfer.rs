//! Streamed file transfers with integrity verification and atomic commits.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use sha2::{Sha256, Sha512};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use url::Url;

use crate::{Error, ReqwestHttpClient, policy::diagnostic_url};

/// Expected digests. Every supplied digest is verified.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hashes {
    /// SHA-1 used by Mojang metadata for legacy interoperability.
    pub sha1: Option<String>,
    /// SHA-256 for content-addressed storage.
    pub sha256: Option<String>,
    /// SHA-512 used by Modrinth packs.
    pub sha512: Option<String>,
}

impl Hashes {
    /// Validates digest syntax and requires at least one trusted digest.
    ///
    /// # Errors
    /// Returns an error if no digest is present or a digest is malformed.
    pub fn validate(&self) -> Result<(), Error> {
        let mut any = false;
        for (hash, length) in [(&self.sha1, 40), (&self.sha256, 64), (&self.sha512, 128)] {
            if let Some(hash) = hash {
                any = true;
                if hash.len() != length || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(Error::InvalidSpec("malformed expected hash".to_owned()));
                }
            }
        }
        if !any {
            return Err(Error::InvalidSpec(
                "a trusted digest is required".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Describes the expected bytes independently of their destination.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileSpec {
    /// Download URL. Mirror handling is added by the download scheduler.
    pub url: Url,
    /// Exact expected length, when published.
    pub size: Option<u64>,
    /// Expected digests.
    pub hashes: Hashes,
}

/// Result of checking or transferring one artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferOutcome {
    /// An existing file passed size and all hash checks.
    AlreadyValid,
    /// A fresh, verified file was atomically committed.
    Downloaded,
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

async fn check_path_components(path: &Path) -> Result<(), Error> {
    let absolute = std::path::absolute(path)
        .map_err(|error| io_error("resolve transfer path", path, error))?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        // Preserve the Windows prefix until RootDir completes it; probing
        // \\?\C: alone fails even when the full canonical path is valid.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => {
                return Err(Error::Integrity {
                    path: current,
                    reason: "links and reparse points are forbidden in transfer paths".to_owned(),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(io_error("inspect transfer path", &current, error)),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Verifies a file without loading it into memory.
///
/// # Errors
/// Returns errors for invalid expected hashes or filesystem failures.
pub async fn verify_file(path: &Path, spec: &FileSpec) -> Result<bool, Error> {
    verify_content(path, spec.size, &spec.hashes).await
}

/// Verifies local content independently of its network or archive origin.
///
/// # Errors
/// Returns an error for invalid hashes, unsafe file types or filesystem failure.
pub async fn verify_content(
    path: &Path,
    size: Option<u64>,
    hashes: &Hashes,
) -> Result<bool, Error> {
    hashes.validate()?;
    check_path_components(path).await?;
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("inspect for verification", path, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || is_reparse_point(&metadata) {
        return Err(Error::Integrity {
            path: path.to_path_buf(),
            reason: "expected an ordinary file, not a link or special file".to_owned(),
        });
    }
    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error("open for verification", path, error)),
    };
    let metadata = file
        .metadata()
        .await
        .map_err(|e| io_error("stat", path, e))?;
    if !metadata.is_file() || size.is_some_and(|size| size != metadata.len()) {
        return Ok(false);
    }
    let expected_length = metadata.len();
    let mut hashed_length = 0_u64;
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .await
            .map_err(|e| io_error("hash", path, e))?;
        if count == 0 {
            break;
        }
        hashed_length = hashed_length
            .checked_add(count as u64)
            .ok_or_else(|| Error::InvalidSpec("hashed byte count overflow".to_owned()))?;
        if hashed_length > expected_length {
            return Ok(false);
        }
        if hashes.sha1.is_some() {
            sha1.update(&buffer[..count]);
        }
        if hashes.sha256.is_some() {
            sha256.update(&buffer[..count]);
        }
        if hashes.sha512.is_some() {
            sha512.update(&buffer[..count]);
        }
    }
    Ok(hashed_length == expected_length
        && [
            (&hashes.sha1, hex::encode(sha1.finalize())),
            (&hashes.sha256, hex::encode(sha256.finalize())),
            (&hashes.sha512, hex::encode(sha512.finalize())),
        ]
        .into_iter()
        .all(|(expected, actual)| {
            expected
                .as_ref()
                .is_none_or(|value| value.eq_ignore_ascii_case(&actual))
        }))
}

impl ReqwestHttpClient {
    /// Downloads into a same-directory temporary file, verifies, then commits.
    ///
    /// # Errors
    /// Returns errors on HTTP, integrity, or filesystem failures. Existing valid
    /// destination bytes are never replaced by an unverified response.
    ///
    /// Existing links and Windows reparse points are rejected before opening or
    /// committing files. Callers must keep the destination directory private to
    /// the launcher; portable path checks cannot prevent a hostile local process
    /// from replacing a parent directory concurrently.
    pub async fn download_verified(
        &self,
        spec: &FileSpec,
        path: &Path,
    ) -> Result<TransferOutcome, Error> {
        spec.hashes.validate()?;
        self.validate_url(&spec.url)?;
        if verify_file(path, spec).await? {
            return Ok(TransferOutcome::AlreadyValid);
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // Reject links in existing ancestors before creating anything, then
        // re-check the newly created prefix before writing into it.
        check_path_components(path).await?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| io_error("create directory", parent, e))?;
        check_path_components(path).await?;
        let temp = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| io_error("create temporary file", parent, e))?;
        let mut file = tokio::fs::File::from_std(
            temp.reopen()
                .map_err(|e| io_error("open temporary file", temp.path(), e))?,
        );
        let mut response = self
            .inner
            .get(spec.url.clone())
            .send()
            .await
            .map_err(|source| Error::Request {
                url: diagnostic_url(&spec.url),
                source: source.without_url(),
            })?;
        if !response.status().is_success() {
            return Err(Error::HttpStatus {
                url: diagnostic_url(response.url()),
                status: response.status().as_u16(),
            });
        }
        self.validate_url(response.url())?;
        if let (Some(expected), Some(actual)) = (spec.size, response.content_length())
            && actual != expected
        {
            return Err(Error::Integrity {
                path: path.to_path_buf(),
                reason: "response Content-Length differs from expected byte length".to_owned(),
            });
        }
        let mut written = 0_u64;
        let limit = spec.size.unwrap_or(2 * 1024 * 1024 * 1024);
        while let Some(chunk) = response.chunk().await.map_err(|source| Error::Request {
            url: diagnostic_url(&spec.url),
            source: source.without_url(),
        })? {
            written = written
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| Error::InvalidSpec("byte count overflow".to_owned()))?;
            if written > limit {
                return Err(Error::Integrity {
                    path: path.to_path_buf(),
                    reason: "response exceeds expected length".to_owned(),
                });
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| io_error("write temporary download", temp.path(), e))?;
        }
        if spec.size.is_some_and(|expected| written != expected) {
            return Err(Error::Integrity {
                path: path.to_path_buf(),
                reason: "response ended before the expected byte length".to_owned(),
            });
        }
        file.sync_all()
            .await
            .map_err(|e| io_error("flush temporary download", temp.path(), e))?;
        drop(file);
        if !verify_file(temp.path(), spec).await? {
            return Err(Error::Integrity {
                path: path.to_path_buf(),
                reason: "size or digest mismatch".to_owned(),
            });
        }
        check_path_components(path).await?;
        temp.persist(path)
            .map_err(|e| io_error("commit verified download", path, e.error))?;
        tracing::info!(path = %path.display(), bytes = written, "verified download committed");
        Ok(TransferOutcome::Downloaded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    #[tokio::test]
    async fn canonical_paths_and_roots_preserve_integrity_checks() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let nested = directory.path().join("中文 path");
        std::fs::create_dir(&nested).expect("fixture directory");
        let path = nested.join("verified.bin");
        std::fs::write(&path, b"verified").expect("fixture content");
        let canonical = std::fs::canonicalize(&path).expect("canonical fixture");
        let root = canonical.ancestors().last().expect("absolute root");
        check_path_components(root).await.expect("valid root");
        let hashes = Hashes {
            sha256: Some(hex::encode(Sha256::digest(b"verified"))),
            ..Hashes::default()
        };
        for file in [&path, &canonical] {
            assert!(
                verify_content(file, Some(8), &hashes)
                    .await
                    .expect("verified content")
            );
            assert!(
                !verify_content(file, Some(9), &hashes)
                    .await
                    .expect("size mismatch")
            );
        }
        let missing = canonical
            .parent()
            .expect("parent")
            .join("not-created/child");
        assert!(
            !verify_content(&missing, Some(8), &hashes)
                .await
                .expect("missing file is not valid")
        );
        assert!(!nested.join("not-created").exists());
    }

    #[tokio::test]
    async fn corrupt_download_preserves_existing_destination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("wrong"))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("client.jar");
        tokio::fs::write(&path, b"old")
            .await
            .expect("write fixture");
        let spec = FileSpec {
            url: Url::parse(&server.uri()).expect("URL"),
            size: Some(5),
            hashes: Hashes {
                sha1: Some(hex::encode(Sha1::digest(b"right"))),
                ..Hashes::default()
            },
        };
        let client = ReqwestHttpClient::new_with_policy(
            "Nudge/test",
            1024,
            crate::policy::NetworkPolicy::LoopbackOnly,
        )
        .expect("client");
        assert!(client.download_verified(&spec, &path).await.is_err());
        assert_eq!(tokio::fs::read(&path).await.expect("read"), b"old");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn download_verified_rejects_symlinked_ancestor_before_creating_directories() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("valid"))
            .expect(0)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("temp directory");
        let outside = tempfile::tempdir().expect("outside directory");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked"))
            .expect("symlink fixture");
        let path = dir.path().join("linked/child/client.jar");
        let spec = FileSpec {
            url: Url::parse(&server.uri()).expect("URL"),
            size: Some(5),
            hashes: Hashes {
                sha1: Some(hex::encode(Sha1::digest(b"valid"))),
                ..Hashes::default()
            },
        };
        let client = ReqwestHttpClient::new_with_policy(
            "Nudge/test",
            1024,
            crate::policy::NetworkPolicy::LoopbackOnly,
        )
        .expect("client");
        assert!(client.download_verified(&spec, &path).await.is_err());
        assert!(
            !outside.path().join("child").exists(),
            "directories must not be created through a symlinked ancestor"
        );
    }

    #[tokio::test]
    async fn valid_destination_skips_network_and_corrupt_file_is_repaired() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("valid"))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().expect("temp directory");
        let path = dir.path().join("client.jar");
        let spec = FileSpec {
            url: Url::parse(&server.uri()).expect("URL"),
            size: Some(5),
            hashes: Hashes {
                sha1: Some(hex::encode(Sha1::digest(b"valid"))),
                ..Hashes::default()
            },
        };
        let client = ReqwestHttpClient::new_with_policy(
            "Nudge/test",
            1024,
            crate::policy::NetworkPolicy::LoopbackOnly,
        )
        .expect("client");
        assert_eq!(
            client
                .download_verified(&spec, &path)
                .await
                .expect("download"),
            TransferOutcome::Downloaded
        );
        assert_eq!(
            client.download_verified(&spec, &path).await.expect("reuse"),
            TransferOutcome::AlreadyValid
        );
    }
}
