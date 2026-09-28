//! The gate in front of the default transport's pool: while the pool holds
//! no connection, one request opens it and the others wait.
//!
//! hyper-util lets one task at a time connect to an HTTP/2 destination, and
//! the others wait for that connection in the pool. A task that began
//! waiting before the first connection was pooled, and whose own connect is
//! polled after that connection's lock was released, finds the lock free and
//! connects a second time. The pool keeps one HTTP/2 connection per
//! destination, so it closes the second one at once. No call fails, but the
//! client paid a TCP and TLS handshake for nothing, and the server saw the
//! connection. The gate keeps every request but one out of hyper-util until
//! the first connection is in the pool, where the checkout of each finds it
//! on its first poll, so that its connect is never started.
//!
//! When the turn of the request that opens ends without a connection, every
//! request waiting for it fails with it, as hyper-util fails the requests
//! waiting for a connect it could not make, and none of them connects on its
//! own: when the connect fails (the HTTP/2 handshake that follows TLS
//! included), and when the request that opens is dropped before a connection
//! existed (its caller's deadline passed, or its task was dropped). A waiting
//! request that took the turn over instead would start a connect at an
//! endpoint that has just failed to answer one, once per waiting call.
//!
//! What the gate does not see: a connection that ends while requests are in
//! flight is replaced inside hyper-util, which resends a request that had
//! not started, and a burst at that moment can still open a second
//! connection that is closed at once.

use std::{
    error::Error as StdError,
    fmt,
    future::{Future, poll_fn},
    io,
    pin::{Pin, pin},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
    time::Duration,
};

use http::{
    HeaderValue, Method, Request, Response, Uri,
    header::CONTENT_LENGTH,
    uri::{self, PathAndQuery},
};
use hyper::{
    body::Incoming,
    rt::{Read, ReadBufCursor, Write},
};
use hyper_util::client::legacy::{
    self,
    connect::{Connect, Connected, Connection, capture_connection},
};
use tokio::sync::{Notify, oneshot};
use tower_service::Service;

use super::{Body, CONNECTION_PREFIX, connection_message};

/// The bit of [`Gate::turns`] that says a request holds the turn to open.
const HELD: u64 = 1;

/// Whether the pool of one transport holds a connection, and which request
/// may open one when it does not.
///
/// The orderings of the atomics:
///
/// - `pooled` is stored `true` with `Release` and loaded with `Acquire`. The
///   opener stores it after hyper-util reported the request's connection,
///   which hyper-util does after it put the connection in the pool; a
///   request that loads `true` therefore also sees that insert when its own
///   checkout locks the pool. `false` is stored `Relaxed`: it publishes
///   nothing, and a request that still loads `true` just after a connect
///   began goes to hyper-util, which makes it wait for that connect as it
///   would without the gate.
/// - `open` is a count that orders nothing, so every access is `Relaxed`. A
///   request reads it and then acts on it, and a stream can close between
///   the two whatever the ordering; a stronger one would not close that
///   window, which is the case the module comment says the gate does not
///   cover.
/// - `turns` is taken with an `Acquire` compare-exchange and given back with
///   a `Release` store. What that buys: the last opener's store of `true` to
///   `pooled` comes before the next opener's connect stores `false`, so that
///   `pooled` cannot end `true` while the new connect is in flight. Every
///   other access is `Relaxed`: a waiting request only compares the count it
///   read with the turn of a failure, which it reads under `failed`'s lock
///   after `changed` woke it, and `changed` orders that wake-up after the
///   release that caused it.
#[derive(Debug, Default)]
pub(super) struct Gate {
    /// Streams the connector handed to hyper that are not dropped yet.
    open: AtomicUsize,
    /// Whether a request was given a pooled connection since the last
    /// connect began.
    pooled: AtomicBool,
    /// [`HELD`] when a request holds the turn to open a connection; the bits
    /// above it count the turns that have ended. Only the holder of the turn
    /// gives it back, so only one request at a time changes the count.
    turns: AtomicU64,
    /// The last turn that ended without a connection, and why. It keeps the
    /// last such turn only, so a waiting request woken by one may quote a
    /// later one when that one ended before the request was polled; it fails
    /// either way. Locked only by an opener whose turn ended without a
    /// connection and by a waiting request that was woken, never across an
    /// await.
    failed: Mutex<Option<Failed>>,
    /// Notified when the opener gives its turn back.
    changed: Notify,
    /// How long a connect may run once the request that started it is gone,
    /// counted from when it began; with `None` it ends with that request.
    bound: Option<Duration>,
}

