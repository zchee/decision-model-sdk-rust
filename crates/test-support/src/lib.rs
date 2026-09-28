//! A real HTTP server on loopback for the tests of this workspace.
//!
//! Nothing here is simulated: [`TestServer`] binds a TCP socket on
//! `127.0.0.1:0`, serves it with hyper, and hands out a base URL that is
//! always an **IP literal**. That matters for connection-count assertions,
//! because a hostname would let the connector race a second socket across the
//! resolved addresses.
//!
//! Three wire protocols are available, selected by [`Protocol`]: HTTP/1.1, h2c
//! (HTTP/2 over cleartext, prior knowledge, no upgrade dance) and HTTP/2 over
//! TLS with ALPN `h2` and a freshly generated self-signed certificate.
//!
//! The server records every request it serves and every TCP connection it
//! accepts - who connected, how the TLS handshake ended, how many requests
//! the connection carried - so a test can assert both what was sent and how
//! many connections carried it, and a failed count explains itself. The
//! caller supplies the response as an async closure, which may capture state
//! (to answer a retry sequence differently on each attempt) and may await (to
//! hold a response back past a deadline).
//!
//! The listener is released when the [`TestServer`] is dropped.
//!
//! Two more endpoints are for failures: [`RefusingPort`], an address at which
//! every connection is refused, and [`SilentServer`], which accepts
//! connections and never answers on them.

#![forbid(unsafe_code)]

mod tls;

use std::{
    convert::Infallible,
    fmt,
    future::Future,
    io,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri, Version,
    header::CONTENT_TYPE,
};
use http_body_util::{BodyExt as _, Full};
use hyper::{body::Incoming, service::service_fn};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::CertificateDer;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{Notify, watch},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

/// The response type a handler returns.
pub type TestResponse = Response<Full<Bytes>>;

/// A response with `status`, `body` and `content-type: application/json`.
#[must_use]
pub fn json_response(status: StatusCode, body: impl Into<Bytes>) -> TestResponse {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

/// A handler future, boxed so that handlers of different concrete types can be
/// stored behind one pointer.
type HandlerFuture = Pin<Box<dyn Future<Output = TestResponse> + Send>>;

/// The stored form of a caller-supplied handler.
type BoxedHandler = Arc<dyn Fn(RecordedRequest) -> HandlerFuture + Send + Sync>;

/// Wire protocol a [`TestServer`] speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Cleartext HTTP/1.1.
    Http1,
    /// Cleartext HTTP/2 with prior knowledge (h2c): the client sends the HTTP/2
    /// preface immediately, with no `Upgrade` negotiation.
    H2c,
    /// HTTP/2 over TLS, negotiated through ALPN. The certificate is generated
    /// per server and is available from [`TestServer::certificate_der`].
    Http2Tls,
}

impl Protocol {
    /// Every protocol, for a test that runs over each of them.
    pub const ALL: [Self; 3] = [Self::Http1, Self::H2c, Self::Http2Tls];
}

/// Everything the server observed about one request it served.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    /// Request method.
    pub method: Method,
    /// Request target as the server received it.
    pub uri: Uri,
    /// Request headers, in the order hyper decoded them.
    pub headers: HeaderMap,
    /// The fully collected request body.
    pub body: Bytes,
    /// The HTTP version the request arrived on.
    pub version: Version,
}

impl RecordedRequest {
    /// Every value of the header `name`, as text, in the order received.
    ///
    /// # Panics
    ///
    /// Panics if a value is not visible ASCII, which no header a test reads
    /// carries.
    #[must_use]
    pub fn header_values(&self, name: &str) -> Vec<&str> {
        self.headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().expect("a header value the test reads is text"))
            .collect()
    }
}

