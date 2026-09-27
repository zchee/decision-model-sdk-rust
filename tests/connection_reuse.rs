//! How many connections a client opens: one, however many requests it sends
//! and however many of them start together, and one more when the server
//! has closed it.
//!
//! Every server here is HTTP/2 over TLS with a certificate the client trusts
//! through `add_root_certificate`, and its URL is an IP literal, so no second
//! socket is raced across resolved addresses and the count the server keeps
//! is the client's pool alone. The client uses its default HTTP version,
//! which for an `https` base URL is HTTP/2 only. A failed count prints the
//! server's record of every connection: who connected, how its TLS
//! handshake ended, and how many requests it carried.

use std::{
    error::Error as StdError,
    io,
    time::{Duration, Instant},
};

use bytes::Bytes;
use http::{Response, StatusCode, Version};
use http_body_util::Full;
use test_support::{Protocol, RefusingPort, SilentServer, TestServer};
use typesafe_sdk::{
    Client, Error, ErrorKind, HttpVersion, Noul, PreparedQuestions, Questions, RetryPolicy,
};

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

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

fn client_for(server: &TestServer) -> Client {
    let certificate = server.certificate_der().expect("a TLS server has a certificate");
    Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .add_root_certificate(certificate.to_vec())
        .build()
        .expect("the client builds")
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
    let closed = async {
        while open_streams(&client) > 0 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), closed).await.unwrap_or_else(|_| {
        panic!(
            "the client still holds {} open stream(s) 5 s after the server closed its \
             connection: {:#?}",
            open_streams(&client),
            server.connections()
        )
    });

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
/// connection error whose cause is the refusal: the request that opens
/// fails, and each failure passes the turn to the next request.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_at_a_refusing_port_fails_every_call_and_ends() {
    const CALLS: usize = 32;

    let port = RefusingPort::new().await.unwrap_or_else(|error| panic!("{error}"));
    let client = Client::builder()
        .api_key("test-key")
        .base_url(port.base_url())
        .http_version(HttpVersion::Http2Only)
        .retry(RetryPolicy::default().max_retries(0))
        .build()
        .expect("the client builds");
    let questions = questions();

    let burst = async {
        let tasks: Vec<_> = (0..CALLS)
            .map(|_| {
                let (client, questions) = (client.clone(), questions.clone());
                tokio::spawn(async move { client.system_one("hello", &questions).send().await })
            })
            .collect();
        let mut errors = Vec::with_capacity(CALLS);
        for task in tasks {
            errors.push(task.await.expect("the task ran").expect_err("nothing answers"));
        }
        errors
    };
    let errors = tokio::time::timeout(Duration::from_secs(10), burst)
        .await
        .expect("every call of the burst ended within 10 s");

    for (index, error) in errors.iter().enumerate() {
        assert!(matches!(error.kind(), ErrorKind::Connection), "call {index}: {error:?}");
        let refused = cause::<io::Error>(error)
            .unwrap_or_else(|| panic!("call {index}: an I/O error under {error:?}"));
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused, "call {index}: {error:?}");
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
