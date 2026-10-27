//! Network primitives shared by all Nudge modules.
//!
//! All outbound requests use explicit URL, redirect, and DNS-address policies.

#[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
compile_error!("Nudge supports Windows x86-64 only");

use std::{future::Future, sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, redirect};
use thiserror::Error;
use tracing::{debug, instrument};
use url::Url;

pub mod api;
pub mod engine;
pub mod paths;
pub mod persistence;
pub mod policy;
pub mod transfer;

use policy::{NetworkPolicy, PolicyResolver, diagnostic_url};

/// Default maximum size accepted by [`ReqwestHttpClient`].
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// Errors produced by the network boundary.
#[derive(Debug, Error)]
pub enum Error {
    /// Download execution was cancelled; its persistent state remains recoverable.
    #[error("download operation cancelled")]
    Cancelled,
    /// The persistent task store could not be read or updated.
    #[error("download task database operation failed")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem operation failed during a verified transfer.
    #[error("failed to {operation} `{path}`")]
    Io {
        /// Attempted operation.
        operation: &'static str,
        /// Relevant path.
        path: std::path::PathBuf,
        /// Filesystem cause.
        #[source]
        source: std::io::Error,
    },
    /// The downloaded file did not match its expected integrity information.
    #[error("integrity check failed for `{path}`: {reason}")]
    Integrity {
        /// Relevant file.
        path: std::path::PathBuf,
        /// Verification failure.
        reason: String,
    },
    /// A transfer lacks mandatory integrity information.
    #[error("invalid download specification: {0}")]
    InvalidSpec(String),
    /// The HTTP client could not be constructed.
    #[error("failed to construct HTTP client")]
    ClientBuild(#[source] reqwest::Error),

    /// A URL violates the outbound request policy.
    #[error("URL `{url}` is not allowed: {reason}")]
    UrlRejected {
        /// Rejected URL.
        url: Url,
        /// Human-readable policy reason.
        reason: &'static str,
    },

    /// The request could not be completed.
    #[error("request to `{url}` failed")]
    Request {
        /// Requested URL.
        url: Url,
        /// Underlying HTTP error.
        #[source]
        source: reqwest::Error,
    },

    /// The server returned a non-successful status.
    #[error("request to `{url}` returned HTTP {status}")]
    HttpStatus {
        /// Final response URL.
        url: Url,
        /// Numeric HTTP status code.
        status: u16,
    },

    /// The response exceeded its configured bound.
    #[error("response from `{url}` exceeded the {limit}-byte limit")]
    ResponseTooLarge {
        /// Final response URL.
        url: Url,
        /// Configured byte limit.
        limit: usize,
    },
}

/// Minimal asynchronous HTTP interface used by higher-level parsers.
///
/// Returning an explicit `Send` future keeps callers usable from Tokio's
/// multi-threaded scheduler without requiring an async-trait macro.
pub trait HttpClient: Send + Sync {
    /// Fetches a response body while enforcing the implementation's size limit.
    fn get_bytes<'a>(
        &'a self,
        url: &'a Url,
    ) -> impl Future<Output = Result<Vec<u8>, Error>> + Send + 'a;
}

/// A reqwest-backed HTTP client with conservative URL and response limits.
#[derive(Clone, Debug)]
pub struct ReqwestHttpClient {
    inner: Client,
    max_response_bytes: usize,
    policy: NetworkPolicy,
}

impl ReqwestHttpClient {
    /// Creates a client restricted to public HTTPS destinations on port 443.
    ///
    /// DNS responses are validated and the validated addresses are passed
    /// directly to the connector. Automatic environment/system proxies are
    /// disabled: remote proxy DNS cannot provide this address guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ClientBuild`] if the platform TLS or resolver setup
    /// prevents reqwest from constructing a client.
    pub fn new(user_agent: &str, max_response_bytes: usize) -> Result<Self, Error> {
        Self::new_with_policy(user_agent, max_response_bytes, NetworkPolicy::PublicHttps)
    }

    /// Creates a client with an explicitly selected outbound policy.
    ///
    /// [`NetworkPolicy::LoopbackOnly`] is intended for injected test servers.
    /// It cannot contact public services, including through a redirect.
    ///
    /// # Errors
    /// Returns an error if TLS or client configuration cannot be initialized.
    pub fn new_with_policy(
        user_agent: &str,
        max_response_bytes: usize,
        policy: NetworkPolicy,
    ) -> Result<Self, Error> {
        let redirect_policy = redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("redirect limit exceeded");
            }

            if let Err(error) = policy.validate_url(attempt.url()) {
                return attempt.error(error);
            }

            attempt.follow()
        });

        let inner = Client::builder()
            .user_agent(user_agent)
            .connect_timeout(Duration::from_secs(10))
            // Artifact bodies may stream for hours. Bound inactivity instead
            // of elapsed transfer time; metadata adds its own total deadline.
            .read_timeout(Duration::from_secs(30))
            .no_proxy()
            .dns_resolver(Arc::new(PolicyResolver::new(policy)))
            .redirect(redirect_policy)
            .build()
            .map_err(Error::ClientBuild)?;

        Ok(Self {
            inner,
            max_response_bytes,
            policy,
        })
    }

    /// Validates an initial or final URL under this client's outbound policy.
    pub(crate) fn validate_url(&self, url: &Url) -> Result<(), Error> {
        self.policy.validate_url(url)
    }
}

