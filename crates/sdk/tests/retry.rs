//! The retry policy through the public API, against a real loopback server.
//!
//! The ports of upstream's `tests/test_retry.py` run inside the crate
//! (`src/retry_tests.rs`), where the clock and the sleep can be replaced.
//! These tests need neither: every delay here is zero - `retry-after-ms: 0`
//! or a zero backoff - so they run on the real clock, the way a caller's
//! program does. They cover what the policy promises beyond upstream's cases:
//! what is never retried on its own, that a body is encoded once whatever the
//! number of attempts, that a caller cannot set the retry count, that calls in
//! flight together keep their own policies, and the log line of a retry.

use std::{
    error::Error as StdError,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, StatusCode};
use serde::{Serialize, Serializer};
use test_support::{Protocol, RecordedRequest, TestResponse, TestServer, json_response};
use tokio::sync::watch;
use typesafe_sdk::{
    Client, ClientBuilder, Error, ErrorKind, Noul, PreparedQuestions, Questions, RetryPolicy,
    StatusSet,
};

#[cfg(feature = "tracing")]
#[path = "support/recorder.rs"]
mod recorder;

/// `RESULT` of upstream `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// A JSON response with `status`, `body`, and `retry-after-ms: 0` when
/// `now` is set.
fn respond(status: u16, body: &[u8], now: bool) -> TestResponse {
    let status = StatusCode::from_u16(status).expect("a test status is valid");
    let mut response = json_response(status, Bytes::copy_from_slice(body));
    if now {
        response.headers_mut().insert("retry-after-ms", HeaderValue::from_static("0"));
    }
    response
}

/// A server answering its `n`th request, counted from 1, with `answer(n)`.
async fn serve<F>(answer: F) -> TestServer
where
    F: Fn(usize, &RecordedRequest) -> TestResponse + Send + Sync + 'static,
{
    TestServer::start_nth(Protocol::Http1, answer).await.expect("the test server starts")
}

/// A builder for a client of `server`, with every setting the environment
/// could supply given.
fn builder(server: &TestServer) -> ClientBuilder {
    Client::builder().api_key("test-key").base_url(server.base_url()).default_model("jev-latest")
}

fn questions() -> PreparedQuestions {
    Questions::new().noul("q", Noul::new().instructions("?")).prepare().expect("prepares")
}

/// A policy that waits no time between attempts.
fn at_once() -> RetryPolicy {
    RetryPolicy::default().backoff_initial(Duration::ZERO)
}

/// The `X-TypeSafe-Retry-Count` values of each request, every one of them.
fn retry_counts(requests: &[RecordedRequest]) -> Vec<Vec<&str>> {
    requests.iter().map(|request| request.header_values("x-typesafe-retry-count")).collect()
}

/// What `retry_counts` reads for `attempts` attempts: nothing, then 1, 2, ...
fn expected_counts(attempts: usize) -> Vec<Vec<String>> {
    (0..attempts)
        .map(|attempt| if attempt == 0 { Vec::new() } else { vec![attempt.to_string()] })
        .collect()
}

// ------------------------------------------------------------ the policy

#[test]
fn the_default_policy_is_upstreams_and_prints_every_setting() {
    assert_eq!(
        format!("{:?}", RetryPolicy::default()),
        concat!(
            "RetryPolicy { max_retries: 2, backoff_initial: 500ms, backoff_max: 5s, ",
            "backoff_jitter: 0.25, http_statuses: {408, 429, 500..=599}, ",
            "respect_retry_after: true, api_connection_error: true, api_timeout_error: true, ",
            "predicate: None, timeout: Some(30s) }",
        )
    );
    let custom = RetryPolicy::new()
        .max_retries(5)
        .backoff_initial(Duration::from_millis(100))
        .backoff_max(Duration::from_secs(1))
        .backoff_jitter(0.5)
        .expect("valid")
        .http_statuses([503, 409].into_iter().collect())
        .respect_retry_after(false)
        .api_connection_error(false)
        .api_timeout_error(false)
        .predicate(|_| false)
        .no_timeout();
    assert_eq!(
        format!("{custom:?}"),
        concat!(
            "RetryPolicy { max_retries: 5, backoff_initial: 100ms, backoff_max: 1s, ",
            "backoff_jitter: 0.5, http_statuses: {409, 503}, respect_retry_after: false, ",
            "api_connection_error: false, api_timeout_error: false, ",
            "predicate: Some(<predicate>), timeout: None }",
        )
    );
}

