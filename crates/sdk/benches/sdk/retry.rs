//! B4: reading `Retry-After`, and the backoff delay.
//!
//! The parse is reached the way a caller reaches it: an API error from a
//! real call through the in-memory transport, then `ApiError::retry_after`,
//! which reads `retry-after-ms`, then `Retry-After` as seconds or as an HTTP
//! date measured against the system clock. The error is made once, outside
//! the timed section.
//!
//! How this can mislead: `retry_after` reads the system clock on every call,
//! so the HTTP-date case includes a `SystemTime::now()`; the other two read
//! it too and never use it. Inside the retry loop the SDK passes the clock
//! reading it already has.
//!
//! `backoff` is the pure delay function with the default policy's numbers
//! (0.5 s initial, 5 s cap, 0.25 jitter), at the first attempt, at one past
//! the cap, and far past it, where the cap is tested in log2 space before
//! any doubling. The random draw is an argument, so no generator is timed.
//! Every input goes through `black_box` and the result is returned to divan,
//! so the compiler can neither fold the call nor drop it. The number is the
//! function alone, a few hundred nanoseconds of pure `f64` arithmetic and one
//! format-and-parse round: it is reported for completeness, not as a target,
//! since it runs once per retry beside a sleep of hundreds of milliseconds.

use std::pin::pin;

use divan::{Bencher, black_box};
use http::StatusCode;
use typesafe_sdk::{__internals as sdk, ApiError, ErrorKind, RetryPolicy};

use crate::{
    service::{InMemory, client, runtime},
    support::{questions, text},
};

/// The three spellings: milliseconds, seconds, and an HTTP date.
fn header(spelling: &str) -> (&'static str, &'static str) {
    match spelling {
        "ms" => ("retry-after-ms", "1500"),
        "seconds" => ("retry-after", "2"),
        "date" => ("retry-after", "Fri, 01 Jan 2100 00:00:00 GMT"),
        other => panic!("no spelling {other}"),
    }
}

/// A 429 carrying `header`, as the error a call returns.
fn rate_limited(header: (&'static str, &'static str)) -> ApiError {
    let client = client(InMemory::failing(StatusCode::TOO_MANY_REQUESTS, header));
    let questions = questions();
    let state = text(64);
    let runtime = runtime();
    let call = client
        .system_one(state.as_str(), &questions)
        .retry(RetryPolicy::default().max_retries(0))
        .send();
    let error = runtime.block_on(pin!(call)).expect_err("a 429 is an error");
    match error.kind() {
        ErrorKind::Api(api) => api.clone(),
        other => panic!("a 429 is an API error, not {other:?}"),
    }
}

#[divan::bench(args = ["ms", "seconds", "date"])]
fn retry_after(bencher: Bencher<'_, '_>, spelling: &str) {
    let error = rate_limited(header(spelling));
    assert!(error.retry_after().is_some(), "{spelling}: {:?} gives a wait", header(spelling));
    bencher.bench_local(|| black_box(&error).retry_after());
}

#[divan::bench(args = [1, 6, 1000])]
fn backoff(bencher: Bencher<'_, '_>, attempt: u32) {
    assert!(sdk::backoff_seconds(attempt, 0.5, 5.0, 0.25, 0.5) > 0.0);
    bencher.bench_local(|| {
        sdk::backoff_seconds(
            black_box(attempt),
            black_box(0.5),
            black_box(5.0),
            black_box(0.25),
            black_box(0.5),
        )
    });
}
