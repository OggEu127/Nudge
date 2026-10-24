//! Persistent, bounded download scheduling with validated resumable transfers.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{File, OpenOptions},
    io,
    num::NonZeroUsize,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{StreamExt as _, stream::FuturesUnordered};
use reqwest::{Response, header};
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::{io::AsyncWriteExt as _, sync::broadcast};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::{
    Error, ReqwestHttpClient,
    policy::diagnostic_url,
    transfer::{FileSpec, verify_file},
};

const PRIVATE_DIRECTORY: &str = ".nudge-downloads";
const MAX_ATTEMPTS: u32 = 4;
const MAX_UNKNOWN_SIZE: u64 = 2 * 1024 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// One independently verifiable artifact and its owned-root-relative target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DownloadJob {
    /// Stable task ID used for persistence and progress.
    pub id: String,
    /// Authoritative URL, expected size, and trusted hashes.
    pub spec: FileSpec,
    /// Fallback URLs for the same exact expected content.
    pub mirrors: Vec<Url>,
    /// Relative destination underneath the manager's owned root.
    pub destination: PathBuf,
    /// Higher values are started first.
    pub priority: i32,
}

/// An observable change to a persisted download task.
#[derive(Clone, Debug)]
pub struct DownloadEvent {
    /// Task ID.
    pub id: String,
    /// Progress or lifecycle change.
    pub kind: DownloadEventKind,
}

/// Download progress is best-effort; a completed event is authoritative.
#[derive(Clone, Debug)]
pub enum DownloadEventKind {
    /// The task was committed to the persistent queue.
    Queued,
    /// A network attempt is starting, numbered from one.
    Started {
        /// One-based attempt number.
        attempt: u32,
    },
    /// Bytes written to this partial file so far; flushed on completion/cancellation.
    Progress {
        /// Completed bytes, including a verified resume offset.
        downloaded: u64,
        /// Expected total, when known.
        total: Option<u64>,
    },
    /// Verified content has been atomically installed.
    Completed {
        /// Whether existing destination or cache avoided the network.
        reused: bool,
    },
    /// A transient failure or fallback source will be retried.
    Retrying {
        /// Next attempt number.
        attempt: u32,
        /// Delay before the next attempt.
        delay_ms: u64,
    },
    /// Cancellation preserved the queued task and partial bytes.
    Cancelled,
    /// All applicable attempts failed; the task remains recoverable.
    Failed {
        /// Description without URL credentials or query parameters.
        message: String,
    },
}

/// A persistent queue, verified cache, and bounded concurrent transfer engine.
///
/// One OS file lock serializes runs against the same root across processes.
/// The SQLite connection is held only for short metadata transactions, never
/// during HTTP or filesystem streaming.
pub struct DownloadManager {
    root: PathBuf,
    private: PathBuf,
    concurrency: NonZeroUsize,
    http: ReqwestHttpClient,
    database: Mutex<Connection>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Validator {
    Etag(String),
    LastModified(String),
}

impl Validator {
    fn value(&self) -> &str {
        match self {
            Self::Etag(value) | Self::LastModified(value) => value,
        }
    }

