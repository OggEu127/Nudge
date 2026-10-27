//! Pure launch-argument construction and bounded Minecraft process supervision.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
    process::Stdio,
};

use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{
    Error, VersionMetadata,
    metadata::Argument,
    resolver::ResolvedVersion,
    rules::{Platform, allowed},
};

const OUTPUT_QUEUE_CAPACITY: usize = 256;
const OUTPUT_CHUNK_SIZE: usize = 8 * 1024;
// A descendant can inherit a pipe even after Java has exited. Waiting forever
// for that descendant's EOF would prevent cancellation, reaping and lease release.
const OUTPUT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Authentication context supplied by the official account layer.
///
/// This type does not claim to authenticate or verify ownership. Real account
/// construction requires an existing, nonempty Microsoft Minecraft token.
/// [`Self::demo`] is usable only with the explicit official demo option.
/// [`Self::offline`] is a local-only identity and must never be described as
/// Microsoft-authenticated.
#[derive(Clone)]
pub struct LaunchIdentity {
    profile_name: String,
    uuid: Uuid,
    access_token: String,
    xuid: String,
    client_id: String,
    demo: bool,
    offline: bool,
}

impl fmt::Debug for LaunchIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchIdentity")
            .field("profile_name", &self.profile_name)
            .field("uuid", &self.uuid)
            .field("access_token", &"[REDACTED]")
            .field("xuid", &"[REDACTED]")
            .field("client_id", &"[REDACTED]")
            .field("demo", &self.demo)
            .field("offline", &self.offline)
            .finish()
    }
}

impl LaunchIdentity {
    /// Builds a supplied Microsoft account context; it performs no network login.
    ///
    /// # Errors
    ///
    /// Rejects invalid profile names, the nil profile UUID, or an empty token.
    pub fn new(profile_name: String, uuid: Uuid, access_token: String) -> Result<Self, Error> {
        if profile_name.is_empty()
            || profile_name.len() > 16
            || !profile_name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(Error::InvalidMetadata(
                "Minecraft profile name must contain 1–16 ASCII letters, digits, or underscores"
                    .to_owned(),
            ));
        }
        if uuid.is_nil() {
            return Err(Error::InvalidMetadata(
                "Microsoft Minecraft profile UUID must not be nil".to_owned(),
            ));
        }
        if access_token.trim().is_empty() || access_token.chars().any(char::is_control) {
            return Err(Error::InvalidMetadata(
                "Minecraft access token is empty or contains control characters".to_owned(),
            ));
        }
        Ok(Self {
            profile_name,
            uuid,
            access_token,
            xuid: String::new(),
            client_id: String::new(),
            demo: false,
            offline: false,
        })
    }

    /// Creates a local-only offline identity for a deterministic player UUID.
    ///
    /// This does **not** authenticate an account, demonstrate ownership, or
    /// grant online-server access. The caller is responsible for showing the
    /// account as offline in the interface. Launching uses Minecraft's standard
    /// legacy user type and a dummy token.
    ///
    /// # Errors
    ///
    /// Rejects invalid player names or the nil UUID.
    pub fn offline(profile_name: String, uuid: Uuid) -> Result<Self, Error> {
        let mut identity = Self::new(profile_name, uuid, "0".to_owned())?;
        identity.offline = true;
        Ok(identity)
    }

    /// Creates an unauthenticated context restricted to Minecraft's `--demo` mode.
    ///
    /// It does not authenticate an account or grant access to the full game.
    #[must_use]
    pub fn demo() -> Self {
        Self {
            profile_name: "Player".to_owned(),
            uuid: Uuid::nil(),
            access_token: "0".to_owned(),
            xuid: String::new(),
            client_id: String::new(),
            demo: true,
            offline: false,
        }
    }

    /// Adds the Xbox user ID returned by the official account flow.
    ///
    /// # Errors
    ///
    /// Rejects empty or nonnumeric identifiers.
    pub fn with_xuid(mut self, xuid: String) -> Result<Self, Error> {
        if xuid.is_empty() || !xuid.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::InvalidMetadata("Xbox user ID is invalid".to_owned()));
        }
        self.xuid = xuid;
        Ok(self)
    }

    /// Adds the registered Microsoft application ID used to obtain the token.
    ///
    /// # Errors
    ///
    /// Rejects an empty ID or control characters.
    pub fn with_client_id(mut self, client_id: String) -> Result<Self, Error> {
        if client_id.trim().is_empty() || client_id.chars().any(char::is_control) {
            return Err(Error::InvalidMetadata(
                "Microsoft application ID is invalid".to_owned(),
            ));
        }
        self.client_id = client_id;
        Ok(self)
    }
}

