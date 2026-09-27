//! How many connections a client opens: one, however many requests it sends
//! and however many of them start together, and one more when the server
//! has closed it. When that one connect fails, every call that waited for it
//! fails with it, and none connects on its own.
//!
//! Every server here is HTTP/2 over TLS with a certificate the client trusts
//! through `add_root_certificate`, and its URL is an IP literal, so no second
//! socket is raced across resolved addresses and the count the server keeps
//! is the client's pool alone. The client uses its default HTTP version,
//! which for an `https` base URL is HTTP/2 only, unless a test says `Auto`.
//! A failed count prints the server's record of every connection: who
//! connected, how its TLS handshake ended, and how many requests it carried.

use std::{
    error::Error as StdError,
    io,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{Response, StatusCode, Version};
use http_body_util::Full;
use test_support::{Protocol, RefusingPort, SilentServer, TestServer, Tls};
use typesafe_sdk::{
    Client, ClientBuilder, Error, ErrorKind, HttpVersion, Noul, PreparedQuestions, Questions,
    RetryPolicy,
};

#[cfg(feature = "tracing")]
#[expect(
    dead_code,
    reason = "the recorder is shared with the logging tests, which use the parts this file does not"
)]
#[path = "support/recorder.rs"]
mod recorder;

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// What the message of a call's error starts with when the call waited for
/// another call's connect, and that connect failed; the other call's
/// failure follows it.
const WAITED: &str = "Connection error: the connect this request waited for failed: ";

async fn tls_server() -> TestServer {
    TestServer::start(Protocol::Http2Tls, |request| async move {
        let body: &'static [u8] =
            if request.method == http::Method::POST { RESULT } else { br#"{"models":[]}"# };
        let mut response = Response::new(Full::new(Bytes::from_static(body)));
        *response.status_mut() = StatusCode::OK;
        response
    })
    .await
    .expect("the test server starts")
}

/// A builder for a client of `server`, trusting its certificate.
fn builder_for(server: &TestServer) -> ClientBuilder {
    let certificate = server.certificate_der().expect("a TLS server has a certificate");
    Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .add_root_certificate(certificate.to_vec())
}

fn client_for(server: &TestServer) -> Client {
    builder_for(server).build().expect("the client builds")
}

/// A client of `base_url` that makes one attempt per call.
fn one_attempt_client(base_url: &str, version: HttpVersion) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(base_url)
        .http_version(version)
        .retry(RetryPolicy::default().max_retries(0))
        .build()
        .expect("the client builds")
}

/// Sends `count` System One calls at once, each from a task of its own, and
/// returns the error each ended with, in the order the calls started, under
/// a bound of `within`.
async fn failing_burst(client: &Client, count: usize, within: Duration) -> Vec<(Duration, Error)> {
    let questions = questions();
    let started = Instant::now();
    let tasks: Vec<_> = (0..count)
        .map(|_| {
            let (client, questions) = (client.clone(), questions.clone());
            tokio::spawn(async move {
                let error = client
                    .system_one("hello", &questions)
                    .send()
                    .await
                    .expect_err("nothing answers");
                (started.elapsed(), error)
            })
        })
        .collect();
    let ended = async {
        let mut errors = Vec::with_capacity(count);
        for task in tasks {
            errors.push(task.await.expect("the task ran"));
        }
        errors
    };
    tokio::time::timeout(within, ended)
        .await
        .unwrap_or_else(|_| panic!("the burst of {count} did not end within {within:?}"))
}

/// Waits under a bound of 5 s until `client` has no open stream: the server
/// closed its connection, and the client has seen the close.
#[cfg(feature = "internals")]
async fn until_no_stream_is_open(client: &Client, server: &TestServer) {
    use typesafe_sdk::__internals::open_streams;

    let closed = async {
        while open_streams(client) > 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), closed).await.unwrap_or_else(|_| {
        panic!(
            "the client still holds {} open stream(s) 5 s after the server closed its \
             connection: {:#?}",
            open_streams(client),
            server.connections()
        )
    });
}

fn questions() -> PreparedQuestions {
    Questions::new().noul("spam", Noul::new().instructions("Spam?")).prepare().expect("prepares")
}

