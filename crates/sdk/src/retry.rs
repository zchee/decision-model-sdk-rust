//! When a failed attempt is worth repeating, and how long to wait first.
//!
//! A [`RetryPolicy`] says which failures are repeated, how often, how long to
//! wait between attempts and how long one call may take in all. A client
//! holds one ([`ClientBuilder::retry`](crate::ClientBuilder::retry)) and a
//! call can replace it for itself alone
//! ([`SystemOne::retry`](crate::SystemOne::retry),
//! [`ListModels::retry`](crate::ListModels::retry)). The defaults are the
//! Python SDK's: two retries, a backoff from 0.5 s doubling up to 5 s with a
//! quarter of it jittered away, statuses 408, 429 and 500-599, connection
//! failures and timeouts, and 30 s for the whole call.
//!
//! The delay is a pure function of the attempt number and a random draw, so
//! the schedule can be asserted exactly rather than observed; the clock, the
//! sleep and the draw are a seam a test fills with a fake, so a retry test
//! costs no wall time and cannot be flaky.
//!
//! A server that says how long to wait is obeyed however long it asks for:
//! `retry-after-ms` or `Retry-After` replaces the backoff, and the backoff cap
//! does not apply to it. Retrying stops when the next delay would carry the
//! call past its budget, and the failure the caller gets is the last one,
//! unchanged.
//!
//! The loop is written here rather than taken from a crate. `backon` and
//! `tower::retry` were weighed and declined: the decision sequence has to be
//! the Python SDK's (which failures, then the server's delay or the backoff,
//! then the attempt count and the budget), the budget has to be measured on a
//! clock a test controls, the request body has to be shared rather than
//! rebuilt, and the loop must not box the attempt's future. Adapting either
//! crate to all four costs more code than the loop, which is short, and
//! `fastrand` supplies the one random number a delay needs.
//!
//! Nothing here spawns a task: the sleep between attempts is awaited inside
//! the call's own future, so dropping that future cancels the wait and sends
//! nothing more.

use std::{
    fmt,
    future::Future,
    io::Write as _,
    sync::Arc,
    time::{Duration, Instant, SystemTime},
};

use http::{HeaderMap, Method, Uri, header::RETRY_AFTER};

use crate::{
    config::ZERO_TIMEOUT,
    constants::RETRY_AFTER_MS_HEADER,
    error::{Error, ErrorKind, parse_retry_after},
    telemetry,
};

// ------------------------------------------------------------ RetryPolicy

/// A caller's rule for deciding whether a failed attempt is retried.
type Predicate = Arc<dyn Fn(&Error) -> bool + Send + Sync>;

/// Which failures a call repeats, how often, and how long it waits first.
///
/// Build one from [`RetryPolicy::default`] and its setters, then give it to a
/// client with [`ClientBuilder::retry`](crate::ClientBuilder::retry) or to one
/// call with [`SystemOne::retry`](crate::SystemOne::retry) or
/// [`ListModels::retry`](crate::ListModels::retry). A policy given to a call
/// replaces the client's for that call only.
///
/// A failed attempt is retried when all of these hold, checked in this order:
///
/// 1. The failure is retryable: a [timeout](ErrorKind::Timeout) (unless
///    [`api_timeout_error`](Self::api_timeout_error) is off), a
///    [connection failure](ErrorKind::Connection) (unless
///    [`api_connection_error`](Self::api_connection_error) is off), or an
///    [API error](ErrorKind::Api) whose status is in
///    [`http_statuses`](Self::http_statuses) - or else the
///    [`predicate`](Self::predicate) says so. A request that could not be
///    built, a response that did not decode and a response over the size limit
///    are never retryable on their own; only the predicate can ask for them.
/// 2. Fewer than [`max_retries`](Self::max_retries) retries have been made.
/// 3. The wait before the next attempt, added to the time the call has
///    already taken, stays below the [`timeout`](Self::timeout) budget.
///
/// The wait is what the server asked for in `retry-after-ms` or
/// `Retry-After`, when [`respect_retry_after`](Self::respect_retry_after) is
/// on and the failure carries one, however long that is. Otherwise it is the
/// backoff: [`backoff_initial`](Self::backoff_initial) doubled once per
/// attempt up to [`backoff_max`](Self::backoff_max), less a random share of up
/// to [`backoff_jitter`](Self::backoff_jitter) of it, in whole milliseconds.
///
/// When retrying stops, the call fails with the last attempt's error exactly
/// as that attempt produced it.
///
/// ```
/// use std::time::Duration;
///
/// use typesafe_sdk::{RetryPolicy, StatusSet};
///
/// let policy = RetryPolicy::default()
///     .max_retries(3)
///     .backoff_max(Duration::from_secs(2))
///     .http_statuses([429, 502, 503, 504].into_iter().collect::<StatusSet>())
///     .timeout(Duration::from_secs(10))?;
/// # Ok::<(), typesafe_sdk::Error>(())
/// ```
#[derive(Clone)]
pub struct RetryPolicy {
    max_retries: u32,
    backoff_initial: Duration,
    backoff_max: Duration,
    backoff_jitter: f64,
    http_statuses: StatusSet,
    respect_retry_after: bool,
    api_connection_error: bool,
    api_timeout_error: bool,
    predicate: Option<Predicate>,
    /// The budget of a whole call, or `None` for no budget.
    timeout: Option<Duration>,
    /// The clock, sleep and draw a test runs the loop on instead of the real
    /// ones.
    #[cfg(test)]
    time: Option<Arc<tests::FakeTime>>,
}