/// User-controlled launch settings, independent of account credentials.
#[derive(Clone, Debug)]
pub struct LaunchOptions {
    /// Previously discovered Java executable.
    pub java: PathBuf,
    /// Initial heap in MiB.
    pub min_memory_mib: u32,
    /// Maximum heap in MiB.
    pub max_memory_mib: u32,
    /// Target platform and enabled launcher features.
    pub platform: Platform,
    /// Explicit additional JVM arguments; each string remains one argument.
    pub extra_jvm_args: Vec<String>,
    /// Optional game window width and height.
    pub resolution: Option<(u32, u32)>,
    /// Launch Minecraft's official, restricted demo mode.
    pub demo: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            java: PathBuf::from(if cfg!(windows) { "java.exe" } else { "java" }),
            min_memory_mib: 512,
            max_memory_mib: 2048,
            platform: Platform::host(),
            extra_jvm_args: Vec::new(),
            resolution: None,
            demo: false,
        }
    }
}

/// A shell-free process specification. Debug deliberately hides all arguments.
pub struct LaunchSpec {
    /// Executable path.
    pub executable: PathBuf,
    /// Isolated game working directory.
    pub working_directory: PathBuf,
    arguments: Vec<OsString>,
}

impl fmt::Debug for LaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchSpec")
            .field("executable", &self.executable)
            .field("working_directory", &self.working_directory)
            .field("argument_count", &self.arguments.len())
            .field("arguments", &"[REDACTED]")
            .finish()
    }
}

impl LaunchSpec {
    /// Returns process arguments, including credentials. Do not log this value.
    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
}

fn path_text(path: &Path) -> Result<String, Error> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        Error::InvalidMetadata(format!(
            "Java launch argument path `{}` is not valid Unicode",
            path.display()
        ))
    })
}

fn substitute(value: &str, variables: &BTreeMap<&str, String>) -> Result<String, Error> {
    let mut result = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(offset) = remaining.find("${") {
        result.push_str(&remaining[..offset]);
        let placeholder = &remaining[offset + 2..];
        let end = placeholder
            .find('}')
            .ok_or_else(|| Error::UnresolvedPlaceholder("unterminated placeholder".to_owned()))?;
        let key = &placeholder[..end];
        let replacement = variables
            .get(key)
            .ok_or_else(|| Error::UnresolvedPlaceholder(key.to_owned()))?;
        result.push_str(replacement);
        remaining = &placeholder[end + 1..];
    }
    result.push_str(remaining);
    if result.contains('\0') {
        return Err(Error::InvalidMetadata(
            "launch argument contains a NUL byte".to_owned(),
        ));
    }
    Ok(result)
}

fn resolve_arguments(
    arguments: &[Argument],
    platform: &Platform,
    variables: &BTreeMap<&str, String>,
) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    for argument in arguments {
        match argument {
            Argument::Literal(value) => result.push(substitute(value, variables)?),
            Argument::Conditional(conditional) if allowed(&conditional.rules, platform)? => {
                for value in conditional.value.iter() {
                    result.push(substitute(value, variables)?);
                }
            }
            Argument::Conditional(_) => {}
        }
    }
    Ok(result)
}

