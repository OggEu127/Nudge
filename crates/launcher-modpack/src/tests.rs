use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
};

use sha1::{Digest as _, Sha1};
use sha2::Sha512;
use zip::{ZipWriter, write::SimpleFileOptions};

use crate::{
    Applicability, ArchiveLimits, Error, TaskKind, extract_overrides, inspect, inspect_with_limits,
};

fn manifest() -> serde_json::Value {
    serde_json::json!({"formatVersion":1,"game":"minecraft","versionId":"release-1","name":"Nudge test pack","dependencies":{"minecraft":"1.21.5","fabric-loader":"0.20.1"},"files":[file("mods/required.jar",Applicability::Required),file("mods/optional.jar",Applicability::Optional),file("mods/server.jar",Applicability::Unsupported)]})
}

fn file(path: &str, client: Applicability) -> serde_json::Value {
    serde_json::json!({"path":path,"hashes":{"sha1":hex::encode(Sha1::digest(b"mod")),"sha512":hex::encode(Sha512::digest(b"mod"))},"downloads":["https://cdn.modrinth.com/data/fixture.jar"],"fileSize":3,"env":{"client":client,"server":"required"}})
}

fn archive(path: &Path, manifest: &serde_json::Value, entries: &[(&str, &[u8])]) {
    let mut zip = ZipWriter::new(File::create(path).expect("fixture ZIP"));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("modrinth.index.json", options)
        .expect("manifest entry");
    zip.write_all(&serde_json::to_vec(manifest).expect("manifest JSON"))
        .expect("manifest bytes");
    for (name, bytes) in entries {
        zip.start_file(*name, options).expect("fixture file");
        zip.write_all(bytes).expect("fixture bytes");
    }
    zip.finish().expect("finish fixture");
}

#[test]
fn content_detection_environment_selection_layering_and_task_dependencies() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("not-an-extension.dat");
    archive(
        &path,
        &manifest(),
        &[
            ("client-overrides/config/test.txt", b"client"),
            ("overrides/config/test.txt", b"base"),
            ("server-overrides/server-only.txt", b"server"),
        ],
    );
    let plan = inspect(&path, directory.path().join("game")).expect("valid MRPACK");
    assert_eq!(plan.required_files.len(), 1);
    assert_eq!(plan.optional_files.len(), 1);
    assert_eq!(plan.excluded_files.len(), 1);
    assert_eq!(plan.overrides.len(), 1);
    assert_eq!(
        plan.overrides[0].archive_name,
        "client-overrides/config/test.txt"
    );
    assert_eq!(
        plan.ordered_tasks().expect("DAG"),
        [
            TaskKind::Runtime,
            TaskKind::PackFiles,
            TaskKind::Overrides,
            TaskKind::Verify,
            TaskKind::Commit
        ]
    );
    let stage = directory.path().join("stage");
    std::fs::create_dir(&stage).expect("private stage");
    extract_overrides(&path, &plan, &stage).expect("extract overrides");
    assert_eq!(
        std::fs::read(stage.join("config/test.txt")).expect("override"),
        b"client"
    );
    assert!(!stage.join("server-only.txt").exists());
    let mut cyclic = plan;
    cyclic.tasks[0].depends_on.push(TaskKind::Commit);
    assert!(cyclic.ordered_tasks().is_err());
}

#[test]
fn rejects_traversal_private_urls_missing_hashes_and_conflicting_loaders() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("bad.mrpack");
    for invalid_path in [
        "../escape",
        "C:/escape",
        "mods/../../escape",
        "mods\\escape",
        "NUL.txt",
        "mods/COM¹.jar",
    ] {
        let mut index = manifest();
        index["files"][0]["path"] = invalid_path.into();
        archive(&path, &index, &[]);
        assert!(
            inspect(&path, directory.path().join("game")).is_err(),
            "accepted {invalid_path}"
        );
    }
    for url in [
        "http://cdn.modrinth.com/a",
        "https://127.0.0.1/a",
        "https://cdn.modrinth.com/a b",
        "https://cdn.modrinth.com/%gg",
        "https://unknown.example/a",
    ] {
        let mut index = manifest();
        index["files"][0]["downloads"] = serde_json::json!([url]);
        archive(&path, &index, &[]);
        assert!(
            inspect(&path, directory.path().join("game")).is_err(),
            "accepted {url}"
        );
    }
    let mut index = manifest();
    index["files"][0]["hashes"]
        .as_object_mut()
        .expect("hashes")
        .remove("sha512");
    archive(&path, &index, &[]);
    assert!(inspect(&path, directory.path().join("game")).is_err());
    let mut index = manifest();
    index["dependencies"]["forge"] = "test".into();
    archive(&path, &index, &[]);
    assert!(inspect(&path, directory.path().join("game")).is_err());
}

