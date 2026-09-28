//! Tests for the backoff schedule, the seconds-to-`Duration` conversion, and
//! the retry loop.
//!
//! Expected values of the schedule are the upstream SDK's own test tables, or
//! were computed by running its `_backoff` (and Python's `round` and
//! `math.ldexp`) under CPython with the same inputs. Floating-point results
//! are compared by their bits, so a sign of zero or a last-place difference
//! fails the test.
//!
//! The loop is tested the way upstream tests it (`tests/test_retry.py`): a
//! real client against a real loopback server, with the clock and the sleep
//! replaced. [`FakeTime`] records every delay the loop asks for and moves its
//! clock by it instead of waiting, and a server handler moves the same clock
//! to stand for time spent answering, so a budget case runs exactly as
//! upstream's monkeypatched `tenacity.time.monotonic` makes it run. Every
//! upstream case is ported and named in the doc comment of its Rust test; a
//! case the README's deviation table lists is tested for the Rust behaviour,
//! and the comment names the row. A deadline is still real time,
//! 50 ms against a handler held on a channel.

use std::{
    convert::Infallible,
    error::Error as StdError,
    io,
    pin::Pin,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use http_body_util::BodyExt as _;
#[cfg(feature = "hyper")]
use serde::Serialize;
#[cfg(feature = "hyper")]
use test_support::{Protocol, RecordedRequest, TestResponse, TestServer, json_response};
use tokio::sync::Notify;
#[cfg(feature = "hyper")]
use tokio::sync::watch;
use tower_service::Service;

use super::*;
#[cfg(feature = "hyper")]
use crate::{
    Client, RawQuestion,
    transport::{BoxError, HttpVersion, HyperTransport, ResponseBody, TransportSettings},
};
use crate::{ClientBuilder, Noul, PreparedQuestions, Questions, error::ApiError, transport::Body};

/// The upstream `RetryPolicy` defaults: `backoff_initial`, `backoff_max` and
/// `backoff_jitter`.
const INITIAL: f64 = 0.5;
const MAX: f64 = 5.0;
const JITTER: f64 = 0.25;

/// The largest `f64` below 1, the top of the range a random draw comes from.
const DRAW_BELOW_ONE: f64 = 1.0 - f64::EPSILON / 2.0;

#[track_caller]
fn assert_same_f64(got: f64, want: f64, context: &str) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{context}: got {got:?} (bits {:#018x}), want {want:?} (bits {:#018x})",
        got.to_bits(),
        want.to_bits(),
    );
}

#[test]
fn default_schedule_doubles_then_caps_as_upstream_test_backoff_dates_cap_and_jitter() {
    let table = [(1, 0.5), (2, 1.0), (3, 2.0), (4, 4.0), (5, 5.0), (20, 5.0)];
    for (attempt, want) in table {
        let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        assert_same_f64(got, want, &format!("attempt {attempt}, draw 0"));
    }
}

#[test]
fn a_full_draw_removes_the_whole_jitter_share_as_upstream() {
    // Upstream patches `random.random` to return 1.0, outside its real range,
    // to reach the largest reduction: 0.5 * (1 - 0.25).
    let got = backoff_seconds(1, INITIAL, MAX, JITTER, 1.0);
    assert_same_f64(got, 0.375, "attempt 1, draw 1.0");
}

#[test]
fn extreme_values_match_upstream_test_backoff_extreme_values() {
    let table = [
        // A delay far below a millisecond rounds to zero.
        (1e-300, 1e300, 1, 0.0),
        // 2000 doublings of 1e-300 would overflow; the log2 cap test stops them.
        (1e-300, 1e300, 2000, 1e300),
        (1e308, 1e308, 1, 1e308),
        // 0.5 is above the cap, and the cap rounds up to 0.001; the result
        // never exceeds the un-jittered delay, so it is the cap itself.
        (0.5, 0.0006, 1, 0.0006),
    ];
    for (initial, max, attempt, want) in table {
        let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
        assert_same_f64(got, want, &format!("initial {initial:e}, max {max:e}, attempt {attempt}"));
    }
}

#[test]
fn zero_initial_or_max_disables_backoff_with_a_positive_zero() {
    let cases = [
        ("zero initial", 0.0, MAX),
        ("zero max", INITIAL, 0.0),
        ("negative-zero initial", -0.0, MAX),
        ("negative-zero max", INITIAL, -0.0),
    ];
    for (name, initial, max) in cases {
        for attempt in [0, 1, 7, u32::MAX] {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.5);
            assert_same_f64(got, 0.0, &format!("{name}, attempt {attempt}"));
        }
    }
}

#[test]
fn attempt_zero_follows_the_formula_and_gives_half_the_initial_delay() {
    let got = backoff_seconds(0, INITIAL, MAX, JITTER, 0.0);
    assert_same_f64(got, 0.25, "attempt 0, draw 0");
}

#[test]
fn the_draw_and_jitter_bounds_match_cpython() {
    let cases = [
        ("draw 0 leaves the delay whole", 1, JITTER, 0.0, 0.5),
        ("the largest draw below 1 rounds to the full reduction", 1, JITTER, DRAW_BELOW_ONE, 0.375),
        ("jitter 0 ignores the draw", 1, 0.0, DRAW_BELOW_ONE, 0.5),
        ("jitter 1 with a draw near 1 leaves almost nothing", 1, 1.0, DRAW_BELOW_ONE, 0.0),
        ("jitter 1 with a draw of one half halves the delay", 3, 1.0, 0.5, 1.0),
        ("a middle draw", 2, JITTER, 0.5, 0.875),
        ("a draw that needs rounding", 4, JITTER, 0.3, 3.7),
    ];
    for (name, attempt, jitter, draw, want) in cases {
        let got = backoff_seconds(attempt, INITIAL, MAX, jitter, draw);
        assert_same_f64(got, want, name);
    }
}

#[test]
fn jitter_never_raises_the_delay_above_the_exponential() {
    for attempt in 0..=8 {
        let exponential = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        for step in 0..=100 {
            let draw = f64::from(step) / 100.0 * DRAW_BELOW_ONE;
            let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, draw);
            assert!(
                got <= exponential && got >= exponential * (1.0 - JITTER) - 0.0005,
                "attempt {attempt}, draw {draw}: {got} is outside [{} - rounding, {exponential}]",
                exponential * (1.0 - JITTER),
            );
        }
    }
}

#[test]
fn rounding_is_python_round_half_even_on_the_exact_value_not_scaled_rounding() {
    // 1.0005 is stored as 1.000499999999999989..., which Python rounds down;
    // scaling by 1000 first gives exactly 1000.5, which `f64::round` rounds up.
    let got = backoff_seconds(1, 1.0005, MAX, JITTER, 0.0);
    assert_same_f64(got, 1.0, "initial 1.0005, draw 0");
    // 0.125 * (1 - 0.5 * 1) is 0.0625 exactly, a true tie: Python keeps the
    // even digit, 0.062, where half-away-from-zero would give 0.063.
    let got = backoff_seconds(1, 0.125, MAX, 1.0, 0.5);
    assert_same_f64(got, 0.062, "initial 0.125, jitter 1, draw 0.5");
}

#[test]
fn round_to_millis_matches_cpython_round() {
    let cases = [
        (0.0625, 0.062),
        (2.0625, 2.062),
        (0.4375, 0.438),
        (1.0005, 1.0),
        (0.0005, 0.001),
        (0.000_499_9, 0.0),
        (0.1875, 0.188),
        (-0.0004, -0.0),
        (-0.0, -0.0),
        (4.5285770979303905e271, 4.5285770979303905e271),
        // The widest text the stack buffer holds: 16 integer digits, a sign,
        // a point and three decimals.
        (-4_503_599_627_370_495.5, -4_503_599_627_370_495.5),
        (4_503_599_627_370_496.0, 4_503_599_627_370_496.0),
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
    ];
    for (x, want) in cases {
        assert_same_f64(round_to_millis(x), want, &format!("round({x:?}, 3)"));
    }
    assert!(round_to_millis(f64::NAN).is_nan(), "round(nan, 3) is nan");
}

#[test]
fn the_default_schedule_never_decreases_and_settles_at_the_cap() {
    let mut previous = 0.0;
    for attempt in 0..=100 {
        let got = backoff_seconds(attempt, INITIAL, MAX, JITTER, 0.0);
        assert!(got >= previous, "attempt {attempt}: {got} is below the previous delay {previous}");
        previous = got;
    }
    assert_same_f64(previous, MAX, "attempt 100");
}