/// A turn that ended without a connection.
#[derive(Debug)]
struct Failed {
    /// The number of turns that had ended once this one had.
    turn: u64,
    /// Why it ended without one.
    failure: Failure,
}

/// Why the turn of the request that opened ended without a connection.
#[derive(Debug, Clone)]
enum Failure {
    /// The connect failed; the opener's error, rendered as the transport
    /// renders the chain of any error it fails with.
    Connect(Arc<str>),
    /// The request that opened was dropped before a connection existed.
    GivenUp,
    /// The request that started the connect was gone, and the connect had
    /// run for this long, the gate's bound, without a connection.
    Bound(Duration),
}

/// What a request finds at the gate.
enum Entry {
    /// The pool holds a connection: go to it.
    Pool,
    /// The turn to open a connection.
    Open(Opener),
    /// The turn this request waited for ended without a connection.
    Failed(WaitedConnectFailed),
}

impl Gate {
    /// A gate whose connect may run for `bound` once the request that
    /// started it is gone; with `None` the connect ends with that request.
    pub(super) fn new(bound: Option<Duration>) -> Self {
        Self { bound, ..Self::default() }
    }

    /// Whether a request can go to the pool without waiting: a request was
    /// given a pooled connection since the last connect began, and a stream
    /// is still open.
    pub(super) fn is_warm(&self) -> bool {
        self.pooled.load(Ordering::Acquire) && self.open.load(Ordering::Relaxed) > 0
    }

    /// The streams the connector handed to hyper that are not dropped yet.
    #[cfg(any(test, feature = "internals"))]
    pub(super) fn open_streams(&self) -> usize {
        self.open.load(Ordering::Relaxed)
    }

    /// Waits until a request may go: to the pool once the gate is warm, or
    /// with the turn to open a connection when no other request holds it.
    /// A request that is waiting when the opener's turn ends without a
    /// connection fails with it; one that comes after finds the turn free.
    async fn enter(self: &Arc<Self>) -> Entry {
        loop {
            // Registered before the state is read, so that a turn given back
            // between the read and the wait still wakes this request.
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            if self.is_warm() {
                return Entry::Pool;
            }
            let turns = self.turns.load(Ordering::Relaxed);
            let ended = if turns & HELD == 0 {
                match self.turns.compare_exchange(
                    turns,
                    turns | HELD,
                    Ordering::Acquire,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        return Entry::Open(Opener {
                            gate: Arc::clone(self),
                            ending: Ending::Pending,
                        });
                    }
                    // Another request took the turn first: this one waits for
                    // the turn that request holds.
                    Err(now) if now & HELD == HELD => now >> 1,
                    // A whole turn began and ended in between: look again.
                    Err(_) => continue,
                }
            } else {
                turns >> 1
            };
            changed.await;
            if let Some(failure) = self.failed_after(ended) {
                return Entry::Failed(WaitedConnectFailed(failure));
            }
        }
    }

    /// Why a turn that ended after `ended` turns had ended without a
    /// connection, if one did.
    fn failed_after(&self, ended: u64) -> Option<Failure> {
        let failed = self.failed.lock().unwrap_or_else(PoisonError::into_inner);
        failed.as_ref().filter(|failed| failed.turn > ended).map(|failed| failed.failure.clone())
    }

    /// A connect began: the gate is cold until a request is given a pooled
    /// connection again.
    fn connect_began(&self) {
        self.pooled.store(false, Ordering::Relaxed);
    }

    /// A request was given a pooled connection.
    fn mark_pooled(&self) {
        self.pooled.store(true, Ordering::Release);
    }

    /// Counts a stream the connector hands to hyper until the returned value
    /// is dropped.
    fn stream_opened(self: &Arc<Self>) -> OpenStream {
        self.open.fetch_add(1, Ordering::Relaxed);
        OpenStream(Arc::clone(self))
    }
}

/// The turn to open a connection. Dropping it gives the turn back and wakes
/// every waiting request: when the turn ended without a connection, which
/// fails them all; when it ended with one, which lets them go on to the pool,
/// or one of them open again.
struct Opener {
    gate: Arc<Gate>,
    ending: Ending,
}