impl RetryPolicy {
    /// The Python SDK's defaults, which [`Default`] also gives: 2 retries, a
    /// backoff from 500 ms up to 5 s with a jitter of 0.25, the statuses of
    /// [`StatusSet::DEFAULT`], `Retry-After` respected, connection failures
    /// and timeouts retried, no predicate, and a budget of 30 s.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_retries: 2,
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(5),
            backoff_jitter: 0.25,
            http_statuses: StatusSet::DEFAULT,
            respect_retry_after: true,
            api_connection_error: true,
            api_timeout_error: true,
            predicate: None,
            timeout: Some(Duration::from_secs(30)),
            #[cfg(test)]
            time: None,
        }
    }

    /// A policy that makes one attempt only, whatever fails.
    ///
    /// Equivalent to `RetryPolicy::new().max_retries(0)`. The client's or
    /// call's per-attempt deadline still applies.
    #[must_use]
    pub const fn none() -> Self {
        let mut policy = Self::new();
        policy.max_retries = 0;
        policy
    }

    /// The most retries after the first attempt; `0` makes one attempt only,
    /// as [`none`](Self::none) does.
    #[must_use]
    pub fn max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// The wait after the first failed attempt, doubled after each later one
    /// up to [`backoff_max`](Self::backoff_max). Zero turns the backoff off,
    /// so retries follow each other at once.
    #[must_use]
    pub fn backoff_initial(mut self, delay: Duration) -> Self {
        self.backoff_initial = delay;
        self
    }

    /// The longest backoff. Zero turns the backoff off. A delay the server
    /// asks for is not capped by it.
    #[must_use]
    pub fn backoff_max(mut self, delay: Duration) -> Self {
        self.backoff_max = delay;
        self
    }

    /// The largest share of each backoff that is randomly taken off it, from
    /// 0 (none) to 1 (up to all of it), so that clients which failed together
    /// do not retry together.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`] error when `jitter` is not a number
    /// from 0 to 1, NaN and the infinities included.
    pub fn backoff_jitter(mut self, jitter: f64) -> Result<Self, Error> {
        // A NaN is outside every range, so it fails the check too.
        if !(0.0..=1.0).contains(&jitter) {
            return Err(Error::config("backoff_jitter must be between zero and one."));
        }
        self.backoff_jitter = jitter;
        Ok(self)
    }

    /// The statuses whose API errors are retried; [`StatusSet::DEFAULT`]
    /// unless set.
    ///
    /// A status counts only for a response the SDK reports as
    /// [`ErrorKind::Api`]. A success status in the set has no effect: a
    /// success response whose body does not decode is an
    /// [`ErrorKind::ResponseValidation`] error and is not retried for its
    /// status, since the same body would come back; a
    /// [`predicate`](Self::predicate) can still ask for it.
    #[must_use]
    pub fn http_statuses(mut self, statuses: StatusSet) -> Self {
        self.http_statuses = statuses;
        self
    }

    /// Whether a delay the server asks for in `retry-after-ms` or
    /// `Retry-After` replaces the backoff. On unless turned off.
    ///
    /// The budget set with [`timeout`](Self::timeout) is what bounds such a
    /// delay: without a budget ([`no_timeout`](Self::no_timeout)) a server's
    /// `Retry-After` is obeyed however long it is. Keep a budget, or turn
    /// this off, when the server is not trusted.
    #[must_use]
    pub fn respect_retry_after(mut self, respect: bool) -> Self {
        self.respect_retry_after = respect;
        self
    }

    /// Whether an attempt that got no response - a refused, failed or broken
    /// connection, [`ErrorKind::Connection`] - is retried. On unless turned
    /// off.
    #[must_use]
    pub fn api_connection_error(mut self, retry: bool) -> Self {
        self.api_connection_error = retry;
        self
    }

    /// Whether an attempt that ran past its deadline,
    /// [`ErrorKind::Timeout`], is retried. On unless turned off.
    #[must_use]
    pub fn api_timeout_error(mut self, retry: bool) -> Self {
        self.api_timeout_error = retry;
        self
    }

    /// A rule of the caller's own: a failure it returns `true` for is
    /// retried, in addition to the ones the other settings retry.
    ///
    /// It is called once for each failed attempt, with the error that attempt
    /// ended with, before the attempt count and the budget are checked; a
    /// failure the other settings already retry may skip it. It sees every
    /// failure an attempt can end with, a response that did not decode or was
    /// over the size limit included, and runs on the task that sends the
    /// call, so it should return quickly.
    #[must_use]
    pub fn predicate<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&Error) -> bool + Send + Sync + 'static,
    {
        self.predicate = Some(Arc::new(predicate));
        self
    }

    /// The budget of one call, including every attempt and every wait
    /// between them. Retrying stops before a wait that would reach it, and
    /// the call fails with the last error. 30 s unless set.
    ///
    /// This is not the deadline of one attempt, which the client and each
    /// call set with their own `timeout`.
    ///
    /// The budget is checked only before a retry and never cuts an attempt short,
    /// so a call lasts at most the budget plus one per-attempt deadline; with
    /// `RetryPolicy::none()` it lasts at most one per-attempt deadline.
    /// Dropping a call's future cancels the attempt in flight, and nothing of
    /// that call is sent or retried after the drop.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`] error for a budget of zero.
    pub fn timeout(mut self, budget: Duration) -> Result<Self, Error> {
        if budget.is_zero() {
            return Err(Error::config(ZERO_TIMEOUT));
        }
        self.timeout = Some(budget);
        Ok(self)
    }

    /// No budget for the whole call: only the attempt count stops retrying.
    ///
    /// The budget is also what bounds a delay the server asks for: without
    /// it, a server's `Retry-After` is obeyed however long it is. Keep a
    /// budget, or turn [`respect_retry_after`](Self::respect_retry_after)
    /// off, when the server is not trusted.
    #[must_use]
    pub fn no_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Runs `attempt` under this policy until it succeeds or the policy stops
    /// retrying, and returns its success or its last error, unchanged: the
    /// loop a call of this SDK runs, for a request the SDK does not send.
    ///
    /// `attempt` is called with the number of attempts made before it, `0`
    /// first. The policy decides from the [`Error`] an attempt returns, as
    /// for the SDK's own calls: which failures are retried, the wait before
    /// the next attempt, the attempt count and the budget. A result the
    /// caller does not want retried belongs in `R`, not in the error.
    ///
    /// `method` and `uri` only name the request. Before each retry the
    /// method and the scheme, host, port and path of `uri` are logged at
    /// `INFO` to the `typesafe_sdk` target, as
    /// `POST https://api.example.com/v1/answers retry 1`; the line exists only
    /// when the SDK is built with its `tracing` feature. The port is printed
    /// only when it is not the scheme's default. The query and the userinfo
    /// are not printed, but the path is, so `uri` must carry no credential.
    ///
    /// The returned future is `Send` when `F` and `Fut` are, so it can be
    /// spawned. It borrows `self`, `method` and `uri` until it completes, so
    /// a spawned task cannot borrow them from its caller's stack: the example
    /// below moves the policy and the URI into it.
    /// Nothing here spawns a task: the waits between attempts are awaited
    /// inside the returned future, and dropping it cancels the attempt in
    /// flight and every retry after it.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use http::{Method, Uri};
    /// use typesafe_sdk::{Error, RetryPolicy};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    /// let policy = RetryPolicy::default().backoff_initial(Duration::ZERO);
    /// // Scheme, host and path only: no query, no userinfo.
    /// let uri = Uri::from_static("https://api.example.com/v1/answers");
    /// let task = tokio::spawn(async move {
    ///     policy
    ///         .run(&Method::POST, &uri, |retry| async move {
    ///             // Send the request here; `retry` attempts came before this one.
    ///             Ok::<_, Error>(format!("answered on attempt {}", retry + 1))
    ///         })
    ///         .await
    /// });
    /// assert_eq!(task.await??, "answered on attempt 1");
    /// # Ok(())
    /// # }
    /// ```
    pub fn run<R, F, Fut>(
        &self,
        method: &Method,
        uri: &Uri,
        attempt: F,
    ) -> impl Future<Output = Result<R, Error>>
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = Result<R, Error>>,
    {
        // The loop's own future, not an `async fn` awaiting it, for the
        // reason the crate-private `retry::run` gives.
        run(self, method, uri, attempt)
    }

    /// Whether a call under this policy can make more than one attempt, and
    /// so has to keep its request body after the first.
    pub(crate) fn can_retry(&self) -> bool {
        self.max_retries > 0
    }

    /// Whether `error` is a failure this policy repeats, before counting
    /// attempts or time.
    fn retryable(&self, error: &Error) -> bool {
        let builtin = match error.kind() {
            ErrorKind::Timeout { .. } => self.api_timeout_error,
            ErrorKind::Connection => self.api_connection_error,
            ErrorKind::Api(api) => self.http_statuses.contains(api.status().as_u16()),
            // A request that could not be built fails the same way again, a
            // response that did not decode is not a status, and a response
            // over the limit will be as large again.
            ErrorKind::InvalidRequest
            | ErrorKind::Config
            | ErrorKind::ResponseValidation(_)
            | ErrorKind::ResponseTooLarge { .. } => false,
        };
        builtin || self.predicate.as_ref().is_some_and(|predicate| predicate(error))
    }

    /// The wait before the next attempt, after `attempts` attempts, the last
    /// of which failed with `error`: the server's delay when it gave one and
    /// this policy respects it, the backoff otherwise.
    fn delay<T: Time>(&self, time: &T, attempts: u32, error: &Error) -> Duration {
        if self.respect_retry_after {
            // The wall clock is read only for a response that names a delay,
            // since only an HTTP date is measured against it.
            let names_delay = |headers: &HeaderMap| {
                headers.contains_key(RETRY_AFTER_MS_HEADER) || headers.contains_key(RETRY_AFTER)
            };
            let asked = match error.kind() {
                ErrorKind::Api(api) if names_delay(api.headers()) => {
                    parse_retry_after(api.headers(), time.system_now())
                }
                // A response that did not decode is still a response, and a
                // predicate may have asked for it to be retried.
                ErrorKind::ResponseValidation(invalid) if names_delay(invalid.headers()) => {
                    parse_retry_after(invalid.headers(), time.system_now())
                }
                _ => None,
            };
            if let Some(asked) = asked {
                return asked;
            }
        }
        seconds_to_duration(backoff_seconds(
            attempts,
            self.backoff_initial.as_secs_f64(),
            self.backoff_max.as_secs_f64(),
            self.backoff_jitter,
            time.draw(),
        ))
    }

    /// The wait before the next attempt, when `retries` retries came before
    /// the attempt that just failed with `error`; `None` when the call should
    /// fail with `error` instead. `started` is when the call began, read only
    /// when the policy has a budget.
    fn next_delay<T: Time>(
        &self,
        time: &T,
        retries: u32,
        error: &Error,
        started: Option<Instant>,
    ) -> Option<Duration> {
        if !self.retryable(error) || retries >= self.max_retries {
            return None;
        }
        // Below `max_retries`, itself a `u32`, so adding one cannot overflow.
        let attempts = retries + 1;
        let delay = self.delay(time, attempts, error);
        if let (Some(budget), Some(started)) = (self.timeout, started) {
            // Saturating: a delay of `Duration::MAX` reaches any budget
            // instead of overflowing.
            let elapsed = time.now().saturating_duration_since(started);
            if elapsed.saturating_add(delay) >= budget {
                return None;
            }
        }
        Some(delay)
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for RetryPolicy {
    /// Every setting; a predicate, which has no text of its own, prints as
    /// `<predicate>`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetryPolicy")
            .field("max_retries", &self.max_retries)
            .field("backoff_initial", &self.backoff_initial)
            .field("backoff_max", &self.backoff_max)
            .field("backoff_jitter", &self.backoff_jitter)
            .field("http_statuses", &self.http_statuses)
            .field("respect_retry_after", &self.respect_retry_after)
            .field("api_connection_error", &self.api_connection_error)
            .field("api_timeout_error", &self.api_timeout_error)
            .field("predicate", &self.predicate.as_ref().map(|_| Opaque))
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// What a predicate prints as.
struct Opaque;

impl fmt::Debug for Opaque {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<predicate>")
    }
}

// -------------------------------------------------------------- StatusSet

/// The number of statuses a [`StatusSet`] can hold: 0 to 639.
const STATUS_LIMIT: u16 = 640;

/// A set of HTTP status codes, as a fixed bitmap: copying one costs 80 bytes
/// and looking a status up costs a shift, with no allocation either way.
///
/// It holds 0 to 639, which covers every status HTTP defines (100 to 599).
/// A status of 640 or more is never contained, and inserting one does
/// nothing and returns `false`.
///
/// In a [`RetryPolicy`] the set is asked only about responses reported as
/// [`ErrorKind::Api`], so a success status in it has no effect; see
/// [`RetryPolicy::http_statuses`].
///
/// ```
/// use typesafe_sdk::StatusSet;
///
/// let mut statuses = StatusSet::DEFAULT;
/// assert!(statuses.contains(503));
/// statuses.remove(503);
/// statuses.insert(409);
/// assert_eq!(format!("{statuses:?}"), "{408, 409, 429, 500..=502, 504..=599}");
///
/// let only = [429, 503].into_iter().collect::<StatusSet>();
/// assert_eq!(only.iter().collect::<Vec<_>>(), [429, 503]);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StatusSet([u64; 10]);

impl StatusSet {
    /// The statuses retried by default, as in the Python SDK: 408 (request
    /// timeout), 429 (too many requests) and every 5xx, 500 to 599.
    pub const DEFAULT: Self = {
        let mut set = Self::empty();
        set.insert(408);
        set.insert(429);
        let mut status = 500;
        while status < 600 {
            set.insert(status);
            status += 1;
        }
        set
    };

    /// A set with no status in it.
    #[must_use]
    pub const fn empty() -> Self {
        Self([0; 10])
    }

    /// Whether `status` is in the set; always `false` from 640 up.
    #[must_use]
    pub const fn contains(&self, status: u16) -> bool {
        match Self::slot(status) {
            Some((word, bit)) => self.0[word] & bit != 0,
            None => false,
        }
    }

    /// Adds `status`, and says whether it was not there before. A status of
    /// 640 or more cannot be held: nothing changes and the answer is `false`.
    pub const fn insert(&mut self, status: u16) -> bool {
        match Self::slot(status) {
            Some((word, bit)) => {
                let added = self.0[word] & bit == 0;
                self.0[word] |= bit;
                added
            }
            None => false,
        }
    }

    /// Removes `status`, and says whether it was there.
    pub const fn remove(&mut self, status: u16) -> bool {
        match Self::slot(status) {
            Some((word, bit)) => {
                let removed = self.0[word] & bit != 0;
                self.0[word] &= !bit;
                removed
            }
            None => false,
        }
    }

    /// Whether the set holds no status.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        let mut word = 0;
        while word < self.0.len() {
            if self.0[word] != 0 {
                return false;
            }
            word += 1;
        }
        true
    }

    /// The statuses in the set, from the lowest up.
    pub fn iter(&self) -> impl Iterator<Item = u16> + use<> {
        let words = self.0;
        (0_u16..).zip(words).flat_map(|(index, word)| {
            let mut rest = word;
            std::iter::from_fn(move || {
                if rest == 0 {
                    return None;
                }
                // `trailing_zeros` of a non-zero `u64` is below 64, so the
                // status fits a `u16` with room to spare.
                let bit = rest.trailing_zeros() as u16;
                // Clears the lowest set bit.
                rest &= rest - 1;
                Some(index * 64 + bit)
            })
        })
    }

    /// The word and the bit of `status`, or `None` past the last one.
    const fn slot(status: u16) -> Option<(usize, u64)> {
        if status >= STATUS_LIMIT {
            return None;
        }
        Some(((status / 64) as usize, 1 << (status % 64)))
    }
}

