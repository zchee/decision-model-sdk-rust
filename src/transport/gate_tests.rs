//! The gate's state machine: the futures are polled by hand with a waker
//! that counts its wakes, so each step is exact. The connector is the real
//! one, against a real listener, except where a connect has to fail after
//! the connector has handed over its stream, or has to wait until a test
//! lets it go.

use std::{
    task::{Wake, Waker},
    time::Instant,
};

use bytes::Bytes;
use http::StatusCode;
use hyper_util::{
    client::legacy::connect::HttpConnector,
    rt::{TokioExecutor, TokioIo},
};
use test_support::{Protocol, TestServer, json_response};
use tokio::{net::TcpStream, sync::watch, task::JoinHandle};

use super::*;

/// A waker that counts how often it was woken.
#[derive(Default)]
struct Wakes(AtomicUsize);

impl Wake for Wakes {
    fn wake(self: Arc<Self>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

impl Wakes {
    fn count(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

/// Polls `future` once with `waker`.
fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>, waker: &Waker) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(waker))
}

/// What one poll of `enter` gave, as a word a failed assertion can print.
fn outcome(polled: &Poll<Entry>) -> String {
    match polled {
        Poll::Pending => String::from("waiting"),
        Poll::Ready(Entry::Pool) => String::from("to the pool"),
        Poll::Ready(Entry::Open(_)) => String::from("the turn to open"),
        Poll::Ready(Entry::Failed(failed)) => format!("failed: {failed}"),
    }
}

/// A gate with one pooled connection whose stream is open.
fn warm_gate() -> (Arc<Gate>, OpenStream) {
    let gate = Arc::new(Gate::default());
    let stream = gate.stream_opened();
    gate.mark_pooled();
    (gate, stream)
}

/// The turn to open, taken by the first request on a cold gate.
fn take_the_turn(gate: &Arc<Gate>, waker: &Waker) -> Opener {
    let mut entered = pin!(gate.enter());
    match poll_once(entered.as_mut(), waker) {
        Poll::Ready(Entry::Open(opener)) => opener,
        other => panic!("the first request on a cold gate is the opener, not {}", outcome(&other)),
    }
}

#[test]
fn the_gate_is_warm_only_with_a_pooled_connection_and_an_open_stream() {
    let gate = Arc::new(Gate::default());
    assert!(!gate.is_warm(), "a new transport has no connection");

    gate.mark_pooled();
    assert!(!gate.is_warm(), "pooled, but no stream is open");

    let stream = gate.stream_opened();
    assert_eq!(gate.open_streams(), 1);
    assert!(gate.is_warm(), "pooled, and the stream is open");

    let second = gate.stream_opened();
    drop(stream);
    assert_eq!(gate.open_streams(), 1);
    assert!(gate.is_warm(), "one of two streams closed");

    drop(second);
    assert_eq!(gate.open_streams(), 0);
    assert!(!gate.is_warm(), "every stream closed: the next request opens");
}

#[test]
fn a_connect_that_begins_makes_the_gate_cold() {
    let (gate, _stream) = warm_gate();
    assert!(gate.is_warm());

    gate.connect_began();
    assert!(!gate.is_warm(), "a connect began, and no request was given its connection yet");

    gate.mark_pooled();
    assert!(gate.is_warm());
}

/// hyper-util's own connector, wrapped as the transport wraps its connector,
/// connecting to a real listener: the call alone makes the gate cold, and
/// the stream it hands over is counted until it is dropped.
#[tokio::test]
async fn a_connect_that_begins_through_the_connector_makes_the_gate_cold() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a loopback port");
    let destination: Uri =
        format!("http://{}", listener.local_addr().expect("its address")).parse().expect("a URI");
    let (gate, stream) = warm_gate();
    let mut connector = Counting::new(HttpConnector::new(), Arc::clone(&gate));