/// Sends `count` System One calls at once, each from a task of its own.
async fn concurrently(client: &Client, questions: &PreparedQuestions, count: usize) {
    let mut tasks = Vec::with_capacity(count);
    for index in 0..count {
        let (client, questions) = (client.clone(), questions.clone());
        tasks.push(tokio::spawn(async move {
            client
                .system_one("hello", &questions)
                .send()
                .await
                .map(|response| response.answers().len())
                .map_err(|error| format!("call {index}: {error}"))
        }));
    }
    for task in tasks {
        let answers = task.await.expect("the task ran").unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(answers, 3);
    }
}

/// Every request the server saw arrived over HTTP/2.
fn assert_all_http2(server: &TestServer) {
    let versions: Vec<Version> = server.requests().iter().map(|request| request.version).collect();
    assert!(versions.iter().all(|version| *version == Version::HTTP_2), "{versions:?}");
}

/// The first cause under `error` of type `T`.
fn cause<T: StdError + 'static>(error: &Error) -> Option<&T> {
    std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| (*cause).source())
        .find_map(|cause| cause.downcast_ref::<T>())
}

/// AC-P4 (a): 100 calls one after another, then 100 at once, over exactly
/// one connection, all of them HTTP/2.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_then_concurrent_calls_share_one_connection() {
    let server = tls_server().await;
    let client = client_for(&server);
    let questions = questions();

    for index in 0..100 {
        client
            .system_one("hello", &questions)
            .send()
            .await
            .unwrap_or_else(|error| panic!("sequential call {index}: {error}"));
    }
    assert_eq!(
        server.accepted_connections(),
        1,
        "after 100 sequential calls: {:#?}",
        server.connections()
    );

    concurrently(&client, &questions, 100).await;
    assert_eq!(server.request_count(), 200);
    assert_eq!(
        server.accepted_connections(),
        1,
        "after 100 concurrent calls: {:#?}",
        server.connections()
    );
    assert_all_http2(&server);
    println!("AC-P4 (a): 200 calls, {} connection(s)", server.accepted_connections());
}

/// AC-P4 (b): 64 calls started together on a client that has no connection
/// yet open exactly one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cold_fan_out_opens_one_connection() {
    let server = tls_server().await;
    let client = client_for(&server);

    concurrently(&client, &questions(), 64).await;
    assert_eq!(server.request_count(), 64);
    assert_eq!(server.accepted_connections(), 1, "{:#?}", server.connections());
    assert_all_http2(&server);
    println!(
        "AC-P4 (b): 64 cold concurrent calls, {} connection(s)",
        server.accepted_connections()
    );
}

/// AC-P4 (c): after `warm_up`, 64 calls started together open no new
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_warm_up_a_fan_out_opens_no_new_connection() {
    let server = tls_server().await;
    let client = client_for(&server);

    client.warm_up().await.expect("the warm-up succeeds");
    let after_warm_up = server.accepted_connections();
    assert_eq!(after_warm_up, 1, "{:#?}", server.connections());

    concurrently(&client, &questions(), 64).await;
    assert_eq!(server.request_count(), 65);
    assert_eq!(server.accepted_connections() - after_warm_up, 0, "{:#?}", server.connections());
    assert_all_http2(&server);
    println!(
        "AC-P4 (c): 64 concurrent calls after warm_up, {} new connection(s)",
        server.accepted_connections() - after_warm_up
    );
}

/// A client whose connection the server closed while it was idle opens one
/// new connection for the next burst, not one per request that starts
/// before it is pooled: the transport sees the closed stream, and the burst
/// finds it has no connection.
#[cfg(feature = "internals")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_after_the_server_closed_the_connection_opens_one_more() {
    use typesafe_sdk::__internals::open_streams;

    let server = tls_server().await;
    let client = client_for(&server);
    client.warm_up().await.expect("the warm-up succeeds");
    assert_eq!(server.accepted_connections(), 1, "{:#?}", server.connections());
    assert_eq!(open_streams(&client), 1, "the warm-up's connection is open");

    server.close_connections();
    until_no_stream_is_open(&client, &server).await;

    concurrently(&client, &questions(), 64).await;
    assert_eq!(server.request_count(), 65);
    let records = server.connections();
    assert_eq!(records.len(), 2, "one more connection for the burst: {records:#?}");
    assert_eq!(
        records.iter().map(test_support::ConnectionRecord::requests).collect::<Vec<_>>(),
        [1, 64],
        "the warm-up on the first, the whole burst on the second: {records:#?}"
    );
    assert_all_http2(&server);
    assert_eq!(open_streams(&client), 1, "the burst's connection is open");
}