/// Builds JVM and game arguments without performing I/O or executing a shell.
///
/// Relative resolver paths are anchored under `root`. Metadata rules select
/// arguments, all referenced placeholders must be known, and explicit memory
/// settings override metadata's suggested heap values.
///
/// # Errors
///
/// Rejects invalid settings, unresolved metadata, unsupported placeholders,
/// unrepresentable classpaths, or a demo identity without explicit demo mode.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn build_launch_spec(
    metadata: &VersionMetadata,
    resolved: &ResolvedVersion,
    root: &Path,
    game_dir: &Path,
    natives_dir: &Path,
    identity: &LaunchIdentity,
    options: &LaunchOptions,
) -> Result<LaunchSpec, Error> {
    if metadata.inherits_from.is_some() {
        return Err(Error::InvalidMetadata(
            "version inheritance must be resolved before launching".to_owned(),
        ));
    }
    if identity.demo && !options.demo {
        return Err(Error::InvalidMetadata(
            "demo identity requires explicit official demo mode".to_owned(),
        ));
    }
    if identity.offline && options.demo {
        return Err(Error::InvalidMetadata(
            "an offline identity cannot be combined with official demo mode".to_owned(),
        ));
    }
    if options.min_memory_mib == 0 || options.max_memory_mib < options.min_memory_mib {
        return Err(Error::InvalidMetadata(
            "heap settings require 0 < minimum <= maximum MiB".to_owned(),
        ));
    }
    if options
        .resolution
        .is_some_and(|(width, height)| width == 0 || height == 0)
    {
        return Err(Error::InvalidMetadata(
            "game window dimensions must be positive".to_owned(),
        ));
    }
    let main_class = metadata
        .main_class
        .as_deref()
        .filter(|value| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._$".contains(&byte))
                && !value.starts_with('.')
        })
        .ok_or_else(|| Error::InvalidMetadata("missing or invalid Java main class".to_owned()))?;
    let mut platform = options.platform.clone();
    platform
        .features
        .insert("is_demo_user".to_owned(), options.demo);
    platform.features.insert(
        "has_custom_resolution".to_owned(),
        options.resolution.is_some(),
    );
    let mut classpath: Vec<PathBuf> = resolved
        .classpath
        .iter()
        .map(|path| root.join(path))
        .collect();
    let client_path = root.join(&resolved.client_path);
    if !classpath.contains(&client_path) {
        classpath.push(client_path);
    }
    let classpath = env::join_paths(classpath).map_err(|_| {
        Error::InvalidMetadata("classpath contains an operating-system path separator".to_owned())
    })?;
    let classpath = classpath
        .into_string()
        .map_err(|_| Error::InvalidMetadata("classpath contains a non-Unicode path".to_owned()))?;
    let uuid = identity.uuid.simple().to_string();
    let mut variables = BTreeMap::from([
        ("auth_player_name", identity.profile_name.clone()),
        ("version_name", metadata.id.clone()),
        ("game_directory", path_text(game_dir)?),
        ("assets_root", path_text(&root.join("assets"))?),
        (
            "game_assets",
            path_text(&if resolved.game_assets_in_resources {
                game_dir.join("resources")
            } else {
                root.join(&resolved.game_assets_path)
            })?,
        ),
        ("assets_index_name", resolved.asset_index_id.clone()),
        ("auth_uuid", uuid.clone()),
        ("auth_access_token", identity.access_token.clone()),
        (
            "auth_session",
            format!("token:{}:{uuid}", identity.access_token),
        ),
        (
            "user_type",
            if identity.demo || identity.offline {
                "legacy"
            } else {
                "msa"
            }
            .to_owned(),
        ),
        ("user_properties", "{}".to_owned()),
        ("auth_xuid", identity.xuid.clone()),
        ("clientid", identity.client_id.clone()),
        (
            "version_type",
            metadata
                .kind
                .clone()
                .unwrap_or_else(|| "release".to_owned()),
        ),
        ("natives_directory", path_text(natives_dir)?),
        ("launcher_name", "Nudge".to_owned()),
        ("launcher_version", env!("CARGO_PKG_VERSION").to_owned()),
        ("classpath", classpath),
        ("library_directory", path_text(&root.join("libraries"))?),
        (
            "classpath_separator",
            if cfg!(windows) { ";" } else { ":" }.to_owned(),
        ),
    ]);
    if let Some((width, height)) = options.resolution {
        variables.insert("resolution_width", width.to_string());
        variables.insert("resolution_height", height.to_string());
    }
    let (mut jvm, mut game) = if let Some(arguments) = &metadata.arguments {
        let mut jvm = resolve_arguments(&arguments.jvm, &platform, &variables)?;
        jvm.extend(
            resolve_arguments(&arguments.default_user_jvm, &platform, &variables)?
                .into_iter()
                .filter(|value| !value.starts_with("-Xmx") && !value.starts_with("-Xms")),
        );
        (
            jvm,
            resolve_arguments(&arguments.game, &platform, &variables)?,
        )
    } else if let Some(legacy) = &metadata.minecraft_arguments {
        // Tokenize the template before interpolation: spaces in a username/path
        // must not introduce extra process arguments.
        let game = shell_words::split(legacy)
            .map_err(|error| {
                Error::InvalidMetadata(format!("invalid legacy game arguments: {error}"))
            })?
            .iter()
            .map(|value| substitute(value, &variables))
            .collect::<Result<_, _>>()?;
        (Vec::new(), game)
    } else {
        return Err(Error::InvalidMetadata(
            "version provides neither modern nor legacy game arguments".to_owned(),
        ));
    };
    if !jvm
        .iter()
        .any(|value| value.starts_with("-Djava.library.path="))
    {
        jvm.push(format!("-Djava.library.path={}", path_text(natives_dir)?));
    }
    if !jvm
        .iter()
        .any(|value| matches!(value.as_str(), "-cp" | "-classpath" | "--class-path"))
    {
        jvm.push("-cp".to_owned());
        jvm.push(variables["classpath"].clone());
    }
    if let Some(path) = &resolved.logging_path {
        let logging = metadata.logging.client.as_ref().ok_or_else(|| {
            Error::InvalidMetadata("resolved logging file lacks its argument template".to_owned())
        })?;
        let logging_variables = BTreeMap::from([("path", path_text(&root.join(path))?)]);
        jvm.push(substitute(&logging.argument, &logging_variables)?);
    }
    jvm.retain(|value| !value.starts_with("-Xmx") && !value.starts_with("-Xms"));
    for argument in &options.extra_jvm_args {
        if argument.starts_with("-Xmx") || argument.starts_with("-Xms") || argument.contains('\0') {
            return Err(Error::InvalidMetadata(
                "extra JVM arguments must not override heap settings or contain NUL bytes"
                    .to_owned(),
            ));
        }
        jvm.push(argument.clone());
    }
    jvm.push(format!("-Xms{}M", options.min_memory_mib));
    jvm.push(format!("-Xmx{}M", options.max_memory_mib));
    if options.demo && !game.iter().any(|value| value == "--demo") {
        game.push("--demo".to_owned());
    }
    jvm.push(main_class.to_owned());
    jvm.extend(game);
    Ok(LaunchSpec {
        executable: options.java.clone(),
        working_directory: game_dir.to_path_buf(),
        arguments: jvm.into_iter().map(OsString::from).collect(),
    })
}

