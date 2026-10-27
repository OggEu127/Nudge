//! Inheritance and deterministic installation planning without filesystem access.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::{
    Error,
    metadata::{Argument, DownloadArtifact, Library, VersionArguments, VersionMetadata},
    rules::{Platform, allowed},
};

/// One content-addressed resource in a Mojang asset index.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetObject {
    /// SHA-1 of the resource content.
    pub hash: String,
    /// Expected byte length.
    pub size: u64,
}

/// Mojang asset index, including legacy materialization requirements.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetIndex {
    /// Logical asset name to content-addressed object.
    pub objects: BTreeMap<String, AssetObject>,
    /// Copy objects to `assets/virtual/<index-id>/<name>` for older versions.
    #[serde(rename = "virtual", default)]
    pub virtual_assets: bool,
    /// Copy objects to the isolated game's `resources/<name>` directory.
    #[serde(default)]
    pub map_to_resources: bool,
}

/// A verified-download requirement, relative to the shared storage directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedFile {
    /// Relative destination in shared storage.
    pub path: PathBuf,
    /// Expected content and download URL.
    pub artifact: DownloadArtifact,
}

/// Native archive to extract after successful content verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeArchive {
    /// Archive path relative to shared storage.
    pub path: PathBuf,
    /// Archive-relative prefixes omitted from extraction.
    pub excludes: Vec<String>,
}

/// Pure, platform-specific installation and launch inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedVersion {
    /// Download and verification requirements, deduplicated by destination.
    pub files: Vec<ResolvedFile>,
    /// Ordered classpath, relative to shared storage; client JAR is last.
    pub classpath: Vec<PathBuf>,
    /// Native archives requiring safe extraction.
    pub natives: Vec<NativeArchive>,
    /// Minecraft client JAR, retaining the inherited base game's identity.
    pub client_path: PathBuf,
    /// Asset-index identifier used in launch arguments.
    pub asset_index_id: String,
    /// Legacy `${game_assets}` destination relative to shared storage.
    pub game_assets_path: PathBuf,
    /// Legacy `${game_assets}` uses this instance's `resources` directory.
    pub game_assets_in_resources: bool,
    /// Logging configuration path relative to shared storage.
    pub logging_path: Option<PathBuf>,
}

/// Validates a portable relative path originating in untrusted metadata.
///
/// Backslashes and Windows filename aliases are rejected so every planned
/// destination is safe before installation.
///
/// # Errors
///
/// Returns an error for absolute paths, traversal, drive prefixes, empty
/// components, control characters, reserved devices, and ambiguous Windows names.
pub fn safe_relative_path(value: &str) -> Result<PathBuf, Error> {
    if value.is_empty() || value.len() > 4096 || value.contains('\\') {
        return Err(invalid_path(value));
    }
    for component in value.split('/') {
        if component.is_empty()
            || component == "."
            || component == ".."
            || component.len() > 255
            || component.ends_with([' ', '.'])
            || component.chars().any(|character| {
                character.is_control()
                    || matches!(character, ':' | '<' | '>' | '"' | '|' | '?' | '*')
            })
            || is_windows_device(component)
        {
            return Err(invalid_path(value));
        }
    }
    Ok(PathBuf::from(value))
}

fn is_windows_device(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ');
    let upper = stem.to_ascii_uppercase();
    if matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let suffix = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"));
    suffix.is_some_and(|suffix| {
        matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    })
}

fn invalid_path(value: &str) -> Error {
    Error::InvalidMetadata(format!("unsafe relative metadata path `{value}`"))
}

fn safe_identifier(value: &str) -> Result<(), Error> {
    safe_relative_path(value)?;
    if value.contains('/') {
        return Err(invalid_path(value));
    }
    Ok(())
}