/// How an opener's turn ended, as its `Drop` reads it.
enum Ending {
    /// Nothing recorded yet. An opener dropped in this state was dropped
    /// before a connection existed: the request that started the connect
    /// gave up on a gate with no bound, or the runtime dropped the task that
    /// ran the connect.
    Pending,
    /// The opener's connect failed with this chain of messages.
    ConnectFailed(Arc<str>),
    /// The opener was given a connection, whether its response then
    /// succeeded or failed on it.
    Connected,
    /// The request that started the connect was gone, and the connect ran
    /// for the gate's bound without a connection.
    Bound(Duration),
}

impl Opener {
    /// Gives the turn back because the opener's connect failed with
    /// `failure`.
    fn fail(mut self, failure: Arc<str>) {
        self.ending = Ending::ConnectFailed(failure);
    }

    /// Gives the turn back because the opener was given a connection.
    fn connected(mut self) {
        self.ending = Ending::Connected;
    }

    /// Gives the turn back because the connect ran for `bound` after the
    /// request that started it was gone.
    fn bound(mut self, bound: Duration) {
        self.ending = Ending::Bound(bound);
    }
}

impl Drop for Opener {
    fn drop(&mut self) {
        let gate = &self.gate;
        // Only the holder of the turn changes `turns`, so this is the count
        // with the held bit set, and nothing changes it until the store.
        let ended = (gate.turns.load(Ordering::Relaxed) >> 1) + 1;
        let failure = match std::mem::replace(&mut self.ending, Ending::Connected) {
            Ending::Pending => Some(Failure::GivenUp),
            Ending::ConnectFailed(chain) => Some(Failure::Connect(chain)),
            Ending::Bound(bound) => Some(Failure::Bound(bound)),
            Ending::Connected => None,
        };
        if let Some(failure) = failure {
            *gate.failed.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(Failed { turn: ended, failure });
        }
        gate.turns.store(ended << 1, Ordering::Release);
        gate.changed.notify_waiters();
    }
}

/// The error of a request that waited for another request's connect, when
/// that request's turn ended without a connection: it says why, quoting the
/// connect's failure when there was one.
///
/// It has no source. The failure is the opener's own error, which is not
/// `Clone`, so it stays the opener's and is quoted here as text.
#[derive(Debug)]
pub(super) struct WaitedConnectFailed(Failure);

impl fmt::Display for WaitedConnectFailed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Failure::Connect(chain) => {
                write!(formatter, "the connect this request waited for failed: {chain}")
            }
            Failure::GivenUp => formatter.write_str(
                "the connect this request waited for was given up: the request that started it \
                 was dropped before a connection existed",
            ),
            Failure::Bound(bound) => write!(
                formatter,
                "the connect this request waited for did not complete within {bound:?} after the \
                 request that started it gave up"
            ),
        }
    }
}

impl StdError for WaitedConnectFailed {}

/// Why a request sent through the gate failed.
pub(super) enum SendError {
    /// The request's own error, as hyper-util returned it.
    Own(legacy::Error),
    /// The request waited for another request's turn, which ended without a
    /// connection.
    Waited(WaitedConnectFailed),
}

/// How the connect of a turn ended, as the task that ran it tells the
/// request that started it.
enum Outcome {
    /// hyper-util put the connection in the pool.
    Connected,
    /// The connect failed. The error is hyper-util's own, which cannot be
    /// copied, so only the request that started the connect carries it.
    Failed(legacy::Error),
}

/// Sends `request` through `gate`: it waits while a connect is in flight,
/// then goes to the pool as any request does. A request that finds no
/// connect in flight starts one in a task of its own, which no request owns,
/// and waits for it like the others.
///
/// The connect therefore outlives the request that started it: a request
/// that gives up - its deadline passed, or its task was dropped - no longer
/// ends the connect the other requests are waiting for; [`open`] says when
/// it ends instead. The request itself is sent only once the connection is
/// in the pool, so a request that gave up before then was never sent.
pub(super) async fn send<C>(
    gate: Arc<Gate>,
    client: legacy::Client<C, Body>,
    request: Request<Body>,
) -> Result<Response<Incoming>, SendError>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    loop {
        let opener = match gate.enter().await {
            Entry::Pool => return client.request(request).await.map_err(SendError::Own),
            Entry::Failed(failed) => return Err(SendError::Waited(failed)),
            Entry::Open(opener) => opener,
        };
        let (outcome, outcome_rx) = oneshot::channel();
        // The task runs on the runtime this request is polled on, as the
        // connection tasks hyper-util spawns through `TokioExecutor` do.
        tokio::spawn(open(opener, client.clone(), probe(&request), outcome));
        match outcome_rx.await {
            // The connection is in the pool: go to it, as a request that
            // waited does.
            Ok(Outcome::Connected) => {}
            Ok(Outcome::Failed(error)) => return Err(SendError::Own(error)),
            // The runtime dropped the task before the connect ended, as it
            // does when it shuts down; the task's turn ended as given up.
            Err(_) => return Err(SendError::Waited(WaitedConnectFailed(Failure::GivenUp))),
        }
    }
}

