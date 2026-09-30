//! B6: calls over a real HTTP/2 + TLS connection on loopback, one at a time
//! and 64 at once.
//!
//! The server is the `TestServer` of `crates/test-support`: hyper serving h2
//! over rustls on `127.0.0.1`, trusted through `add_root_certificate`, on a
//! runtime of its own threads. The client is warmed up first, so every call
//! rides the one pooled connection and no handshake is timed.
//!
//! This target is kept out of the CodSpeed instrumented run on purpose. Its
//! numbers are dominated by the kernel's loopback path, two runtimes' worth
//! of task scheduling and TLS record processing, all of which vary with the
//! machine and none of which is the SDK's code; an instruction count of it
//! would move whenever the scheduler did. It is a wall-clock bench, read on
//! one machine at a time.
//!
//! How this can mislead: loopback has no latency, so this is an upper bound
//! on the throughput one connection gives, not a prediction for a network.
//! The server also records every request it serves (a `Mutex` and a `Vec`
//! push per request), so the sample counts are kept small to bound that
//! growth; its cost is the same for every call.
//!
//! Run with `cargo bench --all-features --bench loopback`.

extern crate codspeed_divan_compat as divan;

#[path = "support/mod.rs"]
mod support;

use std::sync::Arc;

use bytes::Bytes;
use divan::{Bencher, black_box};
use http::{Response, header::CONTENT_TYPE};
use http_body_util::Full;
use test_support::{Protocol, TestServer};
use tokio::{runtime::Runtime, task::JoinSet};
use typesafe_sdk::{Client, PreparedQuestions};

use crate::support::{RESULT, questions, text};

/// `GET /v1/models`, for `warm_up()`.
const MODELS: &[u8] = include_bytes!("../tests/fixtures/models.json");

/// How many calls the concurrent bench keeps in flight.
const FAN_OUT: usize = 64;

/// A server, a warmed-up client trusting it, and the runtime both run on.
struct Loopback {
    runtime: Runtime,
    client: Client,
    questions: Arc<PreparedQuestions>,
    state: Arc<str>,
    // Held so that the listener stays open for the bench's duration.
    _server: TestServer,
}

impl Loopback {
    fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("the runtime builds");
        let server = runtime
            .block_on(TestServer::start(Protocol::Http2Tls, |request| async move {
                let body = if request.uri.path().ends_with("/v1/models") { MODELS } else { RESULT };
                Response::builder()
                    .header(CONTENT_TYPE, "application/json")
                    .body(Full::new(Bytes::from_static(body)))
                    .expect("the response builds")
            }))
            .expect("the server starts");
        let certificate = server.certificate_der().expect("a TLS server has a certificate");
        let client = Client::builder()
            .api_key("bench-key")
            .base_url(server.base_url())
            .default_model("jev-latest")
            .add_root_certificate(certificate.to_vec())
            .build()
            .expect("the client builds");
        runtime.block_on(client.warm_up()).expect("the client warms up");
        Self {
            runtime,
            client,
            questions: Arc::new(questions()),
            state: Arc::from(text(1 << 10)),
            _server: server,
        }
    }
}

fn main() {
    divan::main();
}

#[divan::bench(sample_count = 50, sample_size = 20)]
fn sequential(bencher: Bencher<'_, '_>) {
    let loopback = Loopback::start();
    let call = || {
        loopback
            .runtime
            .block_on(
                loopback.client.system_one(black_box(&*loopback.state), &loopback.questions).send(),
            )
            .expect("the call succeeds")
    };
    assert_eq!(call().answers().len(), 3);
    bencher.bench_local(call);
}

/// 64 calls spawned at once on the runtime and all awaited: one sample is
/// the time until the last of them has its answer.
#[divan::bench(sample_count = 50, sample_size = 2)]
fn concurrent_64(bencher: Bencher<'_, '_>) {
    let loopback = Loopback::start();
    let fan_out = || {
        loopback.runtime.block_on(async {
            let mut calls = JoinSet::new();
            for _ in 0..FAN_OUT {
                let client = loopback.client.clone();
                let questions = Arc::clone(&loopback.questions);
                let state = Arc::clone(&loopback.state);
                calls.spawn(async move {
                    client
                        .system_one(&*state, &questions)
                        .send()
                        .await
                        .map(|response| response.answers().len())
                });
            }
            let mut answered = 0;
            while let Some(done) = calls.join_next().await {
                answered += done.expect("the task completes").expect("the call succeeds");
            }
            answered
        })
    };
    assert_eq!(fan_out(), FAN_OUT * 3);
    bencher.bench_local(fan_out);
}