#[test]
fn extreme_ranges_stay_finite_monotonic_and_exact_through_every_doubling() {
    // The widest ranges an f64 allows: the smallest subnormal up to the
    // largest finite value, and upstream's 1e-300 up to 1e300. Doubling
    // either with `initial * 2f64.powi(exponent)` overflows: 2^1992 is not
    // representable, although 1e-300 times it is.
    for (initial, max) in [(f64::from_bits(1), f64::MAX), (1e-300, 1e300)] {
        let mut previous = 0.0;
        for attempt in 0..=2200 {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
            let context = format!("initial {initial:e}, max {max:e}, attempt {attempt}");
            assert!(got.is_finite() && got <= max, "{context}: {got} is infinite or above the cap");
            assert!(got >= previous, "{context}: {got} is below the previous delay {previous}");
            // Above 2^52 rounding is the identity, so an uncapped delay must be
            // exactly twice the one before it.
            if previous >= 4_503_599_627_370_496.0 && got < max {
                assert_same_f64(got, previous * 2.0, &context);
            }
            previous = got;
        }
        assert_same_f64(previous, max, &format!("initial {initial:e}, max {max:e}, attempt 2200"));
    }
}

#[test]
fn doubling_matches_cpython_ldexp_near_and_at_the_cap() {
    let subnormal = f64::from_bits(1);
    let cases = [
        (1900, 1e-300, 1e300, 4.5285770979303905e271),
        (1992, 1e-300, 1e300, 2.2424427642075284e299),
        // log2(1e300) - log2(1e-300) is 1993.157: exponents up to 1993 double.
        (1994, 1e-300, 1e300, 8.969771056830114e299),
        (1995, 1e-300, 1e300, 1e300),
        (2000, subnormal, f64::MAX, 2.83625966735417e278),
        // log2(f64::MAX) rounds to 1024, so the cap is at exponent 2098:
        // exponent 2097 is 2^-1074 * 2^2097 = 2^1023.
        (2098, subnormal, f64::MAX, 8.98846567431158e307),
        (2099, subnormal, f64::MAX, f64::MAX),
    ];
    for (attempt, initial, max, want) in cases {
        let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
        assert_same_f64(got, want, &format!("initial {initial:e}, max {max:e}, attempt {attempt}"));
    }
}

#[test]
fn attempts_beyond_i32_max_reach_the_cap_without_overflow() {
    let beyond_i32 = 1_u32 << 31;
    for attempt in [beyond_i32, beyond_i32 + 1, u32::MAX] {
        for (initial, max) in [(INITIAL, MAX), (1e-300, 1e300), (f64::from_bits(1), f64::MAX)] {
            let got = backoff_seconds(attempt, initial, max, JITTER, 0.0);
            assert_same_f64(
                got,
                max,
                &format!("initial {initial:e}, max {max:e}, attempt {attempt}"),
            );
        }
    }
}

#[test]
fn inputs_a_policy_rejects_do_not_panic() {
    // An infinite cap never caps, so the doubling saturates to infinity; NaN
    // propagates. The retry policy rejects both before they reach here.
    let got = backoff_seconds(u32::MAX, INITIAL, f64::INFINITY, JITTER, 0.0);
    assert_same_f64(got, f64::INFINITY, "infinite max, attempt u32::MAX");
    assert!(backoff_seconds(1, f64::NAN, MAX, JITTER, 0.0).is_nan(), "NaN initial gives NaN");
    assert_eq!(seconds_to_duration(got), Duration::MAX, "an infinite delay saturates");
}

#[test]
fn scalbn_matches_cpython_ldexp() {
    let cases = [
        (1e-300, 1992, 4.484885528415057e299),
        (f64::from_bits(1), 2097, 8.98846567431158e307),
        (f64::from_bits(1), 2098, f64::INFINITY),
        (1.0, 1023, 8.98846567431158e307),
        (1.0, 1024, f64::INFINITY),
        (1.0, i32::MAX, f64::INFINITY),
        (1.0, -1022, 2.2250738585072014e-308),
        // Results in the subnormal range are rounded once, ties to even.
        (1.5, -1074, 1e-323),
        (3.0, -1075, 1e-323),
        (0.75, -1073, 1e-323),
        (1.0, -1076, 0.0),
        (1e300, -2000, 8.709809816217217e-303),
        (f64::MAX, i32::MIN, 0.0),
        (1e-310, -10, 9.765625e-314),
        (1e-310, 100, 1.2676506002282255e-280),
        (-0.0, 5, -0.0),
    ];
    for (x, n, want) in cases {
        assert_same_f64(scalbn(x, n), want, &format!("ldexp({x:e}, {n})"));
    }
}

#[test]
fn seconds_to_duration_saturates_and_maps_nan_and_negatives_to_zero() {
    let cases = [
        ("zero", 0.0, Duration::ZERO),
        ("negative zero", -0.0, Duration::ZERO),
        ("one nanosecond", 1e-9, Duration::from_nanos(1)),
        ("a backoff delay", 0.375, Duration::from_millis(375)),
        ("the default cap", 5.0, Duration::from_secs(5)),
        ("1e300 seconds, beyond Duration::MAX", 1e300, Duration::MAX),
        ("infinity", f64::INFINITY, Duration::MAX),
        ("NaN", f64::NAN, Duration::ZERO),
        ("minus one", -1.0, Duration::ZERO),
        ("negative infinity", f64::NEG_INFINITY, Duration::ZERO),
    ];
    for (name, seconds, want) in cases {
        assert_eq!(seconds_to_duration(seconds), want, "{name}: {seconds:?} s");
    }
}

// ------------------------------------------------------------- the fake

/// A clock that stands still until something moves it, a sleep that records
/// its delay and moves the clock by it instead of waiting, and a draw fixed
/// by the test.
pub(super) struct FakeTime {
    base: Instant,
    wall: SystemTime,
    offset: Mutex<Duration>,
    delays: Mutex<Vec<Duration>>,
    draw: Mutex<f64>,
    /// Whether a sleep never ends, to hold a call inside its wait.
    hang: bool,
    /// Notified when a hanging sleep is reached.
    parked: Notify,
    /// Set when a hanging sleep's future is dropped.
    sleep_dropped: AtomicBool,
}

impl FakeTime {
    /// A clock whose wall time reads `1_000_000` seconds after the epoch, the
    /// instant upstream's `test_backoff_dates_cap_and_jitter` pins, with a
    /// draw of one half.
    fn new() -> Arc<Self> {
        Arc::new(Self::build(false))
    }

    /// The same, with a sleep that never ends.
    #[cfg(feature = "hyper")]
    fn hanging() -> Arc<Self> {
        Arc::new(Self::build(true))
    }

    fn build(hang: bool) -> Self {
        Self {
            base: Instant::now(),
            wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000),
            offset: Mutex::new(Duration::ZERO),
            delays: Mutex::new(Vec::new()),
            draw: Mutex::new(0.5),
            hang,
            parked: Notify::new(),
            sleep_dropped: AtomicBool::new(false),
        }
    }

    /// Moves the clock, as time spent elsewhere.
    fn advance(&self, by: Duration) {
        let mut offset = lock(&self.offset);
        *offset = offset.saturating_add(by);
    }

    /// Every delay slept so far, oldest first.
    fn delays(&self) -> Vec<Duration> {
        lock(&self.delays).clone()
    }

    #[cfg(feature = "hyper")]
    fn clear_delays(&self) {
        lock(&self.delays).clear();
    }

    fn set_draw(&self, draw: f64) {
        *lock(&self.draw) = draw;
    }
}

impl Time for FakeTime {
    fn now(&self) -> Instant {
        self.base.checked_add(*lock(&self.offset)).expect("a test clock stays in range")
    }

    fn system_now(&self) -> SystemTime {
        self.wall.checked_add(*lock(&self.offset)).expect("a test clock stays in range")
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        lock(&self.delays).push(delay);
        self.advance(delay);
        async move {
            if self.hang {
                let _dropped = SetOnDrop(&self.sleep_dropped);
                self.parked.notify_one();
                std::future::pending::<()>().await;
            }
        }
    }

    fn draw(&self) -> f64 {
        *lock(&self.draw)
    }
}

/// Sets its flag when it is dropped: when the future holding it is dropped
/// while suspended.
struct SetOnDrop<'a>(&'a AtomicBool);

impl Drop for SetOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl RetryPolicy {
    /// This policy, run on `time` instead of the real clocks.
    fn on(mut self, time: &Arc<FakeTime>) -> Self {
        self.time = Some(Arc::clone(time));
        self
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("no test thread panicked while holding a lock")
}

// ----------------------------------------------------------- the harness

/// `RESULT` of upstream `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("../tests/fixtures/result.json");

/// The deadline of an attempt that is meant to time out, against a handler
/// that never answers it.
const SHORT: Duration = Duration::from_millis(50);

/// A JSON response with `status`, `body` and the extra headers.
#[cfg(feature = "hyper")]
fn respond(status: u16, body: &str, headers: &[(&str, &str)]) -> TestResponse {
    let status = StatusCode::from_u16(status).expect("a test status is valid");
    let mut response = json_response(status, Bytes::copy_from_slice(body.as_bytes()));
    for (name, value) in headers {
        response.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).expect("a test header name is valid"),
            HeaderValue::from_str(value).expect("a test header value is valid"),
        );
    }
    response
}

/// A server answering its `n`th request, counted from 1, with `answer(n)`.
#[cfg(feature = "hyper")]
async fn serve<F>(answer: F) -> TestServer
where
    F: Fn(usize, &RecordedRequest) -> TestResponse + Send + Sync + 'static,
{
    TestServer::start_nth(Protocol::Http1, answer).await.expect("the test server starts")
}