/// Source of one bounded output chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Final operating-system child status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessExit {
    /// Exit code, absent for signal termination on Unix.
    pub code: Option<i32>,
    /// Whether the child returned success.
    pub success: bool,
    /// Whether the supervisor received a cancellation request.
    pub cancelled: bool,
}

/// Ordered lifecycle events; `Exited` follows all captured output.
///
/// Output consists of bytes because UTF-8 code points may span read boundaries.
/// A UI should use an incremental decoder. Debug hides game output.
pub enum ProcessEvent {
    /// The child has started.
    Started {
        /// Operating-system process ID.
        pid: Option<u32>,
    },
    /// A bounded chunk from an output pipe.
    Output {
        /// Originating stream.
        stream: OutputStream,
        /// Up to 8 KiB; may contain sensitive game output.
        bytes: Vec<u8>,
    },
    /// The direct child was reaped; readers ended or the drain deadline closed them.
    Exited(ProcessExit),
}

impl fmt::Debug for ProcessEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Started { pid } => formatter.debug_struct("Started").field("pid", pid).finish(),
            Self::Output { stream, bytes } => formatter
                .debug_struct("Output")
                .field("stream", stream)
                .field("byte_count", &bytes.len())
                .finish(),
            Self::Exited(status) => formatter.debug_tuple("Exited").field(status).finish(),
        }
    }
}

/// A supervised child with a bounded output queue.
///
/// Continuously consume [`Self::next_event`] while the game runs, or call
/// [`Self::wait`] to drain and discard output. A full queue intentionally applies
/// backpressure. Dropping the handle requests cancellation and closes output.
pub struct GameProcess {
    events: mpsc::Receiver<ProcessEvent>,
    cancellation: watch::Sender<bool>,
    supervisor: Option<JoinHandle<Result<ProcessExit, Error>>>,
}

impl GameProcess {
    /// Receives the next event, returning `None` after all events are drained.
    pub async fn next_event(&mut self) -> Option<ProcessEvent> {
        self.events.recv().await
    }

    /// Requests termination. Returns false if supervision has already completed.
    #[must_use]
    pub fn cancel(&self) -> bool {
        self.cancellation.send(true).is_ok()
    }

    /// Drains the queue until completion, then returns the final child status.
    ///
    /// # Errors
    ///
    /// Reports child wait, output-read, or supervisor task failures.
    pub async fn wait(mut self) -> Result<ProcessExit, Error> {
        while self.events.recv().await.is_some() {}
        let supervisor = self.supervisor.take().ok_or_else(|| {
            Error::InvalidMetadata("game supervisor was already consumed".to_owned())
        })?;
        supervisor.await.map_err(|source| Error::Io {
            operation: "join game process supervisor",
            path: PathBuf::from("<game process>"),
            source: io::Error::other(source),
        })?
    }
}

impl Drop for GameProcess {
    fn drop(&mut self) {
        // Sending can fail only after the supervisor has finished. Closing the
        // receiver also releases output readers waiting on a full queue.
        let _ = self.cancellation.send(true);
        self.events.close();
    }
}

