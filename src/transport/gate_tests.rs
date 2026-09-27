//! The gate's state machine: the futures are polled by hand with a waker
//! that counts its wakes, so each step is exact. The connector is the real
//! one, against a real listener, except where a connect has to fail after
//! the connector has handed over its stream.

use std::task::{Wake, Waker};

use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioExecutor};

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
    drop(opener);

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
fn an_opener_dropped_without_a_connection_passes_the_turn_on() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));
    let first = take_the_turn(&gate, &waker);
    let mut second = pin!(gate.enter());
    let mut third = pin!(gate.enter());
    assert!(poll_once(second.as_mut(), &waker).is_pending());
    assert!(poll_once(third.as_mut(), &waker).is_pending());

    // Dropped, as by a caller's deadline: nobody fails, and exactly one
    // waiter takes the turn.
    drop(first);
    let second = match poll_once(second.as_mut(), &waker) {
        Poll::Ready(Entry::Open(opener)) => opener,
        other => panic!("the second request takes the turn, not {}", outcome(&other)),
    };
    let polled = poll_once(third.as_mut(), &waker);
    assert!(polled.is_pending(), "the third waits for the second, not {}", outcome(&polled));

    drop(second);
    let polled = poll_once(third.as_mut(), &waker);
    assert!(
        matches!(polled, Poll::Ready(Entry::Open(_))),
        "the third request takes the turn the second gave back, not {}",
        outcome(&polled)
    );
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
    drop(later);
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