/// A client of `server` with `policy`, reading nothing from the environment.
#[cfg(feature = "hyper")]
fn client(server: &TestServer, policy: RetryPolicy) -> Client {
    ClientBuilder::new()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_model("jev-latest")
        .retry(policy)
        .build_with_env(|_: &str| None::<String>)
        .expect("the client builds")
}

/// One noul question, `q`, as upstream's helpers ask it.
fn questions() -> PreparedQuestions {
    Questions::new().noul("q", Noul::new().instructions("?")).prepare().expect("prepares")
}

/// The same question written as a raw question.
#[cfg(feature = "hyper")]
fn raw_questions() -> PreparedQuestions {
    Questions::new()
        .raw("q", RawQuestion::new("noul").field("instructions", "?"))
        .prepare()
        .expect("prepares")
}

/// The two endpoints upstream parametrizes the loop tests over.
#[derive(Debug, Clone, Copy)]
#[cfg(feature = "hyper")]
enum Resource {
    Models,
    SystemOne,
}

#[cfg(feature = "hyper")]
impl Resource {
    const ALL: [Self; 2] = [Self::Models, Self::SystemOne];

    /// The endpoint as an error names it.
    fn endpoint(self, server: &TestServer) -> String {
        match self {
            Self::Models => format!("GET {}/v1/models", server.base_url()),
            Self::SystemOne => format!("POST {}/v1/systemone", server.base_url()),
        }
    }

    /// One call, with `retry` in place of the client's policy when given.
    async fn call(self, client: &Client, retry: Option<RetryPolicy>) -> Result<(), Error> {
        match self {
            Self::Models => {
                let mut request = client.models().list();
                if let Some(policy) = retry {
                    request = request.retry(policy);
                }
                request.send().await.map(drop)
            }
            Self::SystemOne => {
                let questions = questions();
                let mut request = client.system_one("x", &questions);
                if let Some(policy) = retry {
                    request = request.retry(policy);
                }
                request.send().await.map(drop)
            }
        }
    }
}

/// The `X-TypeSafe-Retry-Count` of each request, `None` where it is absent.
#[cfg(feature = "hyper")]
fn retry_counts(requests: &[RecordedRequest]) -> Vec<Option<String>> {
    requests
        .iter()
        .map(|request| {
            request.header_values("x-typesafe-retry-count").first().map(|count| (*count).to_owned())
        })
        .collect()
}

/// What `retry_counts` reads for `attempts` attempts: none, then 1, 2, ...
#[cfg(feature = "hyper")]
fn expected_counts(attempts: usize) -> Vec<Option<String>> {
    (0..attempts).map(|attempt| (attempt > 0).then(|| attempt.to_string())).collect()
}

/// An API error with `status`, rendered as `display` in full, and no cause.
#[track_caller]
fn assert_api(error: &Error, status: u16, display: &str) {
    let ErrorKind::Api(api) = error.kind() else {
        panic!("an API error was expected, not {error:?}");
    };
    assert_eq!(api.status().as_u16(), status, "{error:?}");
    assert_eq!(error.to_string(), display, "{error:?}");
    assert!(error.source().is_none(), "an API error has no cause: {error:?}");
}

/// A config error rendered as `display` in full, with no cause.
#[track_caller]
fn assert_config(result: Result<RetryPolicy, Error>, display: &str) {
    let error = result.expect_err("the policy is refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "a config error was expected: {error:?}");
    assert_eq!(error.to_string(), display, "{error:?}");
    assert!(error.source().is_none(), "{error:?}");
}

/// A transport that fails its first `failures` calls with an I/O error of
/// `kind` whose text is `attempt <n>`, and hands the rest to the default
/// transport: a connection that could not be made, or broke, as upstream's
/// mock transport raises `ConnectError` or `ReadError`. With
/// `ready_failures`, its first calls to `poll_ready` fail instead, with
/// `failed`: a failure before anything is sent, as upstream's
/// `LocalProtocolError`.
#[derive(Clone)]
#[cfg(feature = "hyper")]
struct Flaky {
    inner: HyperTransport,
    calls: Arc<AtomicUsize>,
    failures: usize,
    kind: io::ErrorKind,
    readies: Arc<AtomicUsize>,
    ready_failures: usize,
}

#[cfg(feature = "hyper")]
impl Flaky {
    fn new(failures: usize, kind: io::ErrorKind) -> Self {
        let settings = TransportSettings {
            version: HttpVersion::Auto,
            extra_roots: Vec::new(),
            connect_timeout: None,
            attempt_deadline: None,
        };
        Self {
            inner: HyperTransport::new(settings).expect("the default transport builds"),
            calls: Arc::new(AtomicUsize::new(0)),
            failures,
            kind,
            readies: Arc::new(AtomicUsize::new(0)),
            ready_failures: 0,
        }
    }

    /// A transport whose first `failures` calls to `poll_ready` fail, and
    /// whose calls all go through.
    fn not_ready(failures: usize) -> Self {
        Self { ready_failures: failures, ..Self::new(0, io::ErrorKind::Other) }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn readies(&self) -> usize {
        self.readies.load(Ordering::SeqCst)
    }
}

#[cfg(feature = "hyper")]
type Answered = Pin<Box<dyn Future<Output = Result<Response<ResponseBody>, BoxError>> + Send>>;

#[cfg(feature = "hyper")]
impl Service<Request<Body>> for Flaky {
    type Response = Response<ResponseBody>;
    type Error = BoxError;
    type Future = Answered;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let ready = self.readies.fetch_add(1, Ordering::SeqCst) + 1;
        if ready <= self.ready_failures {
            return Poll::Ready(Err(Box::new(io::Error::other("failed"))));
        }
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Answered {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call <= self.failures {
            let error: BoxError = Box::new(io::Error::new(self.kind, format!("attempt {call}")));
            return Box::pin(std::future::ready(Err(error)));
        }
        Box::pin(self.inner.call(request))
    }
}

/// A client of `server` through `transport`, with `policy`. Every setting the
/// environment could supply is given, so none is read.
#[cfg(feature = "hyper")]
fn flaky_client(server: &TestServer, transport: Flaky, policy: RetryPolicy) -> Client<Flaky> {
    ClientBuilder::new()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_model("jev-latest")
        .retry(policy)
        .build_with_service(transport)
        .expect("the client builds")
}

/// A server that holds its first `held` requests until the test ends and
/// answers the rest with `{"models": []}`: an attempt with a short deadline
/// times out on each held one.
#[cfg(feature = "hyper")]
async fn holding(held: usize) -> (TestServer, watch::Sender<bool>) {
    let (release, released) = watch::channel(false);
    let served = Arc::new(AtomicUsize::new(0));
    let server = TestServer::start(Protocol::Http1, move |_| {
        let mut released = released.clone();
        let attempt = served.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            if attempt <= held {
                // Held on a channel the test never sends on before the
                // attempt gives up, not on a timer.
                drop(released.wait_for(|released| *released).await);
            }
            respond(200, r#"{"models": []}"#, &[])
        }
    })
    .await
    .expect("the test server starts");
    (server, release)
}

/// The result a custom service returns on every attempt, or a held response.
#[derive(Debug, Clone, Copy)]
enum AttemptOutcome {
    Io(io::ErrorKind),
    Status(StatusCode),
    Pending,
}

/// Counts attempts and records when a pending response future is cancelled.
#[derive(Clone)]
struct AttemptService {
    outcome: AttemptOutcome,
    calls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
}

impl Service<Request<Body>> for AttemptService {
    type Response = Response<Body>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, io::Error>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self.outcome;
        let dropped = Arc::clone(&self.dropped);
        Box::pin(async move {
            match outcome {
                AttemptOutcome::Io(kind) => Err(io::Error::new(kind, "attempt failed")),
                AttemptOutcome::Status(status) => {
                    let mut response = Response::new(Body::empty());
                    *response.status_mut() = status;
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        response.headers_mut().insert(RETRY_AFTER, HeaderValue::from_static("0"));
                    }
                    Ok(response)
                }
                AttemptOutcome::Pending => {
                    let _dropped = SetOnDrop(&dropped);
                    std::future::pending().await
                }
            }
        })
    }
}