/// Why a server of this crate could not be started, or why the
/// [`RefusingPort`] cannot be used.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The loopback listener could not be bound.
    #[error("could not bind a loopback listener: {0}")]
    Bind(#[source] std::io::Error),
    /// The port the operating system chose could not be read back.
    #[error("could not read back the bound address: {0}")]
    LocalAddr(#[source] std::io::Error),
    /// The self-signed test certificate could not be generated.
    #[error("could not generate the test certificate: {0}")]
    Certificate(#[source] rcgen::Error),
    /// rustls rejected the generated certificate or the requested versions.
    #[error("could not build the server TLS configuration: {0}")]
    TlsConfig(#[source] rustls::Error),
    /// A connect to the address that should refuse every connection was not
    /// refused: something on this machine answers there.
    #[error("{addr} should refuse every connection, but a connect to it {outcome}")]
    NotRefused {
        /// The address that was expected to refuse.
        addr: SocketAddr,
        /// What the connect did instead: `connected`, `did not end within
        /// ...`, or `failed with ...` and the error.
        outcome: String,
    },
}

/// What became of the TLS handshake of one accepted connection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Tls {
    /// The server speaks cleartext, so there is no handshake.
    None,
    /// The handshake has not ended yet.
    InProgress,
    /// The handshake completed.
    Completed {
        /// The protocol ALPN settled on, if the client offered any.
        alpn: Option<Vec<u8>>,
    },
    /// The handshake failed.
    Failed {
        /// The text of the handshake's error.
        error: String,
        /// The first bytes the peer sent, at most [`FIRST_BYTES`], as it sent
        /// them: a plaintext request that reached a TLS port shows here.
        first_bytes: Vec<u8>,
    },
}

/// How many of the bytes a peer sent first are kept for a connection whose
/// TLS handshake failed.
pub const FIRST_BYTES: usize = 64;

/// What the server saw of one connection it accepted.
///
/// `Debug` prints one line, so that a failing assertion on a connection count
/// can print every connection and say where each came from: the peer, the
/// TLS outcome (with the peer's first bytes escaped when the handshake
/// failed), and how many requests the connection served.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectionRecord {
    peer: SocketAddr,
    tls: Tls,
    requests: usize,
}

impl ConnectionRecord {
    /// The address of the peer that connected.
    #[must_use]
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// What became of the TLS handshake, as of when the record was taken.
    #[must_use]
    pub fn tls(&self) -> &Tls {
        &self.tls
    }

    /// How many requests the connection served, as of when the record was
    /// taken.
    #[must_use]
    pub fn requests(&self) -> usize {
        self.requests
    }
}

impl fmt::Debug for ConnectionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}, ", self.peer)?;
        match &self.tls {
            Tls::None => formatter.write_str("cleartext")?,
            Tls::InProgress => formatter.write_str("TLS handshake in progress")?,
            Tls::Completed { alpn: Some(alpn) } => {
                write!(formatter, "TLS, ALPN {}", Escaped(alpn))?
            }
            Tls::Completed { alpn: None } => formatter.write_str("TLS, no ALPN")?,
            Tls::Failed { error, first_bytes } => {
                write!(formatter, "TLS failed: {error}; first bytes {}", Escaped(first_bytes))?;
            }
        }
        write!(formatter, ", {} request(s)", self.requests)
    }
}

/// Bytes as a quoted string, every byte that is not printable ASCII escaped
/// (`\r`, `\n`, `\x16`, ...).
struct Escaped<'a>(&'a [u8]);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("\"")?;
        for &byte in self.0 {
            write!(formatter, "{}", byte.escape_ascii())?;
        }
        formatter.write_str("\"")
    }
}

/// One accepted connection, as its task updates it.
struct Connection {
    peer: SocketAddr,
    tls: Mutex<Tls>,
    requests: AtomicUsize,
    /// Notified by [`TestServer::close_connections`]. A notification sent
    /// before the task waits on it is kept, so none is lost.
    close: Notify,
    /// Held without a byte read or written, instead of served.
    silent: bool,
}

impl Connection {
    fn record(&self) -> ConnectionRecord {
        ConnectionRecord {
            peer: self.peer,
            tls: lock(&self.tls).clone(),
            // Relaxed: a count read for a report, with nothing ordered
            // against it.
            requests: self.requests.load(Ordering::Relaxed),
        }
    }
}

/// Shared between the accept loop, every connection task and the handle the
/// test holds.
struct State {
    handler: BoxedHandler,
    requests: Mutex<Vec<RecordedRequest>>,
    /// Every accepted connection, in accept order.
    connections: Mutex<Vec<Arc<Connection>>>,
    /// How long after accepting a connection the server starts serving it.
    delay: Duration,
    /// Whether the first connection is held without a byte instead of
    /// served.
    first_silent: bool,
}