/// A burst at a port that refuses every connection ends, every call with a
/// connection error. A refusal arrives in microseconds, so some calls of the
/// burst wait for another call's connect and some open their own: a call
/// that opened fails with its own refusal, and a call that waited fails with
/// the refusal of the connect it waited for, quoted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_at_a_refusing_port_fails_every_call_and_ends() {
    let port = RefusingPort::new().await.unwrap_or_else(|error| panic!("{error}"));
    let client = one_attempt_client(port.base_url(), HttpVersion::Http2Only);

    let errors = failing_burst(&client, 32, Duration::from_secs(10)).await;

    let mut own = Vec::new();
    let mut waited = Vec::new();
    for (index, (_, error)) in errors.iter().enumerate() {
        assert!(matches!(error.kind(), ErrorKind::Connection), "call {index}: {error:?}");
        if let Some(refused) = cause::<io::Error>(error) {
            assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused, "call {index}: {error:?}");
            own.push(error.to_string());
        } else {
            waited.push((index, error.to_string()));
        }
    }
    let first = own.first().unwrap_or_else(|| panic!("no call opened: {errors:#?}"));
    let refusal = first.strip_prefix("Connection error: ").expect("a connection error's message");
    for (index, message) in &waited {
        assert_eq!(*message, format!("{WAITED}{refusal}"), "call {index}");
    }
    #[cfg(feature = "internals")]
    assert_eq!(typesafe_sdk::__internals::open_streams(&client), 0);
}

/// Two calls at a server that accepts and never answers each time out at
/// its own deadline. The first opens and holds the turn while its TLS
/// handshake waits; the second waits at the gate, and its deadline runs
/// while it waits. When the first is dropped at its deadline the second
/// opens a connection of its own, and still ends at its own deadline, not
/// the first's plus its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn calls_at_a_silent_server_time_out_at_their_own_deadlines() {
    const FIRST: Duration = Duration::from_millis(300);
    const SECOND: Duration = Duration::from_millis(1500);

    let server = SilentServer::start().await.expect("the silent server starts");
    let client = Client::builder()
        .api_key("test-key")
        .base_url(format!("https://{}", server.addr()))
        .retry(RetryPolicy::default().max_retries(0))
        .build()
        .expect("the client builds");
    let questions = questions();
    let timed = |deadline: Duration| {
        let (client, questions) = (client.clone(), questions.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            let error = client
                .system_one("hello", &questions)
                .timeout(deadline)
                .send()
                .await
                .expect_err("the server never answers");
            (started.elapsed(), error)
        })
    };

    let first = timed(FIRST);
    let connected = async {
        while server.accepted_connections() == 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), connected)
        .await
        .unwrap_or_else(|_| panic!("the first call did not connect within 5 s: {server:?}"));
    let second = timed(SECOND);

    let (first, second) = tokio::time::timeout(Duration::from_secs(10), async {
        (first.await.expect("the first task ran"), second.await.expect("the second task ran"))
    })
    .await
    .expect("both calls ended within 10 s");

    for ((elapsed, error), deadline) in [(&first, FIRST), (&second, SECOND)] {
        assert!(
            matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == deadline),
            "{deadline:?}: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            format!("Request timed out (timeout={}s).", deadline.as_secs_f64())
        );
        assert!(*elapsed >= deadline, "{deadline:?}: ended after {elapsed:?}");
    }
    // Had the second call's deadline started only when it left the gate, it
    // would have ended after both deadlines.
    assert!(
        second.0 < FIRST + SECOND,
        "the second call ended after {:?}: its deadline did not cover its wait at the gate",
        second.0
    );
    assert_eq!(
        server.accepted_connections(),
        2,
        "the second call connected once the first gave its turn back: {server:?}"
    );
}