#[tokio::test]
async fn none_makes_one_attempt_whatever_fails() {
    const NONE: RetryPolicy = RetryPolicy::none();
    assert_eq!(format!("{NONE:?}"), format!("{:?}", RetryPolicy::new().max_retries(0)));
    let cases = [
        AttemptOutcome::Io(io::ErrorKind::ConnectionRefused),
        AttemptOutcome::Io(io::ErrorKind::TimedOut),
        AttemptOutcome::Status(StatusCode::TOO_MANY_REQUESTS),
        AttemptOutcome::Status(StatusCode::SERVICE_UNAVAILABLE),
    ];
    for outcome in cases {
        let time = FakeTime::new();
        let transport = AttemptService { outcome, calls: Arc::default(), dropped: Arc::default() };
        let client = ClientBuilder::new()
            .api_key("test-key")
            .base_url("http://127.0.0.1:9")
            .default_model("jev-latest")
            .retry(RetryPolicy::none().on(&time))
            .build_with_service(transport.clone())
            .expect("the custom-service client builds");

        let error = client.models().list().send().await.expect_err("every attempt fails");
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1, "{outcome:?}: {error:?}");
        assert!(time.delays().is_empty(), "{outcome:?}: no retry delay is scheduled");
        match outcome {
            AttemptOutcome::Io(kind) => {
                assert!(matches!(error.kind(), ErrorKind::Connection), "{outcome:?}: {error:?}");
                assert_eq!(error.to_string(), "Connection error: attempt failed", "{outcome:?}");
                let source = error.source().expect("the I/O error remains the cause");
                assert_eq!(
                    source.downcast_ref::<io::Error>().expect("an I/O error").kind(),
                    kind,
                    "{outcome:?}"
                );
            }
            AttemptOutcome::Status(status) => {
                let code = status.as_u16();
                assert_api(
                    &error,
                    code,
                    &format!("GET http://127.0.0.1:9/v1/models: {code} status code (no body)"),
                );
            }
            AttemptOutcome::Pending => panic!("the failure cases contain no pending response"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn dropping_the_call_future_cancels_the_attempt_and_nothing_follows() {
    let time = FakeTime::new();
    let transport = AttemptService {
        outcome: AttemptOutcome::Pending,
        calls: Arc::default(),
        dropped: Arc::default(),
    };
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .retry(RetryPolicy::new().on(&time))
        .build_with_service(transport.clone())
        .expect("the custom-service client builds");

    let mut call = Box::pin(client.models().list().send());
    std::future::poll_fn(|cx| {
        assert!(call.as_mut().poll(cx).is_pending(), "the attempt is held in the service");
        Poll::Ready(())
    })
    .await;
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1, "the first attempt started");
    assert!(!transport.dropped.load(Ordering::SeqCst), "the attempt is still in flight");
    drop(call);
    assert!(transport.dropped.load(Ordering::SeqCst), "the in-flight attempt was dropped");

    let past_budget = Duration::from_secs(3600);
    time.advance(past_budget);
    tokio::time::advance(past_budget).await;
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1, "no attempt follows cancellation");
    assert!(time.delays().is_empty(), "no retry wait follows cancellation");
}

#[tokio::test(start_paused = true)]
async fn none_with_a_per_attempt_deadline_times_out_after_exactly_one_call() {
    let transport = AttemptService {
        outcome: AttemptOutcome::Pending,
        calls: Arc::default(),
        dropped: Arc::default(),
    };
    let deadline = Duration::from_secs(1);
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .retry(RetryPolicy::none())
        .timeout(deadline)
        .build_with_service(transport.clone())
        .expect("the custom-service client builds");

    let started = tokio::time::Instant::now();
    let error = client.models().list().send().await.expect_err("the held attempt times out");
    assert_eq!(started.elapsed(), deadline, "the paused clock advances to the attempt deadline");
    assert_eq!(transport.calls.load(Ordering::SeqCst), 1, "none permits only one call");
    assert!(
        matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == deadline),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "Request timed out (timeout=1s).");
    assert!(error.source().is_none(), "the SDK's deadline has no transport cause");
    assert!(transport.dropped.load(Ordering::SeqCst), "the timed-out attempt was dropped");
}

// -------------------------------------------------- ports of test_retry.py

/// The default policy runs on the real clock. Its public settings are
/// upstream's, pinned by their `Debug` in
/// `tests/retry.rs::the_default_policy_is_upstreams_and_prints_every_setting`.
#[test]
fn the_default_policy_runs_on_the_real_clock() {
    assert!(RetryPolicy::default().time.is_none());
}

/// `test_retry_policy_invalid_timeout`: a budget of zero is refused with
/// upstream's message. Its `-1`, `inf` and `nan` cannot be written as a
/// `Duration` (README deviation row "Unknown fields rejected on typed
/// questions; `RetryPolicy` field types checked at run time").
#[test]
fn a_zero_budget_is_refused_as_upstream_test_retry_policy_invalid_timeout() {
    assert_config(
        RetryPolicy::default().timeout(Duration::ZERO),
        "timeout must be a positive, finite number of seconds.",
    );
    let smallest = RetryPolicy::default().timeout(Duration::from_nanos(1)).expect("accepted");
    assert_eq!(smallest.timeout, Some(Duration::from_nanos(1)));
    assert_eq!(RetryPolicy::default().no_timeout().timeout, None);
}

/// `test_zero_backoff_retries`: a zero initial delay, a zero cap, or both,
/// retry at once, whether or not the retry succeeds.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn zero_backoff_retries_at_once_as_upstream_test_zero_backoff_retries() {
    let millis = Duration::from_millis;
    for (initial, max) in
        [(millis(0), millis(5000)), (millis(500), millis(0)), (millis(0), millis(0))]
    {
        for recover in [false, true] {
            let case = format!("initial {initial:?}, max {max:?}, recover {recover}");
            let server = serve(move |attempt, _| {
                if recover && attempt == 2 {
                    respond(200, r#"{"models": []}"#, &[])
                } else {
                    respond(503, r#"{"message": "temporarily unavailable"}"#, &[])
                }
            })
            .await;
            let time = FakeTime::new();
            let policy =
                RetryPolicy::default().max_retries(1).backoff_initial(initial).backoff_max(max);
            let client = client(&server, policy.on(&time));

            let outcome = client.models().list().send().await;
            if recover {
                let response = outcome.unwrap_or_else(|error| panic!("{case}: {error:?}"));
                assert!(response.models().is_empty(), "{case}");
            } else {
                let error = outcome.expect_err("every attempt fails");
                let display =
                    format!("GET {}/v1/models: 503 temporarily unavailable", server.base_url());
                assert_api(&error, 503, &display);
            }
            assert_eq!(retry_counts(&server.requests()), expected_counts(2), "{case}");
            assert_eq!(time.delays(), [Duration::ZERO], "{case}");
        }
    }
}

/// `test_invalid_backoff`: a negative, NaN or infinite delay cannot be written
/// as a `Duration` (README deviation row "Unknown fields rejected on typed
/// questions; `RetryPolicy` field types checked at run time"). The largest one
/// can, and it degrades instead of panicking: under a budget it ends the
/// retrying, and with no budget it is waited out as `Duration::MAX`.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_largest_backoff_degrades_as_upstream_test_invalid_backoff() {
    let server = serve(|_, _| respond(503, r#"{"message": "down"}"#, &[])).await;
    let display = format!("GET {}/v1/models: 503 down", server.base_url());
    let largest =
        || RetryPolicy::default().backoff_initial(Duration::MAX).backoff_max(Duration::MAX);

    let time = FakeTime::new();
    let client = client(&server, largest().on(&time));
    let error = client.models().list().send().await.expect_err("fails");
    assert_api(&error, 503, &display);
    assert_eq!(server.request_count(), 1, "a wait of Duration::MAX reaches any budget");
    assert!(time.delays().is_empty(), "nothing was waited: {:?}", time.delays());

    // With no jitter drawn, the doubling saturates to the largest delay.
    let time = FakeTime::new();
    time.set_draw(0.0);
    let error = client
        .models()
        .list()
        .retry(largest().no_timeout().on(&time))
        .send()
        .await
        .expect_err("fails");
    assert_api(&error, 503, &display);
    assert_eq!(server.request_count(), 1 + 3);
    assert_eq!(time.delays(), [Duration::MAX, Duration::MAX]);
}

/// `test_invalid_backoff_jitter`: a jitter outside 0 to 1 is refused with
/// upstream's message; both ends are accepted.
#[test]
fn a_jitter_outside_zero_to_one_is_refused_as_upstream_test_invalid_backoff_jitter() {
    for jitter in [
        -0.25,
        -0.1,
        1.000_001,
        1.1,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -f64::MIN_POSITIVE,
    ] {
        assert_config(
            RetryPolicy::default().backoff_jitter(jitter),
            "backoff_jitter must be between zero and one.",
        );
    }
    for jitter in [0.0, -0.0, 0.5, 1.0] {
        let policy = RetryPolicy::default().backoff_jitter(jitter).expect("accepted");
        assert_same_f64(policy.backoff_jitter, jitter, "an accepted jitter is kept as given");
    }
}

/// `test_invalid_max_retries`: `-1`, `0.5`, `nan` and `inf` cannot be written
/// as a `u32` (README deviation row "Unknown fields rejected on typed
/// questions; `RetryPolicy` field types checked at run time"). The largest
/// count is accepted and counts without overflowing.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_largest_retry_count_is_accepted_as_upstream_test_invalid_max_retries() {
    let server = serve(|attempt, _| match attempt {
        1 | 2 => respond(503, "{}", &[("retry-after-ms", "0")]),
        _ => respond(200, r#"{"models": []}"#, &[]),
    })
    .await;
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().max_retries(u32::MAX).on(&time));

    client.models().list().send().await.expect("the third attempt succeeds");
    assert_eq!(retry_counts(&server.requests()), expected_counts(3));
}

/// `test_retry_policy_timeout_budget`, row by row, on both endpoints: each
/// attempt takes `duration` on the fake clock and answers 429 with
/// `Retry-After: <delay>`; retrying stops before a wait that would reach the
/// budget, and the error is the last attempt's. Each call gets a fresh
/// budget, so every row runs twice on one client.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_budget_stops_retrying_as_upstream_test_retry_policy_timeout_budget() {
    let secs = Duration::from_secs_f64;
    // (budget, time per attempt, Retry-After as Python's `str(float)` writes
    // it, attempts)
    let rows = [
        (None, 1.0, "0.5", 3),
        (Some(30.0), 10.0, "5.0", 2),
        (Some(2.5), 0.75, "0.5", 2),
        (Some(2.0), 1.0, "0.0", 2),
        (Some(1.0), 0.0, "1.0", 1),
        (Some(1.0), 0.0, "60.0", 1),
    ];
    for resource in Resource::ALL {
        for (budget, duration, delay, attempts) in rows {
            let case =
                format!("{resource:?}, budget {budget:?}, {duration} s per attempt, delay {delay}");
            let time = FakeTime::new();
            let served = Arc::new(AtomicUsize::new(0));
            let server = serve({
                let time = Arc::clone(&time);
                let served = Arc::clone(&served);
                move |_, _| {
                    time.advance(secs(duration));
                    let attempt = served.fetch_add(1, Ordering::SeqCst) + 1;
                    respond(
                        429,
                        &format!(r#"{{"message": "attempt {attempt}"}}"#),
                        &[("Retry-After", delay)],
                    )
                }
            })
            .await;
            let policy = match budget {
                Some(budget) => RetryPolicy::default().timeout(secs(budget)).expect("valid"),
                None => RetryPolicy::default().no_timeout(),
            };
            let client = client(&server, policy.on(&time));

            for call in 0..2 {
                served.store(0, Ordering::SeqCst);
                time.clear_delays();
                let before = server.request_count();
                let error = resource.call(&client, None).await.expect_err("every attempt fails");
                let display = format!("{}: 429 attempt {attempts}", resource.endpoint(&server));
                assert_api(&error, 429, &display);
                assert_eq!(server.request_count() - before, attempts, "{case}, call {call}");
                let wait = secs(delay.parse::<f64>().expect("a number"));
                assert_eq!(time.delays(), vec![wait; attempts - 1], "{case}, call {call}");
            }
        }
    }
}

/// `test_retry_policy_timeout_override` (AC-F9, AC-F10): each attempt takes
/// 20 s on the fake clock, so the client's default 30 s budget stops after
/// two attempts; a call's own policy replaces it for that call alone.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_calls_budget_replaces_the_clients_as_upstream_test_retry_policy_timeout_override() {
    for resource in Resource::ALL {
        let time = FakeTime::new();
        let server = serve({
            let time = Arc::clone(&time);
            move |_, _| {
                time.advance(Duration::from_secs(20));
                respond(429, "", &[("retry-after-ms", "0")])
            }
        })
        .await;
        let client = client(&server, RetryPolicy::default().on(&time));
        let calls = [
            (None, 2),
            (Some(RetryPolicy::default().timeout(Duration::from_secs(1)).expect("valid")), 1),
            (Some(RetryPolicy::default().no_timeout()), 3),
            (None, 2),
        ];
        for (index, (policy, attempts)) in calls.into_iter().enumerate() {
            let before = server.request_count();
            let error = resource
                .call(&client, policy.map(|policy| policy.on(&time)))
                .await
                .expect_err("every attempt fails");
            assert_api(
                &error,
                429,
                &format!("{}: 429 status code (no body)", resource.endpoint(&server)),
            );
            assert_eq!(server.request_count() - before, attempts, "{resource:?}, call {index}");
        }
    }
}