/// Resolves an inheritance chain with cycle and missing-parent detection.
///
/// # Errors
///
/// Returns an error for unknown versions, cycles, excessive chain depth, or
/// malformed Maven coordinates in an inherited profile.
pub fn resolve_inheritance(
    id: &str,
    versions: &BTreeMap<String, VersionMetadata>,
) -> Result<VersionMetadata, Error> {
    let mut seen = BTreeSet::new();
    let mut chain = Vec::new();
    let mut current = id;
    loop {
        if chain.len() >= 64 {
            return Err(Error::InvalidMetadata(
                "version inheritance exceeds 64 levels".to_owned(),
            ));
        }
        if !seen.insert(current.to_owned()) {
            return Err(Error::InvalidMetadata(format!(
                "version inheritance cycle includes `{current}`"
            )));
        }
        let version = versions.get(current).ok_or_else(|| {
            Error::InvalidMetadata(format!("missing inherited version `{current}`"))
        })?;
        if version.id != current {
            return Err(Error::InvalidMetadata(format!(
                "version map key `{current}` differs from profile ID `{}`",
                version.id
            )));
        }
        chain.push(version);
        if let Some(parent) = &version.inherits_from {
            current = parent;
        } else {
            break;
        }
    }
    let mut iterator = chain.into_iter().rev();
    let Some(base) = iterator.next() else {
        return Err(Error::InvalidMetadata("empty inheritance chain".to_owned()));
    };
    let mut result = base.clone();
    for child in iterator {
        result = merge(&result, child)?;
    }
    result.inherits_from = None;
    Ok(result)
}

/// Merges a resolved parent with one child profile.
///
/// Child scalars replace parent scalars, modern argument lists concatenate in
/// parent-first order, and Maven libraries replace matching group/artifact/
/// classifier/extension identities regardless of version. An inherited client
/// keeps its original JAR location even when the profile ID changes.
///
/// # Errors
///
/// Returns an error for malformed library coordinates or invalid legacy quoting.
pub fn merge(parent: &VersionMetadata, child: &VersionMetadata) -> Result<VersionMetadata, Error> {
    let mut result = parent.clone();
    result.id.clone_from(&child.id);
    result.inherits_from = None;
    result.jar = child.jar.clone().or_else(|| {
        if child.downloads.client.is_some() {
            None
        } else {
            Some(parent.jar.clone().unwrap_or_else(|| parent.id.clone()))
        }
    });
    overlay(&mut result.kind, child.kind.as_ref());
    overlay(&mut result.main_class, child.main_class.as_ref());
    overlay(&mut result.asset_index, child.asset_index.as_ref());
    overlay(&mut result.assets, child.assets.as_ref());
    overlay(&mut result.java_version, child.java_version.as_ref());
    overlay(&mut result.time, child.time.as_ref());
    overlay(&mut result.release_time, child.release_time.as_ref());
    result.minimum_launcher_version = parent
        .minimum_launcher_version
        .into_iter()
        .chain(child.minimum_launcher_version)
        .max();
    if let Some(arguments) = &child.arguments {
        let mut combined = parent.arguments.clone().unwrap_or_default();
        if let Some(legacy) = &parent.minecraft_arguments {
            combined.game.extend(
                shell_words::split(legacy)
                    .map_err(|error| {
                        Error::InvalidMetadata(format!(
                            "invalid legacy arguments in `{}`: {error}",
                            parent.id
                        ))
                    })?
                    .into_iter()
                    .map(Argument::Literal),
            );
        }
        append_arguments(&mut combined, arguments);
        result.arguments = Some(combined);
        result.minecraft_arguments = None;
    } else if child.minecraft_arguments.is_some() {
        result
            .minecraft_arguments
            .clone_from(&child.minecraft_arguments);
        result.arguments = None;
    }
    result.libraries = merge_libraries(&parent.libraries, &child.libraries)?;
    overlay(
        &mut result.downloads.client,
        child.downloads.client.as_ref(),
    );
    overlay(
        &mut result.downloads.client_mappings,
        child.downloads.client_mappings.as_ref(),
    );
    overlay(
        &mut result.downloads.server,
        child.downloads.server.as_ref(),
    );
    overlay(
        &mut result.downloads.server_mappings,
        child.downloads.server_mappings.as_ref(),
    );
    result
        .downloads
        .additional
        .extend(child.downloads.additional.clone());
    overlay(&mut result.logging.client, child.logging.client.as_ref());
    Ok(result)
}

