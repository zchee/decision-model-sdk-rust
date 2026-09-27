//! The default transport: one pooled HTTP client per SDK client.
//!
//! It is hyper-util's pooled client over hyper-rustls, trusting the operating
//! system's roots through rustls-platform-verifier, plus any roots the caller
//! added. The rustls configuration is built here, once per client, because a
//! verifier with extra roots is something hyper-rustls' own constructors
//! cannot build.
//!
//! Connections are kept for reuse: an idle one is closed after 90 seconds,
//! and an HTTP/2 connection is kept alive by a PING every 30 seconds, idle or
//! not, so that a load balancer does not drop it between calls. Nagle's
//! algorithm is off, because every request and response here is small.
//!
//! Under `Http2Only` every request of a client shares one connection. While
//! the pool holds none - before the first request, or once the server closed
//! the connection it held and the client has seen the close - one request
//! opens it and the others wait at a gate until hyper-util reports that
//! connection, so a burst opens one connection. The gate does not see a
//! connection that ends while requests are in flight: hyper-util replaces it
//! on its own, and a burst at that moment can still open a second
//! connection, which the pool closes at once.

use std::{
    error::Error as StdError,
    fmt,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use ::hyper::body::Incoming;
use bytes::Bytes;
use http::{Request, Response};
use http_body::{Frame, SizeHint};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{self, connect::HttpConnector},
    rt::{TokioExecutor, TokioTimer},
};
use rustls::{ClientConfig, pki_types::CertificateDer};
use rustls_platform_verifier::{BuilderVerifierExt as _, Verifier};
use tower_service::Service;

use super::{
    Body, BoxError,
    gate::{self, Counting, Gate, SendError},
};
use crate::{error::Error, text};

/// How long an idle pooled connection is kept before it is closed.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How often an HTTP/2 connection is pinged to keep it open.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// Which HTTP versions the default transport speaks.
///
/// The default is [`Http2Only`](HttpVersion::Http2Only) for an `https` base
/// URL and [`Auto`](HttpVersion::Auto) for an `http` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub enum HttpVersion {
    /// HTTP/2 only: negotiated through TLS ALPN on `https`, and spoken with
    /// prior knowledge (h2c) on `http`.
    ///
    /// Every request of a client shares one multiplexed connection. While the
    /// client has no connection - before its first request, or after the
    /// server closed an idle connection and the client has seen the close -
    /// one request opens it and the others started at the same time wait for
    /// it, so they share it too. When a connection ends while requests are in
    /// flight, hyper-util replaces it on its own, and a burst at that moment
    /// can still open a second connection, which is closed at once.
    ///
    /// When that one connect fails - TCP, TLS or the HTTP/2 handshake - every
    /// request that waited for it fails at once, without a connect of its
    /// own, with an [`ErrorKind::Connection`](crate::ErrorKind::Connection)
    /// error whose message quotes the failure. A downcast of its
    /// [`source`](std::error::Error::source) chain finds no hyper-util error:
    /// that error cannot be copied, so only the request that opened carries
    /// it. A request that opened and hit the connect timeout reports
    /// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout), while the requests
    /// that waited report `Connection`.
    ///
    /// When the request that opened is dropped before a connection exists -
    /// its deadline passes, or its task is dropped - the requests waiting for
    /// it fail at once as well, with an `ErrorKind::Connection` error that
    /// says so, as hyper-util fails the requests waiting for a connect it no
    /// longer makes. The default retry policy retries them after its backoff,
    /// and the retry wave opens one connection again when its retries start
    /// within one attempt's deadline of each other, as the default backoff and
    /// the default 10 s deadline guarantee; with a deadline shorter than the
    /// backoff's spread (about 250 ms) it opens as many as hyper-util alone
    /// would. A waiting request that took over instead would start a connect
    /// at an endpoint that has just failed to answer one, once per waiting
    /// call.
    Http2Only,
    /// HTTP/2 or HTTP/1.1 as the server chooses through ALPN on `https`, and
    /// HTTP/1.1 on `http`.
    ///
    /// Use it behind a proxy that speaks HTTP/1.1 only. A client that has no
    /// connection yet may open one per request started at the same time,
    /// because which version the server speaks is known only once one of them
    /// is open.
    Auto,
}

