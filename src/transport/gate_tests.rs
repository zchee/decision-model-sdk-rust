//! The gate's state machine, without a network: the futures are polled by
//! hand with a waker that counts its wakes, so each step is exact.

use std::task::{Wake, Waker};

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

/// A gate with one pooled connection whose stream is open.
fn warm_gate() -> (Arc<Gate>, OpenStream) {
    let gate = Arc::new(Gate::default());
    let stream = gate.stream_opened();
    gate.mark_pooled();
    (gate, stream)
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

#[test]
fn a_warm_gate_lets_every_request_through_without_a_turn() {
    let (gate, _stream) = warm_gate();
    let waker = Waker::from(Arc::new(Wakes::default()));
    for request in 0..3 {
        let mut entered = pin!(gate.enter());
        assert!(
            matches!(poll_once(entered.as_mut(), &waker), Poll::Ready(None)),
            "request {request} goes straight to the pool"
        );
    }
    assert!(!gate.opening.load(Ordering::Relaxed), "no request took the turn");
}

#[test]
fn every_waiter_is_released_by_one_release() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut opener = pin!(gate.enter());
    let Poll::Ready(Some(opener)) = poll_once(opener.as_mut(), &waker) else {
        panic!("the first request on a cold gate is the opener");
    };

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
        let waker = Waker::from(Arc::clone(wakes));
        assert!(
            matches!(poll_once(waiter.as_mut(), &waker), Poll::Ready(None)),
            "waiter {index} goes to the pool, where the connection is"
        );
    }
}

#[test]
fn an_opener_that_ends_without_a_connection_passes_the_turn_on() {
    let gate = Arc::new(Gate::default());
    let waker = Waker::from(Arc::new(Wakes::default()));
    let mut first = pin!(gate.enter());
    let Poll::Ready(Some(first)) = poll_once(first.as_mut(), &waker) else {
        panic!("the first request on a cold gate is the opener");
    };
    let mut second = pin!(gate.enter());
    let mut third = pin!(gate.enter());
    assert!(poll_once(second.as_mut(), &waker).is_pending());
    assert!(poll_once(third.as_mut(), &waker).is_pending());

    // Failed or dropped: either way the opener is dropped without marking
    // the pool, and exactly one waiter takes the turn.
    drop(first);
    let Poll::Ready(Some(second)) = poll_once(second.as_mut(), &waker) else {
        panic!("the second request takes the turn the first gave back");
    };
    assert!(poll_once(third.as_mut(), &waker).is_pending(), "the third waits for the second");

    drop(second);
    assert!(
        matches!(poll_once(third.as_mut(), &waker), Poll::Ready(Some(_))),
        "the third request takes the turn the second gave back"
    );
}