/// A burst at a server that closes each connection 500 ms after it accepted
/// it, so that the TLS handshake fails after 500 ms, with every call of the
/// burst waiting by then. The one connect fails, and every call fails with
/// it at once: the call that opened with its own error, which is hyper-util's
/// and keeps its sources, and every other with the waited error quoting it.
/// None of them connects on its own, so the server accepts one connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_whose_one_connect_fails_slowly_fails_with_it_at_once() {
    const CALLS: usize = 16;
    const CLOSE: Duration = Duration::from_millis(500);

    let server = SilentServer::closing_after(CLOSE).await.expect("the silent server starts");
    let client = one_attempt_client(&format!("https://{}", server.addr()), HttpVersion::Http2Only);

    let errors = failing_burst(&client, CALLS, Duration::from_secs(10)).await;

    let (own, waited): (Vec<_>, Vec<_>) =
        errors.iter().partition(|(_, error)| !error.to_string().starts_with(WAITED));
    let [(_, own)] = own.as_slice() else {
        panic!("exactly one call has its own error: {errors:#?}");
    };
    assert!(matches!(own.kind(), ErrorKind::Connection), "{own:?}");
    assert!(
        cause::<hyper_util::client::legacy::Error>(own).is_some_and(|error| error.is_connect()),
        "the opener's own error is hyper-util's connect error, sources and all: {own:?}"
    );
    let failure = own.to_string();
    let failure = failure.strip_prefix("Connection error: ").expect("a connection error's message");
    assert_eq!(waited.len(), CALLS - 1, "{errors:#?}");
    for (index, (_, error)) in waited.iter().enumerate() {
        assert!(matches!(error.kind(), ErrorKind::Connection), "waiter {index}: {error:?}");
        assert_eq!(error.to_string(), format!("{WAITED}{failure}"), "waiter {index}");
        assert!(
            cause::<hyper_util::client::legacy::Error>(error).is_none(),
            "a waiter's error quotes the failure and does not carry it: {error:?}"
        );
    }
    for (elapsed, error) in &errors {
        assert!(
            (CLOSE..Duration::from_secs(2)).contains(elapsed),
            "a call ended after {elapsed:?}, not with the one connect: {error}"
        );
    }
    assert_eq!(server.accepted_connections(), 1, "one connect for the whole burst: {server:?}");
}

/// The TRACE event hyper-util 0.1.20 logs (`pool.rs`, `Checkout::checkout`)
/// when a request registers in its pool as waiting for a connection that is
/// not there yet.
#[cfg(feature = "tracing")]
const CHECKOUT_WAITING: &str = "checkout waiting for idle connection";

/// The events among those `recorder` saw that say a request registered in
/// hyper-util's pool as waiting for a connection that was not there yet.
///
/// A log line of a dependency is what is asserted because it is the one
/// observable of what the gate prevents: two requests waiting in the pool
/// before a connection exists, which is what lets hyper-util connect twice.
/// With the gate only the request that opens waits there; without it every
/// request of a cold burst does, whether or not the race then happens. The
/// tests assert the count is exactly 1, so that a renamed message fails them
/// instead of passing them.
#[cfg(feature = "tracing")]
fn checkouts_waiting(recorder: &recorder::Recorder) -> Vec<String> {
    let events = recorder.0.lock().expect("not poisoned");
    events
        .iter()
        .filter(|(level, line)| {
            *level == tracing::Level::TRACE
                && line.starts_with("hyper_util::client::legacy::pool ")
                && line.contains(CHECKOUT_WAITING)
        })
        .map(|(_, line)| line.clone())
        .collect()
}