/// What the default transport is built from.
pub(crate) struct TransportSettings {
    pub(crate) version: HttpVersion,
    /// DER-encoded certificates trusted in addition to the operating system's.
    pub(crate) extra_roots: Vec<Vec<u8>>,
    pub(crate) connect_timeout: Option<Duration>,
}

/// The transport a client uses unless it is given another: a pooled HTTP/1.1
/// and HTTP/2 client over TLS.
///
/// Cloning it shares the connection pool.
#[derive(Clone)]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub struct HyperTransport {
    client: legacy::Client<Counting<HttpsConnector<HttpConnector>>, Body>,
    /// Shared by every clone, as the pool is.
    gate: Arc<Gate>,
    version: HttpVersion,
    extra_roots: usize,
    connect_timeout: Option<Duration>,
}

impl HyperTransport {
    /// Builds the transport and its TLS configuration.
    ///
    /// Needs no runtime: nothing connects until the first request, which must
    /// then run on a Tokio runtime with its time driver enabled.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// the certificate verifier cannot be built: an added root is not a
    /// certificate, or the operating system's roots cannot be loaded.
    pub(crate) fn new(settings: TransportSettings) -> Result<Self, Error> {
        let TransportSettings { version, extra_roots, connect_timeout } = settings;
        let root_count = extra_roots.len();
        let tls = tls_config(extra_roots.into_iter().map(CertificateDer::from).collect())?;

        let mut http = HttpConnector::new();
        // The TLS layer above decides between `http` and `https`; the TCP
        // layer has to accept both.
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(connect_timeout);

        // hyper-rustls fills ALPN from what is enabled here; offering only
        // `h2` is what keeps an HTTP/1.1-only server from being accepted
        // under `Http2Only`.
        let https = HttpsConnectorBuilder::new().with_tls_config(tls).https_or_http();
        let connector = match version {
            HttpVersion::Http2Only => https.enable_http2().wrap_connector(http),
            HttpVersion::Auto => https.enable_http1().enable_http2().wrap_connector(http),
        };

        let mut builder = legacy::Client::builder(TokioExecutor::new());
        // hyper panics on a time-based option that has no timer to run on.
        builder
            .timer(TokioTimer::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
            .http2_keep_alive_while_idle(true)
            .http2_only(version == HttpVersion::Http2Only);

        let gate = Arc::new(Gate::default());
        Ok(Self {
            client: builder.build(Counting::new(connector, Arc::clone(&gate))),
            gate,
            version,
            extra_roots: root_count,
            connect_timeout,
        })
    }

    /// The streams the transport's connector handed to hyper that are not
    /// dropped yet.
    #[cfg(feature = "internals")]
    pub(crate) fn open_streams(&self) -> usize {
        self.gate.open_streams()
    }

    /// Whether a request sent now goes straight to the pool.
    #[cfg(feature = "internals")]
    pub(crate) fn pool_is_warm(&self) -> bool {
        self.gate.is_warm()
    }
}

impl fmt::Debug for HyperTransport {
    /// The settings the transport was built with; the roots as a count.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HyperTransport")
            .field("http_version", &self.version)
            .field("extra_roots", &self.extra_roots)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl Service<Request<Body>> for HyperTransport {
    type Response = Response<ResponseBody>;
    type Error = BoxError;
    type Future = HyperResponseFuture;

    /// Always ready: the pool takes any number of requests.
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> HyperResponseFuture {
        // Under `Http2Only`, two atomic loads decide whether the pool holds a
        // connection; only a request sent while it does not goes through the
        // gate, which costs one boxed future and a clone of the client.
        // `Auto` never waits: it needs a connection per concurrent HTTP/1.1
        // request.
        let inner = if self.version == HttpVersion::Http2Only && !self.gate.is_warm() {
            Sent::Gated(Box::pin(gate::send(Arc::clone(&self.gate), self.client.clone(), request)))
        } else {
            Sent::Direct(self.client.request(request))
        };
        HyperResponseFuture { inner, connect_timeout: self.connect_timeout }
    }
}

/// The response of one request sent by [`HyperTransport`].
#[must_use = "futures do nothing unless polled"]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub struct HyperResponseFuture {
    inner: Sent,
    connect_timeout: Option<Duration>,
}

/// A request sent straight to the pool, or through the gate.
enum Sent {
    Direct(legacy::ResponseFuture),
    /// `Sync` as well as `Send`, as hyper-util's own response future is, so
    /// that the public future keeps both.
    Gated(Pin<Box<dyn Future<Output = Result<Response<Incoming>, SendError>> + Send + Sync>>),
}

impl fmt::Debug for HyperResponseFuture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("HyperResponseFuture").finish_non_exhaustive()
    }
}

