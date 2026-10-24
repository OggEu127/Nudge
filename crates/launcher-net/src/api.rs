//! Bounded, redirect-free transport for credential-bearing service requests.
//!
//! Unlike downloads, authorization requests must never forward a body or bearer
//! token to a redirect destination. Response bodies and request headers are
//! deliberately absent from diagnostics.

use std::{fmt, sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use reqwest::{Client, Method, header};
use serde::Serialize;
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroize as _;

use crate::policy::{NetworkPolicy, PolicyResolver};

/// Failures from credential-bearing HTTP operations, without response bodies.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The request was explicitly cancelled.
    #[error("authentication request cancelled")]
    Cancelled,
    /// The client or a request could not be completed.
    #[error("authentication HTTP transport failed")]
    Transport,
    /// A request timed out, allowing device polling to reduce its frequency.
    #[error("authentication HTTP request timed out")]
    Timeout,
    /// The endpoint violates the selected network policy.
    #[error("authentication endpoint rejected by network policy")]
    Endpoint,
    /// The bounded response limit was exceeded.
    #[error("authentication response exceeded its size limit")]
    ResponseTooLarge,
    /// The request could not be encoded.
    #[error("authentication request could not be encoded")]
    Encoding,
    /// An explicit transport bound was invalid.
    #[error("authentication HTTP configuration has an invalid size or timeout bound")]
    Configuration,
}

/// HTTP response whose body is excluded from all debug representations.
pub struct ApiResponse {
    /// Numeric response status, including OAuth's expected non-success statuses.
    pub status: u16,
    body: Vec<u8>,
}

impl Drop for ApiResponse {
    fn drop(&mut self) {
        self.body.zeroize();
    }
}

impl ApiResponse {
    /// Returns sensitive response bytes. Never log or persist this value.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

impl fmt::Debug for ApiResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiResponse")
            .field("status", &self.status)
            .field("body", &"[REDACTED]")
            .finish()
    }
}

/// Secure transport for the small JSON and form bodies used by account APIs.
#[derive(Clone)]
pub struct ApiClient {
    client: Client,
    policy: NetworkPolicy,
    limit: usize,
}

impl ApiClient {
    /// Creates a public HTTPS transport, with redirects and proxies disabled.
    ///
    /// # Errors
    /// Returns an error if the HTTP stack cannot be initialized.
    pub fn new(user_agent: &str) -> Result<Self, Error> {
        Self::with_policy(user_agent, NetworkPolicy::PublicHttps, 1024 * 1024)
    }

    /// Creates an explicitly scoped transport, primarily for loopback tests.
    ///
    /// # Errors
    /// Returns an error for a zero limit or an unavailable HTTP stack.
    pub fn with_policy(
        user_agent: &str,
        policy: NetworkPolicy,
        limit: usize,
    ) -> Result<Self, Error> {
        Self::with_policy_and_timeout(user_agent, policy, limit, Duration::from_secs(30))
    }

