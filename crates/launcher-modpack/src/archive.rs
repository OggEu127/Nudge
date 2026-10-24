//! Bounded ZIP inspection. Only final override bytes may be extracted, to staging.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use launcher_net::{
    paths::relative_path,
    policy::NetworkPolicy,
    transfer::{FileSpec, Hashes},
};
use sha1::{Digest as _, Sha1};
use sha2::{Sha256, Sha512};
use url::Url;
use zip::ZipArchive;

use crate::{
    Applicability, Error, InstallPlan, Loader, LoaderKind, ModrinthIndex, OverrideFile,
    PlannedFile, model::standard_tasks,
};

/// Resource limits applied before allocation and while streaming expanded bytes.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    /// Maximum compressed archive bytes.
    pub archive_bytes: u64,
    /// Maximum declared sum of all expanded entries.
    pub expanded_bytes: u64,
    /// Maximum bytes in an individual entry.
    pub entry_bytes: u64,
    /// Maximum central-directory entries, including ignored entries.
    pub entries: usize,
    /// Maximum UTF-8 manifest bytes.
    pub manifest_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            archive_bytes: 2 * 1024 * 1024 * 1024,
            expanded_bytes: 4 * 1024 * 1024 * 1024,
            entry_bytes: 512 * 1024 * 1024,
            entries: 20_000,
            manifest_bytes: 8 * 1024 * 1024,
        }
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> Error {
    Error::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
fn zip_error(path: &Path, source: zip::result::ZipError) -> Error {
    Error::Zip {
        path: path.to_owned(),
        source,
    }
}

fn open(path: &Path, limits: ArchiveLimits) -> Result<(ZipArchive<File>, String), Error> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| io_error("inspect archive", path, e))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > limits.archive_bytes
    {
        return Err(invalid(
            "archive is not an ordinary file or exceeds the compressed-size limit",
        ));
    }
    let mut file = File::open(path).map_err(|e| io_error("open archive", path, e))?;
    let entries = preflight(&mut file, metadata.len(), limits.entries, path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut count = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| io_error("hash archive", path, e))?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|size| *size <= limits.archive_bytes)
            .ok_or_else(|| invalid("archive grew beyond compressed-size limit"))?;
        hasher.update(&buffer[..read]);
    }
    file.rewind()
        .map_err(|e| io_error("rewind archive", path, e))?;
    let zip = ZipArchive::new(file).map_err(|e| zip_error(path, e))?;
    if zip.len() != entries {
        return Err(invalid("inconsistent or duplicate ZIP directory entries"));
    }
    Ok((zip, hex::encode(hasher.finalize())))
}

fn preflight(file: &mut File, length: u64, limit: usize, path: &Path) -> Result<usize, Error> {
    let size = usize::try_from(length.min(22 + u64::from(u16::MAX)))
        .map_err(|_| invalid("invalid archive length"))?;
    let mut tail = vec![0; size];
    file.seek(SeekFrom::Start(length - size as u64))
        .and_then(|_| file.read_exact(&mut tail))
        .map_err(|e| io_error("read ZIP directory footer", path, e))?;
    let record = (0..tail.len().saturating_sub(21))
        .rev()
        .find_map(|offset| {
            let part = &tail[offset..];
            (part[..4] == *b"PK\x05\x06"
                && part.len() == 22 + usize::from(u16::from_le_bytes([part[20], part[21]])))
            .then_some(part)
        })
        .ok_or_else(|| invalid("missing ZIP directory footer"))?;
    let entries = usize::from(u16::from_le_bytes([record[10], record[11]]));
    if record[4..8] != [0; 4]
        || record[8..10] != record[10..12]
        || entries > limit
        || entries == usize::from(u16::MAX)
        || record[12..16] == [255; 4]
        || record[16..20] == [255; 4]
    {
        return Err(invalid("split/ZIP64 archive or excessive entry count"));
    }
    let directory_size = u64::from(u32::from_le_bytes([
        record[12], record[13], record[14], record[15],
    ]));
    let directory_start = u64::from(u32::from_le_bytes([
        record[16], record[17], record[18], record[19],
    ]));
    let directory_end = directory_start + directory_size;
    if directory_size > 32 * 1024 * 1024 || directory_end != length - record.len() as u64 {
        return Err(invalid(
            "ZIP central directory exceeds its memory budget or has inconsistent offsets",
        ));
    }
    file.seek(SeekFrom::Start(directory_start))
        .map_err(|e| io_error("seek central directory", path, e))?;
    let mut position = directory_start;
    for _ in 0..entries {
        let mut header = [0_u8; 46];
        file.read_exact(&mut header)
            .map_err(|e| io_error("read central entry", path, e))?;
        if header[..4] != *b"PK\x01\x02" {
            return Err(invalid("invalid central directory entry"));
        }
        let name = u64::from(u16::from_le_bytes([header[28], header[29]]));
        let extra = u64::from(u16::from_le_bytes([header[30], header[31]]));
        let comment = u64::from(u16::from_le_bytes([header[32], header[33]]));
        position += 46 + name + extra + comment;
        if name > 4096 || position > directory_end {
            return Err(invalid(
                "central directory entry exceeds its metadata budget",
            ));
        }
        file.seek(SeekFrom::Start(position))
            .map_err(|e| io_error("seek central entry", path, e))?;
    }
    if position != directory_end {
        return Err(invalid("central directory entry count/length mismatch"));
    }
    file.rewind().map_err(|e| io_error("rewind ZIP", path, e))?;
    Ok(entries)
}