/// A cold burst of 64 enters hyper-util's pool once: one request waits there
/// for the connection it opens, and every other finds it pooled.
///
/// On a current-thread runtime, so that every event of the burst, the
/// connection tasks' included, is on this thread, where the recorder is the
/// default subscriber; that also keeps it apart from the tests libtest runs
/// beside it.
#[cfg(feature = "tracing")]
#[tokio::test]
async fn a_cold_burst_of_64_enters_the_pool_once() {
    let server = tls_server().await;
    let client = client_for(&server);

    let recorder = recorder::Recorder::default();
    let installed = recorder::install(&recorder);
    concurrently(&client, &questions(), 64).await;
    drop(installed);

    let waiting = checkouts_waiting(&recorder);
    assert_eq!(waiting.len(), 1, "requests that waited in the pool for a connection: {waiting:#?}");
    assert_eq!(server.accepted_connections(), 1, "{:#?}", server.connections());
}

/// A burst after the server closed the connection, once the client has seen
/// the close, enters hyper-util's pool once as well.
#[cfg(all(feature = "tracing", feature = "internals"))]
#[tokio::test]
async fn a_burst_after_the_server_closed_the_connection_enters_the_pool_once() {
    let server = tls_server().await;
    let client = client_for(&server);
    client.warm_up().await.expect("the warm-up succeeds");
    server.close_connections();
    until_no_stream_is_open(&client, &server).await;

    let recorder = recorder::Recorder::default();
    let installed = recorder::install(&recorder);
    concurrently(&client, &questions(), 64).await;
    drop(installed);

    let waiting = checkouts_waiting(&recorder);
    assert_eq!(waiting.len(), 1, "requests that waited in the pool for a connection: {waiting:#?}");
    assert_eq!(server.accepted_connections(), 2, "{:#?}", server.connections());
}

/// After a call, a request goes straight to the pool; after the server
/// closed the connection and the client dropped its stream, it does not.
#[cfg(feature = "internals")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pool_is_warm_after_a_call_and_not_once_its_stream_is_gone() {
    use typesafe_sdk::__internals::pool_is_warm;

    let server = tls_server().await;
    let client = client_for(&server);
    assert!(!pool_is_warm(&client), "a new client has no connection");

    client.warm_up().await.expect("the warm-up succeeds");
    assert!(pool_is_warm(&client), "the warm-up's connection is pooled and open");

    server.close_connections();
    until_no_stream_is_open(&client, &server).await;
    assert!(!pool_is_warm(&client), "the connection is gone: the next request opens");
}

/// A client under `Auto` speaks HTTP/2 to a TLS server that offers it: the
/// ALPN protocol the server chose reaches hyper-util through the transport's
/// connector.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_under_auto_speaks_http2_to_a_tls_server_that_offers_it() {
    let server = tls_server().await;
    let client =
        builder_for(&server).http_version(HttpVersion::Auto).build().expect("the client builds");

    client
        .system_one("hello", &questions())
        .send()
        .await
        .unwrap_or_else(|error| panic!("the call over HTTP/2: {error}"));
    assert_all_http2(&server);
    let records = server.connections();
    assert_eq!(
        records.iter().map(|record| (record.tls().clone(), record.requests())).collect::<Vec<_>>(),
        [(Tls::Completed { alpn: Some(b"h2".to_vec()) }, 1)],
        "{records:#?}"
    );
}

/// Two calls under `Auto` at a TLS server that never answers both connect
/// at once: `Auto` needs a connection per concurrent HTTP/1.1 request, so
/// nothing makes the second wait for the first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_calls_under_auto_at_a_server_that_never_answers_both_connect() {
    let server = SilentServer::start().await.expect("the silent server starts");
    let client = one_attempt_client(&format!("https://{}", server.addr()), HttpVersion::Auto);
    let questions = questions();
    let calls: Vec<_> = (0..2)
        .map(|_| {
            let (client, questions) = (client.clone(), questions.clone());
            tokio::spawn(async move { client.system_one("hello", &questions).send().await })
        })
        .collect();

    // Well within the calls' own deadline (the client's default, 10 s), at
    // which a second connect that waited for the first would be freed.
    let both = async {
        while server.accepted_connections() < 2 {
            tokio::task::yield_now().await;
        }
    };
    let connected = tokio::time::timeout(Duration::from_secs(2), both).await;
    for call in &calls {
        call.abort();
    }
    connected.unwrap_or_else(|_| {
        panic!("the two calls did not both connect within 2 s: one waited: {server:?}")
    });
}
