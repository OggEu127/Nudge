use std::{
    num::NonZeroUsize,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::broadcast,
};
use tokio_util::sync::CancellationToken;
use url::Url;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

use super::{DownloadEventKind, DownloadJob, DownloadManager};
use crate::{
    ReqwestHttpClient,
    policy::NetworkPolicy,
    transfer::{FileSpec, Hashes},
};

fn client() -> ReqwestHttpClient {
    ReqwestHttpClient::new_with_policy("Nudge/test", 1024, NetworkPolicy::LoopbackOnly)
        .expect("test client")
}

fn manager(root: &std::path::Path, concurrency: usize) -> DownloadManager {
    DownloadManager::with_http(
        root.to_path_buf(),
        NonZeroUsize::new(concurrency).expect("positive concurrency"),
        client(),
    )
    .expect("download manager")
}

fn job(id: &str, url: &str, bytes: &[u8]) -> DownloadJob {
    DownloadJob {
        id: id.to_owned(),
        spec: FileSpec {
            url: Url::parse(url).expect("fixture URL"),
            size: Some(bytes.len() as u64),
            hashes: Hashes {
                sha256: Some(hex::encode(Sha256::digest(bytes))),
                ..Hashes::default()
            },
        },
        mirrors: Vec::new(),
        destination: format!("files/{id}.bin").into(),
        priority: 0,
    }
}

// Keep paused time from auto-advancing while real loopback I/O or the blocking
// filesystem pool is pending. Tests advance only after observing each chunk.
struct ManualClock(tokio::task::JoinHandle<()>);

impl ManualClock {
    fn new() -> Self {
        Self(tokio::spawn(async {
            loop {
                tokio::task::yield_now().await;
            }
        }))
    }
}

impl Drop for ManualClock {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test(start_paused = true)]
async fn slow_stream_survives_five_minutes_without_retrying()
-> Result<(), Box<dyn std::error::Error>> {
    let _clock = ManualClock::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (chunks, mut next_chunk) = tokio::sync::mpsc::channel::<()>(1);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = [0; 4096];
        let received = socket.read(&mut buffer).await?;
        assert!(received > 0, "client request must arrive before response");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 18\r\nConnection: close\r\n\r\n")
            .await?;
        for _ in 0..18 {
            next_chunk.recv().await.expect("send next chunk");
            socket.write_all(b"x").await?;
        }
        Ok::<_, std::io::Error>(())
    });
    let directory = tempfile::tempdir()?;
    let manager = manager(directory.path(), 1);
    let artifact = job("slow", &format!("http://{address}/slow"), &[b'x'; 18]);
    let (events, mut receiver) = broadcast::channel(128);
    let run = manager.run(vec![artifact.clone()], CancellationToken::new(), events);
    tokio::pin!(run);
    let start = tokio::time::Instant::now();
    let mut attempts = 0;
    for received in 1..=17 {
        if received > 1 {
            tokio::time::advance(Duration::from_secs(20)).await;
        }
        chunks.send(()).await?;
        loop {
            tokio::select! {
                result = &mut run => panic!("transfer ended before chunk {received}: {result:?}"),
                event = receiver.recv() => match event?.kind {
                    DownloadEventKind::Started { .. } => attempts += 1,
                    DownloadEventKind::Retrying { .. } => panic!("active stream must not retry"),
                    DownloadEventKind::Progress { downloaded, .. } if downloaded == received => break,
                    _ => {},
                }
            }
        }
    }
    chunks.send(()).await?;
    run.await?;
    server.await??;
    assert!(start.elapsed() > Duration::from_secs(300));
    assert_eq!(attempts, 1);
    assert_eq!(
        tokio::fs::read(directory.path().join(&artifact.destination)).await?,
        [b'x'; 18]
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn stalled_stream_still_times_out_and_can_be_cancelled_during_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let _clock = ManualClock::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut buffer = [0; 4096];
        let received = socket.read(&mut buffer).await?;
        assert!(received > 0, "client request must arrive before response");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nETag: \"v1\"\r\n\r\nx")
            .await?;
        let _ = stopped.await;
        Ok::<_, std::io::Error>(())
    });
    let directory = tempfile::tempdir()?;
    let manager = manager(directory.path(), 1);
    let cancel = CancellationToken::new();
    let (events, mut receiver) = broadcast::channel(128);
    let run = manager.run(
        vec![job("stalled", &format!("http://{address}/stalled"), b"xx")],
        cancel.clone(),
        events,
    );
    tokio::pin!(run);
    loop {
        tokio::select! {
            result = &mut run => panic!("unexpected completion: {result:?}"),
            event = receiver.recv() => if matches!(event?.kind, DownloadEventKind::Progress { downloaded: 1, .. }) { break; },
        }
    }
    tokio::time::advance(Duration::from_secs(31)).await;
    loop {
        tokio::select! {
            result = &mut run => panic!("unexpected completion: {result:?}"),
            event = receiver.recv() => if matches!(event?.kind, DownloadEventKind::Retrying { .. }) { break; },
        }
    }
    cancel.cancel();
    assert!(matches!(run.await, Err(crate::Error::Cancelled)));
    assert!(!directory.path().join("files/stalled.bin").exists());
    let _ = stop.send(());
    server.await??;
    Ok(())
}