/// The request the task that connects sends for `request`: a `CONNECT` to
/// the scheme and authority of its URI, which are hyper-util's pool key for
/// it, declaring a body of one byte and sending none. Nothing else of
/// `request` is taken.
///
/// hyper's HTTP/2 client refuses a `CONNECT` whose declared length is not
/// zero before it writes a frame of it, and hyper-util reports the refusal
/// with the connection it was made on, which stays in the pool: the connect
/// runs as for any request, and no request reaches the server.
fn probe(request: &Request<Body>) -> Request<Body> {
    let uri = request.uri();
    let mut parts = uri::Parts::default();
    parts.scheme = uri.scheme().cloned();
    parts.authority = uri.authority().cloned();
    parts.path_and_query = Some(PathAndQuery::from_static("/"));
    // `from_parts` refuses an authority without a scheme, which hyper-util
    // would refuse as well; the default URI, `/`, has no authority, so
    // hyper-util fails the probe before any connect, as it fails the
    // caller's own request.
    let mut probe = Request::new(Body::empty());
    *probe.uri_mut() = Uri::from_parts(parts).unwrap_or_default();
    *probe.method_mut() = Method::CONNECT;
    // No header but this one, never the caller's credentials or headers: a
    // hyper release that did send the probe would send them to the server.
    probe.headers_mut().insert(CONTENT_LENGTH, HeaderValue::from_static("1"));
    probe
}

/// What the task that connects saw first.
enum Seen {
    /// The probe's response, before hyper-util reported a connection.
    Response(Result<Response<Incoming>, legacy::Error>),
    /// hyper-util reported the probe's connection.
    Connection,
    /// The request that started the connect is gone, and the gate's bound
    /// has passed since the connect began, or the gate has none.
    Abandoned,
}

/// The task that runs the connect of a turn, holding the turn: it sends
/// `probe` and gives the turn back as soon as hyper-util reports the
/// connection it was given, before any response. hyper-util reports it
/// right after it has put a new connection in the pool, so every request
/// released then finds it there.
///
/// A connect that fails - TCP, TLS or the HTTP/2 handshake - fails every
/// waiting request with it, and the request that started it with
/// hyper-util's own error. The connect is never given up while the request
/// that started it waits, whatever the gate's bound: that request may have a
/// deadline of its own longer than the client's. Once that request is gone,
/// the connect is given up when the gate's bound has passed since it began,
/// failing every waiting request with the bound; a gate with no bound gives
/// it up at once, as the request would have given up a connect of its own.
async fn open<C>(
    opener: Opener,
    client: legacy::Client<C, Body>,
    mut probe: Request<Body>,
    mut starter: oneshot::Sender<Outcome>,
) where
    C: Connect + Clone + Send + Sync + 'static,
{
    let gate = Arc::clone(&opener.gate);
    // `sleep` fixes its deadline when it is made, not when it is first
    // polled, so the bound counts from before the connect begins.
    let mut bound = pin!(gate.bound.map(tokio::time::sleep));
    let mut capture = capture_connection(&mut probe);
    let mut response = client.request(probe);
    let mut connected = pin!(capture.wait_for_connection_metadata());
    let mut capture_ended = false;
    let seen = poll_fn(|cx| {
        if let Poll::Ready(result) = Pin::new(&mut response).poll(cx) {
            return Poll::Ready(Seen::Response(result));
        }
        if !capture_ended && let Poll::Ready(metadata) = connected.as_mut().poll(cx) {
            // The guard holds a read lock of hyper-util's watch channel, so it
            // is read and dropped here. A capture that ended without a
            // connection is done: it is never polled again.
            if metadata.is_some() {
                return Poll::Ready(Seen::Connection);
            }
            capture_ended = true;
        }
        // Registers this task to be woken when the request that started the
        // connect is dropped; the bound is looked at only after that.
        ready!(starter.poll_closed(cx));
        match bound.as_mut().as_pin_mut() {
            Some(sleep) => sleep.poll(cx).map(|()| Seen::Abandoned),
            None => Poll::Ready(Seen::Abandoned),
        }
    })
    .await;
    // In the poll that saw the connection: hyper skips a request whose
    // response is gone, and a hyper that sent the probe would reset it.
    drop(response);
    // A `send` to a starter that is gone fails, and there is no one to tell.
    match seen {
        Seen::Connection | Seen::Response(Ok(_)) => {
            gate.mark_pooled();
            opener.connected();
            let _ = starter.send(Outcome::Connected);
        }
        Seen::Response(Err(error)) => {
            // `connect_info()` is the info of the connection on which the
            // error occurred: hyper-util attaches it to every error that
            // happened on a connection, and to none that happened before
            // there was one. `None` is therefore a failed connect, including
            // a failed HTTP/2 handshake after TLS, which hyper-util reports
            // as a `SendRequest` error, not a connect error, so that
            // `is_connect()` alone would miss it. `Some` is hyper refusing
            // the probe on the connection it was given, which is pooled.
            if error.connect_info().is_none() {
                opener.fail(quoted(&error));
                let _ = starter.send(Outcome::Failed(error));
            } else {
                gate.mark_pooled();
                opener.connected();
                let _ = starter.send(Outcome::Connected);
            }
        }
        Seen::Abandoned => match gate.bound {
            Some(bound) => opener.bound(bound),
            // Dropped as it is, the turn ends as given up.
            None => drop(opener),
        },
    }
}