/// Locks `mutex` even when a panicking thread poisoned it: what it holds is
/// a record, and losing it is worse for a failing test than reading past the
/// poison flag.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A running HTTP server on `127.0.0.1`.
///
/// Dropping the value stops accepting, drops the live connections and releases
/// the port.
pub struct TestServer {
    addr: SocketAddr,
    base_url: String,
    certificate: Option<CertificateDer<'static>>,
    state: Arc<State>,
    shutdown: watch::Sender<bool>,
    accept_task: JoinHandle<()>,
}

impl fmt::Debug for TestServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestServer")
            .field("base_url", &self.base_url)
            .field("requests", &self.request_count())
            .field("accepted_connections", &self.accepted_connections())
            .finish_non_exhaustive()
    }
}

impl TestServer {
    /// Binds a listener on `127.0.0.1:0`, starts serving `protocol` and returns
    /// once the port is known.
    ///
    /// `handler` is invoked once per request, after the body has been collected
    /// and the request recorded. It may capture state and may await, which is
    /// how a test drives a retry sequence or holds a response past a deadline.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot
    /// be set up, and [`Error::Certificate`] or [`Error::TlsConfig`] when
    /// [`Protocol::Http2Tls`] is requested and TLS cannot be configured.
    pub async fn start<F, Fut>(protocol: Protocol, handler: F) -> Result<Self, Error>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResponse> + Send + 'static,
    {
        Self::start_with(protocol, Duration::ZERO, false, handler).await
    }