/// `test_default_retry_statuses`: 408, 429 and 5xx are retried twice; other
/// failures, and a redirect the transport does not follow, are not.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_default_statuses_are_retried_as_upstream_test_default_retry_statuses() {
    let rows = [
        (408, 3),
        (429, 3),
        (500, 3),
        (503, 3),
        (599, 3),
        (400, 1),
        (401, 1),
        (403, 1),
        (404, 1),
        (409, 1),
        (422, 1),
        (302, 1),
    ];
    for (status, attempts) in rows {
        let server = serve(move |_, _| {
            respond(status, r#"{"message": "failed"}"#, &[("retry-after-ms", "0")])
        })
        .await;
        let time = FakeTime::new();
        let client = client(&server, RetryPolicy::default().on(&time));

        let error = client.models().list().send().await.expect_err("fails");
        assert_api(
            &error,
            status,
            &format!("GET {}/v1/models: {status} failed", server.base_url()),
        );
        assert_eq!(retry_counts(&server.requests()), expected_counts(attempts), "status {status}");
    }
}

/// `test_connection_retry_recovers`, `ConnectError` and `ReadError`: a
/// transport failure is retried with the backoff, and the third attempt
/// succeeds. The draw is fixed at one half, inside upstream's asserted
/// ranges: 0.5 s less an eighth, rounded, then 1 s less an eighth.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_transport_failure_is_retried_as_upstream_test_connection_retry_recovers() {
    for kind in [io::ErrorKind::ConnectionRefused, io::ErrorKind::ConnectionReset] {
        let server = serve(|_, _| respond(200, r#"{"models": []}"#, &[])).await;
        let time = FakeTime::new();
        let transport = Flaky::new(2, kind);
        let client = flaky_client(&server, transport.clone(), RetryPolicy::default().on(&time));

        let response = client.models().list().send().await.expect("the third attempt succeeds");
        assert!(response.models().is_empty());
        assert_eq!(transport.calls(), 3, "{kind:?}");
        assert_eq!(retry_counts(&server.requests()), [Some("2".to_owned())], "{kind:?}");
        let delays = time.delays();
        assert_eq!(delays, [Duration::from_millis(438), Duration::from_millis(875)], "{kind:?}");
        assert!((0.375..=0.5).contains(&delays[0].as_secs_f64()));
        assert!((0.75..=1.0).contains(&delays[1].as_secs_f64()));
    }
}

/// `test_connection_retry_recovers`, `LocalProtocolError`: a transport that
/// fails before anything is sent - here, its `poll_ready` - is retried like
/// one that fails the call, and the third attempt succeeds.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_failure_before_sending_is_retried_as_upstream_test_connection_retry_recovers() {
    let server = serve(|_, _| respond(200, r#"{"models": []}"#, &[])).await;
    let time = FakeTime::new();
    let transport = Flaky::not_ready(2);
    let client = flaky_client(&server, transport.clone(), RetryPolicy::default().on(&time));

    let response = client.models().list().send().await.expect("the third attempt succeeds");
    assert!(response.models().is_empty());
    assert_eq!(transport.readies(), 3, "one poll_ready per attempt");
    assert_eq!(transport.calls(), 1, "only the third attempt is sent");
    assert_eq!(retry_counts(&server.requests()), [Some("2".to_owned())]);
    assert_eq!(time.delays(), [Duration::from_millis(438), Duration::from_millis(875)]);

    // With no retry, the first failure is the error: a connection error
    // with the transport's error as its cause.
    let transport = Flaky::not_ready(1);
    let error = flaky_client(&server, transport.clone(), RetryPolicy::default().max_retries(0))
        .models()
        .list()
        .send()
        .await
        .expect_err("the transport is not ready");
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: failed");
    let cause = error.source().expect("the transport's error is the cause");
    assert_eq!(cause.to_string(), "failed");
    assert!(cause.downcast_ref::<io::Error>().is_some(), "{error:?}");
    assert_eq!(transport.readies(), 1);
}

/// `test_connection_retry_recovers`, `ReadTimeout`: an attempt that runs
/// past its deadline is retried. The deadline is real, 50 ms, against a
/// handler held on a channel.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_timeout_is_retried_as_upstream_test_connection_retry_recovers() {
    let (server, release) = holding(2).await;
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().on(&time));

    let response =
        client.models().list().timeout(SHORT).send().await.expect("the third attempt succeeds");
    assert!(response.models().is_empty());
    assert_eq!(retry_counts(&server.requests()), expected_counts(3));
    assert_eq!(time.delays(), [Duration::from_millis(438), Duration::from_millis(875)]);
    release.send_modify(|released| *released = true);
}

/// `test_server_delay_through_tenacity`: the server's delay replaces the
/// backoff, `retry-after-ms` before `Retry-After`, however long it is.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_servers_delay_is_waited_as_upstream_test_server_delay_through_tenacity() {
    let rows: [(&[(&str, &str)], Duration); 4] = [
        (&[("Retry-After", "2")], Duration::from_secs(2)),
        (&[("retry-after-ms", "125")], Duration::from_millis(125)),
        (&[("retry-after-ms", "0"), ("Retry-After", "50")], Duration::ZERO),
        (&[("Retry-After", "60")], Duration::from_secs(60)),
    ];
    for (headers, delay) in rows {
        let server = serve(move |attempt, _| match attempt {
            1 => respond(429, "{}", headers),
            _ => respond(200, r#"{"models": []}"#, &[]),
        })
        .await;
        let time = FakeTime::new();
        let client = client(&server, RetryPolicy::default().no_timeout().on(&time));

        client.models().list().send().await.expect("the retry succeeds");
        assert_eq!(time.delays(), [delay], "{headers:?}");
    }
}