impl Default for StatusSet {
    /// [`StatusSet::DEFAULT`], the statuses retried unless a policy says
    /// otherwise.
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl FromIterator<u16> for StatusSet {
    /// Exactly the statuses given; one of 640 or more is left out.
    fn from_iter<I: IntoIterator<Item = u16>>(statuses: I) -> Self {
        let mut set = Self::empty();
        set.extend(statuses);
        set
    }
}

impl Extend<u16> for StatusSet {
    fn extend<I: IntoIterator<Item = u16>>(&mut self, statuses: I) {
        for status in statuses {
            self.insert(status);
        }
    }
}

impl fmt::Debug for StatusSet {
    /// `{408, 429, 500..=599}`: a run of three or more statuses prints as a
    /// range.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("{")?;
        let mut statuses = self.iter().peekable();
        let mut first = true;
        while let Some(start) = statuses.next() {
            let mut end = start;
            while statuses.next_if_eq(&(end + 1)).is_some() {
                end += 1;
            }
            if !first {
                formatter.write_str(", ")?;
            }
            first = false;
            match end - start {
                0 => write!(formatter, "{start}")?,
                1 => write!(formatter, "{start}, {end}")?,
                _ => write!(formatter, "{start}..={end}")?,
            }
        }
        formatter.write_str("}")
    }
}