#[tokio::test]
async fn release_is_atomic_scoped_idempotent_and_prevents_task_resurrection()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir()?;
    let manager = manager(directory.path(), 1);
    let retired = job("retired", &server.uri(), b"old");
    let shared = job("shared", &server.uri(), b"shared");
    // Cancellation persists the plan without needing a network attempt.
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let (events, _) = broadcast::channel(128);
    assert!(
        manager
            .run(
                vec![retired.clone(), shared.clone()],
                cancelled,
                events.clone()
            )
            .await
            .is_err()
    );
    let mut stale = retired.clone();
    stale.spec = job("stale", &server.uri(), b"new").spec;
    assert!(
        manager
            .release_destinations(&[shared.clone(), stale])
            .is_err()
    );
    assert_eq!(
        manager
            .database()?
            .query_row("SELECT count(*) FROM destinations", [], |row| row
                .get::<_, i64>(0))?,
        2
    );
    let lock = manager.lock_run()?;
    assert!(
        manager
            .release_destinations(std::slice::from_ref(&retired))
            .is_err()
    );
    drop(lock);
    std::fs::create_dir_all(directory.path().join("files"))?;
    std::fs::write(directory.path().join(&retired.destination), b"user data")?;
    assert_eq!(
        manager.release_destinations(std::slice::from_ref(&retired))?,
        1
    );
    assert_eq!(
        manager.release_destinations(std::slice::from_ref(&retired))?,
        0
    );
    assert_eq!(
        std::fs::read(directory.path().join(&retired.destination))?,
        b"user data"
    );
    assert_eq!(
        manager
            .database()?
            .query_row("SELECT id FROM downloads", [], |row| row
                .get::<_, String>(0))?,
        shared.id
    );
    // Retire the other pending task. Resume must now make zero requests.
    manager.release_destinations(&[shared])?;
    manager
        .resume_pending(CancellationToken::new(), events.clone())
        .await?;
    assert!(
        server
            .received_requests()
            .await
            .expect("request history")
            .is_empty()
    );
    let mut replacement = job("replacement", &server.uri(), b"new");
    replacement.destination = retired.destination;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        manager.run(vec![replacement], cancelled, events).await,
        Err(crate::Error::Cancelled)
    ));
    Ok(())
}

#[test]
fn ancestor_checks_accept_canonical_roots_and_missing_descendants() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let nested = directory.path().join("中文 path");
    std::fs::create_dir(&nested).expect("fixture directory");
    // On Windows, canonicalize returns a verbatim (\\?\) path. The former
    // component walk tried to read metadata for the incomplete \\?\C: prefix.
    let canonical = std::fs::canonicalize(&nested).expect("canonical fixture");
    let root = canonical.ancestors().last().expect("absolute root");
    for path in [&nested, &canonical, root] {
        super::check_ancestors(path).expect("valid ancestor chain");
    }
    let missing = canonical.join("not-created/child");
    super::check_ancestors(&missing).expect("missing descendants are allowed");
    assert!(!canonical.join("not-created").exists());
    let manager = manager(&canonical, 1);
    assert!(manager.private.join("tasks.sqlite3").is_file());
}

