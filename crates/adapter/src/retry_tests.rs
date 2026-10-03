//! Unit tests of `retry.rs`: the retry wrapper on its own, with closures in
//! the place of a provider.

use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use bytes::Bytes;
use decision_model_sdk::{ApiError, Error as SdkError, ErrorKind as SdkErrorKind, RetryPolicy};
use http::{HeaderMap, StatusCode, Uri};

use super::run_with_retries;
use crate::{
    provider::NonAnswer,
    response::{RetryCategory, RetryReason},
};

/// The SDK error a provider returns for a failure status, with upstream's
/// test body.
fn status_error(status: u16, message: &str) -> SdkError {
    let status = StatusCode::from_u16(status).expect("a valid status");
    let body = Bytes::from(format!(r#"{{"message":"{message}"}}"#));
    ApiError::from_response(status, body, HeaderMap::new()).into()
}

/// Upstream's `RetryPolicy(max_retries=n, backoff_initial=0.001,
/// backoff_jitter=0)`, without the wait: the tests run on the real clock.
fn policy(max_retries: u32) -> RetryPolicy {
    RetryPolicy::new()
        .max_retries(max_retries)
        .backoff_initial(Duration::ZERO)
        .backoff_jitter(0.0)
        .expect("a jitter of zero is valid")
}

fn status_of(error: &SdkError) -> u16 {
    match error.kind() {
        SdkErrorKind::Api(api) => api.status().as_u16(),
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn categories(reasons: &[RetryReason]) -> Vec<RetryCategory> {
    reasons.iter().map(RetryReason::category).collect()
}

// Upstream: tests/utils/test_error_handling.py::test_retries_succeed_after_transient_error
#[tokio::test]
async fn retries_succeed_after_a_transient_error() {
    let calls = AtomicU32::new(0);
    let mut reasons = Vec::new();
    let uri = Uri::from_static("/");

    let outcome = run_with_retries(&policy(1), &uri, &mut reasons, || {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        async move { if call == 1 { Err(status_error(503, "unavailable")) } else { Ok("success") } }
    })
    .await;

    let (result, n_retries) = outcome.expect("the second attempt succeeds");
    assert_eq!(result, "success");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(n_retries, 1);
    assert_eq!(categories(&reasons), [RetryCategory::ProviderError]);
}

// Upstream: tests/utils/test_error_handling.py::test_non_retryable_error_is_not_retried
#[tokio::test]
async fn a_non_retryable_error_is_not_retried() {
    let calls = AtomicU32::new(0);
    let mut reasons = Vec::new();
    let uri = Uri::from_static("/");

    let outcome = run_with_retries(&policy(2), &uri, &mut reasons, || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { Err::<(), _>(status_error(400, "bad")) }
    })
    .await;

    let error = outcome.expect_err("a 400 is returned as it is");
    assert_eq!(status_of(&error), 400);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(reasons.is_empty(), "no retry was made, so no reason is recorded");
}

// Upstream: tests/utils/test_error_handling.py::test_retries_are_exhausted_and_reasons_recorded
#[tokio::test]
async fn retries_are_exhausted_and_the_reasons_recorded() {
    let calls = AtomicU32::new(0);
    let mut reasons = Vec::new();
    let uri = Uri::from_static("/");

    let outcome = run_with_retries(&policy(2), &uri, &mut reasons, || {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        async move { Err::<(), _>(status_error(503, &format!("unavailable {call}"))) }
    })
    .await;

    let error = outcome.expect_err("every attempt fails");
    assert_eq!(status_of(&error), 503);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(categories(&reasons), [RetryCategory::ProviderError, RetryCategory::ProviderError]);
    // Each reason is the text of the attempt that failed before that retry;
    // the last failure is the error itself and adds no reason.
    let messages: Vec<&str> = reasons.iter().map(RetryReason::message).collect();
    assert_eq!(messages, ["503 unavailable 1", "503 unavailable 2"]);
    assert_eq!(error.to_string(), "503 unavailable 3");
}

#[tokio::test]
async fn a_reply_that_is_not_an_answer_ends_the_loop_without_a_retry() {
    let calls = AtomicU32::new(0);
    let mut reasons = Vec::new();
    let uri = Uri::from_static("/");

    let outcome = run_with_retries(&policy(2), &uri, &mut reasons, || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { Ok(Err::<(), _>(NonAnswer::new("the vendor refused"))) }
    })
    .await;

    let (result, n_retries) = outcome.expect("the exchange itself succeeded");
    assert_eq!(result, Err(NonAnswer::new("the vendor refused")));
    assert_eq!(n_retries, 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(reasons.is_empty());
}

#[tokio::test]
async fn reasons_are_appended_after_the_ones_already_recorded() {
    let calls = AtomicU32::new(0);
    let mut reasons = vec![RetryReason::new(RetryCategory::MalformedStructure, "earlier")];
    let uri = Uri::from_static("/");

    let outcome = run_with_retries(&policy(1), &uri, &mut reasons, || {
        let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
        async move { if call == 1 { Err(status_error(429, "slow down")) } else { Ok(call) } }
    })
    .await;

    assert_eq!(outcome.expect("the retry succeeds"), (2, 1));
    assert_eq!(
        categories(&reasons),
        [RetryCategory::MalformedStructure, RetryCategory::ProviderError]
    );
    assert_eq!(reasons[1].message(), "429 slow down");
}

/// The future is `Send` for a `Send` attempt, so a caller can spawn the call
/// it is part of.
#[test]
fn the_retry_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let policy = policy(1);
    let uri = Uri::from_static("/");
    let mut reasons = Vec::new();

    let future = run_with_retries(&policy, &uri, &mut reasons, || async { Ok::<(), SdkError>(()) });

    assert_send(&future);
}