// ------------------------------------------------------------- the loop

/// What the retry loop reads from outside the program: the monotonic clock
/// the budget is measured on, the wall clock an HTTP-date `Retry-After` is
/// measured against, the sleep between attempts, and the random draw of the
/// jitter.
///
/// A call runs on [`Tokio`]'s; the crate's own tests run it on a fake whose
/// clock stands still until the fake sleep or the test server moves it.
trait Time {
    /// Now, on the clock the budget is measured on.
    fn now(&self) -> Instant;
    /// Now, on the wall clock.
    fn system_now(&self) -> SystemTime;
    /// Waits `delay`. Dropping the future cancels the wait.
    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send;
    /// A random number in `[0, 1)`.
    fn draw(&self) -> f64;
}

/// The real clocks, Tokio's timer, and `fastrand`'s thread-local generator.
struct Tokio;

impl Time for Tokio {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn system_now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn sleep(&self, delay: Duration) -> impl Future<Output = ()> + Send {
        // Tokio clamps a deadline past its far future instead of panicking,
        // so even `Duration::MAX` is a valid wait.
        tokio::time::sleep(delay)
    }

    fn draw(&self) -> f64 {
        fastrand::f64()
    }
}

/// Runs `attempt` until it succeeds or `policy` stops retrying, and returns
/// its success or its last error, unchanged.
///
/// `attempt` is called with the number of attempts made before it, which is
/// what `X-TypeSafe-Retry-Count` carries. `method` and `uri` name the request
/// in the event logged before each retry.
///
/// A plain function returning the loop's future rather than an `async fn`
/// awaiting it: a wrapping `async fn` would keep its own copy of `attempt`
/// beside the loop's, and the call's future would carry both.
#[cfg(not(test))]
pub(crate) fn run<R, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    uri: &Uri,
    attempt: F,
) -> impl Future<Output = Result<R, Error>>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<R, Error>>,
{
    run_on(&Tokio, policy, method, uri, attempt)
}