fn overlay<T: Clone>(target: &mut Option<T>, replacement: Option<&T>) {
    if let Some(value) = replacement {
        *target = Some(value.clone());
    }
}

fn append_arguments(parent: &mut VersionArguments, child: &VersionArguments) {
    parent.jvm.extend(child.jvm.iter().cloned());
    parent
        .default_user_jvm
        .extend(child.default_user_jvm.iter().cloned());
    parent.game.extend(child.game.iter().cloned());
}

fn merge_libraries(parent: &[Library], child: &[Library]) -> Result<Vec<Library>, Error> {
    let mut positions = BTreeMap::new();
    let mut result = Vec::new();
    for library in parent.iter().chain(child) {
        let coordinate = MavenCoordinate::parse(&library.name)?;
        let key = coordinate.conflict_key();
        if let Some(index) = positions.get(&key).copied() {
            result[index] = library.clone();
        } else {
            positions.insert(key, result.len());
            result.push(library.clone());
        }
    }
    Ok(result)
}

/// Computes the shared logging configuration destination.
///
/// # Errors
///
/// Returns an error if the logging file has no safe identifier.
pub fn logging_path(artifact: &DownloadArtifact) -> Result<PathBuf, Error> {
    let name = artifact
        .id
        .as_deref()
        .or(artifact.path.as_deref())
        .or_else(|| artifact.url.path_segments().and_then(Iterator::last))
        .ok_or_else(|| Error::InvalidMetadata("logging file has no identifier".to_owned()))?;
    safe_identifier(name)?;
    Ok(PathBuf::from("logging").join(name))
}

/// Produces all downloads, the ordered classpath, and native extraction inputs.
///
/// `version` must already have its inheritance resolved. Paths remain relative
/// to shared storage, which lets the executor choose its own safe storage root.
/// All Mojang files require both a SHA-1 and byte length. Loader libraries with
/// an explicit Maven repository may omit these only when metadata provides none.
///
/// # Errors
///
/// Returns an error for unresolved inheritance, unsafe paths, missing required
/// metadata, malformed digests, conflicting destination files, or invalid rules.
pub fn resolve(
    version: &VersionMetadata,
    asset_index: &AssetIndex,
    platform: &Platform,
) -> Result<ResolvedVersion, Error> {
    if version.inherits_from.is_some() {
        return Err(Error::InvalidMetadata(format!(
            "version `{}` still has unresolved inheritance",
            version.id
        )));
    }
    safe_identifier(&version.id)?;
    let jar = version.jar.as_deref().unwrap_or(&version.id);
    safe_identifier(jar)?;
    let client = version.downloads.client.clone().ok_or_else(|| {
        Error::InvalidMetadata(format!("version `{}` has no client download", version.id))
    })?;
    let client_path = PathBuf::from("versions")
        .join(jar)
        .join(format!("{jar}.jar"));
    let index = version.asset_index.as_ref().ok_or_else(|| {
        Error::InvalidMetadata(format!("version `{}` has no asset index", version.id))
    })?;
    safe_identifier(&index.id)?;
    let mut files = FileCollector::default();
    files.add(client_path.clone(), client, true)?;
    files.add(
        PathBuf::from("assets/indexes").join(format!("{}.json", index.id)),
        DownloadArtifact {
            id: None,
            path: None,
            sha1: Some(index.sha1.clone()),
            size: Some(index.size),
            url: index.url.clone(),
        },
        true,
    )?;
    let (mut classpath, natives) = resolve_libraries(&version.libraries, platform, &mut files)?;
    push_unique(&mut classpath, client_path.clone());
    resolve_assets(asset_index, &mut files)?;
    let logging_path = version
        .logging
        .client
        .as_ref()
        .map(|logging| {
            let path = logging_path(&logging.file)?;
            files.add(path.clone(), logging.file.clone(), true)?;
            Ok::<_, Error>(path)
        })
        .transpose()?;
    Ok(ResolvedVersion {
        files: files.files,
        classpath,
        natives,
        client_path,
        asset_index_id: index.id.clone(),
        game_assets_path: if asset_index.virtual_assets {
            PathBuf::from("assets/virtual").join(&index.id)
        } else {
            PathBuf::from("assets")
        },
        game_assets_in_resources: asset_index.map_to_resources && !asset_index.virtual_assets,
        logging_path,
    })
}