    /// Starts serving `protocol` as [`start`](Self::start) does, but begins
    /// serving each connection `delay` after it accepted it: the TLS
    /// handshake, or the first HTTP byte on cleartext, waits that long, so
    /// that a client's connect takes at least `delay`. The connection's
    /// record shows [`Tls::InProgress`] meanwhile.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub async fn start_slow<F, Fut>(
        protocol: Protocol,
        delay: Duration,
        handler: F,
    ) -> Result<Self, Error>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResponse> + Send + 'static,
    {
        Self::start_with(protocol, delay, false, handler).await
    }

    /// Starts serving `protocol` as [`start`](Self::start) does, but holds
    /// the first connection it accepts without reading or writing a byte,
    /// until the server is dropped or
    /// [`close_connections`](Self::close_connections) is called: a first
    /// connect that stalls while a later one is served at once. The first
    /// connection's record shows [`Tls::InProgress`] (or [`Tls::None`] on
    /// cleartext) and no request.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub async fn start_first_silent<F, Fut>(protocol: Protocol, handler: F) -> Result<Self, Error>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResponse> + Send + 'static,
    {
        Self::start_with(protocol, Duration::ZERO, true, handler).await
    }

    /// [`start`](Self::start) with each connection served `delay` after it
    /// was accepted, and the first one held silent when `first_silent`.
    async fn start_with<F, Fut>(
        protocol: Protocol,
        delay: Duration,
        first_silent: bool,
        handler: F,
    ) -> Result<Self, Error>
    where
        F: Fn(RecordedRequest) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TestResponse> + Send + 'static,
    {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::Bind)?;
        let addr = listener.local_addr().map_err(Error::LocalAddr)?;

        let (acceptor, certificate) = match protocol {
            Protocol::Http1 | Protocol::H2c => (None, None),
            Protocol::Http2Tls => {
                let (acceptor, certificate) = tls::self_signed_acceptor()?;
                (Some(acceptor), Some(certificate))
            }
        };

        let scheme = if certificate.is_some() { "https" } else { "http" };
        let state = Arc::new(State {
            handler: Arc::new(move |request| Box::pin(handler(request)) as HandlerFuture),
            requests: Mutex::new(Vec::new()),
            connections: Mutex::new(Vec::new()),
            delay,
            first_silent,
        });

        let (shutdown, shutdown_rx) = watch::channel(false);
        let accept_task = tokio::spawn(accept_loop(
            listener,
            protocol,
            acceptor,
            Arc::clone(&state),
            shutdown_rx,
        ));

        Ok(Self {
            addr,
            base_url: format!("{scheme}://{addr}"),
            certificate,
            state,
            shutdown,
            accept_task,
        })
    }

    /// Starts serving `protocol` as [`start`](Self::start) does, answering the
    /// `n`th request, counted from 1, with `answer(n, request)`.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub async fn start_nth<F>(protocol: Protocol, answer: F) -> Result<Self, Error>
    where
        F: Fn(usize, &RecordedRequest) -> TestResponse + Send + Sync + 'static,
    {
        let served = AtomicUsize::new(0);
        Self::start(protocol, move |request| {
            let response = answer(served.fetch_add(1, Ordering::SeqCst) + 1, &request);
            async move { response }
        })
        .await
    }

    /// The address the server is listening on, always an IPv4 loopback address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Base URL with no trailing slash, for example `http://127.0.0.1:52341`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Every request served so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.request_log().clone()
    }

    /// How many requests have been served so far.
    #[must_use]
    pub fn request_count(&self) -> usize {
        self.request_log().len()
    }

    /// How many TCP connections have been accepted so far: the length of
    /// [`connections`](Self::connections).
    ///
    /// Counted at accept time, so a connection that fails its TLS handshake is
    /// still counted.
    #[must_use]
    pub fn accepted_connections(&self) -> u64 {
        lock(&self.state.connections).len() as u64
    }

    /// A record of every TCP connection accepted so far, in accept order.
    ///
    /// A record is taken at the time of the call: a handshake still running
    /// shows as [`Tls::InProgress`], and the request counts are those served
    /// so far.
    #[must_use]
    pub fn connections(&self) -> Vec<ConnectionRecord> {
        lock(&self.state.connections).iter().map(|connection| connection.record()).collect()
    }

    /// Ends every connection the server holds now, as a server that closes
    /// its connections does: the client sees each one closed by its peer.
    ///
    /// The listener keeps running, and a connection accepted after this call
    /// is served as usual. The connections end on their own tasks, shortly
    /// after the call returns.
    pub fn close_connections(&self) {
        for connection in lock(&self.state.connections).iter() {
            connection.close.notify_one();
        }
    }

    /// The server's certificate in DER, for [`Protocol::Http2Tls`] only.
    ///
    /// A client that adds this to its root store will accept the handshake; a
    /// client that does not will reject it.
    #[must_use]
    pub fn certificate_der(&self) -> Option<CertificateDer<'static>> {
        self.certificate.clone()
    }

    fn request_log(&self) -> MutexGuard<'_, Vec<RecordedRequest>> {
        lock(&self.state.requests)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        // The watch value is level-triggered, so a connection task that has not
        // reached its await point yet still observes the shutdown.
        let _ = self.shutdown.send(true);
        self.accept_task.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    protocol: Protocol,
    acceptor: Option<TlsAcceptor>,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let (stream, peer) = tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                // A failed accept says nothing about the next one, and a test
                // that cares will fail on its own assertions.
                Err(_) => continue,
            },
        };
        let mut connections = lock(&state.connections);
        let connection = Arc::new(Connection {
            peer,
            tls: Mutex::new(if acceptor.is_some() { Tls::InProgress } else { Tls::None }),
            requests: AtomicUsize::new(0),
            close: Notify::new(),
            silent: state.first_silent && connections.is_empty(),
        });
        connections.push(Arc::clone(&connection));
        drop(connections);
        // Nagle's algorithm would add latency to the small request/response
        // pairs these tests measure.
        let _ = stream.set_nodelay(true);

        let served = serve_connection(
            stream,
            protocol,
            acceptor.clone(),
            Arc::clone(&state),
            Arc::clone(&connection),
        );
        let mut shutdown = shutdown.clone();
        // Dropping the connection's future drops its socket: the client sees
        // the connection closed, whether by `close_connections` or because
        // the server is gone.
        tokio::spawn(async move {
            tokio::select! {
                () = served => {}
                () = connection.close.notified() => {}
                _ = shutdown.changed() => {}
            }
        });
    }
}

async fn serve_connection(
    stream: TcpStream,
    protocol: Protocol,
    acceptor: Option<TlsAcceptor>,
    state: Arc<State>,
    connection: Arc<Connection>,
) {
    if connection.silent {
        // The stream is held, unread, until this future is dropped: by
        // `close_connections` or with the server.
        let _held = stream;
        return std::future::pending().await;
    }
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    let counted = Arc::clone(&connection);
    let service =
        service_fn(move |request| dispatch(Arc::clone(&state), Arc::clone(&counted), request));

    match (protocol, acceptor) {
        (Protocol::Http1, _) => {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        }
        (Protocol::H2c, _) => {
            let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        }
        (Protocol::Http2Tls, Some(acceptor)) => {
            // The handshake reads through a stream that keeps the first bytes
            // the peer sent, and the fallible form of the accept hands the
            // stream back when the handshake fails, so that the record can
            // show what arrived: a plaintext request at a TLS port, say. A
            // failed handshake is also the expected outcome of the
            // untrusted-client test, so it ends the connection quietly.
            let stream = match acceptor.accept(FirstBytes::new(stream)).into_fallible().await {
                Ok(stream) => {
                    let alpn = stream.get_ref().1.alpn_protocol().map(<[u8]>::to_vec);
                    *lock(&connection.tls) = Tls::Completed { alpn };
                    stream
                }
                Err((error, stream)) => {
                    *lock(&connection.tls) =
                        Tls::Failed { error: error.to_string(), first_bytes: stream.first };
                    return;
                }
            };
            let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        }
        (Protocol::Http2Tls, None) => {}
    }
}