    poll_fn(|cx| connector.poll_ready(cx)).await.expect("the connector is ready");
    let connecting = connector.call(destination);
    assert!(!gate.is_warm(), "the connect began: the gate is cold before it ends");

    let counted = connecting.await.expect("the listener accepts");
    assert_eq!(gate.open_streams(), 2, "the stream handed over is counted");
    drop(stream);
    assert!(!gate.is_warm(), "no request was given the new connection yet");
    gate.mark_pooled();
    assert!(gate.is_warm());
    drop(counted);
    assert_eq!(gate.open_streams(), 0, "the dropped stream is no longer counted");
}

#[test]
fn a_warm_gate_lets_every_request_through_without_a_turn() {
    let (gate, _stream) = warm_gate();
    let waker = Waker::from(Arc::new(Wakes::default()));
    for request in 0..3 {
        let mut entered = pin!(gate.enter());
        let polled = poll_once(entered.as_mut(), &waker);
        assert!(
            matches!(polled, Poll::Ready(Entry::Pool)),
            "request {request} goes straight to the pool, not {}",
            outcome(&polled)
        );
    }
    assert_eq!(gate.turns.load(Ordering::Relaxed) & HELD, 0, "no request took the turn");
}

#[test]
fn every_waiter_is_released_by_one_release() {
    let gate = Arc::new(Gate::default());
    let opener = take_the_turn(&gate, &Waker::from(Arc::new(Wakes::default())));

    let wakes: [Arc<Wakes>; 3] = Default::default();
    let mut waiters = [pin!(gate.enter()), pin!(gate.enter()), pin!(gate.enter())];
    for (waiter, wakes) in waiters.iter_mut().zip(&wakes) {
        let waker = Waker::from(Arc::clone(wakes));
        assert!(poll_once(waiter.as_mut(), &waker).is_pending(), "a waiter waits for the turn");
    }

    // The opener has its connection: the pool holds it and its stream is open.
    let _stream = gate.stream_opened();
    gate.mark_pooled();
    opener.connected();

    for (index, (waiter, wakes)) in waiters.iter_mut().zip(&wakes).enumerate() {
        assert_eq!(wakes.count(), 1, "waiter {index} was woken once by the one release");
        let polled = poll_once(waiter.as_mut(), &Waker::from(Arc::clone(wakes)));
        assert!(
            matches!(polled, Poll::Ready(Entry::Pool)),
            "waiter {index} goes to the pool, where the connection is, not {}",
            outcome(&polled)
        );
    }
}

#[test]
fn an_opener_dropped_before_a_connection_fails_every_waiting_request() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));
    let first = take_the_turn(&gate, &waker);
    let mut waiters = [pin!(gate.enter()), pin!(gate.enter())];
    for waiter in &mut waiters {
        assert!(poll_once(waiter.as_mut(), &waker).is_pending());
    }

    // Dropped before a connection was reported, as by its caller's deadline:
    // every request that waited for its connect fails, and none takes the
    // turn over to connect again.
    drop(first);
    for (index, waiter) in waiters.iter_mut().enumerate() {
        let polled = poll_once(waiter.as_mut(), &waker);
        assert_eq!(
            outcome(&polled),
            "failed: the connect this request waited for was given up: \
             the request that started it was dropped before a connection existed",
            "waiter {index}"
        );
    }

    // A request that comes after it finds the turn free and opens.
    let mut later = pin!(gate.enter());
    let polled = poll_once(later.as_mut(), &waker);
    assert!(
        matches!(polled, Poll::Ready(Entry::Open(_))),
        "a request that comes after the drop opens, not {}",
        outcome(&polled)
    );
}