/// An error carrying `headers`, as a 429 would.
fn rate_limited(headers: &[(&str, &str)]) -> Error {
    let mut map = HeaderMap::new();
    for (name, value) in headers {
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("valid"),
            HeaderValue::from_str(value).expect("valid"),
        );
    }
    ApiError::new(StatusCode::TOO_MANY_REQUESTS, Bytes::from_static(b"{}"), map, None).into()
}

/// `test_backoff_dates_cap_and_jitter`: the default backoff doubles up to its
/// cap; a full draw takes a quarter off; a server delay is obeyed however
/// long, as seconds, as milliseconds or as an HTTP date measured on the wall
/// clock; an unreadable one falls back to the backoff. (`test_parse_retry_after`
/// itself is ported with the parser, in `error.rs`'s tests.)
#[test]
fn delays_follow_upstream_test_backoff_dates_cap_and_jitter() {
    let time = FakeTime::new();
    let policy = RetryPolicy::default();
    let no_headers = Error::connection("Connection error: refused", None);
    time.set_draw(0.0);
    for (attempts, want) in [(1, 500), (2, 1000), (3, 2000), (4, 4000), (5, 5000), (20, 5000)] {
        assert_eq!(policy.delay(&*time, attempts, &no_headers), Duration::from_millis(want));
    }
    time.set_draw(1.0);
    assert_eq!(policy.delay(&*time, 1, &no_headers), Duration::from_millis(375));

    let future = httpdate::fmt_http_date(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_010));
    let past = httpdate::fmt_http_date(SystemTime::UNIX_EPOCH + Duration::from_secs(999_990));
    let rows = [
        (vec![("Retry-After", "61")], Duration::from_secs(61)),
        (vec![("retry-after-ms", "60001")], Duration::from_millis(60_001)),
        (vec![("Retry-After", future.as_str())], Duration::from_secs(10)),
        (vec![("Retry-After", past.as_str())], Duration::ZERO),
        (vec![("Retry-After", "bad")], Duration::from_millis(375)),
    ];
    for (headers, want) in rows {
        assert_eq!(policy.delay(&*time, 1, &rate_limited(&headers)), want, "{headers:?}");
    }
}

/// `test_system_one_retry_override` (AC-F9): a call's policy - its count and
/// its statuses - replaces the client's for that call, and the next call
/// without one is back on the client's.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_calls_policy_replaces_the_clients_as_upstream_test_system_one_retry_override() {
    for (client_attempts, call_attempts) in [(1_u32, 3_u32), (3, 1)] {
        let server = serve(|_, request| {
            let status = if request.headers["x-call"] == "override" { 409 } else { 429 };
            respond(status, r#"{"message": "failed"}"#, &[("retry-after-ms", "0")])
        })
        .await;
        let time = FakeTime::new();
        let client =
            client(&server, RetryPolicy::default().max_retries(client_attempts - 1).on(&time));
        let call_policy = RetryPolicy::default()
            .max_retries(call_attempts - 1)
            .http_statuses([409].into_iter().collect())
            .on(&time);
        let questions = raw_questions();

        let calls = [
            ("override", Some(call_policy.clone()), call_attempts, 409),
            ("inherited", None, client_attempts, 429),
            ("override", Some(call_policy), call_attempts, 409),
        ];
        for (name, policy, attempts, status) in calls {
            let before = server.request_count();
            let mut request = client.system_one("hello", &questions).header("x-call", name);
            if let Some(policy) = policy {
                request = request.retry(policy);
            }
            let error = request.send().await.expect_err("fails");
            let display = format!("POST {}/v1/systemone: {status} failed", server.base_url());
            assert_api(&error, status, &display);
            let attempts = usize::try_from(attempts).expect("small");
            assert_eq!(
                retry_counts(&server.requests()[before..]),
                expected_counts(attempts),
                "{name}, client {client_attempts}, call {call_attempts}"
            );
        }
    }
}

/// `test_async_concurrent_retry_state`: calls in flight together each count
/// their own retries.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn concurrent_calls_count_their_own_retries_as_upstream_test_async_concurrent_retry_state() {
    // Each request's `x-call` key and retry count, in arrival order.
    type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;
    let seen: Seen = Arc::default();
    let server = TestServer::start(Protocol::Http1, {
        let seen = Arc::clone(&seen);
        move |request| {
            let key = request.headers["x-call"].to_str().expect("text").to_owned();
            let count = retry_counts(std::slice::from_ref(&request)).remove(0);
            let first = {
                let mut seen = lock(&seen);
                let first = !seen.iter().any(|(known, _)| *known == key);
                seen.push((key, count));
                first
            };
            async move {
                tokio::task::yield_now().await;
                if first {
                    respond(429, "", &[("retry-after-ms", "0")])
                } else {
                    respond(200, r#"{"models": []}"#, &[])
                }
            }
        }
    })
    .await
    .expect("the test server starts");
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().on(&time));

    let keys = ["0", "1", "2", "3"];
    let calls = keys.map(|key| client.models().list().header("x-call", key).send());
    let [a, b, c, d] = calls;
    let (a, b, c, d) = tokio::join!(a, b, c, d);
    for response in [a, b, c, d] {
        assert!(response.expect("every call recovers").models().is_empty());
    }
    let seen = lock(&seen).clone();
    for key in keys {
        let counts: Vec<_> =
            seen.iter().filter(|(known, _)| known == key).map(|(_, count)| count.clone()).collect();
        assert_eq!(counts, expected_counts(2), "call {key}");
    }
}

/// The state upstream's recovery test sends.
#[derive(Serialize)]
#[cfg(feature = "hyper")]
struct Document {
    document: &'static str,
}