/// A stream that keeps a copy of the first [`FIRST_BYTES`] bytes read from
/// it, and forwards everything else unchanged.
struct FirstBytes<S> {
    inner: S,
    first: Vec<u8>,
}

impl<S> FirstBytes<S> {
    fn new(inner: S) -> Self {
        Self { inner, first: Vec::with_capacity(FIRST_BYTES) }
    }
}

// The wrapped stream is `Unpin` (a `TcpStream`), so every method turns the
// pinned reference back into a plain one and pins the inner stream in place
// again, with no `unsafe` projection.
impl<S: AsyncRead + Unpin> AsyncRead for FirstBytes<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        let read = &buf.filled()[before..];
        let room = FIRST_BYTES - this.first.len();
        this.first.extend_from_slice(&read[..read.len().min(room)]);
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for FirstBytes<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

async fn dispatch(
    state: Arc<State>,
    connection: Arc<Connection>,
    request: Request<Incoming>,
) -> Result<TestResponse, Infallible> {
    let (parts, body) = request.into_parts();
    let body = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            // Reporting the failure as a response body is more useful to a
            // failing test than an empty recorded request would be.
            let mut response = Response::new(Full::new(Bytes::from(format!(
                "test server could not read the request body: {error}"
            ))));
            *response.status_mut() = StatusCode::BAD_REQUEST;
            return Ok(response);
        }
    };

    let recorded = RecordedRequest {
        method: parts.method,
        uri: parts.uri,
        headers: parts.headers,
        body,
        version: parts.version,
    };
    lock(&state.requests).push(recorded.clone());
    // Relaxed: a count read for a report, with nothing ordered against it.
    connection.requests.fetch_add(1, Ordering::Relaxed);

    Ok((state.handler)(recorded).await)
}

/// Binds a loopback listener that answers every connection with `reply`, as
/// raw bytes, once the request has arrived, then closes it: a server that
/// does not speak HTTP, or speaks it wrongly.
///
/// # Errors
///
/// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot be
/// set up.
pub async fn raw_server(reply: impl AsRef<[u8]>) -> Result<SocketAddr, Error> {
    let reply = Bytes::copy_from_slice(reply.as_ref());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::Bind)?;
    let addr = listener.local_addr().map_err(Error::LocalAddr)?;
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buffer = [0; 4096];
            // How much of the request one read returns does not matter to
            // the client.
            let _ = stream.read(&mut buffer).await;
            let _ = stream.write_all(&reply).await;
            let _ = stream.shutdown().await;
        }
    });
    Ok(addr)
}

/// The port [`RefusingPort`] names: below the range from which Linux
/// (32768-60999) and macOS (49152-65535) pick the port of a `bind(0)`, so no
/// server of this crate, which binds port 0, is ever given it.
const REFUSING_PORT: u16 = 1;

/// How long [`RefusingPort::new`] waits for its one connect to be refused.
const REFUSAL_BOUND: Duration = Duration::from_secs(2);

/// An address at which every connection is refused: `127.0.0.1:1`.
///
/// A test that sends to it gets a refused connection and never reaches a
/// server of another test, because no `bind(0)` hands out a port below the
/// ephemeral range. A port released by a test, by contrast, can be given to
/// the next server that binds, which then counts the stranger's request as a
/// connection of its own. The value holds no socket: it names the address
/// once [`new`](Self::new) has checked that nothing on this machine answers
/// there.
#[derive(Debug, Clone)]
pub struct RefusingPort {
    addr: SocketAddr,
    base_url: String,
}