/// Inspects a ZIP by its root metadata, irrespective of its filename extension.
///
/// # Errors
/// Rejects unsafe archives, unsupported formats, or invalid MRPACK metadata.
pub fn inspect(path: &Path, target_directory: PathBuf) -> Result<InstallPlan, Error> {
    inspect_with_limits(path, target_directory, ArchiveLimits::default())
}

/// Inspects an archive using explicit resource limits.
///
/// # Errors
/// Enforces the same format and path checks as [`inspect`].
pub fn inspect_with_limits(
    path: &Path,
    target_directory: PathBuf,
    limits: ArchiveLimits,
) -> Result<InstallPlan, Error> {
    let (mut zip, fingerprint) = open(path, limits)?;
    let mut expanded = 0_u64;
    let mut manifest = None;
    let mut overrides = BTreeMap::<(u8, String), OverrideFile>::new();
    let mut archive_names = BTreeSet::new();
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index).map_err(|e| zip_error(path, e))?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| invalid("ZIP names must be UTF-8"))?
            .to_owned();
        let canonical = name.trim_end_matches('/');
        relative_path(canonical)?;
        let mode = entry.unix_mode().unwrap_or_default() & 0o170_000;
        let expected = if entry.is_dir() { 0o040_000 } else { 0o100_000 };
        if entry.encrypted() || (mode != 0 && mode != expected) {
            return Err(invalid(format!(
                "encrypted, linked or special ZIP entry `{name}`"
            )));
        }
        if !archive_names.insert(name.clone()) {
            return Err(invalid(format!("duplicate ZIP entry `{name}`")));
        }
        expanded = expanded
            .checked_add(entry.size())
            .filter(|size| *size <= limits.expanded_bytes)
            .ok_or_else(|| invalid("expanded archive exceeds its limit"))?;
        if entry.size() > limits.entry_bytes {
            return Err(invalid(format!("ZIP entry `{name}` exceeds its limit")));
        }
        if entry.is_dir() {
            continue;
        }
        if name == "modrinth.index.json" {
            if entry.size() > limits.manifest_bytes {
                return Err(invalid("manifest exceeds its limit"));
            }
            let bytes = read_bounded(&mut entry, limits.manifest_bytes, path)?;
            manifest = Some(serde_json::from_slice::<ModrinthIndex>(&bytes)?);
        } else if let Some((layer, relative)) = override_target(&name) {
            let relative = relative_path(relative)?;
            let size = entry.size();
            let hashes = hash_entry(&mut entry, size, path)?;
            let key = (
                layer,
                relative.to_string_lossy().replace('\\', "/").to_lowercase(),
            );
            if overrides
                .insert(
                    key,
                    OverrideFile {
                        archive_index: index,
                        archive_name: name,
                        path: relative,
                        size,
                        hashes,
                    },
                )
                .is_some()
            {
                return Err(invalid("duplicate or case-aliased overrides in one layer"));
            }
        }
    }
    build_plan(
        manifest.ok_or_else(|| invalid("unsupported archive: missing root modrinth.index.json"))?,
        fingerprint,
        target_directory,
        overrides,
    )
}

fn override_target(name: &str) -> Option<(u8, &str)> {
    name.strip_prefix("overrides/")
        .map(|path| (0, path))
        .or_else(|| name.strip_prefix("client-overrides/").map(|path| (1, path)))
}

fn read_bounded(reader: &mut impl Read, limit: u64, path: &Path) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read archive entry", path, e))?;
    if bytes.len() as u64 > limit {
        return Err(invalid("expanded entry exceeded its actual byte limit"));
    }
    Ok(bytes)
}