fn resolve_libraries(
    libraries: &[Library],
    platform: &Platform,
    files: &mut FileCollector,
) -> Result<(Vec<PathBuf>, Vec<NativeArchive>), Error> {
    let mut classpath = Vec::new();
    let mut natives = Vec::new();
    for library in libraries {
        if !allowed(&library.rules, platform)? {
            continue;
        }
        let coordinate = MavenCoordinate::parse(&library.name)?;
        if let Some(artifact) = &library.downloads.artifact {
            let path = library_path(artifact, &coordinate)?;
            files.add(path.clone(), artifact.clone(), library.url.is_none())?;
            push_unique(&mut classpath, path);
        } else if library.natives.is_empty() {
            let artifact = maven_artifact(library, &coordinate)?;
            let path = library_path(&artifact, &coordinate)?;
            files.add(path.clone(), artifact, false)?;
            push_unique(&mut classpath, path);
        }
        if let Some(classifier) = library.natives.get(&platform.os) {
            let classifier = classifier.replace("${arch}", "64");
            let artifact = library
                .downloads
                .classifiers
                .get(&classifier)
                .ok_or_else(|| {
                    Error::InvalidMetadata(format!(
                        "library `{}` has no native classifier `{classifier}`",
                        library.name
                    ))
                })?;
            let mut native_coordinate = coordinate.clone();
            native_coordinate.classifier = Some(classifier);
            let path = library_path(artifact, &native_coordinate)?;
            files.add(path.clone(), artifact.clone(), library.url.is_none())?;
            for excluded in &library.extract.exclude {
                safe_relative_path(excluded.trim_end_matches('/'))?;
            }
            natives.push(NativeArchive {
                path,
                excludes: library.extract.exclude.clone(),
            });
        }
    }
    Ok((classpath, natives))
}

fn resolve_assets(asset_index: &AssetIndex, files: &mut FileCollector) -> Result<(), Error> {
    let mut asset_names = BTreeSet::new();
    for (name, object) in &asset_index.objects {
        safe_relative_path(name)?;
        if !asset_names.insert(name.to_lowercase()) {
            return Err(Error::InvalidMetadata(format!(
                "asset names collide on a case-insensitive filesystem: `{name}`"
            )));
        }
        validate_sha1(&object.hash, name)?;
        let hash = object.hash.to_ascii_lowercase();
        let location = format!("{}/{hash}", &hash[..2]);
        let url = Url::parse(&format!(
            "https://resources.download.minecraft.net/{location}"
        ))
        .map_err(Error::InvalidEndpoint)?;
        files.add(
            PathBuf::from("assets/objects").join(location),
            DownloadArtifact {
                id: None,
                path: None,
                sha1: Some(hash),
                size: Some(object.size),
                url,
            },
            true,
        )?;
    }
    Ok(())
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn validate_sha1(hash: &str, label: &str) -> Result<(), Error> {
    if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidMetadata(format!(
            "SHA-1 for `{label}` is malformed"
        )));
    }
    Ok(())
}

#[derive(Default)]
struct FileCollector {
    files: Vec<ResolvedFile>,
    positions: BTreeMap<String, usize>,
}

