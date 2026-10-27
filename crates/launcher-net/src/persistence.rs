//! Flushed atomic replacement shared by configuration and managed artifacts.

use std::{
    fs, io,
    path::{Component, Path},
};
use tempfile::NamedTempFile;

/// Rejects links/reparse points in existing ancestors and the destination.
///
/// This narrows a path race but is not a sandbox against hostile concurrent
/// writers. Keep managed directories private to the current OS user.
///
/// # Errors
/// Returns an error for links, reparse points or inaccessible ancestors.
pub fn check_path(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    let mut current = std::path::PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt as _;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if metadata.file_type().is_symlink() || reparse {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "link in persistence path",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Syncs a directory entry on Unix; other platforms use atomic replacement only.
///
/// Windows directory handles cannot portably provide Unix directory-fsync
/// semantics through this safe API. No power-loss durability claim is made on
/// those platforms. An error after rename means new bytes may already be visible.
///
/// # Errors
/// Propagates Unix directory-open/sync failures and unsafe path errors.
pub fn sync_directory(path: &Path) -> io::Result<()> {
    check_path(path)?;
    #[cfg(unix)]
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

/// Creates missing directories and syncs each new parent entry on Unix.
///
/// # Errors
/// Rejects links/non-directories and propagates creation or sync errors.
pub fn create_dir_all(path: &Path) -> io::Result<()> {
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    check_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "persistence parent is not a directory",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_dir_all(parent)?;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            check_path(path)?;
            if !fs::symlink_metadata(path)?.is_dir() {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    }
    check_path(path)?;
    sync_directory(parent)
}

/// Flushes and atomically replaces a file, then syncs its parent on Unix.
///
/// The temporary must reside in the destination's directory. The caller still
/// owns semantic validation (hashes, JSON schema) before calling this method.
///
/// # Errors
/// Returns flush, path-check, rename or directory-sync errors. A directory-sync
/// error occurs after replacement; reload before deciding how to retry.
pub fn persist(temporary: NamedTempFile, destination: &Path) -> io::Result<()> {
    commit_with(temporary, destination, |_| Ok(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    FileSync,
    Rename,
    DirectorySync,
}

fn commit_with(
    temporary: NamedTempFile,
    destination: &Path,
    mut checkpoint: impl FnMut(Stage) -> io::Result<()>,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::canonicalize(temporary.path().parent().unwrap_or(Path::new(".")))?
        != fs::canonicalize(parent)?
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "temporary file must share the destination directory",
        ));
    }
    checkpoint(Stage::FileSync)?;
    temporary.as_file().sync_all()?;
    checkpoint(Stage::Rename)?;
    check_path(destination)?;
    temporary
        .persist(destination)
        .map_err(|error| error.error)?;
    checkpoint(Stage::DirectorySync)?;
    sync_directory(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn injected_failures_preserve_old_or_complete_new_bytes_and_allow_retry() -> io::Result<()> {
        let root = tempfile::tempdir()?;
        let destination = root.path().join("record.json");
        for failure in [Stage::FileSync, Stage::Rename, Stage::DirectorySync] {
            fs::write(&destination, b"old")?;
            let mut temporary = NamedTempFile::new_in(root.path())?;
            temporary.write_all(b"new complete")?;
            let result = commit_with(temporary, &destination, |stage| {
                if stage == failure {
                    Err(io::Error::other("injected disk failure"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            let expected: &[u8] = if failure == Stage::DirectorySync {
                b"new complete"
            } else {
                b"old"
            };
            assert_eq!(fs::read(&destination)?, expected);
            assert_eq!(
                fs::read_dir(root.path())?.count(),
                1,
                "temporary must be cleaned"
            );
            let mut retry = NamedTempFile::new_in(root.path())?;
            retry.write_all(b"retry")?;
            persist(retry, &destination)?;
            assert_eq!(fs::read(&destination)?, b"retry");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn late_link_substitution_is_rejected_without_touching_external_file() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let external = outside.path().join("private");
        fs::write(&external, b"private")?;
        let destination = root.path().join("record");
        let mut temporary = NamedTempFile::new_in(root.path())?;
        temporary.write_all(b"replacement")?;
        let result = commit_with(temporary, &destination, |stage| {
            if stage == Stage::Rename {
                symlink(&external, &destination)?;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(external)?, b"private");
        Ok(())
    }
}
