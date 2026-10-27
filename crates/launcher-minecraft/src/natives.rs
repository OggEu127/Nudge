//! Bounded, transactional extraction of native-library archives.
//!
//! Inputs and the destination parent must be launcher-owned. Every existing path
//! component is checked for symbolic links before use, and archive contents are
//! written only inside a private staging directory. These checks do not make a
//! directory shared with a concurrently hostile process safe: portable `std`
//! filesystem APIs cannot atomically resolve paths without following links.

use std::os::windows::fs::MetadataExt as _;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

use zip::ZipArchive;

use crate::{Error, resolver::NativeArchive};

const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 256 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const COPY_BUFFER_BYTES: usize = 32 * 1024;

/// Extracts native archives into a new directory after validating all contents.
///
/// Archive paths are relative to `root`. Files are never executed, linked, or
/// given archive-supplied permissions. Repeated file paths are accepted only
/// when their bytes are identical. A failed extraction leaves no destination.
///
/// # Errors
///
/// Returns an error for unsafe paths, links or special files, resource-limit
/// violations, conflicting native files, corrupt ZIP data, an existing
/// destination, or filesystem failures.
pub fn extract_natives(
    root: &Path,
    archives: &[NativeArchive],
    destination: &Path,
) -> Result<(), Error> {
    let root = absolute_path(root)?;
    reject_symlink_components(&root)?;
    let metadata =
        fs::metadata(&root).map_err(|source| io_error("inspect native root", &root, source))?;
    if !metadata.is_dir() {
        return Err(unsafe_archive(&root, "native root is not a directory"));
    }
    let destination = absolute_path(destination)?;
    reject_symlink_components(&destination)?;
    ensure_absent(&destination)?;
    let parent = destination
        .parent()
        .ok_or_else(|| unsafe_archive(&destination, "destination has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(|source| io_error("create native destination parent", parent, source))?;
    reject_symlink_components(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".nudge-natives-")
        .tempdir_in(parent)
        .map_err(|source| io_error("create native staging directory", parent, source))?;
    let mut extraction = Extraction::default();
    for archive in archives {
        validate_relative_filesystem_path(&archive.path)?;
        let archive_path = root.join(&archive.path);
        reject_symlink_components(&archive_path)?;
        extraction.extract_archive(&archive_path, &archive.excludes, staging.path())?;
    }
    // Recheck immediately before commit. The parent is owned by the launcher;
    // callers must not mutate it concurrently with another installation.
    reject_symlink_components(parent)?;
    ensure_absent(&destination)?;
    fs::rename(staging.path(), &destination)
        .map_err(|source| io_error("commit native directory", &destination, source))?;
    Ok(())
}

#[derive(Default)]
struct Extraction {
    entries: usize,
    declared_bytes: u64,
    actual_bytes: u64,
    // Also reserve implicit directories, preventing case aliases and file/dir
    // collisions consistently on both case-sensitive and Windows filesystems.
    names: BTreeMap<String, (String, bool)>,
}

impl Extraction {
    fn extract_archive(
        &mut self,
        path: &Path,
        excludes: &[String],
        staging: &Path,
    ) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(path)
            .map_err(|source| io_error("inspect native archive", path, source))?;
        if !metadata.is_file() || metadata.len() > MAX_ARCHIVE_BYTES {
            return Err(unsafe_archive(
                path,
                "archive is not a regular file or exceeds 256 MiB",
            ));
        }
        let mut file =
            File::open(path).map_err(|source| io_error("open native archive", path, source))?;
        let declared_entries = preflight_entries(&mut file, metadata.len(), path)?;
        self.entries = self
            .entries
            .checked_add(declared_entries)
            .filter(|count| *count <= MAX_ENTRIES)
            .ok_or_else(|| unsafe_archive(path, "native archives exceed 10000 entries"))?;
        let mut zip = ZipArchive::new(file).map_err(|source| Error::Zip {
            path: path.to_owned(),
            source,
        })?;
        if zip.len() != declared_entries {
            return Err(unsafe_archive(
                path,
                "inconsistent or duplicate ZIP directory entries",
            ));
        }
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|source| Error::Zip {
                path: path.to_owned(),
                source,
            })?;
            let is_dir = entry.is_dir();
            let name = entry.name().to_owned();
            validate_relative_name(&name, is_dir).map_err(|reason| unsafe_archive(path, reason))?;
            let file_type = entry.unix_mode().unwrap_or_default() & 0o170_000;
            let expected_type = if is_dir { 0o040_000 } else { 0o100_000 };
            if file_type != 0 && file_type != expected_type {
                return Err(unsafe_archive(
                    path,
                    format!("non-regular ZIP entry `{name}`"),
                ));
            }
            if entry.size() > MAX_ENTRY_BYTES {
                return Err(unsafe_archive(
                    path,
                    format!("entry `{name}` exceeds 256 MiB"),
                ));
            }
            self.declared_bytes = self
                .declared_bytes
                .checked_add(entry.size())
                .filter(|bytes| *bytes <= MAX_EXPANDED_BYTES)
                .ok_or_else(|| unsafe_archive(path, "expanded archives exceed 512 MiB"))?;
            if excludes.iter().any(|prefix| name.starts_with(prefix)) {
                continue;
            }
            let name = name.trim_end_matches('/');
            let duplicate = self.reserve_name(name, is_dir, path)?;
            let target = staging.join(name);
            if is_dir {
                fs::create_dir_all(&target)
                    .map_err(|source| io_error("create native directory", &target, source))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|source| io_error("create native directory", parent, source))?;
            }
            let expected_size = entry.size();
            self.copy_entry(&mut entry, &target, duplicate, expected_size, path)?;
        }
        Ok(())
    }

    fn reserve_name(&mut self, name: &str, is_dir: bool, archive: &Path) -> Result<bool, Error> {
        let mut prefix = String::new();
        let mut duplicate = false;
        let mut components = name.split('/').peekable();
        while let Some(component) = components.next() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let is_directory = components.peek().is_some() || is_dir;
            let key = prefix.to_lowercase();
            if let Some((previous, previous_is_directory)) = self.names.get(&key) {
                if previous != &prefix || *previous_is_directory != is_directory {
                    return Err(unsafe_archive(
                        archive,
                        format!("conflicting native path `{prefix}`"),
                    ));
                }
                duplicate = !is_directory;
            } else {
                self.names.insert(key, (prefix.clone(), is_directory));
            }
        }
        Ok(duplicate)
    }

    fn copy_entry(
        &mut self,
        entry: &mut impl Read,
        target: &Path,
        duplicate: bool,
        expected_size: u64,
        archive: &Path,
    ) -> Result<(), Error> {
        let mut output = if duplicate {
            File::open(target)
                .map_err(|source| io_error("read repeated native file", target, source))?
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)
                .map_err(|source| io_error("create native file", target, source))?
        };
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        let mut existing = vec![0_u8; COPY_BUFFER_BYTES];
        let mut length = 0_u64;
        loop {
            let count = entry
                .read(&mut buffer)
                .map_err(|source| io_error("decompress native entry", archive, source))?;
            if count == 0 {
                break;
            }
            let count_bytes = count as u64;
            length += count_bytes;
            self.actual_bytes += count_bytes;
            if length > MAX_ENTRY_BYTES
                || length > expected_size
                || self.actual_bytes > MAX_EXPANDED_BYTES
            {
                return Err(unsafe_archive(
                    archive,
                    "decompressed entry exceeds its size limit",
                ));
            }
            if duplicate {
                output
                    .read_exact(&mut existing[..count])
                    .map_err(|source| io_error("compare repeated native file", target, source))?;
                if existing[..count] != buffer[..count] {
                    return Err(unsafe_archive(
                        archive,
                        format!("different contents for `{}`", target.display()),
                    ));
                }
            } else {
                output
                    .write_all(&buffer[..count])
                    .map_err(|source| io_error("write native file", target, source))?;
            }
        }
        if length != expected_size {
            return Err(unsafe_archive(
                archive,
                "decompressed entry length does not match ZIP metadata",
            ));
        }
        if duplicate {
            if output
                .read(&mut existing[..1])
                .map_err(|source| io_error("compare repeated native file", target, source))?
                != 0
            {
                return Err(unsafe_archive(
                    archive,
                    "repeated native files have different lengths",
                ));
            }
        } else {
            output
                .sync_all()
                .map_err(|source| io_error("flush native file", target, source))?;
        }
        Ok(())
    }
}