fn hash_entry(reader: &mut impl Read, expected: u64, path: &Path) -> Result<Hashes, Error> {
    let mut sha1 = Sha1::new();
    let mut sha512 = Sha512::new();
    stream_entry(reader, expected, path, |bytes| {
        sha1.update(bytes);
        sha512.update(bytes);
        Ok(())
    })?;
    Ok(Hashes {
        sha1: Some(hex::encode(sha1.finalize())),
        sha512: Some(hex::encode(sha512.finalize())),
        sha256: None,
    })
}

fn stream_entry(
    reader: &mut impl Read,
    expected: u64,
    path: &Path,
    mut consume: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut count = 0_u64;
    let mut buffer = vec![0; 32 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| io_error("read/CRC-check archive entry", path, e))?;
        if read == 0 {
            break;
        }
        count = count
            .checked_add(read as u64)
            .filter(|count| *count <= expected)
            .ok_or_else(|| invalid("ZIP expanded size exceeds declared size"))?;
        consume(&buffer[..read])?;
    }
    if count != expected {
        return Err(invalid("ZIP expanded size disagrees with declared size"));
    }
    Ok(())
}

fn build_plan(
    index: ModrinthIndex,
    fingerprint: String,
    target: PathBuf,
    overrides: BTreeMap<(u8, String), OverrideFile>,
) -> Result<InstallPlan, Error> {
    if index.format_version != 1 || index.game != "minecraft" {
        return Err(invalid(
            "only Minecraft MRPACK formatVersion 1 is supported",
        ));
    }
    if index.name.trim().is_empty()
        || index.name.len() > 512
        || index.version_id.trim().is_empty()
        || index.version_id.len() > 512
    {
        return Err(invalid("pack name/version is empty or excessive"));
    }
    if index.files.len() > 50_000 {
        return Err(invalid("pack lists too many downloads"));
    }
    let mut dependencies = index.dependencies;
    let minecraft = dependencies
        .remove("minecraft")
        .ok_or_else(|| invalid("missing Minecraft dependency"))?;
    relative_path(&minecraft)?;
    if minecraft.contains('/') {
        return Err(invalid("Minecraft version cannot be a path"));
    }
    let mut loader = None;
    for (key, kind) in [
        ("fabric-loader", LoaderKind::Fabric),
        ("quilt-loader", LoaderKind::Quilt),
        ("forge", LoaderKind::Forge),
        ("neoforge", LoaderKind::NeoForge),
    ] {
        if let Some(version) = dependencies.remove(key) {
            if loader.is_some() || version.trim().is_empty() {
                return Err(invalid("conflicting or empty loader dependencies"));
            }
            loader = Some(Loader { kind, version });
        }
    }
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let mut excluded = Vec::new();
    let mut names = BTreeMap::new();
    let mut file_names = BTreeSet::new();
    for file in index.files {
        if !file_names.insert(file.path.to_lowercase()) {
            return Err(invalid("duplicate manifest file path"));
        }
        reserve_name(&mut names, &file.path)?;
        let (applicability, planned) = parse_file(file)?;
        match applicability {
            Applicability::Required => required.push(planned),
            Applicability::Optional => optional.push(planned),
            Applicability::Unsupported => excluded.push(planned.path),
        }
    }
    let mut winning = BTreeMap::<String, OverrideFile>::new();
    for ((_, key), file) in overrides {
        if let Some(previous) = winning.get(&key)
            && previous.path != file.path
        {
            return Err(invalid("override layers use conflicting path case"));
        }
        winning.insert(key, file);
    }
    for file in winning.values() {
        reserve_name(&mut names, &file.path.to_string_lossy().replace('\\', "/"))?;
    }
    Ok(InstallPlan {
        schema_version: 1,
        archive_sha256: fingerprint,
        target_directory: target,
        name: index.name,
        version: index.version_id,
        minecraft_version: minecraft,
        loader,
        unsupported_dependencies: dependencies,
        required_files: required,
        optional_files: optional,
        excluded_files: excluded,
        overrides: winning.into_values().collect(),
        tasks: standard_tasks(),
    })
}

