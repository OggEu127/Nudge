//! Mojang version metadata models.

use std::collections::BTreeMap;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{Error, rules::Rule};

/// A downloadable file described by Mojang or a loader profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadArtifact {
    /// File identifier supplied by logging metadata.
    #[serde(default)]
    pub id: Option<String>,
    /// Relative destination path when supplied by the metadata.
    #[serde(default)]
    pub path: Option<String>,
    /// Expected SHA-1 digest.
    #[serde(default)]
    pub sha1: Option<String>,
    /// Expected byte length.
    #[serde(default)]
    pub size: Option<u64>,
    /// Download URL.
    pub url: Url,
}

/// Named client/server downloads from a version document.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionDownloads {
    /// Minecraft client JAR.
    #[serde(default)]
    pub client: Option<DownloadArtifact>,
    /// Client mappings, when published.
    #[serde(default)]
    pub client_mappings: Option<DownloadArtifact>,
    /// Dedicated server JAR.
    #[serde(default)]
    pub server: Option<DownloadArtifact>,
    /// Server mappings, when published.
    #[serde(default)]
    pub server_mappings: Option<DownloadArtifact>,
    /// Forward-compatible additional download roles.
    #[serde(flatten)]
    pub additional: BTreeMap<String, DownloadArtifact>,
}

/// Asset-index descriptor referenced by a version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexDescriptor {
    /// Asset index ID used in launch arguments.
    pub id: String,
    /// Expected index SHA-1.
    pub sha1: String,
    /// Expected index length.
    pub size: u64,
    /// Total size of referenced objects when supplied.
    #[serde(default)]
    pub total_size: Option<u64>,
    /// Asset-index URL.
    pub url: Url,
}

/// Java runtime requested by Mojang metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    /// Mojang runtime component name.
    pub component: String,
    /// Required Java major version.
    pub major_version: u32,
}

/// One literal or conditional command-line argument.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    /// Argument applied unconditionally.
    Literal(String),
    /// Argument guarded by launcher rules.
    Conditional(ConditionalArgument),
}

/// One or more values in a conditional argument.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    /// A single argument.
    One(String),
    /// Multiple adjacent arguments.
    Many(Vec<String>),
}

impl ArgumentValue {
    /// Iterates over contained values.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::One(value) => std::slice::from_ref(value).iter().map(String::as_str),
            Self::Many(values) => values.iter().map(String::as_str),
        }
    }
}

/// A rule-guarded argument object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConditionalArgument {
    /// Rules controlling whether the values apply. Empty means unconditional.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// Argument values.
    pub value: ArgumentValue,
}

/// Modern launcher arguments grouped by purpose.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VersionArguments {
    /// Required JVM arguments.
    #[serde(default)]
    pub jvm: Vec<Argument>,
    /// Default user JVM suggestions present in recent metadata.
    #[serde(default)]
    pub default_user_jvm: Vec<Argument>,
    /// Game arguments.
    #[serde(default)]
    pub game: Vec<Argument>,
}

/// Library download variants.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LibraryDownloads {
    /// Cross-platform classpath artifact.
    #[serde(default)]
    pub artifact: Option<DownloadArtifact>,
    /// Native classifiers keyed by classifier name.
    #[serde(default)]
    pub classifiers: BTreeMap<String, DownloadArtifact>,
}

/// Native extraction exclusions.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtractRules {
    /// Archive path prefixes that must not be extracted.
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// One Maven library entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Library {
    /// Maven coordinate.
    pub name: String,
    /// Resolved Mojang downloads.
    #[serde(default)]
    pub downloads: LibraryDownloads,
    /// Optional Maven repository base URL used by loader profiles.
    #[serde(default)]
    pub url: Option<Url>,
    /// Platform rules.
    #[serde(default)]
    pub rules: Vec<Rule>,
    /// OS name to native classifier mapping.
    #[serde(default)]
    pub natives: BTreeMap<String, String>,
    /// Native extraction exclusions.
    #[serde(default)]
    pub extract: ExtractRules,
}

impl Library {
    /// Returns the Maven group and artifact key used for inheritance overrides.
    #[must_use]
    pub fn conflict_key(&self) -> &str {
        self.name
            .rsplit_once(':')
            .map_or(self.name.as_str(), |(prefix, _)| prefix)
    }
}

/// Client logging configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClientLogging {
    /// JVM argument template containing `${path}`.
    pub argument: String,
    /// Logging configuration file.
    pub file: DownloadArtifact,
    /// Mojang logging configuration kind.
    #[serde(rename = "type")]
    pub kind: String,
}