/// `error`'s chain of messages, rendered as the transport renders the chain
/// of any error it fails with, without the prefix of its message.
fn quoted(error: &legacy::Error) -> Arc<str> {
    let message = connection_message(error);
    Arc::from(message.strip_prefix(CONNECTION_PREFIX).unwrap_or(&message))
}

/// Counts one stream in [`Gate::open_streams`] until it is dropped.
struct OpenStream(Arc<Gate>);

impl Drop for OpenStream {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The connector of the default transport: it tells the gate when a connect
/// begins, and counts every stream it hands to hyper.
#[derive(Clone)]
pub(super) struct Counting<C> {
    inner: C,
    gate: Arc<Gate>,
}

impl<C> Counting<C> {
    pub(super) fn new(inner: C, gate: Arc<Gate>) -> Self {
        Self { inner, gate }
    }
}

impl<C> Service<Uri> for Counting<C>
where
    C: Service<Uri>,
    C::Future: Unpin,
{
    type Response = Counted<C::Response>;
    type Error = C::Error;
    type Future = Connecting<C::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), C::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, destination: Uri) -> Connecting<C::Future> {
        self.gate.connect_began();
        Connecting { inner: self.inner.call(destination), gate: Arc::clone(&self.gate) }
    }
}

/// A connect of [`Counting`] in progress.
pub(super) struct Connecting<F> {
    inner: F,
    gate: Arc<Gate>,
}

impl<F, T, E> Future for Connecting<F>
where
    F: Future<Output = Result<T, E>> + Unpin,
{
    type Output = Result<Counted<T>, E>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Both fields are `Unpin`, so the pinned reference can be turned back
        // into a plain one and the inner future pinned in place again.
        let this = self.get_mut();
        let stream = ready!(Pin::new(&mut this.inner).poll(cx))?;
        Poll::Ready(Ok(Counted { inner: stream, _open: this.gate.stream_opened() }))
    }
}

/// A stream the connector handed to hyper, counted while it lives. Every
/// call is forwarded unchanged, `connected()` included: the ALPN protocol
/// the server chose travels in it.
pub(super) struct Counted<T> {
    inner: T,
    _open: OpenStream,
}

// The inner stream is `Unpin`, so every method turns the pinned reference
// back into a plain one and pins the stream in place again, with no `unsafe`
// projection.
impl<T: Read + Unpin> Read for Counted<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<T: Write + Unpin> Write for Counted<T> {
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

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}

impl<T: Connection> Connection for Counted<T> {
    fn connected(&self) -> Connected {
        self.inner.connected()
    }
}

#[cfg(test)]
#[path = "gate_tests.rs"]
mod tests;
