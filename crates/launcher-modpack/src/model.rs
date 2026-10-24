//! Strict MRPACK metadata and frontend-independent installation plans.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use launcher_net::transfer::{FileSpec, Hashes};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Client/server applicability from the MRPACK specification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Applicability {
    /// Required for this environment.
    #[default]
    Required,
    /// May be chosen by the user.
    Optional,
    /// Must not be installed for this environment.
    Unsupported,
}

/// Per-environment applicability; omitted sides default to required.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Environment {
    /// Applicability to a Java client.
    #[serde(default)]
    pub client: Applicability,
    /// Applicability to a dedicated server.
    #[serde(default)]
    pub server: Applicability,
}

/// One strictly typed MRPACK file entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexFile {
    /// Destination relative to the isolated game directory.
    pub path: String,
    /// SHA-1 and SHA-512 are both mandatory and verified.
    pub hashes: Hashes,
    /// One or more HTTPS download locations.
    pub downloads: Vec<String>,
    /// Expected byte length.
    pub file_size: u64,
    /// Missing environment data means client-required.
    #[serde(default)]
    pub env: Environment,
}

/// The root `modrinth.index.json` document.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModrinthIndex {
    /// Supported format version is exactly one.
    pub format_version: u32,
    /// Supported game is Minecraft.
    pub game: String,
    /// Pack release ID.
    pub version_id: String,
    /// Display name.
    pub name: String,
    /// Optional description.
    #[serde(default)]
    pub summary: Option<String>,
    /// Download requirements.
    pub files: Vec<IndexFile>,
    /// Exact game, loader and possible future dependency versions.
    pub dependencies: BTreeMap<String, String>,
}

/// Recognized mod loader families, including future executor extensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoaderKind {
    /// Fabric Loader.
    Fabric,
    /// Quilt Loader (recognized, installation is not implemented).
    Quilt,
    /// `NeoForge` (recognized, installation is not implemented).
    NeoForge,
    /// Forge (recognized, installation is not implemented).
    Forge,
}

/// Exact mod loader requirement.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Loader {
    /// Loader family.
    pub kind: LoaderKind,
    /// Exact published version.
    pub version: String,
}

/// A client file that can be handed to the common download engine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedFile {
    /// Portable game-relative path.
    pub path: PathBuf,
    /// First download source and required integrity information.
    pub spec: FileSpec,
    /// Backup sources for identical expected content.
    pub mirrors: Vec<url::Url>,
}

/// One final override after resolving layer precedence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OverrideFile {
    /// Original archive index.
    pub archive_index: usize,
    /// Original UTF-8 ZIP entry name.
    pub archive_name: String,
    /// Game-relative output path.
    pub path: PathBuf,
    /// Actual expanded length verified while inspecting.
    pub size: u64,
    /// Digests calculated while CRC-validating the original archive entry.
    pub hashes: Hashes,
}

/// Coarse installation steps with explicit dependencies.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskKind {
    /// Resolve and install Minecraft plus the selected loader.
    Runtime,
    /// Download the selected client pack files.
    PackFiles,
    /// Apply the winning override layers.
    Overrides,
    /// Verify the complete final installation.
    Verify,
    /// Mark the isolated instance ready.
    Commit,
}

/// One node in the install graph.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallTask {
    /// Work performed by this node.
    pub kind: TaskKind,
    /// Nodes that must finish first.
    pub depends_on: Vec<TaskKind>,
}

/// Inspection result consumed by frontends and the core executor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallPlan {
    /// Plan schema version.
    pub schema_version: u32,
    /// Source archive SHA-256, binding execution to inspected bytes.
    pub archive_sha256: String,
    /// User-selected destination, never derived from a pack's display name.
    pub target_directory: PathBuf,
    /// Pack display name.
    pub name: String,
    /// Pack version.
    pub version: String,
    /// Requested Minecraft version.
    pub minecraft_version: String,
    /// Optional mod loader.
    pub loader: Option<Loader>,
    /// Unknown dependency keys retained for explicit executor rejection.
    pub unsupported_dependencies: BTreeMap<String, String>,
    /// Files required on clients.
    pub required_files: Vec<PlannedFile>,
    /// Optional files requiring user selection.
    pub optional_files: Vec<PlannedFile>,
    /// Dedicated-server-only entries, reported but never installed in clients.
    pub excluded_files: Vec<PathBuf>,
    /// Final winning overrides, independent of ZIP entry order.
    pub overrides: Vec<OverrideFile>,
    /// Executable dependency graph.
    pub tasks: Vec<InstallTask>,
}

impl InstallPlan {
    /// Returns a deterministic topological order, rejecting incomplete/cyclic graphs.
    ///
    /// # Errors
    /// Rejects duplicate tasks, missing dependencies or cycles.
    pub fn ordered_tasks(&self) -> Result<Vec<TaskKind>, Error> {
        let mut done = BTreeSet::new();
        let mut ordered = Vec::new();
        if self
            .tasks
            .iter()
            .map(|task| task.kind)
            .collect::<BTreeSet<_>>()
            .len()
            != self.tasks.len()
        {
            return Err(Error::Invalid("duplicate installation task".to_owned()));
        }
        while ordered.len() < self.tasks.len() {
            let task = self
                .tasks
                .iter()
                .find(|task| {
                    !done.contains(&task.kind)
                        && task
                            .depends_on
                            .iter()
                            .all(|dependency| done.contains(dependency))
                })
                .ok_or_else(|| Error::Invalid("missing or cyclic task dependencies".to_owned()))?;
            ordered.push(task.kind);
            done.insert(task.kind);
        }
        Ok(ordered)
    }
}

pub(crate) fn standard_tasks() -> Vec<InstallTask> {
    let stages = [
        TaskKind::Runtime,
        TaskKind::PackFiles,
        TaskKind::Overrides,
        TaskKind::Verify,
        TaskKind::Commit,
    ];
    stages
        .iter()
        .enumerate()
        .map(|(index, kind)| InstallTask {
            kind: *kind,
            depends_on: index
                .checked_sub(1)
                .map_or_else(Vec::new, |previous| vec![stages[previous]]),
        })
        .collect()
}