#[test]
fn zip64_entry_metadata_within_limits_remains_readable() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("zip64-entry.mrpack");
    let mut zip = ZipWriter::new(File::create(&path).expect("fixture ZIP"));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true);
    zip.start_file("modrinth.index.json", options)
        .expect("manifest entry");
    zip.write_all(&serde_json::to_vec(&manifest()).expect("manifest JSON"))
        .expect("manifest bytes");
    zip.start_file("overrides/config/test.txt", options)
        .expect("override entry");
    zip.write_all(b"client").expect("override bytes");
    zip.finish().expect("finish fixture");
    let plan = inspect(&path, directory.path().join("game"))
        .expect("ZIP64 entry metadata does not bypass the size limits");
    assert_eq!(plan.overrides.len(), 1);
    let stage = directory.path().join("stage");
    std::fs::create_dir(&stage).expect("private stage");
    extract_overrides(&path, &plan, &stage).expect("extract ZIP64-marked entry");
    assert_eq!(
        std::fs::read(stage.join("config/test.txt")).expect("override"),
        b"client"
    );
}

#[test]
fn rejects_unsafe_zip_paths_links_case_aliases_and_size_limits() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("bad.mrpack");
    for entries in [
        vec![("overrides/../../escape", b"x".as_slice())],
        vec![
            ("overrides/Mods/a.jar", b"a".as_slice()),
            ("overrides/mods/b.jar", b"b".as_slice()),
        ],
        vec![
            ("overrides/config", b"x".as_slice()),
            ("client-overrides/config/file", b"y".as_slice()),
        ],
    ] {
        archive(&path, &manifest(), &entries);
        assert!(inspect(&path, directory.path().join("game")).is_err());
    }
    let mut writer = ZipWriter::new(File::create(&path).expect("ZIP"));
    writer
        .add_symlink(
            "overrides/link",
            "../../escape",
            SimpleFileOptions::default(),
        )
        .expect("symlink entry");
    writer.finish().expect("finish ZIP");
    assert!(inspect(&path, directory.path().join("game")).is_err());
    archive(&path, &manifest(), &[("overrides/large", b"12345")]);
    for limits in [
        ArchiveLimits {
            entries: 0,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            manifest_bytes: 1,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            expanded_bytes: 1,
            ..ArchiveLimits::default()
        },
        ArchiveLimits {
            archive_bytes: 1,
            ..ArchiveLimits::default()
        },
    ] {
        assert!(inspect_with_limits(&path, directory.path().join("game"), limits).is_err());
    }
}

#[test]
fn changed_archive_and_crc_corruption_never_produce_accepted_overrides() {
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("pack.mrpack");
    archive(&path, &manifest(), &[("overrides/config/a", b"first")]);
    let plan = inspect(&path, directory.path().join("game")).expect("inspect");
    archive(&path, &manifest(), &[("overrides/config/a", b"other")]);
    let stage = tempfile::tempdir().expect("stage");
    assert!(extract_overrides(&path, &plan, stage.path()).is_err());
    let mut zip = zip::ZipArchive::new(File::open(&path).expect("open fixture")).expect("read ZIP");
    let start = zip
        .by_name("overrides/config/a")
        .expect("override entry")
        .data_start();
    let start = start.expect("stored entry offset");
    drop(zip);
    let mut bytes = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("modify fixture");
    bytes.seek(SeekFrom::Start(start)).expect("seek payload");
    let mut byte = [0];
    bytes.read_exact(&mut byte).expect("read payload");
    byte[0] ^= 1;
    bytes.seek(SeekFrom::Start(start)).expect("seek payload");
    bytes.write_all(&byte).expect("corrupt CRC");
    drop(bytes);
    assert!(matches!(
        inspect(&path, directory.path().join("game")),
        Err(Error::Io { .. })
    ));
}