impl FileCollector {
    fn add(
        &mut self,
        path: PathBuf,
        artifact: DownloadArtifact,
        require_integrity: bool,
    ) -> Result<(), Error> {
        if require_integrity && (artifact.sha1.is_none() || artifact.size.is_none()) {
            return Err(Error::InvalidMetadata(format!(
                "download `{}` lacks required SHA-1 or byte length",
                path.display()
            )));
        }
        if let Some(hash) = &artifact.sha1 {
            validate_sha1(hash, &path.to_string_lossy())?;
        }
        let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        if let Some(index) = self.positions.get(&key).copied() {
            let previous = &self.files[index];
            if previous.path != path
                || previous.artifact.sha1 != artifact.sha1
                || previous.artifact.size != artifact.size
                || (artifact.sha1.is_none() && previous.artifact.url != artifact.url)
            {
                return Err(Error::InvalidMetadata(format!(
                    "conflicting downloads target `{}`",
                    path.display()
                )));
            }
        } else {
            self.positions.insert(key, self.files.len());
            self.files.push(ResolvedFile { path, artifact });
        }
        Ok(())
    }
}

#[derive(Clone)]
struct MavenCoordinate {
    group: String,
    artifact: String,
    version: String,
    classifier: Option<String>,
    extension: String,
}

impl MavenCoordinate {
    fn parse(name: &str) -> Result<Self, Error> {
        let (coordinate, extension) = name.rsplit_once('@').unwrap_or((name, "jar"));
        let parts: Vec<_> = coordinate.split(':').collect();
        if !(3..=4).contains(&parts.len()) {
            return Err(Error::InvalidMetadata(format!(
                "library coordinate `{name}` is malformed"
            )));
        }
        for part in parts.iter().chain(std::iter::once(&extension)) {
            safe_identifier(part)?;
        }
        for part in parts[0].split('.') {
            safe_identifier(part)?;
        }
        Ok(Self {
            group: parts[0].to_owned(),
            artifact: parts[1].to_owned(),
            version: parts[2].to_owned(),
            classifier: parts.get(3).map(ToString::to_string),
            extension: extension.to_owned(),
        })
    }

    fn conflict_key(&self) -> (String, String, Option<String>, String) {
        (
            self.group.clone(),
            self.artifact.clone(),
            self.classifier.clone(),
            self.extension.clone(),
        )
    }

    fn path(&self) -> Result<PathBuf, Error> {
        let classifier = self
            .classifier
            .as_ref()
            .map_or_else(String::new, |classifier| format!("-{classifier}"));
        safe_relative_path(&format!(
            "{}/{}/{}/{}-{}{classifier}.{}",
            self.group.replace('.', "/"),
            self.artifact,
            self.version,
            self.artifact,
            self.version,
            self.extension
        ))
    }
}

fn library_path(
    artifact: &DownloadArtifact,
    coordinate: &MavenCoordinate,
) -> Result<PathBuf, Error> {
    let relative = if let Some(path) = &artifact.path {
        safe_relative_path(path)?
    } else {
        coordinate.path()?
    };
    Ok(PathBuf::from("libraries").join(relative))
}

/// Resolves a validated Maven coordinate to its repository-relative artifact path.
///
/// # Errors
/// Rejects malformed coordinates and unsafe portable path components.
pub fn maven_path(name: &str) -> Result<PathBuf, Error> {
    MavenCoordinate::parse(name)?.path()
}

fn maven_artifact(
    library: &Library,
    coordinate: &MavenCoordinate,
) -> Result<DownloadArtifact, Error> {
    let base = library.url.as_ref().ok_or_else(|| {
        Error::InvalidMetadata(format!(
            "Mojang library `{}` has no download artifact",
            library.name
        ))
    })?;
    if !base.path().ends_with('/') || base.query().is_some() || base.fragment().is_some() {
        return Err(Error::InvalidMetadata(format!(
            "Maven repository URL `{base}` must end with `/` and have no query or fragment"
        )));
    }
    let path = coordinate.path()?.to_string_lossy().replace('\\', "/");
    let mut url = base.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|()| {
            Error::InvalidMetadata(format!("Maven repository URL `{base}` is not hierarchical"))
        })?;
        segments.pop_if_empty();
        segments.extend(path.split('/'));
    }
    Ok(DownloadArtifact {
        id: None,
        path: Some(path),
        sha1: None,
        size: None,
        url,
    })
}

#[cfg(test)]
mod tests {
    use super::{AssetIndex, merge, resolve, resolve_inheritance, safe_relative_path};
    use crate::{metadata::parse_version_metadata, rules::Platform};
    use std::{collections::BTreeMap, path::PathBuf};

