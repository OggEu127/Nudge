//! Offline tests of the public instance installation and repair workflow.

use std::{cell::RefCell, path::PathBuf};

use launcher_core::instances::{InstallProgress, InstanceManager};
use launcher_minecraft::{VersionMetadata, parse_version_metadata};
use sha1::{Digest as _, Sha1};
use tempfile::TempDir;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const CLIENT: &[u8] = b"verified fixture client jar";
const LIBRARY: &[u8] = b"verified fixture library jar";
const ASSET: &[u8] = b"verified cached sound asset";
const ASSET_INDEX_ID: &str = "fixture-assets";

fn write_pack_archive(path: &std::path::Path, dependencies: &serde_json::Value) {
    use std::io::Write as _;
    let index = serde_json::json!({"formatVersion":1,"game":"minecraft","name":"Offline import fixture","versionId":"1","dependencies":dependencies,"files":[{"path":"mods/fixture.jar","fileSize":3,"hashes":{"sha1":hex::encode(Sha1::digest(b"mod")),"sha512":hex::encode(sha2::Sha512::digest(b"mod"))},"downloads":["https://cdn.modrinth.com/fixture.jar"]}]});
    let mut archive = zip::ZipWriter::new(std::fs::File::create(path).expect("pack fixture"));
    archive
        .start_file(
            "modrinth.index.json",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("manifest entry");
    archive
        .write_all(&serde_json::to_vec(&index).expect("manifest JSON"))
        .expect("manifest bytes");
    archive
        .start_file(
            "client-overrides/config/fixture.conf",
            zip::write::SimpleFileOptions::default(),
        )
        .expect("override entry");
    archive
        .write_all(b"client configuration")
        .expect("override bytes");
    archive.finish().expect("finish archive");
}

fn pack_fixture(path: &std::path::Path, target: PathBuf) -> launcher_modpack::InstallPlan {
    write_pack_archive(path, &serde_json::json!({"minecraft": "fixture-version"}));
    launcher_modpack::inspect(path, target).expect("valid archive plan")
}

async fn seed_interrupted_pack(
    fixture: &Fixture,
    plan: &launcher_modpack::InstallPlan,
) -> InstanceManager {
    use launcher_core::modpack::{ManagedPackFile, PackRecord};
    let mut instance = fixture
        .manager
        .install_profile("pack", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("seed runtime files");
    // Point every runtime URL at an unresolvable host. This test deliberately
    // uses the production HTTP policy to prove that verified local content and
    // cache entries are committed without any network access; the mock-server
    // history assertion below would fail on an unexpected request.
    let unreachable = url::Url::parse("https://offline-fixture.invalid/file").expect("URL");
    instance
        .metadata
        .downloads
        .client
        .as_mut()
        .expect("client")
        .url = unreachable.clone();
    instance.metadata.asset_index.as_mut().expect("index").url = unreachable.clone();
    for library in &mut instance.metadata.libraries {
        library.downloads.artifact.as_mut().expect("library").url = unreachable.clone();
    }
    let file = &plan.required_files[0];
    let overlay = &plan.overrides[0];
    instance.installed = false;
    instance.pack = Some(PackRecord {
        source_archive: None,
        archive_sha256: plan.archive_sha256.clone(),
        name: plan.name.clone(),
        version: plan.version.clone(),
        selected_optional: std::collections::BTreeSet::default(),
        files: vec![
            ManagedPackFile {
                path: file.path.clone(),
                size: 3,
                hashes: file.spec.hashes.clone(),
                verify_on_launch: true,
            },
            ManagedPackFile {
                path: overlay.path.clone(),
                size: overlay.size,
                hashes: overlay.hashes.clone(),
                verify_on_launch: false,
            },
        ],
    });
    let record = fixture
        .directory
        .path()
        .join("instances/pack/instance.json");
    tokio::fs::write(record, serde_json::to_vec(&instance).expect("record JSON"))
        .await
        .expect("simulate interrupted record");
    let game = fixture.manager.game_directory("pack").expect("game path");
    tokio::fs::create_dir_all(game.join("mods"))
        .await
        .expect("mods directory");
    tokio::fs::write(game.join(&file.path), b"mod")
        .await
        .expect("verified existing mod");
    let cache = fixture
        .directory
        .path()
        .join(".nudge-downloads/cache")
        .join(format!(
            "sha512-{}",
            file.spec.hashes.sha512.as_deref().expect("SHA512")
        ));
    tokio::fs::write(cache, b"mod")
        .await
        .expect("verified cache");
    InstanceManager::new(fixture.directory.path())
        .expect("deliberate production-policy offline test")
}

#[tokio::test]
async fn cancelled_launch_preparation_never_creates_or_starts_an_instance() {
    let fixture = Fixture::new().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    let result = fixture
        .manager
        .launch_controlled(
            "cancelled",
            &launcher_minecraft::launch::LaunchIdentity::demo(),
            launcher_minecraft::launch::LaunchOptions {
                demo: true,
                ..Default::default()
            },
            None,
            &cancel,
        )
        .await;
    assert!(matches!(
        result,
        Err(launcher_core::Error::Network(
            launcher_net::Error::Cancelled
        ))
    ));
    assert_eq!(fixture.request_paths().await, Vec::<String>::new());
    assert!(
        !fixture
            .directory
            .path()
            .join("instances/cancelled")
            .exists()
    );
}

#[tokio::test]
async fn inspection_rejects_recognized_but_unsupported_loaders() {
    let fixture = Fixture::new().await;
    let archive = fixture.directory.path().join("quilt.mrpack");
    write_pack_archive(
        &archive,
        &serde_json::json!({"minecraft": "fixture-version", "quilt-loader": "0.20.0"}),
    );
    let error = fixture
        .manager
        .inspect_modpack(&archive, "quilt")
        .await
        .expect_err("unsupported loader must not produce a preview");
    assert!(matches!(
        &error,
        launcher_core::Error::InvalidInput(message) if message.contains("not supported")
    ));
    assert!(!fixture.directory.path().join("instances/quilt").exists());
}

#[tokio::test]
async fn import_requires_the_reviewed_archive_before_creating_an_instance() {
    let fixture = Fixture::new().await;
    let archive = fixture.directory.path().join("reviewed.mrpack");
    let game = fixture
        .manager
        .game_directory("reviewed")
        .expect("game path");
    let _plan = pack_fixture(&archive, game);
    let before = fixture.request_paths().await;
    let error = fixture
        .manager
        .import_modpack(
            &archive,
            "reviewed",
            launcher_core::modpack::ImportOptions {
                expected_archive_sha256: Some("0".repeat(64)),
                ..Default::default()
            },
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .expect_err("different archive must be reviewed again");
    assert!(error.to_string().contains("changed after inspection"));
    assert_eq!(fixture.request_paths().await, before);
    assert!(!fixture.directory.path().join("instances/reviewed").exists());
}

#[tokio::test]
async fn pack_resume_uses_only_verified_local_content_applies_overrides_and_checks_mods() {
    let fixture = Fixture::new().await;
    let archive = fixture.directory.path().join("fixture.anything");
    let game = fixture.manager.game_directory("pack").expect("game");
    let plan = pack_fixture(&archive, game.clone());
    let manager = seed_interrupted_pack(&fixture, &plan).await;
    let before = fixture.request_paths().await;
    let imported = manager
        .import_modpack(
            &archive,
            "pack",
            launcher_core::modpack::ImportOptions::default(),
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("fully offline resume");
    assert!(imported.installed);
    assert_eq!(
        tokio::fs::read(game.join("config/fixture.conf"))
            .await
            .expect("override"),
        b"client configuration"
    );
    tokio::fs::write(game.join("config/fixture.conf"), b"user preference")
        .await
        .expect("editable config");
    manager
        .verify(&imported)
        .await
        .expect("user configuration remains editable");
    tokio::fs::write(game.join("mods/fixture.jar"), b"corrupt")
        .await
        .expect("corrupt mod");
    assert!(manager.verify(&imported).await.is_err());
    manager
        .import_modpack(
            &archive,
            "pack",
            launcher_core::modpack::ImportOptions::default(),
            tokio_util::sync::CancellationToken::new(),
            |_| {},
        )
        .await
        .expect("repair from verified cache");
    assert_eq!(
        tokio::fs::read(game.join("mods/fixture.jar"))
            .await
            .expect("repaired mod"),
        b"mod"
    );
    assert_eq!(fixture.request_paths().await, before);
}

struct Fixture {
    directory: TempDir,
    server: MockServer,
    manager: InstanceManager,
    metadata: VersionMetadata,
}

impl Fixture {
    async fn new() -> Self {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir().expect("fixture data directory");
        let http = launcher_net::ReqwestHttpClient::new_with_policy(
            "Nudge/test",
            launcher_net::DEFAULT_MAX_RESPONSE_BYTES,
            launcher_net::policy::NetworkPolicy::LoopbackOnly,
        )
        .expect("loopback test HTTP client");
        let manager = InstanceManager::with_http(directory.path(), http).with_asset_source(
            url::Url::parse(&format!("{}/assets/", server.uri())).expect("test asset source"),
        );
        let asset_hash = hex::encode(Sha1::digest(ASSET));
        let index = serde_json::to_vec(&serde_json::json!({
            "virtual": true,
            "map_to_resources": true,
            "objects": {
                "sound/fixture.ogg": {
                    "hash": asset_hash,
                    "size": ASSET.len()
                }
            }
        }))
        .expect("fixture asset-index JSON");
        let json = serde_json::json!({
            "id": "fixture-version",
            "type": "release",
            "mainClass": "example.FixtureMain",
            "arguments": {
                "game": ["--username", "${auth_player_name}"],
                "jvm": ["-cp", "${classpath}"]
            },
            "downloads": {
                "client": {
                    "url": format!("{}/client.jar", server.uri()),
                    "sha1": hex::encode(Sha1::digest(CLIENT)),
                    "size": CLIENT.len()
                }
            },
            "assetIndex": {
                "id": ASSET_INDEX_ID,
                "url": format!("{}/index.json", server.uri()),
                "sha1": hex::encode(Sha1::digest(&index)),
                "size": index.len()
            },
            "libraries": [{
                "name": "org.nudge.fixture:library:1",
                "downloads": {
                    "artifact": {
                        "path": "org/nudge/fixture/library/1/library-1.jar",
                        "url": format!("{}/library.jar", server.uri()),
                        "sha1": hex::encode(Sha1::digest(LIBRARY)),
                        "size": LIBRARY.len()
                    }
                }
            }]
        });
        let metadata = parse_version_metadata(&serde_json::to_vec(&json).expect("version JSON"))
            .expect("valid fixture version metadata");
        for (endpoint, body) in [
            ("/client.jar", CLIENT.to_vec()),
            ("/library.jar", LIBRARY.to_vec()),
            ("/index.json", index),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .mount(&server)
                .await;
        }

        // Asset URLs are pinned to Mojang. Seeding a verified content-addressed
        // object exercises local reuse without making this test internet-dependent.
        let object = directory
            .path()
            .join("assets/objects")
            .join(&asset_hash[..2])
            .join(&asset_hash);
        tokio::fs::create_dir_all(object.parent().expect("asset parent"))
            .await
            .expect("create asset cache");
        tokio::fs::write(object, ASSET)
            .await
            .expect("seed verified asset cache");

        Self {
            directory,
            server,
            manager,
            metadata,
        }
    }

    fn library_path(&self) -> PathBuf {
        self.directory
            .path()
            .join("libraries/org/nudge/fixture/library/1/library-1.jar")
    }

    async fn request_paths(&self) -> Vec<String> {
        self.server
            .received_requests()
            .await
            .expect("mock HTTP request history")
            .into_iter()
            .map(|request| request.url.path().to_owned())
            .collect()
    }

    async fn assert_legacy_assets(&self, id: &str) {
        let game = self.manager.game_directory(id).expect("game path");
        assert_eq!(
            tokio::fs::read(
                self.directory
                    .path()
                    .join("assets/virtual")
                    .join(ASSET_INDEX_ID)
                    .join("sound/fixture.ogg")
            )
            .await
            .expect("legacy virtual asset"),
            ASSET
        );
        assert_eq!(
            tokio::fs::read(game.join("resources/sound/fixture.ogg"))
                .await
                .expect("legacy resources asset"),
            ASSET
        );
    }
}

#[tokio::test]
async fn installs_verifies_and_repairs_an_instance_without_overwriting_saves() {
    let fixture = Fixture::new().await;
    let progress = RefCell::<Vec<InstallProgress>>::default();
    let instance = fixture
        .manager
        .install_profile(
            "demo",
            "fixture-version",
            fixture.metadata.clone(),
            |event| {
                progress.borrow_mut().push(event);
            },
        )
        .await
        .expect("install fixture instance");
    assert!(instance.installed);
    assert_eq!(
        fixture.manager.list().await.expect("list instances").len(),
        1
    );
    {
        let events = progress.borrow();
        assert_eq!(events.len(), 4);
        assert!(
            events
                .last()
                .is_some_and(|event| event.completed == event.total)
        );
        assert!(
            events
                .iter()
                .any(|event| { event.path.starts_with("assets/objects") && event.reused })
        );
    }

    let plan = fixture
        .manager
        .verify(&instance)
        .await
        .expect("verify all installed files");
    assert_eq!(plan.classpath.len(), 2);
    assert_eq!(fixture.request_paths().await.len(), 3);
    let game = fixture.manager.game_directory("demo").expect("game path");
    fixture.assert_legacy_assets("demo").await;

    let save = game.join("saves/world/level.dat");
    tokio::fs::create_dir_all(save.parent().expect("save parent"))
        .await
        .expect("create game-save directory");
    tokio::fs::write(&save, b"irreplaceable user save")
        .await
        .expect("create game save");
    tokio::fs::write(fixture.library_path(), b"corrupt")
        .await
        .expect("corrupt library fixture");
    let error = fixture
        .manager
        .verify(&instance)
        .await
        .expect_err("corrupted library must fail verification");
    assert!(error.to_string().contains("library-1.jar"));

    let repaired = fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("repair existing profile");
    fixture
        .manager
        .verify(&repaired)
        .await
        .expect("verify repaired profile");
    assert_eq!(
        tokio::fs::read(&save).await.expect("read preserved save"),
        b"irreplaceable user save"
    );
    let requests = fixture.request_paths().await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|path| *path == "/library.jar")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|path| *path == "/client.jar")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|path| *path == "/index.json")
            .count(),
        1
    );
}

