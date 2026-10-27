//! Versioned launcher configuration and crash-safe persistence.

use std::{
    fs,
    io::{self, Write as _},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

use crate::{Error, Result};

/// Latest configuration schema understood by this build.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// User-editable launcher settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LauncherConfig {
    /// On-disk schema version.
    pub schema_version: u32,
    /// Download-related preferences.
    pub downloads: DownloadSettings,
    /// Game launch preferences.
    pub game: GameSettings,
    /// Public Microsoft application and selected account preferences (no tokens).
    pub auth: AuthSettings,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            downloads: DownloadSettings::default(),
            game: GameSettings::default(),
            auth: AuthSettings::default(),
        }
    }
}

/// Download preferences shared with the download engine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadSettings {
    /// Maximum number of simultaneous downloads.
    pub max_concurrent_downloads: NonZeroUsize,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: NonZeroUsize::new(8)
                .expect("the constant default concurrency is non-zero"),
        }
    }
}

/// Game process preferences.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameSettings {
    /// Explicit Java executable, or automatic compatible runtime discovery.
    pub java_executable: Option<PathBuf>,
    /// Initial heap allocation in MiB.
    pub min_memory_mib: u32,
    /// Maximum heap allocation in MiB.
    pub max_memory_mib: u32,
    /// Whether a successful installation should immediately launch the game.
    pub auto_launch_after_install: bool,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            java_executable: None,
            min_memory_mib: 512,
            max_memory_mib: 4096,
            auto_launch_after_install: false,
        }
    }
}

/// Non-secret account preferences. Refresh tokens belong exclusively in the OS vault.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthSettings {
    /// Nudge's registered public-client application ID; never a client secret.
    pub microsoft_client_id: Option<String>,
    /// Selected Minecraft profile UUID, if an account has been authorized.
    pub selected_account: Option<String>,
}

impl LauncherConfig {
    /// Checks settings before they cross into network/process boundaries.
    ///
    /// # Errors
    /// Returns an error for unsupported schemas, invalid concurrency or heap bounds.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(Error::UnsupportedConfigSchema {
                found: self.schema_version,
                supported: CONFIG_SCHEMA_VERSION,
            });
        }
        if self.downloads.max_concurrent_downloads.get() > 64 {
            return Err(Error::InvalidInput(
                "download concurrency must be between 1 and 64".into(),
            ));
        }
        if self.game.min_memory_mib < 128
            || self.game.min_memory_mib > self.game.max_memory_mib
            || self.game.max_memory_mib > 1_048_576
        {
            return Err(Error::InvalidInput(
                "heap sizes must satisfy 128 <= minimum <= maximum <= 1048576 MiB".into(),
            ));
        }
        if self
            .game
            .java_executable
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(Error::InvalidInput(
                "an explicit Java executable cannot be empty".into(),
            ));
        }
        for value in [&self.auth.microsoft_client_id, &self.auth.selected_account]
            .into_iter()
            .flatten()
        {
            if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return Err(Error::InvalidInput("account identifiers must be nonempty and at most 128 characters without control characters".into()));
            }
        }
        Ok(())
    }
}

/// Platform-specific filesystem locations used by Nudge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    /// JSON configuration file.
    pub config_file: PathBuf,
    /// Persistent application data directory.
    pub data_dir: PathBuf,
    /// Reusable download cache directory.
    pub cache_dir: PathBuf,
}