    fn version(extra: serde_json::Value) -> crate::VersionMetadata {
        let mut json = serde_json::json!({
            "id": "base", "mainClass": "example.Main", "type": "release",
            "downloads":{"client":{
                "url":"https://example.com/client.jar",
                "sha1":"1111111111111111111111111111111111111111", "size":123
            }},
            "assetIndex":{
                "id":"test", "url":"https://example.com/index.json",
                "sha1":"2222222222222222222222222222222222222222", "size":456
            },
            "libraries":[]
        });
        let serde_json::Value::Object(extra) = extra else {
            panic!("test metadata patch must be an object");
        };
        for (key, value) in extra {
            json[&key] = value;
        }
        parse_version_metadata(&serde_json::to_vec(&json).expect("test JSON"))
            .expect("valid test version")
    }

    fn windows() -> Platform {
        Platform {
            os: "windows".to_owned(),
            arch: "amd64".to_owned(),
            version: "10.0.19045".to_owned(),
            features: BTreeMap::new(),
        }
    }

    #[test]
    fn rejects_windows_path_traversal_and_aliases() {
        for value in [
            "",
            "/root",
            "../foo",
            "foo/../bar",
            "foo//bar",
            "./foo",
            "C:/x",
            "foo\\bar",
            "foo/.",
            "foo/",
            "NUL",
            "aux.txt",
            "COM1.jar",
            "COM1 .jar",
            "LPT²",
            "a/b.",
            "a/b ",
            "file:stream",
            "a/\nname",
            "a?b",
        ] {
            assert!(safe_relative_path(value).is_err(), "accepted {value:?}");
        }
        assert!(safe_relative_path("org/example/1.0/example-1.0.jar").is_ok());
        assert!(safe_relative_path("textures/方块.png").is_ok());
    }

    #[test]
    fn inheritance_overrides_versions_but_keeps_native_classifier_and_jar() {
        let parent = version(serde_json::json!({
            "arguments":{"jvm":["parent"],"game":["--parent"]},
            "libraries":[{"name":"org.example:library:1"},
                         {"name":"org.example:library:1:natives-windows"}]
        }));
        let child = version(serde_json::json!({
            "id":"fabric", "inheritsFrom":"base", "downloads":{},
            "arguments":{"jvm":["child"],"game":["--child"]},
            "libraries":[{"name":"org.example:library:2"}]
        }));
        let merged = merge(&parent, &child).expect("valid inheritance");
        assert_eq!(merged.jar.as_deref(), Some("base"));
        assert_eq!(merged.libraries.len(), 2);
        assert_eq!(merged.libraries[0].name, "org.example:library:2");
        assert_eq!(
            merged.libraries[1].name,
            "org.example:library:1:natives-windows"
        );
        let args = merged.arguments.expect("merged arguments");
        assert_eq!(args.jvm.len(), 2);
        assert_eq!(args.game.len(), 2);
    }

    #[test]
    fn inheritance_converts_legacy_arguments_when_child_uses_modern_arguments() {
        let parent = version(serde_json::json!({
            "minecraftArguments":"--username ${auth_player_name} --label 'two words'"
        }));
        let child = version(serde_json::json!({
            "id":"child", "downloads":{}, "arguments":{"game":["--child"]}
        }));
        let merged = merge(&parent, &child).expect("valid legacy inheritance");
        assert!(merged.minecraft_arguments.is_none());
        assert_eq!(merged.arguments.expect("arguments").game.len(), 5);
    }

    #[test]
    fn inheritance_detects_cycles_and_missing_parents() {
        let first = version(serde_json::json!({"id":"first", "inheritsFrom":"second"}));
        let second = version(serde_json::json!({"id":"second", "inheritsFrom":"first"}));
        let map = BTreeMap::from([("first".to_owned(), first), ("second".to_owned(), second)]);
        assert!(resolve_inheritance("first", &map).is_err());
        assert!(resolve_inheritance("missing", &map).is_err());
    }