/// `test_system_one_retry_recovers_with_overrides`: a call with its own model,
/// deadline and headers times out, is rate limited, then succeeds; every
/// attempt sends the same body and headers, and the next call without
/// overrides is back on the client's settings. Upstream's `httpx.Timeout`
/// parametrization is a per-phase deadline this SDK does not have (README
/// deviation row "Timeout per httpx phase; `httpx.Timeout` objects"): the Rust
/// call has one deadline per attempt, 50 ms here so the first attempt times
/// out in real time, and what the server can observe is asserted instead of
/// the transport's own timeout settings.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_call_recovers_with_its_overrides_as_upstream_test_system_one_retry_recovers_with_overrides()
 {
    for raw in [false, true] {
        let (release, released) = watch::channel(false);
        let served = Arc::new(AtomicUsize::new(0));
        let server = TestServer::start(Protocol::Http1, move |_| {
            let mut released = released.clone();
            let attempt = served.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                match attempt {
                    1 => {
                        drop(released.wait_for(|released| *released).await);
                        respond(200, "{}", &[])
                    }
                    2 => respond(429, r#"{"message": "slow down"}"#, &[("retry-after-ms", "125")]),
                    _ => respond(200, std::str::from_utf8(RESULT).expect("UTF-8"), &[]),
                }
            }
        })
        .await
        .expect("the test server starts");
        let time = FakeTime::new();
        let client = ClientBuilder::new()
            .api_key("test-key")
            .base_url(server.base_url())
            .default_model("client-model")
            .timeout(Duration::from_secs(7))
            .default_header("x-default", "kept")
            .retry(RetryPolicy::default().on(&time))
            .build_with_env(|_: &str| None::<String>)
            .expect("the client builds");
        let questions = if raw { raw_questions() } else { questions() };

        let response = client
            .system_one(&Document { document: "hello" }, &questions)
            .model("call-model")
            .timeout(SHORT)
            .header("x-call", "override")
            .header("authorization", "must-not-win")
            .send()
            .await
            .unwrap_or_else(|error| panic!("raw {raw}: {error:?}"));
        let score = response.answers().score("quality").expect("answered").score();
        let confidence = response.answers().choice("tone").expect("answered").confidence();
        assert_same_f64(score, 1.7, "quality");
        assert_same_f64(confidence, 0.9, "tone");

        let requests = server.requests();
        let expected = concat!(
            r#"{"state":{"document":"hello"},"model":"call-model","#,
            r#""questions":{"q":{"type":"noul","instructions":"?"}}}"#,
        );
        for request in &requests {
            assert_eq!(request.body, expected.as_bytes(), "raw {raw}");
            assert_eq!(request.headers["authorization"], "Bearer test-key");
            assert_eq!(request.headers["x-default"], "kept");
            assert_eq!(request.headers["x-call"], "override");
        }
        assert_eq!(retry_counts(&requests), expected_counts(3), "raw {raw}");
        let delays = time.delays();
        assert_eq!(delays, [Duration::from_millis(438), Duration::from_millis(125)]);
        assert!((0.375..=0.5).contains(&delays[0].as_secs_f64()));

        client.system_one("next", &questions).send().await.expect("the next call succeeds");
        let last = server.requests().pop().expect("one more request");
        let body = std::str::from_utf8(&last.body).expect("UTF-8");
        assert!(body.contains(r#""model":"client-model""#), "{body}");
        assert!(!last.headers.contains_key("x-call"));
        assert!(!last.headers.contains_key("x-typesafe-retry-count"));
        release.send_modify(|released| *released = true);
    }
}

/// `test_concurrent_system_one_overrides` (AC-F9): three calls in flight
/// together, two with a policy of their own, each keep their own count,
/// state and model. Upstream also reads each attempt's timeout off the
/// request; a Rust deadline never reaches the server, so it is not asserted.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn concurrent_calls_keep_their_own_policies_as_upstream_test_concurrent_system_one_overrides()
{
    let server = TestServer::start(Protocol::Http1, |_| async {
        tokio::task::yield_now().await;
        respond(429, r#"{"message": "retry"}"#, &[("retry-after-ms", "0")])
    })
    .await
    .expect("the test server starts");
    let time = FakeTime::new();
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_model("jev-latest")
        .timeout(Duration::from_secs(9))
        .retry(RetryPolicy::default().max_retries(1).on(&time))
        .build_with_env(|_: &str| None::<String>)
        .expect("the client builds");
    let questions = questions();

    let call = |name: &'static str, retries: Option<u32>, seconds: u64| {
        let mut request = client
            .system_one(name, &questions)
            .model(name)
            .header("x-call", name)
            .timeout(Duration::from_secs(seconds));
        if let Some(retries) = retries {
            request = request.retry(RetryPolicy::default().max_retries(retries - 1).on(&time));
        }
        request.send()
    };
    let (one, three, default) =
        tokio::join!(call("one", Some(1), 1), call("three", Some(3), 3), call("default", None, 2));
    for (name, outcome) in [("one", one), ("three", three), ("default", default)] {
        let Err(error) = outcome else { panic!("{name}: every attempt is rate limited") };
        assert_api(&error, 429, &format!("POST {}/v1/systemone: 429 retry", server.base_url()));
    }
    let requests = server.requests();
    for (name, count) in [("one", 1), ("three", 3), ("default", 2)] {
        let mine: Vec<_> =
            requests.iter().filter(|request| request.headers["x-call"] == name).cloned().collect();
        assert_eq!(retry_counts(&mine), expected_counts(count), "{name}");
        let body = format!(r#"{{"state":"{name}","model":"{name}","#);
        for request in &mine {
            let sent = std::str::from_utf8(&request.body).expect("UTF-8");
            assert!(sent.starts_with(&body), "{name}: {sent}");
        }
    }
}

/// `test_exhausted_transport_retry`, `ReadTimeout`: every attempt times out,
/// and the call fails with the last timeout. Upstream also finds httpx's own
/// `ReadTimeout` as the cause; the Rust deadline is the SDK's own, so a
/// timeout has no cause, and it carries the deadline it was given.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_last_timeout_is_returned_as_upstream_test_exhausted_transport_retry() {
    let (server, release) = holding(usize::MAX).await;
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().on(&time));
    let policy = RetryPolicy::default()
        .backoff_initial(Duration::from_millis(1))
        .backoff_max(Duration::from_millis(1))
        .on(&time);
    let questions = questions();

    let error = client
        .system_one("x", &questions)
        .retry(policy)
        .timeout(SHORT)
        .send()
        .await
        .expect_err("every attempt times out");
    assert!(
        matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == SHORT),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "Request timed out (timeout=0.05s).");
    assert!(error.source().is_none(), "{error:?}");
    assert_eq!(server.request_count(), 3);
    release.send_modify(|released| *released = true);
}

/// `test_exhausted_transport_retry`, `ConnectError`: every attempt fails to
/// connect, and the call fails with the last failure, whose cause is the
/// transport's own error from the third attempt.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_last_connection_failure_is_returned_as_upstream_test_exhausted_transport_retry() {
    let server = serve(|_, _| respond(200, "{}", &[])).await;
    let time = FakeTime::new();
    let transport = Flaky::new(usize::MAX, io::ErrorKind::ConnectionRefused);
    let policy = RetryPolicy::default()
        .backoff_initial(Duration::from_millis(1))
        .backoff_max(Duration::from_millis(1))
        .on(&time);
    let client = flaky_client(&server, transport.clone(), RetryPolicy::default().on(&time));
    let questions = questions();

    let error = client
        .system_one("x", &questions)
        .retry(policy)
        .send()
        .await
        .expect_err("every attempt fails");
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: attempt 3");
    let cause = error
        .source()
        .and_then(|cause| cause.downcast_ref::<io::Error>())
        .unwrap_or_else(|| panic!("the transport's error is the cause: {error:?}"));
    assert_eq!(cause.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(cause.to_string(), "attempt 3");
    assert_eq!(transport.calls(), 3);
    assert_eq!(server.request_count(), 0, "no attempt reached the server");
    assert_eq!(time.delays(), [Duration::from_millis(1), Duration::from_millis(1)]);
}

/// `test_exhausted_retry_preserves_final_http_error` (AC-F10): the error of
/// the last attempt is returned whole - status, body, request id, message -
/// not wrapped and not the first one.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_last_api_error_is_returned_whole_as_upstream_test_exhausted_retry_preserves_final_http_error()
 {
    let server = serve(|attempt, _| {
        let status = [429, 500, 503][attempt - 1];
        let id = format!("request-{attempt}");
        respond(
            status,
            &format!(r#"{{"message": "attempt {attempt}"}}"#),
            &[("x-typesafe-request-id", id.as_str()), ("retry-after-ms", "0")],
        )
    })
    .await;
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().on(&time));
    let questions = questions();

    let error = client.system_one("x", &questions).send().await.expect_err("fails");
    assert_eq!(server.request_count(), 3);
    let display =
        format!("POST {}/v1/systemone: 503 attempt 3 (request_id=request-3)", server.base_url());
    assert_api(&error, 503, &display);
    let ErrorKind::Api(api) = error.kind() else { unreachable!("asserted above") };
    assert_eq!(api.body(), br#"{"message": "attempt 3"}"#);
    assert_eq!(api.request_id(), Some("request-3"));
    assert_eq!(api.message(), "attempt 3");
}

/// `test_cancel_pending_retry` (AC-F8): dropping a call while it waits to
/// retry drops the wait itself - no task was spawned to hold it - and
/// nothing more is sent.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn dropping_a_waiting_call_cancels_its_retry_as_upstream_test_cancel_pending_retry() {
    let server = serve(|_, _| respond(429, "", &[])).await;
    let time = FakeTime::hanging();
    let client = client(&server, RetryPolicy::default().on(&time));

    let mut call = Box::pin(client.models().list().send());
    tokio::select! {
        outcome = &mut call => panic!("the call ended instead of waiting: {outcome:?}"),
        () = time.parked.notified() => {}
    }
    assert!(!time.sleep_dropped.load(Ordering::SeqCst), "the wait is still pending");
    drop(call);
    assert!(time.sleep_dropped.load(Ordering::SeqCst), "dropping the call dropped the wait");
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    assert_eq!(server.request_count(), 1);
    assert_eq!(time.delays().len(), 1);
}