impl HttpClient for ReqwestHttpClient {
    #[instrument(skip(self, url), fields(host = url.host_str().unwrap_or_default()))]
    async fn get_bytes(&self, url: &Url) -> Result<Vec<u8>, Error> {
        self.validate_url(url)?;

        let response = self
            .inner
            .get(url.clone())
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|source| Error::Request {
                url: diagnostic_url(url),
                source: source.without_url(),
            })?;
        let final_url = response.url().clone();
        self.validate_url(&final_url)?;
        let final_url = diagnostic_url(&final_url);

        if !response.status().is_success() {
            return Err(Error::HttpStatus {
                url: final_url,
                status: response.status().as_u16(),
            });
        }

        if response
            .content_length()
            .is_some_and(|size| size > self.max_response_bytes as u64)
        {
            return Err(Error::ResponseTooLarge {
                url: final_url,
                limit: self.max_response_bytes,
            });
        }

        let mut body = Vec::with_capacity(
            response
                .content_length()
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(0)
                .min(self.max_response_bytes),
        );
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| Error::Request {
                url: final_url.clone(),
                source: source.without_url(),
            })?;
            let next_len =
                body.len()
                    .checked_add(chunk.len())
                    .ok_or_else(|| Error::ResponseTooLarge {
                        url: final_url.clone(),
                        limit: self.max_response_bytes,
                    })?;
            if next_len > self.max_response_bytes {
                return Err(Error::ResponseTooLarge {
                    url: final_url,
                    limit: self.max_response_bytes,
                });
            }
            body.extend_from_slice(&chunk);
        }

        debug!(bytes = body.len(), "HTTP response received");
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, HttpClient as _, ReqwestHttpClient, policy::NetworkPolicy};
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn rejects_remote_plain_http() {
        let client = ReqwestHttpClient::new("Nudge/test", 1024).expect("client should build");
        let url = Url::parse("http://example.com/data").expect("fixture URL should parse");

        let error = client
            .get_bytes(&url)
            .await
            .expect_err("remote HTTP must be rejected");

        assert!(matches!(error, Error::UrlRejected { .. }));
    }

    #[tokio::test]
    async fn production_client_rejects_loopback_before_sending_a_request() {
        let server = MockServer::start().await;
        let client = ReqwestHttpClient::new("Nudge/test", 1024).expect("client");
        assert!(matches!(
            client
                .get_bytes(&Url::parse(&server.uri()).expect("server URL"))
                .await,
            Err(Error::UrlRejected { .. })
        ));
        assert_eq!(server.received_requests().await.expect("requests").len(), 0);
    }

    #[tokio::test]
    async fn explicit_loopback_policy_follows_safe_relative_redirects() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/first"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/final"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/final"))
            .respond_with(ResponseTemplate::new(200).set_body_string("verified destination"))
            .mount(&server)
            .await;
        let client =
            ReqwestHttpClient::new_with_policy("Nudge/test", 1024, NetworkPolicy::LoopbackOnly)
                .expect("client");
        let body = client
            .get_bytes(&Url::parse(&format!("{}/first", server.uri())).expect("URL"))
            .await
            .expect("safe redirect");
        assert_eq!(body, b"verified destination");
    }

    #[tokio::test]
    async fn redirects_cannot_escape_the_selected_network_scope() {
        let server = MockServer::start().await;
        for (endpoint, target) in [
            ("/private", "https://10.0.0.1/secret"),
            ("/public", "https://example.com/secret"),
        ] {
            Mock::given(method("GET"))
                .and(path(endpoint))
                .respond_with(ResponseTemplate::new(302).insert_header("Location", target))
                .mount(&server)
                .await;
        }
        let client =
            ReqwestHttpClient::new_with_policy("Nudge/test", 1024, NetworkPolicy::LoopbackOnly)
                .expect("client");
        for endpoint in ["/private", "/public"] {
            assert!(
                client
                    .get_bytes(&Url::parse(&format!("{}{endpoint}", server.uri())).expect("URL"))
                    .await
                    .is_err()
            );
        }
        assert_eq!(server.received_requests().await.expect("requests").len(), 2);
    }

    #[tokio::test]
    async fn redirect_loops_stop_at_the_configured_limit() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
            .mount(&server)
            .await;
        let client =
            ReqwestHttpClient::new_with_policy("Nudge/test", 1024, NetworkPolicy::LoopbackOnly)
                .expect("client");
        assert!(
            client
                .get_bytes(&Url::parse(&server.uri()).expect("URL"))
                .await
                .is_err()
        );
        assert!(server.received_requests().await.expect("requests").len() <= 10);
    }

    #[tokio::test]
    async fn failure_diagnostics_never_disclose_signed_download_queries() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/failed"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_string("too large"))
            .mount(&server)
            .await;
        let client =
            ReqwestHttpClient::new_with_policy("Nudge/test", 4, NetworkPolicy::LoopbackOnly)
                .expect("client");
        for endpoint in ["failed", "large"] {
            let url = Url::parse(&format!(
                "{}/{endpoint}?signature=never-log-this",
                server.uri()
            ))
            .expect("URL");
            let error = client
                .get_bytes(&url)
                .await
                .expect_err("HTTP or response-size failure");
            assert!(!format!("{error:?} {error}").contains("never-log-this"));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let address = listener.local_addr().expect("local address");
        drop(listener);
        let url =
            Url::parse(&format!("http://{address}/file?signature=never-log-this")).expect("URL");
        let error = client.get_bytes(&url).await.expect_err("closed listener");
        assert!(!format!("{error:?} {error}").contains("never-log-this"));
        assert!(matches!(error, Error::Request { .. }));
    }
}
