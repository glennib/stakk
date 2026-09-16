//! HTTP transport seam for the Forgejo forge and the forge probe.
//!
//! [`Transport`] is one method — take an `http::Request<String>`, hand back
//! an `http::Response<String>` — mirroring [`crate::jj::runner::JjRunner`]
//! and the `gh` runner in `auth`: the forge builds every request itself and
//! never sees a socket, so its unit tests give it a mock that replays canned
//! responses and records what was requested. [`ReqwestTransport`] is the
//! real one.

use std::time::Duration;

use thiserror::Error;

/// A failure below the HTTP layer: connection refused, DNS, TLS, a body that
/// could not be read. An HTTP error *status* is not a transport error — the
/// response is returned as is and the forge maps the status.
#[derive(Debug, Error)]
#[error("HTTP transport failure: {source}")]
pub struct TransportError {
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl TransportError {
    pub fn new(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            source: Box::new(source),
        }
    }
}

impl From<reqwest::Error> for TransportError {
    fn from(error: reqwest::Error) -> Self {
        Self::new(error)
    }
}

/// Sends one HTTP request and returns the response, whatever its status.
pub trait Transport: Send + Sync {
    fn send(
        &self,
        request: http::Request<String>,
    ) -> impl std::future::Future<Output = Result<http::Response<String>, TransportError>> + Send;
}

/// `User-Agent` sent with every request.
const USER_AGENT: &str = concat!("stakk/", env!("CARGO_PKG_VERSION"));

/// How long one forge-probe request may take in total — connect, TLS,
/// headers and body. Generous for a reachable host, short enough that an
/// unreachable one does not stall a submission.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The real transport, over a [`reqwest::Client`].
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    /// Build the client the forge talks through. No network activity happens
    /// here.
    pub fn new() -> Result<Self, TransportError> {
        Self::build(|builder| builder)
    }

    /// Build the client the forge probe uses: it never follows a redirect — a
    /// `3xx` is an answer about the host, not a hop to take — and gives up
    /// after [`PROBE_TIMEOUT`].
    pub fn probing() -> Result<Self, TransportError> {
        Self::build(|builder| {
            builder
                .redirect(reqwest::redirect::Policy::none())
                .timeout(PROBE_TIMEOUT)
        })
    }

    fn build(
        configure: impl FnOnce(reqwest::ClientBuilder) -> reqwest::ClientBuilder,
    ) -> Result<Self, TransportError> {
        install_crypto_provider();
        let client = configure(reqwest::Client::builder().user_agent(USER_AGENT)).build()?;
        Ok(Self { client })
    }
}