#[tokio::test]
async fn refuses_to_replace_an_instance_with_a_different_profile() {
    let fixture = Fixture::new().await;
    fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("install original profile");
    let mut different = fixture.metadata.clone();
    different.id = "another-profile".to_owned();
    assert!(
        fixture
            .manager
            .install_profile("demo", "fixture-version", different, |_| {})
            .await
            .is_err()
    );
    let record = fixture.manager.load("demo").await.expect("original record");
    assert_eq!(record.metadata.id, "fixture-version");
    assert!(record.installed);
    assert_eq!(fixture.request_paths().await.len(), 3);
}

#[tokio::test]
async fn repairing_hardlinked_legacy_assets_preserves_the_external_file() {
    let fixture = Fixture::new().await;
    let instance = fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("install fixture instance");
    let outside = tempfile::tempdir().expect("external asset directory");
    let external = outside.path().join("unrelated-user-file");
    let original = b"unrelated file that must never be truncated";
    tokio::fs::write(&external, original)
        .await
        .expect("write external user file");
    let game = fixture.manager.game_directory("demo").expect("game path");
    let destinations = [
        game.join("resources/sound/fixture.ogg"),
        fixture
            .directory
            .path()
            .join("assets/virtual")
            .join(ASSET_INDEX_ID)
            .join("sound/fixture.ogg"),
    ];
    for destination in &destinations {
        tokio::fs::remove_file(destination)
            .await
            .expect("replace derived fixture asset");
        tokio::fs::hard_link(&external, destination)
            .await
            .expect("hard-link derived asset to external user file");
    }

    fixture
        .manager
        .verify(&instance)
        .await
        .expect("atomically repair derived assets");
    fixture.assert_legacy_assets("demo").await;
    assert_eq!(
        tokio::fs::read(&external)
            .await
            .expect("read preserved external user file"),
        original
    );
    for destination in destinations {
        tokio::fs::write(destination, b"subsequent derived asset change")
            .await
            .expect("change repaired asset independently");
    }
    assert_eq!(
        tokio::fs::read(&external)
            .await
            .expect("external file is no longer aliased"),
        original
    );
    assert_eq!(fixture.request_paths().await.len(), 3);
}