#[test]
fn an_opener_given_a_connection_fails_no_one() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));

    // Given a connection the pool keeps: every waiter goes on to the pool.
    let opener = take_the_turn(&gate, &waker);
    let mut waiters = [pin!(gate.enter()), pin!(gate.enter())];
    for waiter in &mut waiters {
        assert!(poll_once(waiter.as_mut(), &waker).is_pending());
    }
    let stream = gate.stream_opened();
    gate.mark_pooled();
    opener.connected();
    for (index, waiter) in waiters.iter_mut().enumerate() {
        let polled = poll_once(waiter.as_mut(), &waker);
        assert!(
            matches!(polled, Poll::Ready(Entry::Pool)),
            "waiter {index} goes to the pool, not {}",
            outcome(&polled)
        );
    }

    // Given a connection that is gone again, as when its response failed on
    // it: nobody fails, and one waiting request opens.
    drop(stream);
    let opener = take_the_turn(&gate, &waker);
    let mut first = pin!(gate.enter());
    let mut second = pin!(gate.enter());
    assert!(poll_once(first.as_mut(), &waker).is_pending());
    assert!(poll_once(second.as_mut(), &waker).is_pending());
    opener.connected();
    let polled = poll_once(first.as_mut(), &waker);
    assert!(
        matches!(polled, Poll::Ready(Entry::Open(_))),
        "the first waiter takes the turn, not {}",
        outcome(&polled)
    );
    let polled = poll_once(second.as_mut(), &waker);
    assert!(polled.is_pending(), "the second waits for the first, not {}", outcome(&polled));
}

#[test]
fn a_failed_connect_fails_every_waiting_request_and_not_one_that_comes_after() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));
    let opener = take_the_turn(&gate, &waker);
    let mut waiters = [pin!(gate.enter()), pin!(gate.enter())];
    for waiter in &mut waiters {
        assert!(poll_once(waiter.as_mut(), &waker).is_pending());
    }

    opener.fail(Arc::from("client error (Connect): tcp connect error: refused"));
    for (index, waiter) in waiters.iter_mut().enumerate() {
        let polled = poll_once(waiter.as_mut(), &waker);
        assert_eq!(
            outcome(&polled),
            "failed: the connect this request waited for failed: \
             client error (Connect): tcp connect error: refused",
            "waiter {index}"
        );
    }

    // A request that comes after the failure finds the turn free and opens,
    // and one that waits for it is failed only by its failure, not by the
    // earlier one.
    let later = take_the_turn(&gate, &waker);
    let mut waiting = pin!(gate.enter());
    assert!(poll_once(waiting.as_mut(), &waker).is_pending());
    later.connected();
    let polled = poll_once(waiting.as_mut(), &waker);
    assert!(
        matches!(polled, Poll::Ready(Entry::Open(_))),
        "the earlier failure is not this turn's: {}",
        outcome(&polled)
    );
}

/// A connector that hands over a stream on which the HTTP/2 handshake
/// cannot happen: its first write fails, and it never has anything to read.
/// It counts its calls.
#[derive(Clone, Default)]
struct BrokenPipe {
    calls: Arc<AtomicUsize>,
}

impl Service<Uri> for BrokenPipe {
    type Response = BrokenStream;
    type Error = io::Error;
    type Future = SecondPoll;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> SecondPoll {
        self.calls.fetch_add(1, Ordering::Relaxed);
        SecondPoll { polled: false }
    }
}

/// A connect of [`BrokenPipe`]: it ends on its second poll, and on its first
/// wakes its task and returns `Pending`, as no real connect ends on its
/// first poll. Were it to end at once, the opener's handshake would fail
/// inside the opener's first poll, before another request could reach the
/// gate to wait.
struct SecondPoll {
    polled: bool,
}

impl Future for SecondPoll {
    type Output = Result<BrokenStream, io::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if this.polled {
            return Poll::Ready(Ok(BrokenStream));
        }
        this.polled = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// The stream of [`BrokenPipe`]: a connection the connector made and the
/// peer broke before the HTTP/2 preface could be written.
struct BrokenStream;

impl Read for BrokenStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

impl Write for BrokenStream {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::from(io::ErrorKind::BrokenPipe)))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl Connection for BrokenStream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

