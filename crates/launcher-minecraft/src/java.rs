//! Java discovery and bounded, asynchronous runtime inspection.

use std::{
    collections::BTreeSet,
    env, io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{io::AsyncReadExt, process::Command};

use crate::{Error, VersionMetadata};

const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_INSPECTION_OUTPUT: u64 = 64 * 1024;

/// An inspected Java executable. Discovery never installs or modifies Java.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JavaRuntime {
    /// Resolved executable path.
    pub executable: PathBuf,
    /// Java feature version, such as 8, 17, or 21.
    pub major: u32,
    /// Version string reported by the runtime.
    pub version: String,
    /// Runtime architecture reported by `os.arch`, when available.
    pub architecture: Option<String>,
}

/// Returns the version's Java requirement, using Java 8 for legacy metadata.
#[must_use]
pub fn required_java_major(metadata: &VersionMetadata) -> u32 {
    metadata
        .java_version
        .as_ref()
        .map_or(8, |version| version.major_version)
}

/// Parses Java's historical `1.8.0_...` and modern `21.0.6+...` formats.
#[must_use]
pub fn parse_java_major(version: &str) -> Option<u32> {
    let version = version.trim().trim_matches('"');
    let feature = version.strip_prefix("1.").unwrap_or(version);
    let digits: String = feature.chars().take_while(char::is_ascii_digit).collect();
    let major = digits.parse::<u32>().ok()?;
    (major > 0).then_some(major)
}

async fn bounded_output(reader: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>, io::Error> {
    let mut output = Vec::new();
    reader
        .take(MAX_INSPECTION_OUTPUT + 1)
        .read_to_end(&mut output)
        .await?;
    if output.len() as u64 > MAX_INSPECTION_OUTPUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Java inspection output exceeded its limit",
        ));
    }
    Ok(output)
}

/// Executes `java -XshowSettings:properties -version` with a five-second timeout.
///
/// Standard input is closed, inherited Java injection variables are removed,
/// and each output stream is limited to 64 KiB. No shell is involved.
///
/// # Errors
///
/// Returns a contextual inspection error for process failures, excessive output,
/// or a timeout, or an invalid-version error for unusable output.
pub async fn inspect_java(path: &Path) -> Result<JavaRuntime, Error> {
    let executable = std::fs::canonicalize(path).map_err(|source| Error::JavaInspection {
        path: path.to_path_buf(),
        source,
    })?;
    let mut command = Command::new(&executable);
    command
        .args(["-XshowSettings:properties", "-version"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in ["JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS"] {
        command.env_remove(name);
    }
    let mut child = crate::process::spawn_with_busy_retry(&mut command).map_err(|source| {
        Error::JavaInspection {
            path: executable.clone(),
            source,
        }
    })?;
    let stdout = child.stdout.take().ok_or_else(|| Error::JavaInspection {
        path: executable.clone(),
        source: io::Error::other("Java stdout pipe is unavailable"),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| Error::JavaInspection {
        path: executable.clone(),
        source: io::Error::other("Java stderr pipe is unavailable"),
    })?;
    let result = tokio::time::timeout(INSPECTION_TIMEOUT, async {
        tokio::try_join!(bounded_output(stdout), bounded_output(stderr), child.wait())
    })
    .await;
    let (stdout, stderr, status) = match result {
        Ok(Ok(output)) => output,
        Ok(Err(source)) => {
            let _ = child.kill().await;
            return Err(Error::JavaInspection {
                path: executable,
                source,
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            return Err(Error::JavaInspection {
                path: executable,
                source: io::Error::new(io::ErrorKind::TimedOut, "Java inspection timed out"),
            });
        }
    };
    if !status.success() {
        return Err(Error::JavaInspection {
            path: executable,
            source: io::Error::other(format!("Java inspection exited with {status}")),
        });
    }
    let output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let version = output
        .lines()
        .find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name.trim() == "java.version").then(|| value.trim().to_owned())
        })
        .or_else(|| {
            output.lines().find_map(|line| {
                let line = line.trim();
                if line.starts_with("java version ") || line.starts_with("openjdk version ") {
                    return line.split('"').nth(1).map(str::to_owned);
                }
                None
            })
        })
        .ok_or_else(|| Error::InvalidJavaVersion {
            path: executable.clone(),
        })?;
    let major = parse_java_major(&version).ok_or_else(|| Error::InvalidJavaVersion {
        path: executable.clone(),
    })?;
    let architecture = output.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        (name.trim() == "os.arch").then(|| value.trim().to_owned())
    });
    Ok(JavaRuntime {
        executable,
        major,
        version,
        architecture,
    })
}

fn java_name() -> &'static str {
    if cfg!(windows) { "java.exe" } else { "java" }
}

fn add_home(candidates: &mut Vec<PathBuf>, home: &Path) {
    candidates.push(home.join("bin").join(java_name()));
    if cfg!(target_os = "macos") {
        candidates.push(home.join("Contents/Home/bin").join(java_name()));
    }
}

fn add_installation_root(candidates: &mut Vec<PathBuf>, root: &Path) {
    add_home(candidates, root);
    // Limit broad OS directories: discovery must remain bounded even if a user
    // accidentally points a Java root at a directory containing many entries.
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.take(256).flatten() {
            add_home(candidates, &entry.path());
        }
    }
}