#[test]
fn a_status_set_holds_0_to_639_and_prints_runs_as_ranges() {
    let default = StatusSet::default();
    assert_eq!(default, StatusSet::DEFAULT);
    for status in [408, 429, 500, 503, 599] {
        assert!(default.contains(status), "{status}");
    }
    for status in [0, 200, 302, 400, 404, 409, 499, 600, 639, 640, u16::MAX] {
        assert!(!default.contains(status), "{status}");
    }
    assert_eq!(default.iter().count(), 102);
    assert_eq!(format!("{default:?}"), "{408, 429, 500..=599}");

    let mut set = StatusSet::empty();
    assert!(set.is_empty());
    assert_eq!(format!("{set:?}"), "{}");
    assert!(set.insert(639), "the last status it can hold");
    assert!(!set.is_empty(), "a set holding only 639, in its last word, is not empty");
    let first_word: StatusSet = [0].into_iter().collect();
    assert!(!first_word.is_empty(), "a set holding only 0, in its first word, is not empty");
    assert!(!set.insert(639), "already there");
    assert!(!set.insert(640), "beyond the last status");
    assert!(!set.insert(u16::MAX));
    assert!(!set.contains(640));
    assert!(set.insert(0));
    assert!(set.insert(1));
    assert_eq!(format!("{set:?}"), "{0, 1, 639}");
    assert!(set.remove(639));
    assert!(!set.remove(639));
    assert!(!set.remove(640));
    set.extend([2, 3, 700]);
    assert_eq!(set.iter().collect::<Vec<_>>(), [0, 1, 2, 3]);
    assert_eq!(format!("{set:?}"), "{0..=3}");

    let every: StatusSet = (0..=u16::MAX).collect();
    assert_eq!(every.iter().count(), 640);
    assert_eq!(format!("{every:?}"), "{0..=639}");
    let copied = every;
    assert_eq!(copied, every, "a set is Copy");
}

/// A policy set on the client or on a call shows in their `Debug`; none shows
/// when none was set.
#[test]
fn a_policy_is_printed_where_it_was_set() {
    let policy = RetryPolicy::default().max_retries(0);
    let shown = format!("{policy:?}");
    let builder = Client::builder().api_key("test-key").retry(policy.clone());
    assert!(format!("{builder:?}").ends_with(&format!(", retry: {shown} }}")), "{builder:?}");
    assert!(!format!("{:?}", Client::builder()).contains("retry"));

    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build()
        .expect("the client builds");
    let questions = questions();
    let request = client.system_one("x", &questions);
    assert!(!format!("{request:?}").contains("retry"), "{request:?}");
    let request = request.retry(policy.clone());
    assert!(format!("{request:?}").ends_with(&format!(", retry: {shown}, .. }}")), "{request:?}");
    let list = client.models().list();
    assert!(!format!("{list:?}").contains("retry"), "{list:?}");
    let list = list.retry(policy);
    assert!(format!("{list:?}").ends_with(&format!(", retry: {shown}, .. }}")), "{list:?}");
}

// -------------------------------------------------- what is retried at all