// The future of `RetryPolicy::run` that ships, the one above, is `Send` for a
// `Send` attempt: checked by every build of the library, every feature set
// included. The unit tests see the `cfg(test)` twin below instead.
#[cfg(not(test))]
const _: () = {
    fn assert_send<T: Send>(_: &T) {}
    #[expect(dead_code, reason = "compiled for its Send check, never called")]
    fn check(policy: &RetryPolicy, method: &Method, uri: &Uri) {
        let future = policy.run(method, uri, |_| async { Ok::<(), Error>(()) });
        assert_send(&future);
    }
};

/// [`run`], on the fake clock of the crate's own tests when the policy
/// carries one.
#[cfg(test)]
pub(crate) async fn run<R, F, Fut>(
    policy: &RetryPolicy,
    method: &Method,
    uri: &Uri,
    attempt: F,
) -> Result<R, Error>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<R, Error>>,
{
    if let Some(time) = &policy.time {
        return run_on(&**time, policy, method, uri, attempt).await;
    }
    run_on(&Tokio, policy, method, uri, attempt).await
}

/// [`run`] on the given clock, sleep and draw.
async fn run_on<T, R, F, Fut>(
    time: &T,
    policy: &RetryPolicy,
    method: &Method,
    uri: &Uri,
    mut attempt: F,
) -> Result<R, Error>
where
    T: Time,
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<R, Error>>,
{
    // The budget counts from the start of the call, the first attempt
    // included; with no budget, or no retry to spend it on, the clock is not
    // read at all.
    let started = (policy.timeout.is_some() && policy.can_retry()).then(|| time.now());
    let mut retry = 0_u32;
    loop {
        if retry > 0 {
            telemetry::retrying(telemetry::Exchange::new(method, uri, retry, false));
        }
        let error = match attempt(retry).await {
            Ok(done) => return Ok(done),
            Err(error) => error,
        };
        let Some(delay) = policy.next_delay(time, retry, &error, started) else {
            return Err(error);
        };
        drop(error);
        time.sleep(delay).await;
        // `next_delay` answers only while `retry` is below `max_retries`.
        retry += 1;
    }
}