// Read the small ZIP end record before handing the archive to `zip`, so an
// attacker cannot force it to allocate millions of central-directory records.
// Native JARs within our limits have no need for ZIP64 or split-volume archives.
fn preflight_entries(file: &mut File, length: u64, path: &Path) -> Result<usize, Error> {
    const MAX_TAIL: u64 = 22 + u16::MAX as u64;
    let tail_length = usize::try_from(length.min(MAX_TAIL))
        .map_err(|_| unsafe_archive(path, "invalid ZIP length"))?;
    let mut tail = vec![0_u8; tail_length];
    file.seek(SeekFrom::Start(length - tail_length as u64))
        .map_err(|source| io_error("seek ZIP end record", path, source))?;
    file.read_exact(&mut tail)
        .map_err(|source| io_error("read ZIP end record", path, source))?;
    let record = (0..tail.len().saturating_sub(21))
        .rev()
        .find_map(|offset| {
            let record = &tail[offset..];
            if record[..4] != *b"PK\x05\x06" {
                return None;
            }
            let comment_length = usize::from(u16::from_le_bytes([record[20], record[21]]));
            (record.len() == 22 + comment_length).then_some(record)
        })
        .ok_or_else(|| unsafe_archive(path, "missing or invalid ZIP end record"))?;
    let disk = u16::from_le_bytes([record[4], record[5]]);
    let directory_disk = u16::from_le_bytes([record[6], record[7]]);
    let disk_entries = u16::from_le_bytes([record[8], record[9]]);
    let entries = u16::from_le_bytes([record[10], record[11]]);
    if disk != 0
        || directory_disk != 0
        || disk_entries != entries
        || usize::from(entries) > MAX_ENTRIES
        || entries == u16::MAX
        || record[12..16] == [255; 4]
        || record[16..20] == [255; 4]
    {
        return Err(unsafe_archive(
            path,
            "unsupported ZIP64, split archive, or excessive entry count",
        ));
    }
    // Bound central-directory allocation before zip parses attacker-controlled
    // names, extra fields and comments. Counting entries alone is insufficient.
    let directory_size = u64::from(u32::from_le_bytes([
        record[12], record[13], record[14], record[15],
    ]));
    let directory_start = u64::from(u32::from_le_bytes([
        record[16], record[17], record[18], record[19],
    ]));
    let directory_end = directory_start + directory_size;
    if directory_size > 16 * 1024 * 1024 || directory_end != length - record.len() as u64 {
        return Err(unsafe_archive(
            path,
            "native ZIP directory exceeds its memory budget or has inconsistent offsets",
        ));
    }
    file.seek(SeekFrom::Start(directory_start))
        .map_err(|source| io_error("seek native directory", path, source))?;
    let mut position = directory_start;
    for _ in 0..entries {
        let mut header = [0_u8; 46];
        file.read_exact(&mut header)
            .map_err(|source| io_error("read native directory entry", path, source))?;
        if header[..4] != *b"PK\x01\x02" {
            return Err(unsafe_archive(path, "invalid native directory entry"));
        }
        let name = u64::from(u16::from_le_bytes([header[28], header[29]]));
        let extra = u64::from(u16::from_le_bytes([header[30], header[31]]));
        let comment = u64::from(u16::from_le_bytes([header[32], header[33]]));
        position += 46 + name + extra + comment;
        if name > 4096 || position > directory_end {
            return Err(unsafe_archive(
                path,
                "native directory entry exceeds its metadata budget",
            ));
        }
        file.seek(SeekFrom::Start(position))
            .map_err(|source| io_error("seek native directory entry", path, source))?;
    }
    if position != directory_end {
        return Err(unsafe_archive(
            path,
            "native directory count/length mismatch",
        ));
    }
    file.rewind()
        .map_err(|source| io_error("rewind native archive", path, source))?;
    Ok(usize::from(entries))
}