impl RefusingPort {
    /// Connects to `127.0.0.1:1` once and returns the address when the
    /// connect is refused.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotRefused`] when the connect succeeds, fails another
    /// way, or does not end within 2 seconds: a machine that runs something
    /// at port 1 cannot run the tests that send there, and says so by name.
    pub async fn new() -> Result<Self, Error> {
        Self::at(SocketAddr::from((Ipv4Addr::LOCALHOST, REFUSING_PORT)), REFUSAL_BOUND).await
    }

    /// [`new`](Self::new) for any address and bound.
    async fn at(addr: SocketAddr, bound: Duration) -> Result<Self, Error> {
        let outcome = match tokio::time::timeout(bound, TcpStream::connect(addr)).await {
            Ok(Err(error)) if error.kind() == io::ErrorKind::ConnectionRefused => {
                return Ok(Self { addr, base_url: format!("http://{addr}") });
            }
            Ok(Ok(_)) => String::from("connected"),
            Ok(Err(error)) => format!("failed with {:?}: {error}", error.kind()),
            Err(_) => format!("did not end within {bound:?}"),
        };
        Err(Error::NotRefused { addr, outcome })
    }

    /// The address, `127.0.0.1:1`.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The base URL, `http://127.0.0.1:1`.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// How long a [`SilentServer`] waits before it accepts again after a failed
/// accept.
const ACCEPT_PAUSE: Duration = Duration::from_millis(10);

/// A listener on `127.0.0.1` that accepts every connection and neither reads
/// nor writes on it: a server that never answers.
///
/// Made with [`start`](Self::start), it holds every connection open, for a
/// deadline that runs out while a client connects. Made with
/// [`closing_after`](Self::closing_after), it closes each connection a fixed
/// delay after it accepted it, so that a client's TLS handshake fails after
/// that delay: a connect that fails slowly.
///
/// Dropping the value closes the listener and every connection it holds.
pub struct SilentServer {
    addr: SocketAddr,
    accepted: Arc<AtomicU64>,
    accept_task: JoinHandle<()>,
}

impl fmt::Debug for SilentServer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SilentServer")
            .field("addr", &self.addr)
            .field("accepted_connections", &self.accepted_connections())
            .finish_non_exhaustive()
    }
}

impl SilentServer {
    /// Binds a listener on `127.0.0.1:0` and starts accepting, holding every
    /// connection open until the value is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot
    /// be set up.
    pub async fn start() -> Result<Self, Error> {
        Self::bind(None).await
    }

    /// Binds a listener on `127.0.0.1:0` and starts accepting, closing each
    /// connection `delay` after it accepted it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Bind`] or [`Error::LocalAddr`] when the socket cannot
    /// be set up.
    pub async fn closing_after(delay: Duration) -> Result<Self, Error> {
        Self::bind(Some(delay)).await
    }

    async fn bind(close_after: Option<Duration>) -> Result<Self, Error> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.map_err(Error::Bind)?;
        let addr = listener.local_addr().map_err(Error::LocalAddr)?;
        let accepted = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&accepted);
        let accept_task = tokio::spawn(async move {
            // The streams are kept, unread, for as long as the task runs: in
            // `held`, or each by a task of `closing` until its delay has
            // passed. Aborting this task drops both, and dropping a `JoinSet`
            // aborts its tasks, so every connection ends with the server.
            let mut held = Vec::new();
            let mut closing = tokio::task::JoinSet::new();
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        // Relaxed: a count read for a report, with nothing
                        // ordered against it.
                        counter.fetch_add(1, Ordering::Relaxed);
                        match close_after {
                            None => held.push(stream),
                            Some(delay) => {
                                closing.spawn(async move {
                                    tokio::time::sleep(delay).await;
                                    drop(stream);
                                });
                            }
                        }
                        // The tasks that have closed their connection are
                        // reaped, so the set holds only the open ones.
                        while closing.try_join_next().is_some() {}
                    }
                    // A failed accept (out of file descriptors, say) is tried
                    // again after a pause, not at once: at once, the loop
                    // would keep a worker thread spinning for as long as the
                    // failure lasts.
                    Err(_) => tokio::time::sleep(ACCEPT_PAUSE).await,
                }
            }
        });
        Ok(Self { addr, accepted, accept_task })
    }

    /// The address the server listens on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// How many connections have been accepted so far.
    #[must_use]
    pub fn accepted_connections(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }
}

impl Drop for SilentServer {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
