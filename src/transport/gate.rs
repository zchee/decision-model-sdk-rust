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
//! What the gate does not see: a connection that ends while requests are in
//! flight is replaced inside hyper-util, which resends a request that had
//! not started, and a burst at that moment can still open a second
//! connection that is closed at once.

use std::{
    future::{Future, poll_fn},
    io,
    pin::{Pin, pin},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll, ready},
};

use http::{Request, Response, Uri};
use hyper::{
    body::Incoming,
    rt::{Read, ReadBufCursor, Write},
};
use hyper_util::client::legacy::{
    self,
    connect::{Connect, Connected, Connection, capture_connection},
};
use tokio::sync::Notify;
use tower_service::Service;

use super::Body;

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
/// - `opening` is handed from one request to the next like a lock: taken
///   with an `Acquire` compare-exchange and given back with a `Release`
///   store, so that the next opener sees what the last one did, its store to
///   `pooled` included. A request that fails to take it reads nothing
///   through it (`Relaxed`), and waits for `changed`, which orders what
///   follows through its own lock.
#[derive(Debug, Default)]
pub(super) struct Gate {
    /// Streams the connector handed to hyper that are not dropped yet.
    open: AtomicUsize,
    /// Whether a request was given a pooled connection since the last
    /// connect began.
    pooled: AtomicBool,
    /// Whether a request holds the turn to open a connection.
    opening: AtomicBool,
    /// Notified when the opener gives its turn back.
    changed: Notify,
}

impl Gate {
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

    /// Waits until a request may go: `None` once the gate is warm, or the
    /// turn to open a connection when no other request holds it.
    async fn enter(self: &Arc<Self>) -> Option<Opener> {
        loop {
            // Registered before the state is read, so that a turn given back
            // between the read and the wait still wakes this request.
            let mut changed = pin!(self.changed.notified());
            changed.as_mut().enable();
            if self.is_warm() {
                return None;
            }
            if self
                .opening
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Some(Opener { gate: Arc::clone(self) });
            }
            changed.await;
        }
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
/// every waiting request, whether the opener has its connection, failed, or
/// was dropped before it could finish.
struct Opener {
    gate: Arc<Gate>,
}

impl Drop for Opener {
    fn drop(&mut self) {
        self.gate.opening.store(false, Ordering::Release);
        self.gate.changed.notify_waiters();
    }
}

/// Sends `request` through `gate`: it waits while another request opens a
/// connection, then goes to the pool as any request does, or opens the
/// connection itself.
///
/// The opener gives its turn back as soon as hyper-util reports the
/// connection it was given, before the response arrives. Hyper-util reports
/// it right after it has put a new connection in the pool, so every request
/// released then finds it there. A report without a connection, or a
/// response that fails first, gives the turn back without marking the pool,
/// and the next request opens.
pub(super) async fn send<C>(
    gate: Arc<Gate>,
    client: legacy::Client<C, Body>,
    mut request: Request<Body>,
) -> Result<Response<Incoming>, legacy::Error>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    let Some(opener) = gate.enter().await else {
        return client.request(request).await;
    };
    let mut capture = capture_connection(&mut request);
    let mut response = client.request(request);
    let mut connected = pin!(capture.wait_for_connection_metadata());
    let mut capture_ended = false;
    let first = poll_fn(|cx| {
        if let Poll::Ready(result) = Pin::new(&mut response).poll(cx) {
            return Poll::Ready(Some(result));
        }
        if !capture_ended && let Poll::Ready(metadata) = connected.as_mut().poll(cx) {
            // The guard holds a read lock of hyper-util's watch channel, so it
            // is read and dropped here. A capture that ended without a
            // connection is done: it is never polled again.
            if metadata.is_some() {
                return Poll::Ready(None);
            }
            capture_ended = true;
        }
        Poll::Pending
    })
    .await;
    match first {
        Some(result) => {
            if result.is_ok() {
                gate.mark_pooled();
            }
            drop(opener);
            result
        }
        None => {
            gate.mark_pooled();
            drop(opener);
            response.await
        }
    }
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