impl Future for HyperResponseFuture {
    type Output = Result<Response<ResponseBody>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Both fields are `Unpin` (the gated future is pinned in its box), so
        // the pinned reference can be turned back into a plain one and the
        // inner future pinned in place again.
        let this = self.get_mut();
        let polled = match &mut this.inner {
            Sent::Direct(future) => Pin::new(future).poll(cx).map_err(SendError::Own),
            Sent::Gated(future) => future.as_mut().poll(cx),
        };
        match polled {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(response)) => Poll::Ready(Ok(response.map(ResponseBody))),
            Poll::Ready(Err(SendError::Own(error))) => {
                Poll::Ready(Err(failure(error, this.connect_timeout)))
            }
            Poll::Ready(Err(SendError::Waited(error))) => Poll::Ready(Err(Box::new(error))),
        }
    }
}

/// The body of a response [`HyperTransport`] received, read frame by frame as
/// it arrives.
///
/// It is hyper's own body under a name of this crate's, so that a new major
/// version of hyper is not a breaking change here. Every call is forwarded as
/// it is, and nothing is boxed or copied: the length the server declared is
/// still what [`size_hint`](http_body::Body::size_hint) reports, which is what
/// lets a response over the limit be refused before a byte of it is read.
///
/// `Debug` prints no part of the body.
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub struct ResponseBody(Incoming);

impl fmt::Debug for ResponseBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("ResponseBody").finish_non_exhaustive()
    }
}

impl http_body::Body for ResponseBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        // hyper's body is `Unpin`, so the pinned reference can be turned back
        // into a plain one and the body pinned in place again, with no
        // `unsafe` projection. Its error is boxed only when one occurs.
        Pin::new(&mut self.get_mut().0).poll_frame(cx).map_err(Into::into)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

/// What a failed request becomes: a timeout when it was the connect timeout
/// that ran out, the client's own error otherwise.
fn failure(error: legacy::Error, connect_timeout: Option<Duration>) -> BoxError {
    match connect_timeout {
        Some(timeout) if error.is_connect() && timed_out(&error) => {
            Box::new(Error::timeout(timeout))
        }
        _ => Box::new(error),
    }
}

/// Whether anything in the chain of `error` is an I/O error that timed out.
fn timed_out(error: &(dyn StdError + 'static)) -> bool {
    let mut link = Some(error);
    while let Some(current) = link {
        if current
            .downcast_ref::<io::Error>()
            .is_some_and(|io| io.kind() == io::ErrorKind::TimedOut)
        {
            return true;
        }
        link = current.source();
    }
    false
}

/// The rustls configuration: TLS 1.2 and 1.3 with aws-lc-rs, the operating
/// system's roots, and `extra_roots` on top of them.
///
/// The crypto provider is named rather than taken from the process default,
/// so a second provider elsewhere in the program cannot change it. ALPN is
/// left empty: hyper-rustls sets it from the versions the connector enables,
/// and refuses a configuration that already has it.
fn tls_config(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(verifier_error)?;
    let config = if extra_roots.is_empty() {
        builder.with_platform_verifier().map_err(verifier_error)?.with_no_client_auth()
    } else {
        let verifier =
            Verifier::new_with_extra_roots(extra_roots, provider).map_err(verifier_error)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth()
    };
    Ok(config)
}

/// The error for a TLS configuration that cannot be built.
///
/// The verifier's text can quote a certificate the caller added or the
/// platform's own diagnostics, so it is escaped and bounded like any text
/// this SDK did not write.
fn verifier_error(error: rustls::Error) -> Error {
    Error::config(format!(
        "The TLS certificate verifier could not be built: {}.",
        text::bounded(&error, text::MAX_MESSAGE_CHARS)
    ))
}

#[cfg(test)]
#[path = "hyper_tests.rs"]
mod tests;