fn validate_relative_name(name: &str, is_dir: bool) -> Result<(), &'static str> {
    let name = if is_dir {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    if name.is_empty()
        || name.contains('\\')
        || name.contains(':')
        || name.chars().any(char::is_control)
    {
        return Err("empty or non-portable archive path");
    }
    for component in name.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.ends_with(['.', ' '])
        {
            return Err("absolute, traversing, or non-portable archive path");
        }
        let stem = component
            .split('.')
            .next()
            .unwrap_or(component)
            .to_ascii_uppercase();
        let stem = stem.trim_end_matches(' ');
        let reserved = matches!(
            stem,
            "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
        ) || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
        if reserved || component.contains(['<', '>', '"', '|', '?', '*']) {
            return Err("Windows device name or reserved path character");
        }
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, Error> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(unsafe_archive(path, "empty or traversing filesystem path"));
    }
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|source| io_error("resolve native path", path, source))
    }
}

fn validate_relative_filesystem_path(path: &Path) -> Result<(), Error> {
    if path.as_os_str().is_empty() {
        return Err(unsafe_archive(path, "empty archive path"));
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(unsafe_archive(path, "non-relative archive path"));
        };
        let component = component
            .to_str()
            .ok_or_else(|| unsafe_archive(path, "archive path is not UTF-8"))?;
        validate_relative_name(component, false).map_err(|reason| unsafe_archive(path, reason))?;
    }
    Ok(())
}