fn parse_file(file: crate::IndexFile) -> Result<(Applicability, PlannedFile), Error> {
    let path = relative_path(&file.path)?;
    file.hashes.validate()?;
    if file.hashes.sha1.is_none() || file.hashes.sha512.is_none() {
        return Err(invalid(format!(
            "MRPACK file `{}` must contain both SHA-1 and SHA-512",
            file.path
        )));
    }
    if file.file_size > 2 * 1024 * 1024 * 1024 {
        return Err(invalid("pack file exceeds the 2 GiB file limit"));
    }
    if file.downloads.is_empty() || file.downloads.len() > 9 {
        return Err(invalid("download source count must be 1–9"));
    }
    let mut urls = Vec::new();
    for source in file.downloads {
        validate_uri(&source)?;
        let url = Url::parse(&source).map_err(|_| invalid("malformed download URL"))?;
        NetworkPolicy::PublicHttps.validate_url(&url)?;
        if !matches!(
            url.host_str(),
            Some("cdn.modrinth.com" | "github.com" | "raw.githubusercontent.com" | "gitlab.com")
        ) {
            return Err(invalid(
                "MRPACK source host is not in the documented download allowlist",
            ));
        }
        urls.push(url);
    }
    let primary = urls.remove(0);
    Ok((
        file.env.client,
        PlannedFile {
            path,
            spec: FileSpec {
                url: primary,
                size: Some(file.file_size),
                hashes: file.hashes,
            },
            mirrors: urls,
        },
    ))
}

fn validate_uri(value: &str) -> Result<(), Error> {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(invalid("URL contains an invalid percent escape"));
            }
            index += 3;
            continue;
        }
        if !byte.is_ascii_alphanumeric() && !b"-._~:/?#@!$&'()*+,;=".contains(&byte) {
            return Err(invalid(
                "URL contains an unencoded character forbidden by RFC 3986",
            ));
        }
        index += 1;
    }
    Ok(())
}

fn reserve_name(names: &mut BTreeMap<String, (String, bool)>, name: &str) -> Result<(), Error> {
    let pieces: Vec<_> = name.split('/').collect();
    let mut prefix = String::new();
    for (index, piece) in pieces.iter().enumerate() {
        if index > 0 {
            prefix.push('/');
        }
        prefix.push_str(piece);
        let is_file = index == pieces.len() - 1;
        if let Some((original, previous)) = names.get(&prefix.to_lowercase())
            && (*previous != is_file || original != &prefix)
        {
            return Err(invalid("file/directory or case-aliased path collision"));
        }
        names.insert(prefix.to_lowercase(), (prefix.clone(), is_file));
    }
    Ok(())
}

/// Extracts only inspected winning overrides into an existing private empty stage.
///
/// Never executes files or applies archive permissions. The caller owns final commits.
///
/// # Errors
/// Rejects changed archive bytes, unexpected entries, CRC/size/digest failures and IO errors.
pub fn extract_overrides(path: &Path, plan: &InstallPlan, stage: &Path) -> Result<(), Error> {
    let (mut zip, fingerprint) = open(path, ArchiveLimits::default())?;
    if fingerprint != plan.archive_sha256 {
        return Err(invalid("archive changed after inspection"));
    }
    if stage
        .symlink_metadata()
        .map_err(|e| io_error("inspect private stage", stage, e))?
        .file_type()
        .is_symlink()
    {
        return Err(invalid("staging path is a link"));
    }
    if std::fs::read_dir(stage)
        .map_err(|e| io_error("inspect empty private stage", stage, e))?
        .next()
        .is_some()
    {
        return Err(invalid("override staging directory must be empty"));
    }
    for file in &plan.overrides {
        let mut entry = zip
            .by_index(file.archive_index)
            .map_err(|e| zip_error(path, e))?;
        if entry.name() != file.archive_name || entry.size() != file.size {
            return Err(invalid("override metadata changed after inspection"));
        }
        let target = stage.join(relative_path(
            &file.path.to_string_lossy().replace('\\', "/"),
        )?);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| io_error("create private override directory", parent, e))?;
        }
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|e| io_error("create private override file", &target, e))?;
        let mut sha1 = Sha1::new();
        let mut sha512 = Sha512::new();
        stream_entry(&mut entry, file.size, path, |bytes| {
            sha1.update(bytes);
            sha512.update(bytes);
            output
                .write_all(bytes)
                .map_err(|e| io_error("write private override", &target, e))
        })?;
        if file.hashes.sha1.as_deref() != Some(hex::encode(sha1.finalize()).as_str())
            || file.hashes.sha512.as_deref() != Some(hex::encode(sha512.finalize()).as_str())
        {
            return Err(invalid("override changed after inspection"));
        }
        output
            .sync_all()
            .map_err(|e| io_error("flush private override", &target, e))?;
    }
    Ok(())
}