#[tokio::test]
async fn every_backup_source_can_be_used_and_content_reservations_are_immutable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/valid"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .expect(4)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temp directory");
    let manager = manager(directory.path(), 2);
    let mut artifact = job("backup", &format!("{}/missing", server.uri()), b"valid");
    artifact.mirrors = (0..3)
        .map(|_| Url::parse(&format!("{}/missing", server.uri())).expect("mirror URL"))
        .collect();
    artifact
        .mirrors
        .push(Url::parse(&format!("{}/valid", server.uri())).expect("last mirror"));
    let (events, _) = broadcast::channel(128);
    manager
        .run(
            vec![artifact.clone()],
            CancellationToken::new(),
            events.clone(),
        )
        .await
        .expect("fifth source succeeds");
    let mut conflicting = job(
        "another-instance",
        &format!("{}/valid", server.uri()),
        b"other",
    );
    conflicting.destination = artifact.destination.clone();
    let error = manager
        .run(vec![conflicting], CancellationToken::new(), events)
        .await
        .expect_err("shared destination digest conflict");
    assert!(error.to_string().contains("different expected content"));
    assert_eq!(
        tokio::fs::read(directory.path().join(artifact.destination))
            .await
            .expect("unchanged destination"),
        b"valid"
    );
}

#[tokio::test]
async fn cache_is_verified_and_shared_content_is_fetched_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"content".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let manager = manager(directory.path(), 2);
    let jobs = vec![
        job("one", &server.uri(), b"content"),
        job("two", &server.uri(), b"content"),
    ];
    let (sender, _) = broadcast::channel(128);
    manager
        .run(jobs.clone(), CancellationToken::new(), sender.clone())
        .await
        .expect("download jobs");
    for job in &jobs {
        assert_eq!(
            tokio::fs::read(directory.path().join(&job.destination))
                .await
                .expect("installed bytes"),
            b"content"
        );
    }
    manager
        .run(jobs, CancellationToken::new(), sender)
        .await
        .expect("validated reuse");
}

#[tokio::test]
async fn checksum_failure_never_replaces_existing_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("wrong"))
        .expect(4)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let manager = manager(directory.path(), 1);
    let job = job("safe", &server.uri(), b"right");
    tokio::fs::create_dir(directory.path().join("files"))
        .await
        .expect("fixture directory");
    tokio::fs::write(directory.path().join(&job.destination), b"prior")
        .await
        .expect("fixture bytes");
    let (events, _) = broadcast::channel(128);
    assert!(
        manager
            .run(vec![job.clone()], CancellationToken::new(), events)
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read(directory.path().join(&job.destination))
            .await
            .expect("prior bytes"),
        b"prior"
    );
}

#[tokio::test]
async fn retries_transient_status_but_not_permanent_status() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let counted = attempts.clone();
    Mock::given(method("GET"))
        .and(path("/transient"))
        .respond_with(move |_: &wiremock::Request| {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(503)
            } else {
                ResponseTemplate::new(200).set_body_string("valid")
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/missing"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let manager = manager(directory.path(), 1);
    let (events, _) = broadcast::channel(128);
    manager
        .run(
            vec![job(
                "transient",
                &format!("{}/transient", server.uri()),
                b"valid",
            )],
            CancellationToken::new(),
            events.clone(),
        )
        .await
        .expect("transient recovery");
    let error = manager
        .run(
            vec![job(
                "permanent",
                &format!("{}/missing", server.uri()),
                b"never",
            )],
            CancellationToken::new(),
            events,
        )
        .await
        .expect_err("permanent error");
    assert!(matches!(
        error,
        crate::Error::HttpStatus { status: 404, .. }
    ));
}

#[tokio::test]
async fn cancelled_queue_survives_database_reopen_and_resumes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("valid"))
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let first = manager(directory.path(), 1);
    let (events, _) = broadcast::channel(128);
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        first
            .run(
                vec![job("recover", &server.uri(), b"valid")],
                cancellation,
                events.clone()
            )
            .await,
        Err(crate::Error::Cancelled)
    ));
    drop(first);
    manager(directory.path(), 1)
        .resume_pending(CancellationToken::new(), events)
        .await
        .expect("recovered queue");
    assert_eq!(
        tokio::fs::read(directory.path().join("files/recover.bin"))
            .await
            .expect("recovered bytes"),
        b"valid"
    );
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut header = Vec::new();
    let mut byte = [0];
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 16 * 1024, "bounded request headers");
        assert_eq!(stream.read(&mut byte).await.expect("read request"), 1);
        header.push(byte[0]);
    }
    String::from_utf8(header).expect("ASCII request")
}