fn is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    // Covers junctions and other redirections in addition to symlinks.
    metadata.file_attributes() & 0x400 != 0
}

fn reject_symlink_components(path: &Path) -> Result<(), Error> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link_or_reparse_point(&metadata) => {
                return Err(unsafe_archive(
                    path,
                    format!("symbolic link component `{}`", current.display()),
                ));
            }
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error("inspect native path component", &current, source)),
        }
    }
    Ok(())
}

fn ensure_absent(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::NativeDestinationExists(path.to_owned())),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect native destination", path, source)),
    }
}

fn unsafe_archive(path: &Path, reason: impl Into<String>) -> Error {
    Error::UnsafeArchive {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::{ZipWriter, write::SimpleFileOptions};

    fn write_archive(
        root: &Path,
        name: &str,
        entries: &[(&str, &[u8])],
    ) -> Result<NativeArchive, Box<dyn std::error::Error>> {
        let mut writer = ZipWriter::new(File::create(root.join(name))?);
        for (entry, contents) in entries {
            writer.start_file(*entry, SimpleFileOptions::default())?;
            writer.write_all(contents)?;
        }
        writer.finish()?;
        Ok(NativeArchive {
            path: PathBuf::from(name),
            excludes: vec!["META-INF/".to_owned()],
        })
    }

    #[test]
    fn extracts_valid_files_and_accepts_identical_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let first = write_archive(
            root.path(),
            "first.jar",
            &[
                ("lib/native.dll", b"native"),
                ("META-INF/signature", b"ignored"),
            ],
        )?;
        let second = write_archive(
            root.path(),
            "second.jar",
            &[("lib/native.dll", b"native"), ("other.so", b"other")],
        )?;
        let destination = root.path().join("natives");
        extract_natives(root.path(), &[first, second], &destination)?;
        assert_eq!(fs::read(destination.join("lib/native.dll"))?, b"native");
        assert_eq!(fs::read(destination.join("other.so"))?, b"other");
        assert!(!destination.join("META-INF").exists());
        assert!(matches!(
            extract_natives(root.path(), &[], &destination),
            Err(Error::NativeDestinationExists(_))
        ));
        Ok(())
    }

    #[test]
    fn rejects_traversal_and_windows_aliases() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        for name in [
            "../escaped.dll",
            "/absolute.dll",
            "C:/native.dll",
            "lib\\native.dll",
            "CON.dll",
            "dir/NUL",
            "native.dll.",
            "COM1.dll",
            "COM¹.dll",
        ] {
            let archive = write_archive(root.path(), "unsafe.jar", &[(name, b"bad")])?;
            let destination = root.path().join("natives");
            assert!(
                matches!(
                    extract_natives(root.path(), &[archive], &destination),
                    Err(Error::UnsafeArchive { .. })
                ),
                "accepted {name}"
            );
            assert!(!destination.exists());
        }
        Ok(())
    }

    #[test]
    fn rejects_links_and_special_modes() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("link.jar");
        let mut writer = ZipWriter::new(File::create(&path)?);
        writer.add_symlink("native.so", "../../outside", SimpleFileOptions::default())?;
        writer.finish()?;
        let archive = NativeArchive {
            path: PathBuf::from("link.jar"),
            excludes: vec![],
        };
        assert!(matches!(
            extract_natives(root.path(), &[archive], &root.path().join("natives")),
            Err(Error::UnsafeArchive { .. })
        ));
        for mode in [0o020_644_u32, 0o060_644, 0o010_644, 0o140_644] {
            let archive = write_archive(root.path(), "special.jar", &[("native.so", b"data")])?;
            let special_path = root.path().join("special.jar");
            let mut bytes = fs::read(&special_path)?;
            let central = bytes
                .windows(4)
                .position(|window| window == b"PK\x01\x02")
                .ok_or("missing central directory")?;
            bytes[central + 5] = 3; // Unix creator, so the mode bits are authoritative.
            bytes[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            fs::write(special_path, bytes)?;
            assert!(matches!(
                extract_natives(root.path(), &[archive], &root.path().join("natives")),
                Err(Error::UnsafeArchive { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn corrupt_crc_never_commits_partial_native_files() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let archive = write_archive(
            root.path(),
            "corrupt.jar",
            &[("first.so", b"first"), ("broken.so", b"broken")],
        )?;
        let path = root.path().join("corrupt.jar");
        let mut bytes = fs::read(&path)?;
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .ok_or("missing central directory")?;
        bytes[central + 16] ^= 0xff;
        fs::write(path, bytes)?;
        let destination = root.path().join("natives");
        assert!(extract_natives(root.path(), &[archive], &destination).is_err());
        assert!(!destination.exists());
        assert!(
            !fs::read_dir(root.path())?.any(|entry| entry.is_ok_and(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".nudge-natives-")))
        );
        Ok(())
    }

    #[test]
    fn rejects_excessive_entry_counts_before_parsing_zip_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let archive = write_archive(root.path(), "many.jar", &[("native.so", b"native")])?;
        let path = root.path().join("many.jar");
        let mut bytes = fs::read(&path)?;
        let end = bytes.len() - 22;
        let count = u16::try_from(MAX_ENTRIES + 1)?;
        bytes[end + 8..end + 10].copy_from_slice(&count.to_le_bytes());
        bytes[end + 10..end + 12].copy_from_slice(&count.to_le_bytes());
        fs::write(path, bytes)?;
        assert!(matches!(
            extract_natives(root.path(), &[archive], &root.path().join("natives")),
            Err(Error::UnsafeArchive { .. })
        ));
        Ok(())
    }

    #[test]
    fn rejects_inconsistent_directory_lengths_and_oversized_names()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let archive = write_archive(root.path(), "directory.jar", &[("native.so", b"native")])?;
        let path = root.path().join("directory.jar");
        let original = fs::read(&path)?;
        let end = original.len() - 22;
        let central = original
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .ok_or("missing central directory")?;
        let mut bytes = original.clone();
        bytes[end + 12..end + 16].copy_from_slice(&(17_u32 * 1024 * 1024).to_le_bytes());
        fs::write(&path, bytes)?;
        assert!(matches!(
            extract_natives(
                root.path(),
                std::slice::from_ref(&archive),
                &root.path().join("natives")
            ),
            Err(Error::UnsafeArchive { .. })
        ));
        let mut bytes = original;
        bytes[central + 28..central + 30].copy_from_slice(&4097_u16.to_le_bytes());
        fs::write(&path, bytes)?;
        assert!(matches!(
            extract_natives(root.path(), &[archive], &root.path().join("natives")),
            Err(Error::UnsafeArchive { .. })
        ));
        assert!(!root.path().join("natives").exists());
        Ok(())
    }

    #[test]
    fn rejects_different_duplicates_and_case_collisions_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let first = write_archive(root.path(), "first.jar", &[("Lib/native.so", b"first")])?;
        let second = write_archive(root.path(), "second.jar", &[("Lib/native.so", b"other")])?;
        let destination = root.path().join("natives");
        assert!(matches!(
            extract_natives(root.path(), &[first, second], &destination),
            Err(Error::UnsafeArchive { .. })
        ));
        assert!(!destination.exists());
        let archive = write_archive(
            root.path(),
            "case.jar",
            &[("Lib/native.so", b"a"), ("lib/other.so", b"b")],
        )?;
        assert!(matches!(
            extract_natives(root.path(), &[archive], &destination),
            Err(Error::UnsafeArchive { .. })
        ));
        assert!(!destination.exists());
        Ok(())
    }

    #[test]
    fn rejects_oversized_archive_and_entry_metadata() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        File::create(root.path().join("large.jar"))?.set_len(MAX_ARCHIVE_BYTES + 1)?;
        let archive = NativeArchive {
            path: PathBuf::from("large.jar"),
            excludes: vec![],
        };
        assert!(matches!(
            extract_natives(root.path(), &[archive], &root.path().join("natives")),
            Err(Error::UnsafeArchive { .. })
        ));
        let archive = write_archive(root.path(), "declared.jar", &[("native.so", b"small")])?;
        let mut bytes = fs::read(root.path().join("declared.jar"))?;
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .ok_or("missing central directory")?;
        let oversized = u32::try_from(MAX_ENTRY_BYTES + 1)?;
        bytes[central + 24..central + 28].copy_from_slice(&oversized.to_le_bytes());
        fs::write(root.path().join("declared.jar"), bytes)?;
        assert!(matches!(
            extract_natives(root.path(), &[archive], &root.path().join("natives")),
            Err(Error::UnsafeArchive { .. })
        ));
        Ok(())
    }
}