async fn forward_output(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    stream: OutputStream,
    events: mpsc::Sender<ProcessEvent>,
    executable: PathBuf,
) -> Result<(), Error> {
    let mut buffer = vec![0; OUTPUT_CHUNK_SIZE];
    loop {
        let length = reader.read(&mut buffer).await.map_err(|source| Error::Io {
            operation: "read Minecraft process output",
            path: executable.clone(),
            source,
        })?;
        if length == 0 {
            return Ok(());
        }
        if events
            .send(ProcessEvent::Output {
                stream,
                bytes: buffer[..length].to_vec(),
            })
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

/// Spawns a shell-free game process and supervises both output pipes.
///
/// Java option injection variables are removed; other OS environment variables
/// are inherited. Standard input is closed. Cancellation kills and reaps the
/// direct child. No arguments or credentials are written to tracing logs.
///
/// # Errors
///
/// Returns a contextual error when the executable cannot be started.
pub fn spawn_game(spec: LaunchSpec) -> Result<GameProcess, Error> {
    spawn_game_with_resource(spec, ())
}

/// Starts a game while retaining an owned resource until its supervisor finishes.
///
/// The resource is moved into the supervisor, not the consumer-facing handle.
/// Use this for instance leases and private native-library directories: dropping
/// [`GameProcess`] requests cancellation, but cannot release those resources
/// before the direct child is reaped and its output readers finish or are stopped.
/// Descendants are not killed; inherited output pipes are drained for at most two
/// seconds after the direct child exits, then closed to avoid an unbounded wait.
///
/// # Errors
/// Returns a contextual error when the executable cannot be started.
pub fn spawn_game_with_resource<T: Send + 'static>(
    spec: LaunchSpec,
    resource: T,
) -> Result<GameProcess, Error> {
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.arguments)
        .current_dir(&spec.working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for name in ["JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "_JAVA_OPTIONS"] {
        command.env_remove(name);
    }
    let mut child = crate::process::spawn_with_busy_retry(&mut command).map_err(|source| {
        Error::ProcessStart {
            path: spec.executable.clone(),
            source,
        }
    })?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or_else(|| Error::ProcessStart {
        path: spec.executable.clone(),
        source: io::Error::other("Minecraft stdout pipe is unavailable"),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| Error::ProcessStart {
        path: spec.executable.clone(),
        source: io::Error::other("Minecraft stderr pipe is unavailable"),
    })?;
    let (sender, events) = mpsc::channel(OUTPUT_QUEUE_CAPACITY);
    let (cancellation, cancelled) = watch::channel(false);
    tracing::info!(pid, executable = %spec.executable.display(), "Minecraft process started");
    let supervisor = tokio::spawn(async move {
        // Started precedes any output; a disconnected caller cancels below.
        let _ = sender.send(ProcessEvent::Started { pid }).await;
        let stdout = tokio::spawn(forward_output(
            stdout,
            OutputStream::Stdout,
            sender.clone(),
            spec.executable.clone(),
        ));
        let stderr = tokio::spawn(forward_output(
            stderr,
            OutputStream::Stderr,
            sender.clone(),
            spec.executable.clone(),
        ));
        let result = supervise_game(
            child,
            [stdout, stderr],
            cancelled,
            &sender,
            &spec.executable,
        )
        .await;
        // Explicitly consume this value after supervision, making its ownership
        // independent of the receiver and all early-return paths in the helper.
        drop(resource);
        result
    });
    Ok(GameProcess {
        events,
        cancellation,
        supervisor: Some(supervisor),
    })
}

async fn supervise_game(
    mut child: tokio::process::Child,
    readers: [JoinHandle<Result<(), Error>>; 2],
    mut cancelled: watch::Receiver<bool>,
    events: &mpsc::Sender<ProcessEvent>,
    executable: &Path,
) -> Result<ProcessExit, Error> {
    let pid = child.id();
    let mut readers = readers.map(Some);
    let mut output_failure = None;
    let (mut status, was_cancelled) = loop {
        let [stdout, stderr] = &mut readers;
        tokio::select! {
            status = child.wait() => break (status, false),
            _ = cancelled.changed() => break (terminate_child(&mut child).await, true),
            result = await_output(stdout) => {
                *stdout = None;
                record_output_result(result, executable, &mut output_failure);
                if output_failure.is_some() {
                    break (terminate_child(&mut child).await, false);
                }
            },
            result = await_output(stderr) => {
                *stderr = None;
                record_output_result(result, executable, &mut output_failure);
                if output_failure.is_some() {
                    break (terminate_child(&mut child).await, false);
                }
            },
        }
    };
    if status.is_err() {
        // A transient wait failure must not abandon a still-owned child. The
        // original error is retained if the final termination/wait also fails.
        if let Err(error) = child.start_kill() {
            tracing::warn!(pid, error = %error, "Minecraft cleanup termination request failed");
        }
        if let Ok(final_status) = child.wait().await {
            status = Ok(final_status);
        }
    }
    let output_result = drain_output(readers, executable).await;
    let output_result = output_failure.map_or(output_result, Err);
    let mut exit = ProcessExit {
        code: status
            .as_ref()
            .ok()
            .and_then(std::process::ExitStatus::code),
        success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
        cancelled: was_cancelled,
    };
    let result = status
        .map_err(|source| Error::Io {
            operation: "wait for Minecraft process",
            path: executable.to_owned(),
            source,
        })
        .and(output_result)
        .map(|()| exit);
    if result.is_err() {
        exit.success = false;
    }
    tracing::info!(
        pid,
        code = exit.code,
        cancelled = exit.cancelled,
        success = exit.success,
        "Minecraft process exited"
    );
    // Reader failures and worker panics must still produce a terminal event.
    // wait() additionally returns the contextual error, rather than fake success.
    let _ = events.send(ProcessEvent::Exited(exit)).await;
    result
}

async fn terminate_child(
    child: &mut tokio::process::Child,
) -> io::Result<std::process::ExitStatus> {
    // Always wait, even if kill races a natural exit. Returning immediately
    // from a kill error can leave a direct child unreaped.
    if let Err(error) = child.start_kill() {
        tracing::warn!(pid = child.id(), error = %error, "Minecraft termination request failed; waiting for child");
    }
    child.wait().await
}

async fn await_output(
    reader: &mut Option<JoinHandle<Result<(), Error>>>,
) -> Result<Result<(), Error>, tokio::task::JoinError> {
    match reader {
        Some(reader) => reader.await,
        None => std::future::pending().await,
    }
}

async fn drain_output(
    mut readers: [Option<JoinHandle<Result<(), Error>>>; 2],
    executable: &Path,
) -> Result<(), Error> {
    let mut failure = None;
    let drained = tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, async {
        for reader in &mut readers {
            if let Some(handle) = reader.as_mut() {
                record_output_result(handle.await, executable, &mut failure);
                *reader = None;
            }
        }
    })
    .await;
    if drained.is_err() {
        tracing::warn!("Minecraft output drain deadline reached; closing inherited output pipes");
        for reader in readers.iter().flatten() {
            reader.abort();
        }
        for reader in readers.into_iter().flatten() {
            match reader.await {
                Err(error) if error.is_cancelled() => {}
                result => record_output_result(result, executable, &mut failure),
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

fn record_output_result(
    result: Result<Result<(), Error>, tokio::task::JoinError>,
    executable: &Path,
    failure: &mut Option<Error>,
) {
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error,
        Err(_) => Error::Io {
            operation: "join Minecraft output reader",
            path: executable.to_owned(),
            // A panic payload could contain captured output. Do not expose it.
            source: io::Error::other(
                "Minecraft output worker panicked or was unexpectedly aborted",
            ),
        },
    };
    if failure.is_none() {
        *failure = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::{LaunchIdentity, LaunchOptions, build_launch_spec};
    use crate::{metadata::parse_version_metadata, resolver::ResolvedVersion};

    fn resolved() -> ResolvedVersion {
        ResolvedVersion {
            files: Vec::new(),
            classpath: vec!["libraries/a library.jar".into()],
            natives: Vec::new(),
            client_path: "versions/test/test.jar".into(),
            asset_index_id: "test".to_owned(),
            logging_path: None,
            game_assets_path: "assets".into(),
            game_assets_in_resources: false,
        }
    }

    fn identity() -> LaunchIdentity {
        LaunchIdentity::new(
            "TestPlayer".to_owned(),
            uuid::Uuid::from_u128(1),
            "secret-access-token".to_owned(),
        )
        .expect("fixture identity")
    }

    #[test]
    fn legacy_substitution_keeps_paths_as_single_arguments_and_hides_tokens() {
        let metadata = parse_version_metadata(br#"{
            "id":"test","mainClass":"net.minecraft.client.main.Main",
            "minecraftArguments":"--gameDir ${game_directory} --accessToken ${auth_access_token} --username ${auth_player_name}"
        }"#).expect("fixture metadata");
        let root = std::env::temp_dir().join("Nudge shared data");
        let game = root.join("a game instance");
        let user = identity();
        let spec = build_launch_spec(
            &metadata,
            &resolved(),
            &root,
            &game,
            &root.join("natives"),
            &user,
            &LaunchOptions::default(),
        )
        .expect("launch specification");
        let args = spec.arguments();
        let position = args
            .iter()
            .position(|value| value == "--gameDir")
            .expect("game argument");
        assert_eq!(args[position + 1], game.as_os_str());
        assert!(!format!("{spec:?}").contains("secret-access-token"));
        assert!(!format!("{user:?}").contains("secret-access-token"));
    }

    #[test]
    fn pre_sixteen_game_assets_argument_uses_instance_resources() {
        let metadata = parse_version_metadata(
            br#"{"id":"1.5.2","mainClass":"net.minecraft.client.main.Main","minecraftArguments":"--assetsDir ${game_assets}"}"#,
        )
        .expect("legacy metadata");
        let root = std::env::temp_dir().join("Nudge shared data");
        let game = root.join("instances/legacy/game");
        let mut plan = resolved();
        plan.game_assets_in_resources = true;
        let spec = build_launch_spec(
            &metadata,
            &plan,
            &root,
            &game,
            &root.join("natives"),
            &identity(),
            &LaunchOptions::default(),
        )
        .expect("legacy launch specification");
        let args = spec.arguments();
        let position = args
            .iter()
            .position(|value| value == "--assetsDir")
            .expect("assets argument");
        assert_eq!(args[position + 1], game.join("resources").as_os_str());
    }

    #[test]
    fn offline_identity_uses_legacy_authentication_without_demo_mode() {
        let metadata = parse_version_metadata(br#"{
            "id":"test","mainClass":"net.minecraft.client.main.Main",
            "minecraftArguments":"--userType ${user_type} --accessToken ${auth_access_token} --xuid ${auth_xuid} --clientId ${clientid}"
        }"#).expect("fixture metadata");
        let directory = std::env::temp_dir();
        let offline = LaunchIdentity::offline("OfflinePlayer".to_owned(), uuid::Uuid::from_u128(7))
            .expect("offline identity");
        let spec = build_launch_spec(
            &metadata,
            &resolved(),
            &directory,
            &directory,
            &directory,
            &offline,
            &LaunchOptions::default(),
        )
        .expect("offline launch specification");
        let arguments = spec
            .arguments()
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for pair in [
            ["--userType", "legacy"],
            ["--accessToken", "0"],
            ["--xuid", ""],
            ["--clientId", ""],
        ] {
            let position = arguments
                .iter()
                .position(|value| value == pair[0])
                .expect("authentication argument");
            assert_eq!(arguments[position + 1], pair[1]);
        }
        assert!(!arguments.iter().any(|value| value == "--demo"));
        assert!(
            build_launch_spec(
                &metadata,
                &resolved(),
                &directory,
                &directory,
                &directory,
                &offline,
                &LaunchOptions {
                    demo: true,
                    ..LaunchOptions::default()
                },
            )
            .is_err()
        );
        assert!(LaunchIdentity::offline("bad name".to_owned(), uuid::Uuid::from_u128(8)).is_err());
        assert!(LaunchIdentity::offline("OfflinePlayer".to_owned(), uuid::Uuid::nil()).is_err());
    }

    #[test]
    fn modern_rules_and_demo_restriction_are_enforced() {
        let metadata = parse_version_metadata(br#"{
            "id":"test","mainClass":"net.minecraft.client.main.Main",
            "arguments":{"jvm":["-cp","${classpath}"],"game":[
                {"rules":[{"action":"allow","features":{"is_demo_user":true}}],"value":"--demo"},
                {"rules":[{"action":"allow","features":{"has_custom_resolution":true}}],"value":["--width","${resolution_width}"]}
            ]}
        }"#).expect("fixture metadata");
        let directory = std::env::temp_dir();
        assert!(
            build_launch_spec(
                &metadata,
                &resolved(),
                &directory,
                &directory,
                &directory,
                &LaunchIdentity::demo(),
                &LaunchOptions::default()
            )
            .is_err()
        );
        let options = LaunchOptions {
            demo: true,
            resolution: Some((1280, 720)),
            ..LaunchOptions::default()
        };
        let spec = build_launch_spec(
            &metadata,
            &resolved(),
            &directory,
            &directory,
            &directory,
            &LaunchIdentity::demo(),
            &options,
        )
        .expect("demo launch");
        assert_eq!(
            spec.arguments()
                .iter()
                .filter(|value| *value == "--demo")
                .count(),
            1
        );
        assert!(spec.arguments().iter().any(|value| value == "1280"));
    }

    #[test]
    fn unknown_placeholders_are_errors_without_exposing_values() {
        let metadata = parse_version_metadata(
            br#"{
            "id":"test","mainClass":"net.minecraft.client.main.Main",
            "arguments":{"game":["${not_implemented}"]}
        }"#,
        )
        .expect("fixture metadata");
        let directory = std::env::temp_dir();
        let error = build_launch_spec(
            &metadata,
            &resolved(),
            &directory,
            &directory,
            &directory,
            &identity(),
            &LaunchOptions::default(),
        )
        .expect_err("unknown placeholder");
        assert!(matches!(error, crate::Error::UnresolvedPlaceholder(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn captures_both_streams_then_exit_and_preserves_argument_boundaries() {
        use std::{ffi::OsString, os::unix::fs::PermissionsExt};

        let directory = tempfile::tempdir().expect("temporary directory");
        let executable = directory.path().join("fake java");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s' \"$1\"\nprintf 'error output' >&2\nexit 7\n",
        )
        .expect("write script");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("permissions");
        let spec = super::LaunchSpec {
            executable,
            working_directory: directory.path().to_path_buf(),
            arguments: vec![OsString::from("one argument with spaces; $HOME")],
        };
        let mut process = super::spawn_game(spec).expect("spawn fixture");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut exit = None;
        while let Some(event) = process.next_event().await {
            match event {
                super::ProcessEvent::Output {
                    stream: super::OutputStream::Stdout,
                    bytes,
                } => stdout.extend(bytes),
                super::ProcessEvent::Output {
                    stream: super::OutputStream::Stderr,
                    bytes,
                } => stderr.extend(bytes),
                super::ProcessEvent::Exited(status) => exit = Some(status),
                super::ProcessEvent::Started { .. } => {}
            }
        }
        assert_eq!(stdout, b"one argument with spaces; $HOME");
        assert_eq!(stderr, b"error output");
        assert_eq!(exit.expect("exit event").code, Some(7));
        assert_eq!(
            process.wait().await.expect("supervisor status").code,
            Some(7)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_reaps_the_child() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let spec = super::LaunchSpec {
            executable: "/bin/sh".into(),
            working_directory: directory.path().to_path_buf(),
            arguments: vec!["-c".into(), "exec sleep 30".into()],
        };
        let process = super::spawn_game(spec).expect("spawn fixture");
        assert!(process.cancel());
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), process.wait())
            .await
            .expect("cancellation timeout")
            .expect("cancelled status");
        assert!(exit.cancelled);
        assert!(!exit.success);
    }

    #[cfg(unix)]
    async fn stop_fixture_descendant(pid: u32) -> std::io::Result<()> {
        // The ID comes only from our fixed test shell, not launcher metadata.
        // Capture diagnostics rather than emitting an expected ESRCH race.
        let output = tokio::process::Command::new("/bin/kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output()
            .await?;
        if !output.status.success() {
            let probe = tokio::process::Command::new("/bin/kill")
                .arg("-0")
                .arg(pid.to_string())
                .output()
                .await?;
            if probe.status.success() {
                return Err(std::io::Error::other(
                    "test descendant could not be terminated",
                ));
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn inherited_pipes_cannot_prevent_a_natural_terminal_event()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;
        let directory = tempfile::tempdir()?;
        let spec = super::LaunchSpec {
            executable: "/bin/sh".into(),
            working_directory: directory.path().to_owned(),
            arguments: vec![
                "-c".into(),
                "sleep 30 & printf '%s\\n' \"$!\"; exit 0".into(),
            ],
        };
        let mut process = super::spawn_game(spec)?;
        let mut output = Vec::new();
        let terminal = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match process.next_event().await {
                    Some(super::ProcessEvent::Output {
                        stream: super::OutputStream::Stdout,
                        bytes,
                    }) => output.extend(bytes),
                    Some(super::ProcessEvent::Exited(exit)) => break Ok(exit),
                    Some(_) => {}
                    None => break Err("terminal event was missing"),
                }
            }
        })
        .await?;
        let descendant = std::str::from_utf8(&output)?.trim().parse::<u32>()?;
        stop_fixture_descendant(descendant).await?;
        let terminal = terminal?;
        assert!(terminal.success);
        assert!(!terminal.cancelled);
        assert_eq!(process.wait().await?, terminal);
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_consumers_reap_before_releasing_owned_resources_despite_inherited_pipes()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;
        struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyOnDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let directory = tempfile::tempdir()?;
        let (released, mut resource_released) = tokio::sync::oneshot::channel();
        let spec = super::LaunchSpec {
            executable: "/bin/sh".into(),
            working_directory: directory.path().to_owned(),
            arguments: vec![
                "-c".into(),
                "sleep 30 & printf '%s\\n' \"$!\"; exec sleep 30".into(),
            ],
        };
        let mut process = super::spawn_game_with_resource(spec, NotifyOnDrop(Some(released)))?;
        let mut output = Vec::new();
        let mut pid = None;
        tokio::time::timeout(Duration::from_secs(5), async {
            while !output.contains(&b'\n') {
                match process.next_event().await {
                    Some(super::ProcessEvent::Started { pid: child_pid }) => pid = child_pid,
                    Some(super::ProcessEvent::Output {
                        stream: super::OutputStream::Stdout,
                        bytes,
                    }) => output.extend(bytes),
                    Some(_) => {}
                    None => return Err("process ended before fixture was ready"),
                }
            }
            Ok(())
        })
        .await??;
        let descendant = std::str::from_utf8(&output)?.trim().parse::<u32>()?;
        assert!(matches!(
            resource_released.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(process);
        let released = tokio::time::timeout(Duration::from_secs(5), resource_released).await;
        stop_fixture_descendant(descendant).await?;
        released??;
        let probe = tokio::process::Command::new("/bin/kill")
            .arg("-0")
            .arg(pid.ok_or("child PID unavailable")?.to_string())
            .output()
            .await?;
        assert!(
            !probe.status.success(),
            "owned resources were released before the direct child was reaped"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_worker_panics_still_emit_failed_terminal_events()
    -> Result<(), Box<dyn std::error::Error>> {
        let child = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("exec sleep 30")
            .kill_on_drop(true)
            .spawn()?;
        let reader: tokio::task::JoinHandle<Result<(), crate::Error>> = tokio::spawn(async {
            panic!("synthetic output worker failure");
        });
        let other = tokio::spawn(async { Ok(()) });
        let (_cancellation, cancelled) = tokio::sync::watch::channel(false);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::supervise_game(
                child,
                [reader, other],
                cancelled,
                &sender,
                std::path::Path::new("/bin/sh"),
            ),
        )
        .await?;
        let error = result.err().ok_or("reader panic must fail supervision")?;
        assert!(
            matches!(receiver.recv().await, Some(super::ProcessEvent::Exited(exit)) if !exit.success)
        );
        assert!(matches!(
            error,
            crate::Error::Io {
                operation: "join Minecraft output reader",
                ..
            }
        ));
        assert!(!format!("{error}").contains("synthetic output worker failure"));
        Ok(())
    }
}