/// The delay, in seconds, before the attempt after attempt number `attempt`
/// failed.
///
/// The delay starts at `initial`, doubles with each attempt and stops growing
/// at `max`; `jitter` then removes a random share of it, up to `jitter` of the
/// whole, so that clients which failed together do not retry together. `draw`
/// is that random number, in `[0, 1)`, taken as an argument so the delay is a
/// pure function of its inputs. A zero `initial` or `max` disables backoff.
///
/// Attempts are numbered from 1, the first try. `attempt` 0 is not a number
/// the retry loop produces; it follows the same arithmetic and gives half of
/// `initial`, which keeps the schedule monotonic from 0 upwards. The type is
/// `u32` because every value converts exactly into `f64` and `i64`, which the
/// cap test and the doubling depend on, and no call is retried four billion
/// times.
///
/// The cap is tested in log2 space, before any doubling, so no intermediate
/// value overflows however large `attempt` is; the doubling itself is an exact
/// `ldexp`, not `initial * 2^exponent`, whose power of two alone would
/// overflow before the product came back under `max`.
///
/// The result is rounded to milliseconds and never exceeds the delay before
/// jitter: a delay that rounds up past a sub-millisecond `max` is `max`.
/// Rounding is exact decimal rounding of the binary value with ties to even,
/// the rule of Python's `round(delay, 3)`, rather than scaling by 1000 and
/// rounding the product: that shortcut rounds `1.0005` (stored just below it)
/// up to `1.001`, and the exact tie `0.0625` up to `0.063`, where this and
/// Python give `1.0` and `0.062`.
///
/// Non-finite or negative inputs are rejected where a retry policy is built,
/// so none is checked here; none of them panics.
pub(crate) fn backoff_seconds(attempt: u32, initial: f64, max: f64, jitter: f64, draw: f64) -> f64 {
    if initial == 0.0 || max == 0.0 {
        return 0.0;
    }
    // The subtraction is exact: every `u32` is representable in an `f64`.
    let exponential = if f64::from(attempt) - 1.0 >= max.log2() - initial.log2() {
        max
    } else {
        // Reaching here bounds the exponent by the log2 distance between two
        // finite values, under 2100, so the conversion only saturates when
        // `max` is infinite, where `scalbn` overflows to infinity either way.
        let exponent = i32::try_from(i64::from(attempt) - 1).unwrap_or(i32::MAX);
        scalbn(initial, exponent)
    };
    let delay = exponential * (1.0 - draw * jitter);
    exponential.min(round_to_millis(delay))
}