/// What a request sent through the gate ended with, as a failed assertion
/// can print it.
fn ended(result: &Result<Response<Incoming>, SendError>) -> String {
    match result {
        Ok(response) => format!("a response with status {}", response.status()),
        Err(SendError::Own(error)) => format!("its own error: {error:?}"),
        Err(SendError::Waited(error)) => format!("the waited error: {error}"),
    }
}

/// A connect whose HTTP/2 handshake fails after the connector handed over
/// its stream fails the requests waiting for it, as a failed connect does.
/// hyper-util reports that failure as a `SendRequest` error, not a connect
/// error, and attaches no connection to it: that is what the gate goes by.
#[tokio::test]
async fn a_failed_http2_handshake_fails_every_waiting_request() {
    let gate = Arc::new(Gate::default());
    let connector = BrokenPipe::default();
    let calls = Arc::clone(&connector.calls);
    let client = legacy::Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(Counting::new(connector, Arc::clone(&gate)));
    let request =
        || Request::get("http://127.0.0.1:9/v1/models").body(Body::empty()).expect("a request");

    // Spawned in this order on a current-thread runtime, the first takes the
    // turn and the second waits for it.
    let first = tokio::spawn(send(Arc::clone(&gate), client.clone(), request()));
    let second = tokio::spawn(send(Arc::clone(&gate), client.clone(), request()));
    let first = first.await.expect("the first task ran");
    let second = second.await.expect("the second task ran");

    let Err(SendError::Own(opener)) = &first else {
        panic!("the first request opens and fails with its own error, not {}", ended(&first));
    };
    assert!(opener.connect_info().is_none(), "no connection was reported: {opener:?}");
    assert!(!opener.is_connect(), "hyper-util does not call it a connect error: {opener:?}");
    let Err(SendError::Waited(waited)) = &second else {
        panic!("the second request fails with the waited error, not {}", ended(&second));
    };
    assert_eq!(
        waited.to_string(),
        format!("the connect this request waited for failed: {}", quoted(opener))
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1, "one connect for both requests");

    // A request that comes after the failure opens: the connector is called
    // again.
    let third = send(Arc::clone(&gate), client, request()).await;
    assert!(
        matches!(third, Err(SendError::Own(_))),
        "the third request opens and fails with its own error, not {}",
        ended(&third)
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2, "the third request connected again");
}

/// A request as the SDK sends one, with everything a probe must not carry:
/// a path and a query, the credential, the SDK's headers, a body, and an
/// extension.
fn a_call(uri: &str) -> Request<Body> {
    let mut call = Request::post(uri)
        .header("authorization", "Bearer test-key")
        .header("user-agent", "typesafe-sdk-rust/0.0.0")
        .header("x-typesafe-sdk", "typesafe-sdk-rust/0.0.0")
        .header("content-type", "application/json")
        .header("content-length", "2")
        .body(Body::from(Bytes::from_static(b"{}")))
        .expect("a request");
    call.extensions_mut().insert(Duration::from_secs(1));
    call
}

#[test]
fn the_probe_is_a_connect_to_the_authority_that_declares_a_body_it_does_not_send() {
    let connect = probe(&a_call("https://127.0.0.1:9/base/v1/systemone?model=jev"));

    assert_eq!(connect.method(), Method::CONNECT);
    let uri = connect.uri();
    assert_eq!(
        (uri.scheme_str(), uri.authority().map(uri::Authority::as_str)),
        (Some("https"), Some("127.0.0.1:9")),
        "the call's scheme and authority: {uri}"
    );
    assert_eq!(
        (uri.path(), uri.query()),
        ("/", None),
        "no path but the root a URI with a scheme must have, and no query: {uri}"
    );
    assert_eq!(uri, "https://127.0.0.1:9/");
    let headers: Vec<(&str, &[u8])> =
        connect.headers().iter().map(|(name, value)| (name.as_str(), value.as_bytes())).collect();
    assert_eq!(
        headers,
        [("content-length", &b"1"[..])],
        "exactly one header, the declared length: none of the call's, the credential included"
    );
    assert!(connect.body().is_empty(), "{:?}", connect.body());
    assert!(connect.extensions().is_empty(), "none of the call's extensions");

    // A URI hyper-util sends no request to becomes one it refuses the same
    // way, before any connect.
    for uri in ["/v1/models", "127.0.0.1:9"] {
        assert_eq!(probe(&a_call(uri)).uri(), "/", "the probe for {uri:?}");
    }
}