/// Make `ring` the process-level rustls crypto provider unless one is
/// installed already.
///
/// reqwest's `rustls-no-provider` feature links rustls without choosing a
/// backend, and its `Client::build()` panics when no process-level default
/// is installed — it does *not* fall back to rustls's own resolution from
/// crate features. octocrab's hyper-rustls does use that resolution, but only
/// when it first builds a client, which a Forgejo-only run never does. `ring`
/// is the one provider in the dependency tree (octocrab links it), so naming
/// it here changes nothing about which code does the crypto; it only runs the
/// installation before reqwest asks. A provider installed earlier — octocrab
/// first, or a second transport — makes `install_default` return `Err`, which
/// is the state wanted and is ignored.
fn install_crypto_provider() {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

impl Transport for ReqwestTransport {
    async fn send(
        &self,
        request: http::Request<String>,
    ) -> Result<http::Response<String>, TransportError> {
        let (parts, body) = request.into_parts();
        let mut builder = self
            .client
            .request(parts.method, parts.uri.to_string())
            .headers(parts.headers);
        if !body.is_empty() {
            builder = builder.body(body);
        }

        let response = builder.send().await?;
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await?;

        let mut converted = http::Response::new(text);
        *converted.status_mut() = status;
        *converted.headers_mut() = headers;
        Ok(converted)
    }
}

/// Test doubles for [`Transport`], shared by the Forgejo forge's unit tests
/// and by every other module that speaks through the seam.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::HashMap;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::Transport;
    use super::TransportError;

    /// A `Transport` that answers from a queue of canned responses and keeps
    /// every request it was handed. Running out of canned responses is a
    /// test bug and panics.
    pub(crate) struct MockTransport {
        responses: Mutex<VecDeque<http::Response<String>>>,
        pub(crate) requests: Mutex<Vec<http::Request<String>>>,
    }

    impl MockTransport {
        pub(crate) fn new(responses: Vec<http::Response<String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn remaining_responses(&self) -> usize {
            self.responses.lock().unwrap().len()
        }
    }

    impl Transport for MockTransport {
        fn send(
            &self,
            request: http::Request<String>,
        ) -> impl std::future::Future<Output = Result<http::Response<String>, TransportError>> + Send
        {
            self.requests.lock().unwrap().push(request);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("MockTransport: no canned response left for this request");
            async move { Ok(response) }
        }
    }

    /// A `Transport` that fails below HTTP on every request.
    pub(crate) struct FailingTransport;

    impl Transport for FailingTransport {
        fn send(
            &self,
            _request: http::Request<String>,
        ) -> impl std::future::Future<Output = Result<http::Response<String>, TransportError>> + Send
        {
            std::future::ready(Err(TransportError::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "connection refused",
            ))))
        }
    }

    /// A body-only response with the given status and no headers.
    pub(crate) fn response(status: u16, body: &str) -> http::Response<String> {
        let mut response = http::Response::new(body.to_string());
        *response.status_mut() = http::StatusCode::from_u16(status).unwrap();
        response
    }

    /// What a [`RoutedTransport`] answers for one URI.
    pub(crate) enum Answer {
        Response(http::Response<String>),
        /// A failure below HTTP, as `FailingTransport` produces.
        Refused,
    }

    /// A `Transport` that answers by request URI, for callers that send
    /// several requests at once and cannot promise an order. Each URI
    /// answers once; a request for an unknown or already-answered URI is a
    /// test bug and panics.
    pub(crate) struct RoutedTransport {
        answers: Mutex<HashMap<String, Answer>>,
        pub(crate) requests: Mutex<Vec<http::Request<String>>>,
    }

    impl RoutedTransport {
        pub(crate) fn new(answers: impl IntoIterator<Item = (String, Answer)>) -> Self {
            Self {
                answers: Mutex::new(answers.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl Transport for RoutedTransport {
        fn send(
            &self,
            request: http::Request<String>,
        ) -> impl std::future::Future<Output = Result<http::Response<String>, TransportError>> + Send
        {
            let uri = request.uri().to_string();
            let answer = self
                .answers
                .lock()
                .unwrap()
                .remove(&uri)
                .unwrap_or_else(|| panic!("RoutedTransport: no answer left for {uri}"));
            self.requests.lock().unwrap().push(request);
            std::future::ready(match answer {
                Answer::Response(response) => Ok(response),
                Answer::Refused => Err(TransportError::new(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "connection refused",
                ))),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Building the real client is the TLS-provider check, and this test is
    /// what stands in for a real `https` Forgejo host: the harness only ever
    /// talks plain `http` to a local instance, where TLS is never set up and
    /// a provider problem stays invisible.
    ///
    /// reqwest resolves the rustls provider when the client is built, not on
    /// the first handshake, and panics if none is installed. Under nextest
    /// every test is its own process, so this one starts with no provider
    /// installed and exercises `install_crypto_provider` for real; under a
    /// shared-process runner another test may have installed it first, and
    /// `new()` has to succeed either way — which is also why building a
    /// second transport must not fail.
    #[test]
    fn the_real_client_builds_with_a_provider_installed() {
        ReqwestTransport::new().expect("the reqwest client should build without a network");
        assert!(
            rustls::crypto::CryptoProvider::get_default().is_some(),
            "building the transport should leave a process-level provider installed"
        );
        ReqwestTransport::new().expect("a second transport should build too");
    }

    /// Same check for the probe client, which configures the builder
    /// differently and must install the provider all the same.
    #[test]
    fn the_probing_client_builds_with_a_provider_installed() {
        ReqwestTransport::probing().expect("the probe client should build without a network");
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }

    #[test]
    fn user_agent_names_stakk_and_its_version() {
        assert_eq!(USER_AGENT, format!("stakk/{}", env!("CARGO_PKG_VERSION")));
    }
}