    /// Creates an explicitly scoped and bounded transport for service adapters.
    ///
    /// A short injected deadline makes timeout and polling-recovery behavior
    /// testable without changing the production 30-second request timeout.
    ///
    /// # Errors
    /// Rejects zero limits, zero timeouts, timeouts over five minutes, and an
    /// unavailable HTTP stack.
    pub fn with_policy_and_timeout(
        user_agent: &str,
        policy: NetworkPolicy,
        limit: usize,
        timeout: Duration,
    ) -> Result<Self, Error> {
        if limit == 0 || timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(Error::Configuration);
        }
        let client = Client::builder()
            .user_agent(user_agent)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(PolicyResolver::new(policy)))
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self {
            client,
            policy,
            limit,
        })
    }

    /// Posts a URL-encoded body, without following redirects.
    ///
    /// # Errors
    /// Returns policy, cancellation, transport, or response-size failures.
    pub async fn post_form(
        &self,
        url: &Url,
        fields: &[(&str, &str)],
        cancel: &CancellationToken,
    ) -> Result<ApiResponse, Error> {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields.iter().copied())
            .finish();
        self.request(
            Method::POST,
            url,
            Some(("application/x-www-form-urlencoded", encoded.into_bytes())),
            None,
            cancel,
        )
        .await
    }

    /// Posts a small JSON body, without following redirects.
    ///
    /// # Errors
    /// Returns encoding, policy, cancellation, transport, or size failures.
    pub async fn post_json<T: Serialize + Sync>(
        &self,
        url: &Url,
        body: &T,
        cancel: &CancellationToken,
    ) -> Result<ApiResponse, Error> {
        let encoded = serde_json::to_vec(body).map_err(|_| Error::Encoding)?;
        self.request(
            Method::POST,
            url,
            Some(("application/json", encoded)),
            None,
            cancel,
        )
        .await
    }

    /// Fetches a bearer-authenticated response, without following redirects.
    ///
    /// # Errors
    /// Returns policy, cancellation, transport, or response-size failures.
    pub async fn get_bearer(
        &self,
        url: &Url,
        token: &str,
        cancel: &CancellationToken,
    ) -> Result<ApiResponse, Error> {
        self.request(Method::GET, url, None, Some(token), cancel)
            .await
    }

    async fn request(
        &self,
        method: Method,
        url: &Url,
        body: Option<(&str, Vec<u8>)>,
        token: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<ApiResponse, Error> {
        self.policy.validate_url(url).map_err(|_| Error::Endpoint)?;
        if url.query().is_some() && token.is_none() {
            // Authentication secrets belong in request bodies, never query logs.
            return Err(Error::Endpoint);
        }
        let mut request = self
            .client
            .request(method, url.clone())
            .header(header::ACCEPT, "application/json")
            .header("x-xbl-contract-version", "1");
        if let Some((kind, data)) = body {
            request = request.header(header::CONTENT_TYPE, kind).body(data);
        }
        if let Some(token) = token {
            let mut value = header::HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|_| Error::Encoding)?;
            value.set_sensitive(true);
            request = request.header(header::AUTHORIZATION, value);
        }
        let operation = async {
            let response = request
                .send()
                .await
                .map_err(|error| transport_error(&error))?;
            let status = response.status().as_u16();
            if response
                .content_length()
                .is_some_and(|size| size > self.limit as u64)
            {
                return Err(Error::ResponseTooLarge);
            }
            let mut body = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| transport_error(&error))?;
                if chunk.len() > self.limit.saturating_sub(body.len()) {
                    return Err(Error::ResponseTooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(ApiResponse { status, body })
        };
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(Error::Cancelled),
            response = operation => response,
        }
    }
}

fn transport_error(error: &reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path},
    };

    #[tokio::test]
    async fn secrets_are_not_forwarded_through_redirects() -> Result<(), Box<dyn std::error::Error>>
    {
        let original = MockServer::start().await;
        let other = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", other.uri()))
            .expect(1)
            .mount(&original)
            .await;
        let client = ApiClient::with_policy("Nudge-test", NetworkPolicy::LoopbackOnly, 1024)?;
        let response = client
            .post_form(
                &Url::parse(&original.uri())?,
                &[("refresh_token", "private-fixture-secret")],
                &CancellationToken::new(),
            )
            .await?;
        assert_eq!(response.status, 307);
        assert!(
            other
                .received_requests()
                .await
                .ok_or("missing request recorder")?
                .is_empty()
        );
        assert!(!format!("{response:?}").contains("private-fixture-secret"));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_oversized_bodies_and_cancelled_or_disallowed_requests()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = MockServer::start().await;
        Mock::given(path("/large"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 2048]))
            .mount(&server)
            .await;
        let client = ApiClient::with_policy("Nudge-test", NetworkPolicy::LoopbackOnly, 1024)?;
        let url = Url::parse(&format!("{}/large", server.uri()))?;
        assert!(matches!(
            client
                .get_bearer(&url, "fixture", &CancellationToken::new())
                .await,
            Err(Error::ResponseTooLarge)
        ));
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            client.get_bearer(&url, "fixture", &cancelled).await,
            Err(Error::Cancelled)
        ));
        let public = ApiClient::new("Nudge-test")?;
        assert!(matches!(
            public
                .get_bearer(&url, "fixture", &CancellationToken::new())
                .await,
            Err(Error::Endpoint)
        ));
        let mut query = url;
        query.set_query(Some("refresh_token=fixture"));
        assert!(matches!(
            client
                .post_form(&query, &[], &CancellationToken::new())
                .await,
            Err(Error::Endpoint)
        ));
        Ok(())
    }
}