/// The default policy, unmodified, through the public API: a client built
/// without `.retry()` retries a 503 twice and numbers the retries. The server
/// asks for no wait (`retry-after-ms: 0`), which the default respects, so the
/// default's 0.5 s and 1 s backoff is never slept here.
#[tokio::test]
async fn a_client_without_a_policy_retries_as_upstream_does() {
    let server = serve(|_, _| respond(503, br#"{"message": "down"}"#, true)).await;
    let client = builder(&server).build().expect("the client builds");

    let error = client.models().list().send().await.expect_err("every attempt fails");
    assert_eq!(error.to_string(), format!("GET {}/v1/models: 503 down", server.base_url()));
    assert_eq!(retry_counts(&server.requests()), expected_counts(3));
}

/// A call's own policy beats the client's in both directions, and the
/// client's is back for the next call.
#[tokio::test]
async fn a_calls_policy_beats_the_clients() {
    let server = serve(|_, _| respond(503, br#"{"message": "down"}"#, true)).await;
    let default = builder(&server).build().expect("the client builds");
    let once = builder(&server)
        .retry(RetryPolicy::default().max_retries(0))
        .build()
        .expect("the client builds");

    let calls = [
        (&default, Some(RetryPolicy::default().max_retries(1)), 2),
        (&default, None, 3),
        (&once, Some(RetryPolicy::default()), 3),
        (&once, None, 1),
    ];
    for (index, (client, policy, attempts)) in calls.into_iter().enumerate() {
        let before = server.request_count();
        let mut request = client.models().list();
        if let Some(policy) = policy {
            request = request.retry(policy);
        }
        let error = request.send().await.expect_err("every attempt fails");
        assert_eq!(error.to_string(), format!("GET {}/v1/models: 503 down", server.base_url()));
        let requests = server.requests();
        assert_eq!(retry_counts(&requests[before..]), expected_counts(attempts), "call {index}");
    }
}

/// A success body over the limit will be as large again: it is not retried
/// unless the caller's predicate asks, and then the error is returned whole.
#[tokio::test]
async fn a_response_too_large_is_retried_only_when_the_predicate_asks() {
    let big = vec![b' '; 2000];
    let server = serve(move |_, _| respond(200, &big, false)).await;
    let client = builder(&server).max_response_bytes(1000).build().expect("the client builds");

    let assert_too_large = |error: &Error| {
        assert!(matches!(error.kind(), ErrorKind::ResponseTooLarge { limit: 1000 }), "{error:?}");
        assert_eq!(
            error.to_string(),
            "The response body exceeded the limit of 1000 bytes and was not read."
        );
        assert!(error.source().is_none(), "{error:?}");
    };
    let error = client.models().list().send().await.expect_err("too large");
    assert_too_large(&error);
    assert_eq!(server.request_count(), 1, "not retried on its own");

    let asking = at_once()
        .max_retries(1)
        .predicate(|error| matches!(error.kind(), ErrorKind::ResponseTooLarge { .. }));
    let error = client.models().list().retry(asking).send().await.expect_err("too large");
    assert_too_large(&error);
    assert_eq!(server.request_count(), 1 + 2, "retried because the predicate asked");
}

/// A success response that does not decode is not a status to retry; a
/// predicate sees it, because decoding is part of the attempt.
#[tokio::test]
async fn a_response_that_does_not_decode_is_retried_only_when_the_predicate_asks() {
    let server = serve(|_, _| respond(200, b"[]", false)).await;
    let client = builder(&server).build().expect("the client builds");

    let error = client.models().list().send().await.expect_err("not a model list");
    let ErrorKind::ResponseValidation(invalid) = error.kind() else {
        panic!("a response-validation error was expected: {error:?}");
    };
    assert_eq!(invalid.status(), StatusCode::OK);
    assert_eq!(server.request_count(), 1, "not retried on its own");

    let asking =
        at_once().predicate(|error| matches!(error.kind(), ErrorKind::ResponseValidation(_)));
    let error = client.models().list().retry(asking).send().await.expect_err("never decodes");
    assert!(matches!(error.kind(), ErrorKind::ResponseValidation(_)), "{error:?}");
    assert_eq!(server.request_count(), 1 + 3, "retried because the predicate asked");
}

/// A success status in the policy's set does not make a response that did
/// not decode retryable: the set is asked about API errors only. The
/// predicate still can ask for it.
#[tokio::test]
async fn a_success_status_in_the_set_does_not_retry_a_response_that_does_not_decode() {
    let server = serve(|_, _| respond(200, b"[]", false)).await;
    let client = builder(&server).build().expect("the client builds");
    let mut statuses = StatusSet::DEFAULT;
    statuses.insert(200);
    let with_200 = || at_once().http_statuses(statuses);

    let error =
        client.models().list().retry(with_200()).send().await.expect_err("not a model list");
    let ErrorKind::ResponseValidation(invalid) = error.kind() else {
        panic!("a response-validation error was expected: {error:?}");
    };
    assert_eq!(invalid.status(), StatusCode::OK);
    assert_eq!(server.request_count(), 1, "a 2xx in the set is not a reason to retry");

    let asking =
        with_200().predicate(|error| matches!(error.kind(), ErrorKind::ResponseValidation(_)));
    let error = client.models().list().retry(asking).send().await.expect_err("never decodes");
    assert!(matches!(error.kind(), ErrorKind::ResponseValidation(_)), "{error:?}");
    assert_eq!(server.request_count(), 1 + 3, "retried because the predicate asked");
}

/// A deadline that passes is retried on the real clock: 50 ms against a
/// handler held on a channel, twice, then answered.
#[tokio::test]
async fn an_attempt_past_its_deadline_is_retried_on_the_real_clock() {
    let (release, released) = watch::channel(false);
    let served = Arc::new(AtomicUsize::new(0));
    let server = TestServer::start(Protocol::Http1, move |_| {
        let mut released = released.clone();
        let attempt = served.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            if attempt <= 2 {
                drop(released.wait_for(|released| *released).await);
            }
            respond(200, br#"{"models": []}"#, false)
        }
    })
    .await
    .expect("the test server starts");
    let client = builder(&server).retry(at_once()).build().expect("the client builds");

    let response = client
        .models()
        .list()
        .timeout(Duration::from_millis(50))
        .send()
        .await
        .expect("the third attempt answers");
    assert!(response.models().is_empty());
    assert_eq!(retry_counts(&server.requests()), expected_counts(3));
    release.send_modify(|released| *released = true);
}

// ------------------------------------------------------ what a retry sends

/// A state that counts how often it is encoded.
struct Counted<'a> {
    encodes: &'a AtomicUsize,
}

impl Serialize for Counted<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.encodes.fetch_add(1, Ordering::SeqCst);
        serializer.serialize_str("hello")
    }
}

/// Every attempt sends the bytes the one encode produced; nothing is encoded
/// again for a retry.
#[tokio::test]
async fn a_body_is_encoded_once_whatever_the_number_of_attempts() {
    let server = serve(|attempt, _| match attempt {
        1 | 2 => respond(503, b"{}", true),
        _ => respond(200, RESULT, false),
    })
    .await;
    let client = builder(&server).build().expect("the client builds");
    let questions = questions();
    let encodes = AtomicUsize::new(0);

    let response = client
        .system_one(&Counted { encodes: &encodes }, &questions)
        .send()
        .await
        .expect("the third attempt succeeds");
    assert_eq!(response.answers().len(), 3);
    assert_eq!(encodes.load(Ordering::SeqCst), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let expected = concat!(
        r#"{"state":"hello","model":"jev-latest","#,
        r#""questions":{"q":{"type":"noul","instructions":"?"}}}"#,
    );
    for request in &requests {
        assert_eq!(request.body, expected.as_bytes());
    }
}

/// A caller's own `X-TypeSafe-Retry-Count`, as a default or on the call, is
/// dropped on every attempt, and the SDK's is the only one sent.
#[tokio::test]
async fn a_callers_retry_count_is_dropped_on_every_attempt() {
    let server = serve(|_, _| respond(503, b"{}", true)).await;
    let client = builder(&server)
        .default_header("X-TypeSafe-Retry-Count", "98")
        .build()
        .expect("the client builds");
    let questions = questions();

    client
        .system_one("x", &questions)
        .header("x-typesafe-retry-count", "99")
        .send()
        .await
        .expect_err("every attempt fails");
    client
        .models()
        .list()
        .header("X-TYPESAFE-RETRY-COUNT", "99")
        .send()
        .await
        .expect_err("every attempt fails");
    let requests = server.requests();
    assert_eq!(retry_counts(&requests[..3]), expected_counts(3));
    assert_eq!(retry_counts(&requests[3..]), expected_counts(3));
}

/// 64 calls in flight on several threads, each with a policy of its own or
/// the client's, see only their own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_calls_on_several_threads_keep_their_own_policies() {
    let server = TestServer::start(Protocol::Http1, |_| async { respond(429, b"{}", true) })
        .await
        .expect("the test server starts");
    let client = builder(&server).retry(at_once().max_retries(1)).build().expect("builds");
    let questions = Arc::new(questions());

    // Call `index` retries `index % 4` times on a policy of its own when the
    // index is even, and once on the client's when it is odd.
    let expected = |index: usize| if index.is_multiple_of(2) { index % 4 + 1 } else { 2 };
    let mut calls = tokio::task::JoinSet::new();
    for index in 0..64_usize {
        let client = client.clone();
        let questions = Arc::clone(&questions);
        calls.spawn(async move {
            let name = index.to_string();
            let mut request = client.system_one(&name, &questions).header("x-call", name.clone());
            if index.is_multiple_of(2) {
                let retries = u32::try_from(index % 4).expect("small");
                request = request.retry(at_once().max_retries(retries));
            }
            (index, request.send().await)
        });
    }
    while let Some(joined) = calls.join_next().await {
        let (index, outcome) = joined.expect("no call panicked");
        let error = outcome.expect_err("every attempt is rate limited");
        assert!(matches!(error.kind(), ErrorKind::Api(_)), "call {index}: {error:?}");
    }

    let requests = server.requests();
    assert_eq!(requests.len(), (0..64).map(expected).sum::<usize>());
    for index in 0..64 {
        let name = index.to_string();
        let mine: Vec<_> =
            requests.iter().filter(|request| request.headers["x-call"] == *name).cloned().collect();
        assert_eq!(retry_counts(&mine), expected_counts(expected(index)), "call {index}");
        let state = format!(r#"{{"state":"{name}","#);
        assert!(
            mine.iter().all(|request| request.body.starts_with(state.as_bytes())),
            "call {index}"
        );
    }
}

// ------------------------------------------------------------- logging

/// The `INFO` line upstream writes before a retry (`transport.py:73`):
/// `<METHOD> <url> retry <n>`, between the lines of the attempts around it,
/// and without anything the failed response carried.
#[cfg(feature = "tracing")]
mod logging {
    use tracing::Level;

    use super::*;
    use crate::recorder::{Recorder, assert_timed, install};

    #[tokio::test]
    async fn a_retry_is_announced_at_info_before_it_is_sent() {
        let recorder = Recorder::default();
        let _installed = install(&recorder);
        let server = serve(|attempt, _| match attempt {
            1 => respond(503, br#"{"message": "secret-body"}"#, true),
            _ => respond(200, br#"{"models": []}"#, false),
        })
        .await;
        let client = builder(&server).build().expect("the client builds");

        client.models().list().send().await.expect("the retry succeeds");

        let endpoint = format!("GET {}/v1/models", server.base_url());
        let info = recorder.at(Level::INFO);
        let [first, retry, second] = &info[..] else { panic!("three INFO lines: {info:#?}") };
        assert_timed(first, &format!("message={endpoint} <- 503 in "), " (request -)");
        assert_eq!(*retry, format!("message={endpoint} retry 1"));
        assert_timed(second, &format!("message={endpoint} <- 200 in "), " (request -)");

        let debug = recorder.at(Level::DEBUG);
        let sending: Vec<_> =
            debug.iter().filter(|line| line.contains("message=sending request")).collect();
        assert_eq!(sending.len(), 2, "{debug:#?}");
        assert!(sending[0].contains(" retry=0 "), "{}", sending[0]);
        assert!(sending[1].contains(" retry=1 "), "{}", sending[1]);
        for line in info.iter().chain(&debug) {
            assert!(!line.contains("secret-body"), "a response body reached {line}");
        }
    }
}