/// Logging section of version metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LoggingConfiguration {
    /// Client logging configuration.
    #[serde(default)]
    pub client: Option<ClientLogging>,
}

/// A complete or inherited Minecraft version metadata document.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionMetadata {
    /// Version/profile identifier.
    pub id: String,
    /// Parent profile identifier for loader profiles.
    #[serde(default)]
    pub inherits_from: Option<String>,
    /// Explicit client JAR version override.
    #[serde(default)]
    pub jar: Option<String>,
    /// Version category.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Main Java class.
    #[serde(default)]
    pub main_class: Option<String>,
    /// Modern structured arguments.
    #[serde(default)]
    pub arguments: Option<VersionArguments>,
    /// Legacy space-delimited game arguments.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    /// Libraries in classpath order.
    #[serde(default)]
    pub libraries: Vec<Library>,
    /// Named version downloads.
    #[serde(default)]
    pub downloads: VersionDownloads,
    /// Asset index descriptor.
    #[serde(default)]
    pub asset_index: Option<AssetIndexDescriptor>,
    /// Legacy asset name.
    #[serde(default)]
    pub assets: Option<String>,
    /// Required Java runtime.
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    /// Client logging configuration.
    #[serde(default)]
    pub logging: LoggingConfiguration,
    /// Metadata update time.
    #[serde(default)]
    pub time: Option<DateTime<FixedOffset>>,
    /// Version release time.
    #[serde(default)]
    pub release_time: Option<DateTime<FixedOffset>>,
    /// Minimum official launcher version.
    #[serde(default)]
    pub minimum_launcher_version: Option<u32>,
}

impl VersionMetadata {
    fn validate(&self) -> Result<(), Error> {
        if self.id.trim().is_empty() {
            return Err(Error::InvalidMetadata("version ID is empty".to_owned()));
        }
        if self.arguments.is_some() && self.minecraft_arguments.is_some() {
            return Err(Error::InvalidMetadata(format!(
                "version `{}` contains both modern and legacy arguments",
                self.id
            )));
        }
        validate_artifact(self.downloads.client.as_ref(), "client")?;
        if let Some(index) = &self.asset_index {
            validate_sha1(&index.sha1, "asset index")?;
        }
        for library in &self.libraries {
            if library.name.split(':').count() < 3 {
                return Err(Error::InvalidMetadata(format!(
                    "library coordinate `{}` is malformed",
                    library.name
                )));
            }
            validate_artifact(library.downloads.artifact.as_ref(), &library.name)?;
            for artifact in library.downloads.classifiers.values() {
                validate_artifact(Some(artifact), &library.name)?;
            }
        }
        Ok(())
    }
}

fn validate_artifact(artifact: Option<&DownloadArtifact>, label: &str) -> Result<(), Error> {
    if let Some(artifact) = artifact
        && let Some(sha1) = &artifact.sha1
    {
        validate_sha1(sha1, label)?;
    }
    Ok(())
}

fn validate_sha1(sha1: &str, label: &str) -> Result<(), Error> {
    if sha1.len() != 40 || !sha1.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::InvalidMetadata(format!(
            "SHA-1 for `{label}` is malformed"
        )));
    }
    Ok(())
}

/// Parses and validates a version metadata document.
///
/// # Errors
///
/// Returns an error for malformed JSON or inconsistent metadata.
pub fn parse_version_metadata(bytes: &[u8]) -> Result<VersionMetadata, Error> {
    let metadata: VersionMetadata = serde_json::from_slice(bytes)?;
    metadata.validate()?;
    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::parse_version_metadata;

    #[test]
    fn parses_modern_arguments_and_java_requirement() {
        let json = br#"{
          "id":"1.21.5","type":"release","mainClass":"net.minecraft.client.main.Main",
          "arguments":{"jvm":["-cp","${classpath}"],"game":["--username","${auth_player_name}"]},
          "javaVersion":{"component":"java-runtime-delta","majorVersion":21},
          "libraries":[],"downloads":{}
        }"#;

        let metadata = parse_version_metadata(json).expect("fixture should parse");
        assert_eq!(
            metadata.java_version.expect("Java metadata").major_version,
            21
        );
        assert_eq!(metadata.arguments.expect("arguments").game.len(), 2);
    }

    #[test]
    fn parses_legacy_arguments() {
        let json = br#"{
          "id":"1.6.4","mainClass":"net.minecraft.client.main.Main",
          "minecraftArguments":"--username ${auth_player_name}","libraries":[],"downloads":{}
        }"#;

        let metadata = parse_version_metadata(json).expect("fixture should parse");
        assert!(metadata.arguments.is_none());
        assert!(metadata.minecraft_arguments.is_some());
    }
}
