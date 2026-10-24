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

#[cfg(unix)]
#[tokio::test]
async fn rejects_traversal_intermediate_links_and_parallel_root_writers() {
    let directory = tempfile::tempdir().expect("owned root");
    let outside = tempfile::tempdir().expect("outside root");
    let manager = manager(directory.path(), 1);
    let mut task = job("unsafe", "http://127.0.0.1:1/file", b"data");
    task.destination = "../escape".into();
    let (events, _) = broadcast::channel(128);
    assert!(
        manager
            .run(vec![task.clone()], CancellationToken::new(), events.clone())
            .await
            .is_err()
    );
    std::os::unix::fs::symlink(outside.path(), directory.path().join("linked"))
        .expect("fixture link");
    tokio::fs::create_dir(outside.path().join("child"))
        .await
        .expect("outside child");
    task.destination = "linked/child/file".into();
    assert!(
        manager
            .run(vec![task], CancellationToken::new(), events.clone())
            .await
            .is_err()
    );
    assert!(!outside.path().join("child/file").exists());
    let _held = manager.lock_run().expect("exclusive root lock");
    assert!(
        manager
            .run(Vec::new(), CancellationToken::new(), events)
            .await
            .is_err()
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