#[tokio::test]
async fn a_held_instance_lock_prevents_installation_until_released() {
    let fixture = Fixture::new().await;
    let directory = fixture.directory.path().join("instances/demo");
    tokio::fs::create_dir_all(&directory)
        .await
        .expect("create instance directory");
    let lease = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join("instance.lock"))
        .expect("open instance lock");
    lease.try_lock().expect("hold instance lock");
    let error = fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect_err("a second installation must not acquire the held lease");
    assert!(error.to_string().contains("busy"));
    assert_eq!(fixture.request_paths().await, [] as [String; 0]);
    assert!(!directory.join("instance.json").exists());
    drop(lease);

    let instance = fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("released lease permits installation");
    assert!(instance.installed);
}

#[tokio::test]
async fn link_checks_accept_canonical_roots_and_missing_descendants() {
    use launcher_core::instances::check_no_links;

    let directory = tempfile::tempdir().expect("temporary directory");
    let nested = directory.path().join("中文 path");
    std::fs::create_dir(&nested).expect("fixture directory");
    let canonical = std::fs::canonicalize(&nested).expect("canonical fixture");
    let root = canonical.ancestors().last().expect("absolute root");
    for path in [&nested, &canonical, root] {
        check_no_links(path).await.expect("valid ancestor chain");
    }
    check_no_links(&canonical.join("not-created/child"))
        .await
        .expect("missing descendants are allowed");
    assert!(!canonical.join("not-created").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn windows_junctions_are_rejected_by_all_ancestor_checks() {
    use std::os::windows::process::CommandExt as _;

    use launcher_core::instances::check_no_links;
    use launcher_net::{
        engine::DownloadManager,
        transfer::{Hashes, verify_content},
    };

    let directory = tempfile::tempdir().expect("temporary directory");
    let outside = tempfile::tempdir().expect("outside directory");
    std::fs::write(outside.path().join("sentinel.bin"), b"untouched").expect("outside sentinel");
    let base = std::fs::canonicalize(directory.path()).expect("canonical fixture");
    let linked = base.join("junction");
    // NTFS junctions do not require Developer Mode or symlink privileges.
    // Expand fixture paths inside quotes, rather than interpolating command text.
    let output = std::process::Command::new("cmd")
        .args(["/D", "/C"])
        .raw_arg("mklink /J \"%NUDGE_TEST_LINK%\" \"%NUDGE_TEST_TARGET%\"")
        .env("NUDGE_TEST_LINK", directory.path().join("junction"))
        .env("NUDGE_TEST_TARGET", outside.path())
        .output()
        .expect("create junction fixture");
    assert!(
        output.status.success(),
        "cannot create junction fixture: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let error = check_no_links(&linked.join("child"))
        .await
        .expect_err("instance paths must reject junctions");
    assert!(
        matches!(error, launcher_core::Error::InvalidInput(_)),
        "{error:?}"
    );
    let error = DownloadManager::new(linked.clone(), std::num::NonZeroUsize::MIN)
        .err()
        .expect("download roots must reject junctions");
    assert!(
        matches!(error, launcher_net::Error::InvalidSpec(_)),
        "{error:?}"
    );
    let error = verify_content(
        &linked.join("sentinel.bin"),
        Some(9),
        &Hashes {
            sha1: Some(hex::encode(Sha1::digest(b"untouched"))),
            ..Hashes::default()
        },
    )
    .await
    .expect_err("file verification must reject junctions");
    assert!(
        matches!(error, launcher_net::Error::Integrity { .. }),
        "{error:?}"
    );
    assert!(!outside.path().join(".nudge-downloads").exists());
    assert_eq!(
        std::fs::read(outside.path().join("sentinel.bin")).expect("sentinel"),
        b"untouched"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_a_symlinked_instance_directory_before_any_download() {
    let fixture = Fixture::new().await;
    let outside = tempfile::tempdir().expect("external directory");
    let instances = fixture.directory.path().join("instances");
    tokio::fs::create_dir_all(&instances)
        .await
        .expect("create instances directory");
    std::os::unix::fs::symlink(outside.path(), instances.join("demo"))
        .expect("symlink fixture instance");
    assert!(
        fixture
            .manager
            .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
            .await
            .is_err()
    );
    assert_eq!(fixture.request_paths().await, [] as [String; 0]);
    assert!(!outside.path().join("instance.json").exists());
    assert!(!outside.path().join("game").exists());
}

#[tokio::test]
async fn failed_install_retains_an_incomplete_record_and_resumes() {
    let fixture = Fixture::new().await;
    let outage = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let availability = outage.clone();
    Mock::given(method("GET"))
        .and(path("/library.jar"))
        .respond_with(move |_: &wiremock::Request| {
            if availability.load(std::sync::atomic::Ordering::SeqCst) {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_bytes(LIBRARY.to_vec())
            }
        })
        .with_priority(1)
        .mount(&fixture.server)
        .await;
    assert!(
        fixture
            .manager
            .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
            .await
            .is_err()
    );
    let incomplete = fixture
        .manager
        .load("demo")
        .await
        .expect("incomplete record remains available");
    assert!(!incomplete.installed);
    assert!(fixture.manager.verify(&incomplete).await.is_err());
    outage.store(false, std::sync::atomic::Ordering::SeqCst);
    let installed = fixture
        .manager
        .install_profile("demo", "fixture-version", fixture.metadata.clone(), |_| {})
        .await
        .expect("resume failed installation");
    fixture
        .manager
        .verify(&installed)
        .await
        .expect("resumed installation is complete");
    let requests = fixture.request_paths().await;
    assert_eq!(
        requests
            .iter()
            .filter(|path| *path == "/client.jar")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|path| *path == "/index.json")
            .count(),
        1
    );
}