impl AppPaths {
    /// Discovers native application directories for the current platform.
    ///
    /// # Errors
    ///
    /// Returns an error when the platform does not expose a suitable user
    /// configuration directory.
    pub fn discover() -> Result<Self> {
        let dirs =
            ProjectDirs::from("dev", "Nudge", "Nudge").ok_or(Error::ConfigDirectoryUnavailable)?;
        Ok(Self {
            config_file: dirs.config_dir().join("config.json"),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }
}

/// Reads and writes one versioned JSON configuration file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// Creates a store at an explicit path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the backing file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the current configuration, creating defaults only when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or parsed, uses an
    /// unsupported schema, or default configuration cannot be persisted.
    pub fn load_or_create(&self) -> Result<LauncherConfig> {
        match fs::read(&self.path) {
            Ok(bytes) => self.parse(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let config = LauncherConfig::default();
                self.save(&config)?;
                Ok(config)
            }
            Err(source) => Err(Error::ReadConfig {
                path: self.path.clone(),
                source,
            }),
        }
    }

    /// Persists configuration through a flushed temporary file in the same directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema is unsupported or the destination
    /// directory, temporary write, flush, or atomic persistence fails.
    pub fn save(&self, config: &LauncherConfig) -> Result<()> {
        config.validate()?;

        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        launcher_net::persistence::check_path(&self.path).map_err(|source| {
            Error::PersistConfig {
                path: self.path.clone(),
                source,
            }
        })?;
        launcher_net::persistence::create_dir_all(parent).map_err(|source| {
            Error::CreateDirectory {
                path: parent.to_path_buf(),
                source,
            }
        })?;

        let mut temporary = NamedTempFile::new_in(parent).map_err(|source| Error::WriteConfig {
            directory: parent.to_path_buf(),
            source,
        })?;
        serde_json::to_writer_pretty(temporary.as_file_mut(), config)
            .map_err(Error::SerializeConfig)?;
        temporary
            .write_all(b"\n")
            .and_then(|()| temporary.as_file_mut().sync_all())
            .map_err(|source| Error::WriteConfig {
                directory: parent.to_path_buf(),
                source,
            })?;
        launcher_net::persistence::persist(temporary, &self.path).map_err(|source| {
            Error::PersistConfig {
                path: self.path.clone(),
                source,
            }
        })?;

        Ok(())
    }

    fn parse(&self, bytes: &[u8]) -> Result<LauncherConfig> {
        let config: LauncherConfig =
            serde_json::from_slice(bytes).map_err(|source| Error::ParseConfig {
                path: self.path.clone(),
                source,
            })?;
        config.validate()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{ConfigStore, LauncherConfig};

    #[test]
    fn creates_and_reloads_default_config() {
        let directory = tempdir().expect("temporary directory should exist");
        let store = ConfigStore::new(directory.path().join("nested/config.json"));

        let created = store.load_or_create().expect("defaults should be created");
        let loaded = store.load_or_create().expect("saved config should load");

        assert_eq!(created, LauncherConfig::default());
        assert_eq!(loaded, created);
    }

    #[test]
    fn saves_updated_config_over_existing_file() {
        let directory = tempdir().expect("temporary directory should exist");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let mut config = LauncherConfig::default();
        store.save(&config).expect("initial save should succeed");

        config.game.max_memory_mib = 8192;
        store
            .save(&config)
            .expect("replacement save should succeed");

        assert_eq!(
            store
                .load_or_create()
                .expect("updated config should load")
                .game
                .max_memory_mib,
            8192
        );
    }

    #[test]
    fn malformed_config_is_not_silently_replaced() {
        let directory = tempdir().expect("temporary directory should exist");
        let path = directory.path().join("config.json");
        std::fs::write(&path, b"not-json").expect("fixture should be written");
        let store = ConfigStore::new(path.clone());

        store
            .load_or_create()
            .expect_err("malformed config must be reported");
        assert_eq!(
            std::fs::read(&path).expect("fixture should remain"),
            b"not-json"
        );
    }

    #[test]
    fn existing_schema_one_gains_safe_new_defaults() {
        let directory = tempdir().expect("temporary directory");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let old = br#"{"schema_version":1,"game":{"max_memory_mib":8192}}"#;
        let loaded = store.parse(old).expect("backwards-compatible schema");
        assert_eq!(loaded.game.min_memory_mib, 512);
        assert_eq!(loaded.game.max_memory_mib, 8192);
        assert_eq!(loaded.game.java_executable, None);
        assert_eq!(loaded.auth.selected_account, None);
    }

    #[test]
    fn invalid_settings_do_not_replace_existing_configuration() {
        let directory = tempdir().expect("temporary directory");
        let store = ConfigStore::new(directory.path().join("config.json"));
        let original = store.load_or_create().expect("initial config");
        let mut invalid = original.clone();
        invalid.game.min_memory_mib = original.game.max_memory_mib + 1;
        store.save(&invalid).expect_err("invalid heap bounds");
        assert_eq!(store.load_or_create().expect("preserved config"), original);
        invalid = original.clone();
        invalid.downloads.max_concurrent_downloads =
            std::num::NonZeroUsize::new(65).expect("nonzero");
        store.save(&invalid).expect_err("bounded concurrency");
        assert_eq!(store.load_or_create().expect("preserved config"), original);
    }
}