    fn matches(&self, response: &Response) -> bool {
        let name = match self {
            Self::Etag(_) => header::ETAG,
            Self::LastModified(_) => header::LAST_MODIFIED,
        };
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            == Some(self.value())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PartialState {
    source: Url,
    validator: Validator,
    total: Option<u64>,
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> Error {
    Error::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn invalid(message: impl Into<String>) -> Error {
    Error::InvalidSpec(message.into())
}

fn require_plain(path: &Path, directory: bool) -> Result<bool, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => Err(
            invalid(format!("symbolic link is forbidden: `{}`", path.display())),
        ),
        Ok(metadata) if metadata.is_dir() == directory && (directory || metadata.is_file()) => {
            Ok(true)
        }
        Ok(_) => Err(invalid(format!(
            "unexpected filesystem entry: `{}`",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error("inspect download path", path, error)),
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

fn check_ancestors(path: &Path) -> Result<(), Error> {
    let absolute =
        std::path::absolute(path).map_err(|e| io_error("resolve download path", path, e))?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() || is_reparse_point(&meta) => {
                return Err(invalid(format!(
                    "link in download path `{}`",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_error("inspect download ancestor", &current, e)),
        }
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<(), Error> {
    check_ancestors(path)?;
    if require_plain(path, true)? {
        return Ok(());
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        create_directory(parent)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            require_plain(path, true)?;
            Ok(())
        }
        Err(error) => Err(io_error("create download directory", path, error)),
    }
}

fn validate_destination(path: &Path) -> Result<(), Error> {
    let mut first = true;
    if path.as_os_str().is_empty() {
        return Err(invalid("download destination is empty"));
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err(invalid(
                "download destination must contain only relative normal path components",
            ));
        };
        let name = component
            .to_str()
            .ok_or_else(|| invalid("download destination is not Unicode"))?;
        if crate::paths::is_unsafe_component(name)
            || (first && name.eq_ignore_ascii_case(PRIVATE_DIRECTORY))
        {
            return Err(invalid(
                "download destination contains a reserved or unsafe component",
            ));
        }
        first = false;
    }
    Ok(())
}

fn content_key(spec: &FileSpec) -> Result<String, Error> {
    spec.hashes.validate()?;
    for (algorithm, hash) in [
        ("sha512", &spec.hashes.sha512),
        ("sha256", &spec.hashes.sha256),
        ("sha1", &spec.hashes.sha1),
    ] {
        if let Some(hash) = hash {
            return Ok(format!("{algorithm}-{}", hash.to_ascii_lowercase()));
        }
    }
    Err(invalid("download has no expected digest"))
}

fn destination_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn same_expected_content(left: &FileSpec, right: &FileSpec) -> bool {
    if left
        .size
        .zip(right.size)
        .is_some_and(|(left, right)| left != right)
    {
        return false;
    }
    let mut common_digest = false;
    for (left, right) in [
        (&left.hashes.sha1, &right.hashes.sha1),
        (&left.hashes.sha256, &right.hashes.sha256),
        (&left.hashes.sha512, &right.hashes.sha512),
    ] {
        if let (Some(left), Some(right)) = (left, right) {
            if !left.eq_ignore_ascii_case(right) {
                return false;
            }
            common_digest = true;
        }
    }
    common_digest
}

fn partial_key(job: &DownloadJob) -> Result<String, Error> {
    let mut digest = Sha256::new();
    digest.update(job.id.as_bytes());
    digest.update([0]);
    digest.update(job.destination.as_os_str().to_string_lossy().as_bytes());
    digest.update([0]);
    digest.update(content_key(&job.spec)?.as_bytes());
    Ok(hex::encode(digest.finalize()))
}

fn emit(events: &broadcast::Sender<DownloadEvent>, job: &DownloadJob, kind: DownloadEventKind) {
    // Broadcast without listeners is normal for CLI/library callers. Lagging
    // observers can discard progress and still read the persisted final state.
    let _ = events.send(DownloadEvent {
        id: job.id.clone(),
        kind,
    });
}

fn failure_message(error: &Error) -> String {
    match error {
        Error::HttpStatus { status, .. } => format!("server returned HTTP {status}"),
        Error::Request { .. } => "HTTP transport failed".to_owned(),
        Error::Integrity { reason, .. } => format!("integrity verification failed: {reason}"),
        Error::Cancelled => "download cancelled".to_owned(),
        Error::Io { operation, .. } => format!("filesystem/stream operation failed: {operation}"),
        _ => "download rejected by safety or persistence policy".to_owned(),
    }
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::HttpStatus { status, .. } => matches!(status, 408 | 429 | 500..=599),
        Error::Request { source, .. } => {
            source.is_timeout() || source.is_connect() || source.is_body() || source.is_decode()
        }
        Error::Io {
            source,
            operation: "read download body",
            ..
        } => source.kind() == io::ErrorKind::TimedOut,
        Error::Integrity { .. } => true,
        _ => false,
    }
}

impl DownloadManager {
    /// Opens a persistent queue using the public-HTTPS network policy.
    ///
    /// # Errors
    /// Returns contextual path, HTTP-client, or SQLite initialization errors.
    pub fn new(root: PathBuf, concurrency: NonZeroUsize) -> Result<Self, Error> {
        Self::with_http(
            root,
            concurrency,
            ReqwestHttpClient::new("Nudge/0.1", crate::DEFAULT_MAX_RESPONSE_BYTES)?,
        )
    }

    /// Opens a queue with an explicitly configured HTTP client.
    ///
    /// This is the opt-in seam for loopback-only mock HTTP tests.
    ///
    /// # Errors
    /// Returns errors for unsafe filesystem entries or database initialization.
    pub fn with_http(
        root: impl Into<PathBuf>,
        concurrency: NonZeroUsize,
        http: ReqwestHttpClient,
    ) -> Result<Self, Error> {
        let root = root.into();
        if concurrency.get() > 64 {
            return Err(invalid("maximum concurrency must not exceed 64"));
        }
        create_directory(&root)?;
        let root = std::fs::canonicalize(&root)
            .map_err(|error| io_error("resolve owned download root", &root, error))?;
        let private = root.join(PRIVATE_DIRECTORY);
        for path in [&private, &private.join("cache"), &private.join("partials")] {
            create_directory(path)?;
        }
        // Queued source URLs may contain expiring signatures. Do not expose
        // their SQLite/partial sidecars to other local users through the umask.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| io_error("protect private download state", &private, error))?;
        }
        let db_path = private.join("tasks.sqlite3");
        require_plain(&db_path, false)?;
        require_plain(&private.join("tasks.sqlite3-wal"), false)?;
        require_plain(&private.join("tasks.sqlite3-shm"), false)?;
        let database = Connection::open(&db_path)?;
        database.busy_timeout(Duration::from_secs(10))?;
        database.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS downloads (id TEXT PRIMARY KEY NOT NULL, job TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','running','completed','failed'))); CREATE TABLE IF NOT EXISTS destinations (path TEXT PRIMARY KEY NOT NULL, spec TEXT NOT NULL);")?;
        Ok(Self {
            root,
            private,
            concurrency,
            http,
            database: Mutex::new(database),
        })
    }

    fn database(&self) -> Result<std::sync::MutexGuard<'_, Connection>, Error> {
        self.database
            .lock()
            .map_err(|_| invalid("download database lock is poisoned"))
    }

    fn state(&self, id: &str, state: &str) -> Result<(), Error> {
        self.database()?.execute(
            "UPDATE downloads SET state=?2 WHERE id=?1",
            params![id, state],
        )?;
        Ok(())
    }

    fn lock_run(&self) -> Result<File, Error> {
        require_plain(&self.root, true)?;
        for directory in [
            &self.private,
            &self.private.join("cache"),
            &self.private.join("partials"),
        ] {
            require_plain(directory, true)?;
        }
        let path = self.private.join("engine.lock");
        require_plain(&path, false)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| io_error("open download engine lock", &path, error))?;
        file.try_lock().map_err(|error| {
            io_error(
                "lock download engine (another run may be active)",
                &path,
                io::Error::other(error),
            )
        })?;
        Ok(file)
    }

    fn validate_jobs(&self, jobs: &[DownloadJob]) -> Result<(), Error> {
        let mut ids = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        for job in jobs {
            if job.id.trim().is_empty()
                || job.id.len() > 256
                || job.id.chars().any(char::is_control)
                || !ids.insert(&job.id)
            {
                return Err(invalid(
                    "download task IDs must be unique, nonempty, bounded strings",
                ));
            }
            validate_destination(&job.destination)?;
            let key = destination_key(&job.destination);
            if !destinations.insert(key) {
                return Err(invalid("download tasks have conflicting destination paths"));
            }
            job.spec.hashes.validate()?;
            self.http.validate_url(&job.spec.url)?;
            if job.mirrors.len() > 8 {
                return Err(invalid("download task exceeds eight fallback sources"));
            }
            for mirror in &job.mirrors {
                self.http.validate_url(mirror)?;
            }
        }
        Ok(())
    }

    /// Persists and runs jobs with bounded concurrency and descending priority.
    ///
    /// Successful files survive other task failures. Cancellation and failures
    /// leave queue records and resumable partial files for a subsequent run.
    ///
    /// # Errors
    /// Returns the first task failure after all scheduled work finishes, or
    /// [`Error::Cancelled`] after active transfers preserve their partial state.
    pub async fn run(
        &self,
        mut jobs: Vec<DownloadJob>,
        cancel: CancellationToken,
        events: broadcast::Sender<DownloadEvent>,
    ) -> Result<(), Error> {
        let _lock = self.lock_run()?;
        self.validate_jobs(&jobs)?;
        {
            let mut database = self.database()?;
            let transaction = database.transaction()?;
            transaction.execute(
                "UPDATE downloads SET state='pending' WHERE state='running'",
                [],
            )?;
            for job in &jobs {
                let destination = destination_key(&job.destination);
                let previous: Option<String> = transaction
                    .query_row(
                        "SELECT spec FROM destinations WHERE path=?1",
                        [&destination],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(previous) = previous {
                    let previous: FileSpec = serde_json::from_str(&previous).map_err(|_| {
                        invalid("persisted destination integrity record is malformed")
                    })?;
                    if !same_expected_content(&previous, &job.spec) {
                        return Err(invalid(format!(
                            "destination `{}` is already reserved for different expected content",
                            job.destination.display()
                        )));
                    }
                } else {
                    let spec = serde_json::to_string(&job.spec)
                        .map_err(|_| invalid("cannot serialize destination integrity record"))?;
                    transaction.execute(
                        "INSERT INTO destinations(path,spec) VALUES (?1,?2)",
                        params![destination, spec],
                    )?;
                }
                let json = serde_json::to_string(job)
                    .map_err(|_| invalid("cannot serialize persistent download job"))?;
                transaction.execute("INSERT INTO downloads(id,job,state) VALUES (?1,?2,'pending') ON CONFLICT(id) DO UPDATE SET job=excluded.job,state='pending'", params![job.id, json])?;
            }
            transaction.commit()?;
        }
        jobs.sort_by(|left, right| {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| left.id.cmp(&right.id))
        });
        let mut locks = BTreeMap::<String, Arc<tokio::sync::Mutex<()>>>::new();
        for job in &jobs {
            locks
                .entry(content_key(&job.spec)?)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())));
            emit(&events, job, DownloadEventKind::Queued);
        }
        let mut queue = VecDeque::from(jobs);
        let mut active = FuturesUnordered::new();
        let mut first_failure = None;
        loop {
            while active.len() < self.concurrency.get() && !cancel.is_cancelled() {
                let Some(job) = queue.pop_front() else {
                    break;
                };
                let lock = locks
                    .get(&content_key(&job.spec)?)
                    .cloned()
                    .ok_or_else(|| invalid("missing internal content lock"))?;
                active.push(self.run_job(job, lock, cancel.clone(), events.clone()));
            }
            let Some(result) = active.next().await else {
                break;
            };
            if let Err(error) = result
                && first_failure.is_none()
            {
                first_failure = Some(error);
            }
        }
        if cancel.is_cancelled() {
            for job in queue {
                emit(&events, &job, DownloadEventKind::Cancelled);
            }
            return Err(Error::Cancelled);
        }
        first_failure.map_or(Ok(()), Err)
    }

    /// Reloads all incomplete tasks, including previous failures, and retries.
    ///
    /// # Errors
    /// Returns persistence, validation, transfer, or cancellation errors.
    pub async fn resume_pending(
        &self,
        cancel: CancellationToken,
        events: broadcast::Sender<DownloadEvent>,
    ) -> Result<(), Error> {
        let jobs = {
            let database = self.database()?;
            let mut statement = database
                .prepare("SELECT job FROM downloads WHERE state != 'completed' ORDER BY id")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut jobs = Vec::new();
            for row in rows {
                jobs.push(
                    serde_json::from_str(&row?)
                        .map_err(|_| invalid("persistent download job is malformed"))?,
                );
            }
            jobs
        };
        self.run(jobs, cancel, events).await
    }

    fn destination(&self, relative: &Path) -> Result<PathBuf, Error> {
        validate_destination(relative)?;
        require_plain(&self.root, true)?;
        let destination = self.root.join(relative);
        let mut parent = self.root.clone();
        if let Some(relative_parent) = relative.parent() {
            for component in relative_parent.components() {
                parent.push(component.as_os_str());
                // Walk from the owned root rather than just statting the final
                // parent: an intermediate directory could itself be a link.
                create_directory(&parent)?;
            }
        }
        require_plain(&destination, false)?;
        Ok(destination)
    }

    async fn verified(
        &self,
        path: &Path,
        spec: &FileSpec,
        cancel: &CancellationToken,
    ) -> Result<bool, Error> {
        require_plain(path, false)?;
        tokio::select! { () = cancel.cancelled() => Err(Error::Cancelled), result = verify_file(path,spec) => result }
    }

    async fn run_job(
        &self,
        job: DownloadJob,
        lock: Arc<tokio::sync::Mutex<()>>,
        cancel: CancellationToken,
        events: broadcast::Sender<DownloadEvent>,
    ) -> Result<(), Error> {
        let result = self.run_job_inner(&job, lock, &cancel, &events).await;
        match &result {
            Ok(reused) => {
                self.state(&job.id, "completed")?;
                emit(
                    &events,
                    &job,
                    DownloadEventKind::Completed { reused: *reused },
                );
                tracing::info!(task_id = %job.id, reused, "download task completed");
            }
            Err(Error::Cancelled) => {
                self.state(&job.id, "pending")?;
                emit(&events, &job, DownloadEventKind::Cancelled);
                tracing::info!(task_id = %job.id, "download task cancelled");
            }
            Err(error) => {
                self.state(&job.id, "failed")?;
                emit(
                    &events,
                    &job,
                    DownloadEventKind::Failed {
                        message: failure_message(error),
                    },
                );
                tracing::warn!(task_id = %job.id, category = %failure_message(error), "download task failed");
            }
        }
        result.map(|_| ())
    }

    async fn run_job_inner(
        &self,
        job: &DownloadJob,
        lock: Arc<tokio::sync::Mutex<()>>,
        cancel: &CancellationToken,
        events: &broadcast::Sender<DownloadEvent>,
    ) -> Result<bool, Error> {
        let _content_guard = tokio::select! { () = cancel.cancelled() => return Err(Error::Cancelled), guard = lock.lock() => guard };
        self.state(&job.id, "running")?;
        let destination = self.destination(&job.destination)?;
        if self.verified(&destination, &job.spec, cancel).await? {
            return Ok(true);
        }
        let cache = self.private.join("cache").join(content_key(&job.spec)?);
        if self.verified(&cache, &job.spec, cancel).await? {
            self.atomic_copy(&cache, &destination, &job.spec).await?;
            return Ok(true);
        }
        let part = self
            .private
            .join("partials")
            .join(format!("{}.part", partial_key(job)?));
        let sidecar = part.with_extension("json");
        require_plain(&part, false)?;
        require_plain(&sidecar, false)?;
        let sources: Vec<&Url> = std::iter::once(&job.spec.url)
            .chain(job.mirrors.iter())
            .collect();
        let budget =
            MAX_ATTEMPTS * u32::try_from(sources.len()).map_err(|_| invalid("too many sources"))?;
        let mut exhausted = BTreeSet::new();
        let mut source_index = 0;
        for attempt in 1..=budget {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            while exhausted.contains(&source_index) {
                source_index = (source_index + 1) % sources.len();
            }
            let source = sources[source_index];
            emit(events, job, DownloadEventKind::Started { attempt });
            tracing::info!(task_id = %job.id, attempt, "download attempt started");
            match self
                .transfer(job, source, &part, &sidecar, cancel, events)
                .await
            {
                Ok(()) => {
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    self.atomic_copy(&part, &cache, &job.spec).await?;
                    if cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    self.atomic_copy(&cache, &destination, &job.spec).await?;
                    remove_optional(&part).await?;
                    remove_optional(&sidecar).await?;
                    return Ok(false);
                }
                Err(error) => {
                    if !retryable(&error) {
                        exhausted.insert(source_index);
                    }
                    if attempt == budget
                        || exhausted.len() == sources.len()
                        || matches!(error, Error::Cancelled)
                    {
                        return Err(error);
                    }
                    source_index = (source_index + 1) % sources.len();
                    let delay_ms = (250_u64 << (attempt - 1).min(5)).min(8_000);
                    emit(
                        events,
                        job,
                        DownloadEventKind::Retrying {
                            attempt: attempt + 1,
                            delay_ms,
                        },
                    );
                    tokio::select! { () = cancel.cancelled() => return Err(Error::Cancelled), () = tokio::time::sleep(Duration::from_millis(delay_ms)) => {} }
                }
            }
        }
        Err(invalid("download attempt budget exhausted"))
    }

    async fn atomic_copy(
        &self,
        source: &Path,
        destination: &Path,
        spec: &FileSpec,
    ) -> Result<(), Error> {
        require_plain(source, false)?;
        let parent = destination
            .parent()
            .ok_or_else(|| invalid("atomic destination has no parent"))?;
        create_directory(parent)?;
        require_plain(destination, false)?;
        let temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| io_error("create verified commit temporary file", parent, error))?;
        tokio::fs::copy(source, temporary.path())
            .await
            .map_err(|error| io_error("copy verified artifact", temporary.path(), error))?;
        if !verify_file(temporary.path(), spec).await? {
            return Err(Error::Integrity {
                path: destination.to_path_buf(),
                reason: "artifact changed during commit".to_owned(),
            });
        }
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| io_error("sync verified artifact", temporary.path(), error))?;
        require_plain(destination, false)?;
        temporary.persist(destination).map_err(|error| {
            io_error(
                "atomically commit verified artifact",
                destination,
                error.error,
            )
        })?;
        Ok(())
    }