/// hyper-util's own connector to a real listener, whose connects each wait
/// until the test lets them go: a connect in flight for as long as a test
/// needs, at a server that answers at once once it is reached. It counts its
/// calls.
#[derive(Clone)]
struct HeldConnect {
    inner: HttpConnector,
    go: watch::Receiver<bool>,
    calls: Arc<AtomicUsize>,
}

impl Service<Uri> for HeldConnect {
    type Response = TokioIo<TcpStream>;
    type Error = Box<dyn StdError + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: Uri) -> Self::Future {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let (mut inner, mut go) = (self.inner.clone(), self.go.clone());
        Box::pin(async move {
            go.wait_for(|go| *go).await?;
            poll_fn(|cx| inner.poll_ready(cx)).await?;
            Ok(inner.call(destination).await?)
        })
    }
}

/// An h2c server that answers every request at once, and a client that
/// reaches it through [`HeldConnect`] and a gate.
struct Held {
    server: TestServer,
    gate: Arc<Gate>,
    client: legacy::Client<Counting<HeldConnect>, Body>,
    /// Lets every connect of the connector go, the ones already waiting
    /// included.
    go: watch::Sender<bool>,
    calls: Arc<AtomicUsize>,
}

impl Held {
    /// A gate whose connect may run for `bound` once the request that
    /// started it is gone.
    async fn new(bound: Option<Duration>) -> Self {
        let server = TestServer::start(Protocol::H2c, |_request| async {
            json_response(StatusCode::OK, r#"{"models":[]}"#)
        })
        .await
        .expect("the test server starts");
        let gate = Arc::new(Gate::new(bound));
        let (go, held) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let connector =
            HeldConnect { inner: HttpConnector::new(), go: held, calls: Arc::clone(&calls) };
        let client = legacy::Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(Counting::new(connector, Arc::clone(&gate)));
        Self { server, gate, client, go, calls }
    }

    /// Sends a `GET` of the models through the gate, from a task of its own.
    fn send(&self) -> JoinHandle<Result<Response<Incoming>, SendError>> {
        let request = Request::get(format!("{}/v1/models", self.server.base_url()))
            .body(Body::empty())
            .expect("a request");
        tokio::spawn(send(Arc::clone(&self.gate), self.client.clone(), request))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    /// Waits under a bound of 5 s until the connector has been called
    /// `count` times.
    async fn until_called(&self, count: usize) {
        let called = async {
            while self.calls() < count {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), called).await.unwrap_or_else(|_| {
            panic!("the connector was called {} time(s) in 5 s, not {count}", self.calls())
        });
    }

    /// Lets the connects go.
    fn release(&self) {
        self.go.send_replace(true);
    }

    /// Every request the server saw, as method and path: the requests of
    /// the test only, never the probe.
    fn requests(&self) -> Vec<(Method, String)> {
        self.server
            .requests()
            .iter()
            .map(|request| (request.method.clone(), request.uri.path().to_owned()))
            .collect()
    }
}

/// Lets every task that is ready run: on the current-thread runtime of these
/// tests, a task spawned before this reaches its first await.
async fn settle() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

/// What a request sent from a task of its own ended with, awaited under a
/// bound of 5 s.
async fn outcome_of(
    task: JoinHandle<Result<Response<Incoming>, SendError>>,
) -> Result<Response<Incoming>, SendError> {
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the request ended within 5 s")
        .expect("the request's task ran to its end")
}

/// The request that started a connect gives up, and the connect runs on: a
/// request that comes after that waits for it rather than connecting again,
/// and is served on it.
#[tokio::test]
async fn the_connect_outlives_the_request_that_started_it_and_serves_a_later_one() {
    let held = Held::new(Some(Duration::from_secs(30))).await;
    let starter = held.send();
    held.until_called(1).await;

    starter.abort();
    let starter = starter.await;
    assert!(
        matches!(&starter, Err(error) if error.is_cancelled()),
        "the request that started the connect is gone, not {:?}",
        starter.map(|result| ended(&result))
    );
    let later = held.send();
    settle().await;
    assert!(!later.is_finished(), "the later request waits for the connect in flight");
    assert_eq!(held.calls(), 1, "and does not connect on its own");

    held.release();
    let later = outcome_of(later).await;
    let Ok(response) = &later else {
        panic!("the later request is served on the connect, not {}", ended(&later));
    };
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(held.calls(), 1, "one connect for both requests");
    assert_eq!(held.requests(), [(Method::GET, String::from("/v1/models"))]);
    assert_eq!(held.server.accepted_connections(), 1, "{:#?}", held.server.connections());
}

/// A request waiting for a connect does not fail when the request that
/// started it gives up while the gate's bound has not passed: it is served
/// once the connection exists.
#[tokio::test]
async fn a_starter_that_gives_up_fails_no_one_before_the_bound() {
    const BOUND: Duration = Duration::from_millis(600);

    let held = Held::new(Some(BOUND)).await;
    let started = Instant::now();
    let starter = held.send();
    held.until_called(1).await;
    let waiter = held.send();
    settle().await;

    starter.abort();
    tokio::time::sleep(BOUND / 3).await;
    assert!(
        !waiter.is_finished(),
        "{:?} after the connect began, within its bound of {BOUND:?}, the waiter still waits",
        started.elapsed()
    );
    assert!(held.gate.failed_after(0).is_none(), "no turn ended without a connection");
    assert_eq!(held.gate.turns.load(Ordering::Relaxed) & HELD, HELD, "the turn is still held");

    held.release();
    let waiter = outcome_of(waiter).await;
    assert!(
        matches!(&waiter, Ok(response) if response.status() == StatusCode::OK),
        "the waiter is served, not {}",
        ended(&waiter)
    );
    assert_eq!(held.calls(), 1);
    assert_eq!(held.requests(), [(Method::GET, String::from("/v1/models"))]);
}

/// Once the request that started a connect is gone and the bound has passed
/// since the connect began, the connect is given up: the request waiting for
/// it fails with the bound, and a request after that connects again.
#[tokio::test]
async fn a_connect_is_given_up_once_its_starter_is_gone_and_the_bound_has_passed() {
    const BOUND: Duration = Duration::from_millis(200);

    let held = Held::new(Some(BOUND)).await;
    let started = Instant::now();
    let starter = held.send();
    held.until_called(1).await;
    let waiter = held.send();
    settle().await;

    starter.abort();
    let waiter = outcome_of(waiter).await;
    let elapsed = started.elapsed();
    let Err(SendError::Waited(failed)) = &waiter else {
        panic!("the waiter fails with the bound, not {}", ended(&waiter));
    };
    assert_eq!(
        failed.to_string(),
        "the connect this request waited for did not complete within 200ms after the request \
         that started it gave up"
    );
    assert!(elapsed >= BOUND, "the waiter failed {elapsed:?} after the connect began");

    let later = held.send();
    held.until_called(2).await;
    held.release();
    let later = outcome_of(later).await;
    assert!(
        matches!(&later, Ok(response) if response.status() == StatusCode::OK),
        "a request after the bound connects again and is served, not {}",
        ended(&later)
    );
    assert_eq!(held.calls(), 2, "the later request's connect is the second");
    assert_eq!(held.requests(), [(Method::GET, String::from("/v1/models"))]);
    assert_eq!(held.server.accepted_connections(), 1, "the given-up connect never reached it");
}

/// The bound applies only once the request that started the connect is
/// gone: while that request waits, the connect runs past the bound and
/// serves it and the request waiting beside it.
#[tokio::test]
async fn the_bound_passing_while_the_starter_waits_does_not_end_the_connect() {
    const BOUND: Duration = Duration::from_millis(100);

    let held = Held::new(Some(BOUND)).await;
    let starter = held.send();
    held.until_called(1).await;
    let waiter = held.send();
    tokio::time::sleep(BOUND * 3).await;
    assert!(!starter.is_finished(), "the starter still waits, past the bound");
    assert!(!waiter.is_finished(), "and so does the waiter");

    held.release();
    for (name, request) in [("starter", starter), ("waiter", waiter)] {
        let request = outcome_of(request).await;
        assert!(
            matches!(&request, Ok(response) if response.status() == StatusCode::OK),
            "the {name} is served, not {}",
            ended(&request)
        );
    }
    assert_eq!(held.calls(), 1, "one connect for both");
    assert_eq!(held.requests().len(), 2, "{:?}", held.requests());
}

/// A gate with no bound - a client with no deadline and no connect timeout
/// - gives the connect up with the request that started it: the request
/// waiting for it fails at once, as given up, and a request after that
/// connects again.
#[tokio::test]
async fn without_a_bound_the_connect_ends_with_the_request_that_started_it() {
    let held = Held::new(None).await;
    let starter = held.send();
    held.until_called(1).await;
    let waiter = held.send();
    settle().await;

    starter.abort();
    let waiter = outcome_of(waiter).await;
    let Err(SendError::Waited(failed)) = &waiter else {
        panic!("the waiter fails as given up, not {}", ended(&waiter));
    };
    assert_eq!(
        failed.to_string(),
        "the connect this request waited for was given up: the request that started it was \
         dropped before a connection existed"
    );

    let later = held.send();
    held.until_called(2).await;
    held.release();
    let later = outcome_of(later).await;
    assert!(
        matches!(&later, Ok(response) if response.status() == StatusCode::OK),
        "a request after the drop connects again and is served, not {}",
        ended(&later)
    );
    assert_eq!(held.calls(), 2);
}

/// The task that connects gives the turn back when hyper-util reports the
/// connection, not when the probe's response arrives. The probe here is a
/// request the server would take and never answer, so a task that waited
/// for its response would never say the connection exists.
#[tokio::test]
async fn the_connect_ends_when_the_connection_is_reported_not_at_the_response() {
    let server = TestServer::start(Protocol::H2c, |_request| std::future::pending())
        .await
        .expect("the test server starts");
    let gate = Arc::new(Gate::new(Some(Duration::from_secs(30))));
    let client = legacy::Client::builder(TokioExecutor::new())
        .http2_only(true)
        .build(Counting::new(HttpConnector::new(), Arc::clone(&gate)));
    let opener = take_the_turn(&gate, &Waker::from(Arc::new(Wakes::default())));
    let (outcome, outcome_rx) = oneshot::channel();
    let answered = Request::get(format!("{}/v1/models", server.base_url()))
        .body(Body::empty())
        .expect("a request");

    // A clone, as the transport keeps its client: the pool lives as long as
    // one does.
    tokio::spawn(open(opener, client.clone(), answered, outcome));
    let outcome = tokio::time::timeout(Duration::from_secs(5), outcome_rx)
        .await
        .expect("the connect ended within 5 s")
        .expect("the task said how");
    assert!(
        matches!(outcome, Outcome::Connected),
        "the connection exists: {}",
        match outcome {
            Outcome::Connected => String::from("connected"),
            Outcome::Failed(error) => format!("failed: {error:?}"),
        }
    );
    assert!(gate.is_warm(), "the connection is pooled and its stream open");
    assert_eq!(gate.turns.load(Ordering::Relaxed) & HELD, 0, "the turn was given back");
}