#[tokio::test]
async fn interrupted_response_resumes_only_with_matching_validator_and_range() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.expect("first connection");
        let request = read_request(&mut first).await;
        assert!(!request.to_ascii_lowercase().contains("range:"));
        first.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nETag: \"stable\"\r\nConnection: close\r\n\r\nabcde").await.expect("partial response");
        first.shutdown().await.expect("interrupt response");
        drop(first);
        let (mut second, _) = listener.accept().await.expect("resume connection");
        let request = read_request(&mut second).await.to_ascii_lowercase();
        assert!(request.contains("range: bytes=5-\r\n"), "{request}");
        assert!(request.contains("if-range: \"stable\"\r\n"));
        second.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 5\r\nContent-Range: bytes 5-9/10\r\nETag: \"stable\"\r\nConnection: close\r\n\r\nfghij").await.expect("resumed response");
    });
    let directory = tempfile::tempdir().expect("temporary directory");
    let (events, _) = broadcast::channel(128);
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        manager(directory.path(), 1).run(
            vec![job(
                "resume",
                &format!("http://{address}/file"),
                b"abcdefghij",
            )],
            CancellationToken::new(),
            events,
        ),
    )
    .await
    .expect("bounded resume");
    result.expect("resumed download");
    server.await.expect("server task");
    assert_eq!(
        tokio::fs::read(directory.path().join("files/resume.bin"))
            .await
            .expect("downloaded bytes"),
        b"abcdefghij"
    );
}

#[tokio::test]
async fn ignored_range_restarts_without_appending() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"new\"")
                .set_body_string("abcdefghij"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let manager = manager(directory.path(), 1);
    let job = job("restart", &server.uri(), b"abcdefghij");
    let part = manager.private.join("partials").join(format!(
        "{}.part",
        super::partial_key(&job).expect("partial key")
    ));
    tokio::fs::write(&part, b"abcde")
        .await
        .expect("old partial");
    super::save_partial(
        &part.with_extension("json"),
        &super::PartialState {
            source: job.spec.url.clone(),
            validator: super::Validator::Etag("\"old\"".to_owned()),
            total: Some(10),
        },
    )
    .expect("old validator");
    let (events, _) = broadcast::channel(128);
    manager
        .run(vec![job], CancellationToken::new(), events)
        .await
        .expect("clean restart");
    assert_eq!(
        tokio::fs::read(directory.path().join("files/restart.bin"))
            .await
            .expect("installed bytes"),
        b"abcdefghij"
    );
}

#[tokio::test]
async fn priority_and_concurrency_bound_requests() {
    let server = MockServer::start().await;
    let starts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed = starts.clone();
    Mock::given(method("GET"))
        .respond_with(move |request: &wiremock::Request| {
            observed
                .lock()
                .expect("observations")
                .push((request.url.path().to_owned(), std::time::Instant::now()));
            ResponseTemplate::new(200)
                .set_body_string(request.url.path())
                .set_delay(Duration::from_millis(200))
        })
        .expect(4)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let mut jobs = Vec::new();
    for priority in 0..4 {
        let path = format!("/{priority}");
        let mut task = job(
            &priority.to_string(),
            &format!("{}{path}", server.uri()),
            path.as_bytes(),
        );
        task.priority = priority;
        jobs.push(task);
    }
    let (events, _) = broadcast::channel(128);
    manager(directory.path(), 2)
        .run(jobs, CancellationToken::new(), events)
        .await
        .expect("bounded queue");
    let starts = starts.lock().expect("observations");
    assert_eq!(starts.len(), 4);
    let first: std::collections::BTreeSet<_> =
        starts[..2].iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(first, std::collections::BTreeSet::from(["/3", "/2"]));
    assert!(
        starts[2].1.duration_since(starts[0].1) >= Duration::from_millis(150),
        "a third request started before an active slot completed"
    );
}

#[tokio::test]
async fn cancellation_during_request_is_observable_and_recoverable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("valid")
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().expect("temporary directory");
    let manager = manager(directory.path(), 1);
    let (events, mut receiver) = broadcast::channel::<super::DownloadEvent>(128);
    let cancel = CancellationToken::new();
    let request_cancel = cancel.clone();
    let observe = tokio::spawn(async move {
        while let Ok(event) = receiver.recv().await {
            if matches!(event.kind, DownloadEventKind::Started { .. }) {
                request_cancel.cancel();
                break;
            }
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        manager.run(vec![job("cancel", &server.uri(), b"valid")], cancel, events),
    )
    .await
    .expect("prompt cancellation");
    assert!(matches!(result, Err(crate::Error::Cancelled)));
    observe.await.expect("observer");
    let database = manager.database().expect("queue database");
    let state: String = database
        .query_row("SELECT state FROM downloads WHERE id='cancel'", [], |row| {
            row.get(0)
        })
        .expect("persisted task");
    assert_eq!(state, "pending");
}