    // Keep the range-validation and append/restart transitions adjacent so the
    // safety decision can be reviewed without following state across methods.
    #[allow(clippy::too_many_lines)]
    async fn transfer(
        &self,
        job: &DownloadJob,
        source: &Url,
        part: &Path,
        sidecar: &Path,
        cancel: &CancellationToken,
        events: &broadcast::Sender<DownloadEvent>,
    ) -> Result<(), Error> {
        if self.verified(part, &job.spec, cancel).await? {
            return Ok(());
        }
        let mut partial = load_partial(sidecar)?;
        let mut offset = match tokio::fs::metadata(part).await {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
            Err(error) => return Err(io_error("inspect partial length", part, error)),
        };
        if partial.as_ref().is_none_or(|state| state.source != *source)
            || job.spec.size.is_some_and(|size| offset > size)
        {
            offset = 0;
            partial = None;
        }
        // A range mismatch may be retried once as a clean full transfer within
        // this attempt. Never append merely because status happens to be 206.
        for _ in 0..2 {
            self.http.validate_url(source)?;
            let mut request = self
                .http
                .inner
                .get(source.clone())
                .timeout(REQUEST_TIMEOUT)
                .header(header::ACCEPT_ENCODING, "identity");
            if offset > 0
                && let Some(state) = &partial
            {
                request = request
                    .header(header::RANGE, format!("bytes={offset}-"))
                    .header(header::IF_RANGE, state.validator.value());
            }
            let response = tokio::select! {
                () = cancel.cancelled() => return Err(Error::Cancelled),
                response = request.send() => response.map_err(|source_error| Error::Request { url: diagnostic_url(source), source: source_error.without_url() })?,
            };
            self.http.validate_url(response.url())?;
            let status = response.status().as_u16();
            if status == 416 {
                if self.verified(part, &job.spec, cancel).await? {
                    return Ok(());
                }
                offset = 0;
                partial = None;
                continue;
            }
            if status != 200 && status != 206 {
                return Err(Error::HttpStatus {
                    url: diagnostic_url(response.url()),
                    status,
                });
            }
            if response
                .headers()
                .get(header::CONTENT_ENCODING)
                .is_some_and(|encoding| encoding != "identity")
            {
                return Err(invalid(
                    "encoded artifact responses cannot be resumed safely",
                ));
            }
            let range = response
                .headers()
                .get(header::CONTENT_RANGE)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_content_range);
            if status == 206
                && (offset == 0
                    || !range.is_some_and(|(start, _, total)| {
                        start == offset
                            && job.spec.size.is_none_or(|size| size == total)
                            && partial.as_ref().is_some_and(|state| {
                                state.validator.matches(&response)
                                    && state.total.is_none_or(|expected| expected == total)
                            })
                    }))
            {
                offset = 0;
                partial = None;
                continue;
            }
            if status == 200 {
                offset = 0;
            }
            let total = if status == 206 {
                range.map(|(_, _, total)| total)
            } else {
                response.content_length()
            };
            if job
                .spec
                .size
                .zip(total)
                .is_some_and(|(expected, actual)| expected != actual)
            {
                return Err(Error::Integrity {
                    path: part.to_path_buf(),
                    reason: "response length disagrees with expected size".to_owned(),
                });
            }
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).create(true);
            if offset == 0 {
                options.truncate(true);
            } else {
                options.append(true);
            }
            require_plain(part, false)?;
            let file = options
                .open(part)
                .await
                .map_err(|error| io_error("open partial download", part, error))?;
            if let Some(validator) = response_validator(&response) {
                save_partial(
                    sidecar,
                    &PartialState {
                        source: source.clone(),
                        validator,
                        total,
                    },
                )?;
            } else {
                remove_optional(sidecar).await?;
            }
            self.stream_response(response, file, job, part, offset, total, cancel, events)
                .await?;
            if !self.verified(part, &job.spec, cancel).await? {
                remove_optional(part).await?;
                remove_optional(sidecar).await?;
                return Err(Error::Integrity {
                    path: part.to_path_buf(),
                    reason: "size or digest mismatch".to_owned(),
                });
            }
            return Ok(());
        }
        Err(Error::Integrity {
            path: part.to_path_buf(),
            reason: "server returned an inconsistent range response".to_owned(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn stream_response(
        &self,
        mut response: Response,
        mut file: tokio::fs::File,
        job: &DownloadJob,
        part: &Path,
        mut written: u64,
        total: Option<u64>,
        cancel: &CancellationToken,
        events: &broadcast::Sender<DownloadEvent>,
    ) -> Result<(), Error> {
        let limit = job.spec.size.unwrap_or(MAX_UNKNOWN_SIZE);
        loop {
            let next = tokio::select! {
                () = cancel.cancelled() => { file.sync_all().await.map_err(|error| io_error("preserve cancelled partial download", part, error))?; return Err(Error::Cancelled); },
                result = tokio::time::timeout(READ_TIMEOUT,response.chunk()) => result.map_err(|_| io_error("read download body", part, io::Error::new(io::ErrorKind::TimedOut,"download body stalled")))?.map_err(|source| Error::Request { url: diagnostic_url(&job.spec.url), source: source.without_url() })?,
            };
            let Some(chunk) = next else {
                break;
            };
            written = written
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| invalid("download byte counter overflow"))?;
            if written > limit {
                return Err(Error::Integrity {
                    path: part.to_path_buf(),
                    reason: "response exceeds expected size or download safety limit".to_owned(),
                });
            }
            file.write_all(&chunk)
                .await
                .map_err(|error| io_error("write partial download", part, error))?;
            emit(
                events,
                job,
                DownloadEventKind::Progress {
                    downloaded: written,
                    total: job.spec.size.or(total),
                },
            );
        }
        file.sync_all()
            .await
            .map_err(|error| io_error("sync partial download", part, error))?;
        Ok(())
    }
}

fn response_validator(response: &Response) -> Option<Validator> {
    if let Some(value) = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with('"') && value.ends_with('"'))
    {
        return Some(Validator::Etag(value.to_owned()));
    }
    response
        .headers()
        .get(header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(|value| Validator::LastModified(value.to_owned()))
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

fn load_partial(path: &Path) -> Result<Option<PartialState>, Error> {
    if !require_plain(path, false)? {
        return Ok(None);
    }
    let metadata = std::fs::metadata(path)
        .map_err(|error| io_error("inspect partial validator", path, error))?;
    if metadata.len() > 16 * 1024 {
        return Err(invalid("partial validator file exceeds its safety limit"));
    }
    let bytes =
        std::fs::read(path).map_err(|error| io_error("read partial validator", path, error))?;
    // A crash or an old-format sidecar loses resumability, never integrity.
    Ok(serde_json::from_slice(&bytes).ok())
}

fn save_partial(path: &Path, state: &PartialState) -> Result<(), Error> {
    require_plain(path, false)?;
    let parent = path
        .parent()
        .ok_or_else(|| invalid("partial validator has no parent"))?;
    let temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| io_error("create partial validator", parent, error))?;
    serde_json::to_writer(temporary.as_file(), state)
        .map_err(|_| invalid("cannot serialize partial validator"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| io_error("sync partial validator", temporary.path(), error))?;
    temporary
        .persist(path)
        .map_err(|error| io_error("commit partial validator", path, error.error))?;
    Ok(())
}

async fn remove_optional(path: &Path) -> Result<(), Error> {
    require_plain(path, false)?;
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(
            "remove obsolete download temporary file",
            path,
            error,
        )),
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