    #[test]
    fn plan_filters_platforms_deduplicates_objects_and_preserves_classpath_order() {
        let version = version(serde_json::json!({
            "libraries":[
                {"name":"org.example:common:1", "downloads":{"artifact":{
                    "path":"org/example/common/1/common-1.jar", "url":"https://example.com/common.jar",
                    "sha1":"3333333333333333333333333333333333333333", "size":1
                }}},
                {"name":"org.example:excluded:1", "rules":[{"action":"disallow","os":{"name":"windows"}}]},
                {"name":"org.example:native:1", "natives":{"windows":"natives-windows-${arch}"},
                 "downloads":{"classifiers":{"natives-windows-64":{
                    "path":"org/example/native/1/native-1-natives-windows-64.jar",
                    "url":"https://example.com/native.jar",
                    "sha1":"4444444444444444444444444444444444444444", "size":2
                 }}}, "extract":{"exclude":["META-INF/"]}}
            ]
        }));
        let assets: AssetIndex = serde_json::from_value(serde_json::json!({
            "virtual":true,
            "objects":{
                "sound/a.ogg":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":3},
                "sound/b.ogg":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":3}
            }
        }))
        .expect("asset index");
        let plan = resolve(&version, &assets, &windows()).expect("valid plan");
        assert_eq!(plan.files.len(), 5);
        assert_eq!(plan.classpath.len(), 2);
        assert_eq!(
            plan.classpath.last(),
            Some(&PathBuf::from("versions/base/base.jar"))
        );
        assert_eq!(plan.natives.len(), 1);
        assert_eq!(plan.natives[0].excludes, ["META-INF/"]);
        assert_eq!(plan.game_assets_path, PathBuf::from("assets/virtual/test"));
        assert!(!plan.game_assets_in_resources);
    }

    #[test]
    fn pre_sixteen_assets_use_instance_resources_for_legacy_argument() {
        let assets: AssetIndex = serde_json::from_value(serde_json::json!({
            "map_to_resources": true,
            "objects": {}
        }))
        .expect("legacy asset index");
        let plan = resolve(&version(serde_json::json!({})), &assets, &windows())
            .expect("valid legacy plan");
        assert!(plan.game_assets_in_resources);
    }

    #[test]
    fn plan_rejects_missing_hashes_unsafe_assets_and_destination_collisions() {
        let unsafe_assets: AssetIndex = serde_json::from_value(serde_json::json!({
            "objects":{"../escape":{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","size":3}}
        }))
        .expect("asset index");
        assert!(resolve(&version(serde_json::json!({})), &unsafe_assets, &windows()).is_err());
        let missing_hash = version(serde_json::json!({
            "libraries":[{"name":"org.example:library:1","downloads":{"artifact":{
                "path":"library.jar","url":"https://example.com/library.jar","size":1
            }}}]
        }));
        assert!(resolve(&missing_hash, &AssetIndex::default(), &windows()).is_err());
        let collision = version(serde_json::json!({
            "libraries":[
                {"name":"org.example:a:1","downloads":{"artifact":{
                    "path":"library.jar","url":"https://example.com/a.jar","size":1,
                    "sha1":"3333333333333333333333333333333333333333"
                }}},
                {"name":"org.example:b:1","downloads":{"artifact":{
                    "path":"LIBRARY.jar","url":"https://example.com/b.jar","size":1,
                    "sha1":"4444444444444444444444444444444444444444"
                }}}
            ]
        }));
        assert!(resolve(&collision, &AssetIndex::default(), &windows()).is_err());
    }

    #[test]
    fn loader_maven_fallback_encodes_path_components() {
        let version = version(serde_json::json!({
            "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.10",
                          "url":"https://maven.fabricmc.net/"}]
        }));
        let plan = resolve(&version, &AssetIndex::default(), &windows()).expect("loader plan");
        let library = &plan.files[2];
        assert_eq!(
            library.artifact.url.as_str(),
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.16.10/fabric-loader-0.16.10.jar"
        );
        assert!(library.artifact.sha1.is_none());
    }
}