/// `test_retry_policy_max_retries`: `max_retries` more attempts after the
/// first, and none for 0.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn max_retries_counts_attempts_as_upstream_test_retry_policy_max_retries() {
    for (retries, attempts) in [(0, 1), (1, 2), (4, 5)] {
        let server =
            serve(|_, _| respond(429, r#"{"message": "slow"}"#, &[("retry-after-ms", "0")])).await;
        let time = FakeTime::new();
        let client = client(&server, RetryPolicy::default().max_retries(retries).on(&time));

        let error = client.models().list().send().await.expect_err("fails");
        assert_api(&error, 429, &format!("GET {}/v1/models: 429 slow", server.base_url()));
        assert_eq!(server.request_count(), attempts, "max_retries {retries}");
    }
}

/// `test_retry_policy_custom_statuses`: a policy's own statuses replace the
/// default ones.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn custom_statuses_replace_the_default_as_upstream_test_retry_policy_custom_statuses() {
    for (status, attempts) in [(409, 3), (500, 1)] {
        let server =
            serve(move |_, _| respond(status, r#"{"message": "x"}"#, &[("retry-after-ms", "0")]))
                .await;
        let time = FakeTime::new();
        let policy = RetryPolicy::default().http_statuses([409].into_iter().collect()).on(&time);
        let client = client(&server, policy);

        let error = client.models().list().send().await.expect_err("fails");
        assert_api(&error, status, &format!("GET {}/v1/models: {status} x", server.base_url()));
        assert_eq!(server.request_count(), attempts, "status {status}");
    }
}

/// `test_retry_policy_per_call_override` (AC-F9): a call's `max_retries(0)`
/// replaces the client's 2.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_calls_count_replaces_the_clients_as_upstream_test_retry_policy_per_call_override() {
    let server = serve(|_, _| respond(429, "{}", &[("retry-after-ms", "0")])).await;
    let time = FakeTime::new();
    let client = client(&server, RetryPolicy::default().max_retries(2).on(&time));

    let error = client
        .models()
        .list()
        .retry(RetryPolicy::default().max_retries(0).on(&time))
        .send()
        .await
        .expect_err("fails");
    assert_api(&error, 429, &format!("GET {}/v1/models: 429 {{}}", server.base_url()));
    assert_eq!(server.request_count(), 1);
}

/// `test_retry_policy_exceptions_and_predicate`: a predicate opts a 404 in.
/// Upstream's `exceptions={TypeSafeAPIError}` is dropped (README deviation
/// row "`RetryPolicy.exceptions`"); the Rust spelling of it is a predicate that
/// matches the kind, tested as the second row.
#[cfg(feature = "hyper")]
#[tokio::test]
async fn a_predicate_opts_a_failure_in_as_upstream_test_retry_policy_exceptions_and_predicate() {
    let by_status = |error: &Error| matches!(error.kind(), ErrorKind::Api(api) if api.status() == StatusCode::NOT_FOUND);
    let by_kind = |error: &Error| matches!(error.kind(), ErrorKind::Api(_));
    type Named = (&'static str, fn(&Error) -> bool);
    let predicates: [Named; 2] = [("status", by_status), ("kind", by_kind)];
    for (name, predicate) in predicates {
        let server =
            serve(|_, _| respond(404, r#"{"message": "gone"}"#, &[("retry-after-ms", "0")])).await;
        let time = FakeTime::new();
        let policy = RetryPolicy::default().max_retries(1).predicate(predicate).on(&time);
        let client = client(&server, policy);

        let error = client.models().list().send().await.expect_err("fails");
        assert_api(&error, 404, &format!("GET {}/v1/models: 404 gone", server.base_url()));
        assert_eq!(server.request_count(), 2, "by {name}: a 404 is retried only because it asks");
    }
}

/// `test_retry_policy_wait_options`: the server's delay unless it is not
/// respected, then the backoff from its own initial delay.
#[test]
fn the_delay_follows_the_wait_options_as_upstream_test_retry_policy_wait_options() {
    let time = FakeTime::new();
    time.set_draw(0.0);
    let error = rate_limited(&[("Retry-After", "5")]);
    let respected = RetryPolicy::default();
    let ignored = RetryPolicy::default().respect_retry_after(false);
    let short = RetryPolicy::default()
        .backoff_initial(Duration::from_millis(200))
        .respect_retry_after(false);
    assert_eq!(respected.delay(&*time, 1, &error), Duration::from_secs(5));
    assert_eq!(ignored.delay(&*time, 1, &error), Duration::from_millis(500));
    assert_eq!(short.delay(&*time, 1, &error), Duration::from_millis(200));
}

/// `test_backoff_extreme_values` is ported on the pure schedule above, where
/// its `1e-300` and `1e300` can be written; a policy holds `Duration`s, whose
/// extremes are zero and `Duration::MAX`. Both come through the policy as the
/// schedule gives them.
#[test]
fn extreme_durations_come_through_the_policy_as_upstream_test_backoff_extreme_values() {
    let time = FakeTime::new();
    time.set_draw(0.0);
    let error = Error::connection("Connection error: refused", None);
    let largest = RetryPolicy::default().backoff_initial(Duration::MAX).backoff_max(Duration::MAX);
    assert_eq!(largest.delay(&*time, 1, &error), Duration::MAX);
    assert_eq!(largest.delay(&*time, u32::MAX, &error), Duration::MAX);
    let smallest =
        RetryPolicy::default().backoff_initial(Duration::from_nanos(1)).backoff_max(Duration::MAX);
    assert_eq!(smallest.delay(&*time, 1, &error), Duration::ZERO, "rounded to milliseconds");
    let capped = RetryPolicy::default().backoff_max(Duration::from_micros(600));
    assert_eq!(capped.delay(&*time, 1, &error), Duration::from_micros(600));
}

// ------------------------------------------------------- beyond upstream

/// Which failures are retried on their own, and that a predicate is asked
/// about every other one - the order of `retry.py:100-109`.
#[test]
fn only_timeouts_connections_and_listed_statuses_are_retried_on_their_own() {
    let policy = RetryPolicy::default();
    let too_large = Error::response_too_large(1024);
    let cases: [(&str, Error, bool); 7] = [
        ("timeout", Error::timeout(SHORT), true),
        ("connection", Error::connection("Connection error: x", None), true),
        ("429", rate_limited(&[]), true),
        ("too large", Error::response_too_large(1024), false),
        ("invalid request", Error::invalid_request("x"), false),
        ("config", Error::config("x"), false),
        (
            "404",
            ApiError::new(StatusCode::NOT_FOUND, Bytes::new(), HeaderMap::new(), None).into(),
            false,
        ),
    ];
    for (name, error, retried) in &cases {
        assert_eq!(policy.retryable(error), *retried, "{name}");
    }
    let off = policy.clone().api_timeout_error(false).api_connection_error(false);
    assert!(!off.retryable(&Error::timeout(SHORT)));
    assert!(!off.retryable(&Error::connection("Connection error: x", None)));

    let asked = Arc::new(AtomicUsize::new(0));
    let counting = RetryPolicy::default().predicate({
        let asked = Arc::clone(&asked);
        move |error| {
            asked.fetch_add(1, Ordering::SeqCst);
            matches!(error.kind(), ErrorKind::ResponseTooLarge { .. })
        }
    });
    assert!(counting.retryable(&too_large), "the predicate can ask for any failure");
    assert!(counting.retryable(&Error::timeout(SHORT)));
    assert_eq!(asked.load(Ordering::SeqCst), 1, "a failure retried on its own skips the predicate");
}

/// The count is checked after the predicate, and the budget after the count,
/// as tenacity checks `retry` before `stop`: a predicate sees the failure of
/// the last attempt too.
#[test]
fn the_predicate_is_asked_before_the_count_and_the_budget() {
    let time = FakeTime::new();
    let asked = Arc::new(AtomicUsize::new(0));
    let policy = RetryPolicy::default().max_retries(0).predicate({
        let asked = Arc::clone(&asked);
        move |_| {
            asked.fetch_add(1, Ordering::SeqCst);
            true
        }
    });
    let error = Error::invalid_request("x");
    assert_eq!(policy.next_delay(&*time, 0, &error, Some(time.now())), None);
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(time.delays().is_empty(), "nothing slept: {:?}", time.delays());
}

/// A budget is reached when the wait would carry the call exactly to it, as
/// tenacity's `stop_before_delay` compares with `>=`.
#[test]
fn a_wait_that_reaches_the_budget_exactly_ends_the_retrying() {
    let time = FakeTime::new();
    time.set_draw(0.0);
    let started = time.now();
    let policy = RetryPolicy::default().timeout(Duration::from_secs(10)).expect("valid");
    let error = rate_limited(&[("retry-after-ms", "2000")]);
    time.advance(Duration::from_millis(7_999));
    assert_eq!(policy.next_delay(&*time, 0, &error, Some(started)), Some(Duration::from_secs(2)));
    time.advance(Duration::from_millis(1));
    assert_eq!(policy.next_delay(&*time, 0, &error, Some(started)), None);
}

/// A transport that answers `failures` times with 503 and then with the
/// fixture, and records where each request's body bytes live.
#[derive(Clone)]
struct Recording {
    pointers: Arc<Mutex<Vec<usize>>>,
    failures: usize,
}

type InMemory = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

impl Service<Request<Body>> for Recording {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = InMemory;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> InMemory {
        let pointers = Arc::clone(&self.pointers);
        let failures = self.failures;
        Box::pin(async move {
            let mut body = request.into_body();
            let frame = body.frame().await.expect("one frame");
            let data = frame.unwrap_or_else(|never| match never {}).into_data().expect("data");
            let attempt = {
                let mut pointers = lock(&pointers);
                pointers.push(data.as_ptr().addr());
                pointers.len()
            };
            if attempt <= failures {
                let mut response = Response::new(Body::from(Bytes::from_static(b"{}")));
                *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
                response.headers_mut().insert("retry-after-ms", HeaderValue::from_static("0"));
                return Ok(response);
            }
            Ok(Response::new(Body::from(Bytes::from_static(RESULT))))
        })
    }
}

/// The body is encoded once, and every attempt sends the very same bytes:
/// one allocation shared, not a copy per attempt.
#[tokio::test]
async fn every_attempt_sends_the_same_bytes_of_one_encoded_body() {
    let time = FakeTime::new();
    let transport = Recording { pointers: Arc::default(), failures: 2 };
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .retry(RetryPolicy::default().on(&time))
        .build_with_service(transport.clone())
        .expect("the client builds");
    let questions = questions();

    let response = client.system_one("hello", &questions).send().await.expect("recovers");
    assert_eq!(response.answers().len(), 3);
    let pointers = lock(&transport.pointers).clone();
    assert_eq!(pointers.len(), 3);
    assert!(pointers.iter().all(|pointer| *pointer == pointers[0]), "{pointers:?}");
}