fn discovery_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = env::var_os("JAVA_HOME") {
        add_home(&mut candidates, Path::new(&home));
    }
    if let Some(path) = env::var_os("PATH") {
        candidates.extend(env::split_paths(&path).map(|entry| entry.join(java_name())));
    }
    if cfg!(windows) {
        for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(root) = env::var_os(variable) {
                for vendor in ["Java", "Eclipse Adoptium", "Microsoft", "Azul Systems"] {
                    add_installation_root(&mut candidates, &Path::new(&root).join(vendor));
                }
            }
        }
    } else if cfg!(target_os = "macos") {
        add_installation_root(
            &mut candidates,
            Path::new("/Library/Java/JavaVirtualMachines"),
        );
        if let Some(home) = env::var_os("HOME") {
            add_installation_root(
                &mut candidates,
                &Path::new(&home).join("Library/Java/JavaVirtualMachines"),
            );
        }
        for root in ["/opt/homebrew/opt/openjdk", "/usr/local/opt/openjdk"] {
            add_home(&mut candidates, Path::new(root));
        }
    } else {
        for root in ["/usr/lib/jvm", "/usr/java", "/opt/java", "/opt/jdk"] {
            add_installation_root(&mut candidates, Path::new(root));
        }
    }
    candidates
}

fn matches_host_architecture(runtime: &JavaRuntime) -> bool {
    let Some(architecture) = runtime.architecture.as_deref() else {
        // Some vendor runtimes omit settings output. Discovery still accepts
        // their version report; callers can make a stricter policy decision.
        return true;
    };
    match env::consts::ARCH {
        "x86_64" => matches!(architecture, "amd64" | "x86_64"),
        "x86" => matches!(architecture, "x86" | "i386" | "i486" | "i586" | "i686"),
        "aarch64" => matches!(architecture, "aarch64" | "arm64"),
        host => architecture == host,
    }
}

/// Finds an exact-major runtime compatible with the host's native libraries,
/// preferring an explicit path, then `JAVA_HOME`, `PATH`, and conventional
/// operating-system installation directories.
///
/// An explicit executable is authoritative: failure or a version mismatch is
/// reported instead of silently selecting another runtime.
///
/// # Errors
///
/// Returns an inspection error for an invalid explicit executable, or
/// [`Error::JavaNotFound`] when no exact-major runtime is available.
pub async fn discover_java(
    required_major: u32,
    explicit: Option<&Path>,
) -> Result<JavaRuntime, Error> {
    if required_major == 0 {
        return Err(Error::InvalidMetadata(
            "required Java major version must be positive".to_owned(),
        ));
    }
    if let Some(path) = explicit {
        let runtime = inspect_java(path).await?;
        if runtime.major != required_major {
            return Err(Error::InvalidMetadata(format!(
                "Java executable `{}` reports Java {}, but this version requires Java {required_major}",
                runtime.executable.display(),
                runtime.major
            )));
        }
        if !matches_host_architecture(&runtime) {
            return Err(Error::InvalidMetadata(format!(
                "Java executable `{}` has architecture `{}`, incompatible with host `{}` native libraries",
                runtime.executable.display(),
                runtime.architecture.as_deref().unwrap_or("unknown"),
                env::consts::ARCH
            )));
        }
        return Ok(runtime);
    }
    let mut inspected = BTreeSet::new();
    for candidate in discovery_candidates() {
        let Ok(path) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !inspected.insert(path.clone()) {
            continue;
        }
        match inspect_java(&path).await {
            Ok(runtime)
                if runtime.major == required_major && matches_host_architecture(&runtime) =>
            {
                return Ok(runtime);
            }
            Ok(runtime) => tracing::debug!(
                executable = %path.display(),
                major = runtime.major,
                architecture = ?runtime.architecture,
                required_major,
                "skipping incompatible Java runtime"
            ),
            Err(error) => tracing::debug!(
                executable = %path.display(),
                %error,
                "skipping unusable Java runtime"
            ),
        }
    }
    Err(Error::JavaNotFound { required_major })
}

#[cfg(test)]
mod tests {
    use super::parse_java_major;

    #[test]
    fn parses_legacy_and_modern_feature_versions() {
        for (version, expected) in [
            ("1.8.0_442", Some(8)),
            ("17.0.14", Some(17)),
            ("21.0.6+7-LTS", Some(21)),
            ("25-ea", Some(25)),
            ("\"21.0.1\"", Some(21)),
            ("", None),
            ("not-java", None),
            ("0.1", None),
        ] {
            assert_eq!(parse_java_major(version), expected, "{version}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inspects_script_path_with_spaces_and_rejects_wrong_major() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("test java");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '    java.version = 21.0.6\\n    os.arch = amd64\\n' >&2\n",
        )
        .expect("write fixture");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let runtime = super::inspect_java(&executable).await.expect("inspection");
        assert_eq!(runtime.major, 21);
        assert_eq!(runtime.architecture.as_deref(), Some("amd64"));
        assert!(super::discover_java(17, Some(&executable)).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_explicit_runtime_for_another_architecture() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("java");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '    java.version = 21.0.6\\n    os.arch = incompatible-test-arch\\n' >&2\n",
        )
        .expect("write fixture");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let error = super::discover_java(21, Some(&executable))
            .await
            .expect_err("foreign architecture must be rejected");
        assert!(
            matches!(&error, crate::Error::InvalidMetadata(message) if message.contains("incompatible with host")),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_nonzero_inspection_status() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("java");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf 'openjdk version \"21\"\\n' >&2\nexit 1\n",
        )
        .expect("write fixture");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        assert!(super::inspect_java(&executable).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_excessive_output_without_unbounded_buffering() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("java");
        std::fs::write(
            &executable,
            "#!/bin/sh\nwhile :; do printf '0123456789012345678901234567890123456789012345678901234567890123'; done\n",
        )
        .expect("write fixture");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable permissions");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            super::inspect_java(&executable),
        )
        .await
        .expect("output limit should terminate promptly");
        assert!(matches!(result, Err(crate::Error::JavaInspection { .. })));
    }
}