/// Converts a delay in seconds into a [`Duration`], saturating instead of
/// failing.
///
/// NaN, zero and negative values become [`Duration::ZERO`]; a value too large
/// for a `Duration`, infinity included, becomes [`Duration::MAX`].
fn seconds_to_duration(seconds: f64) -> Duration {
    if seconds.is_nan() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    // `from_secs_f64` would panic on overflow; the fallible form reports it,
    // and a positive, non-NaN value can only fail by being too large.
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

/// `x * 2^n`, correctly rounded: C's `scalbn`, which std does not provide.
///
/// This is musl's algorithm. A power of two outside the normal range of an
/// `f64` cannot be written as one value, so a large `n` is applied in steps of
/// `2^1023`; while scaling down, each step is `2^-1022` times `2^53`, which
/// keeps the intermediate value normal so the result is rounded only once.
/// `n` is clamped after two steps, where any finite nonzero `x` has already
/// overflowed or underflowed.
fn scalbn(x: f64, n: i32) -> f64 {
    const TWO_POW_1023: f64 = f64::from_bits(0x7FE0_0000_0000_0000);
    const TWO_POW_MINUS_969: f64 = f64::from_bits(0x0360_0000_0000_0000);
    let mut y = x;
    let mut n = n;
    if n > 1023 {
        y *= TWO_POW_1023;
        n -= 1023;
        if n > 1023 {
            y *= TWO_POW_1023;
            n = (n - 1023).min(1023);
        }
    } else if n < -1022 {
        y *= TWO_POW_MINUS_969;
        n += 1022 - 53;
        if n < -1022 {
            y *= TWO_POW_MINUS_969;
            n = (n + 1022 - 53).max(-1022);
        }
    }
    // `n` is now in `[-1022, 1023]`, so the biased exponent `1023 + n` is in
    // `[1, 2046]`: a normal power of two built directly from its bits.
    y * f64::from_bits(u64::from((0x3FF + n).unsigned_abs()) << 52)
}

/// `x` rounded to three decimal places, as Python's `round(x, 3)`.
///
/// Formatting with a precision rounds the exact decimal expansion of the
/// binary value, ties to even, and parsing back picks the nearest `f64`: the
/// same two steps CPython's `round` takes. From `2^52` up every `f64` is an
/// integer, so larger values, infinities and NaN are returned unchanged, which
/// also bounds the text to 21 bytes and keeps it on the stack.
fn round_to_millis(x: f64) -> f64 {
    const INTEGRAL_FROM: f64 = 4_503_599_627_370_496.0; // 2^52
    if x.is_nan() || x.abs() >= INTEGRAL_FROM {
        return x;
    }
    let mut buf = [0_u8; 24];
    // Writing into `&mut [u8]` advances the slice past what was written, so
    // the length left over tells how much was.
    let unused = {
        let mut rest = &mut buf[..];
        write!(rest, "{x:.3}")
            .expect("invariant: a value below 2^52 needs at most 21 bytes at three decimals");
        rest.len()
    };
    let written = buf.len() - unused;
    std::str::from_utf8(&buf[..written])
        .expect("invariant: formatted digits are ASCII")
        .parse()
        .expect("invariant: a formatted finite f64 parses back")
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
