//! What one whole System One call costs in allocations, beyond what its
//! transport costs.
//!
//! The call goes through an in-memory transport that answers the upstream
//! `RESULT` fixture, so everything measured is the SDK's own work: the body
//! encode, the per-attempt header map, request assembly, reading the response
//! and decoding it. The transport's own allocations are measured separately,
//! by calling it directly with a request built beforehand, and subtracted.
//!
//! Scenario, as the budget pins it: a 1 KB string `state`, three questions,
//! no per-call headers, the default deadline, no `tracing` subscriber, the
//! identical calls after a warm-up call. Both runs consume the response body
//! the same way - frame by frame - and the SDK additionally keeps it. Every
//! section, the itemized steps included, is measured [`support::RUNS`] times
//! and held to its stable minimum, for the reason `support` gives.
//!
//! The budget is the frozen inventory: encode 1 + the retained retry body 1 +
//! header map 2 + request assembly 0 + the queue of collected frames 1 +
//! decode 7 = 12 blocks under the default retry policy (the decode's 7 are
//! itemized in `alloc_decode.rs`). A call that cannot retry
//! (`max_retries(0)`) keeps no body for a second attempt, so it costs the same
//! without the retained body: 11 blocks.
//!
//! The budgets are asserted on calls whose futures are pinned on the stack
//! before the runtime drives them. Tokio 1.53.1 boxes a future larger than
//! `BOX_FUTURE_THRESHOLD` - 2048 bytes in a debug build, 16384 in a release
//! build (`runtime/mod.rs:627-631`) - when it is handed to
//! `Runtime::block_on` (`runtime/runtime.rs:338-345`) or `spawn`
//! (`runtime/runtime.rs:243-252`). That box is one block of the future's own
//! size, allocated by the runtime rather than the SDK, and a caller awaiting
//! the call inside a task never pays it; a pinned reference is one pointer
//! wide, so the pinned runs count the SDK's blocks only. The same calls
//! unpinned are measured and printed on every run as well, with the futures'
//! sizes, so the runtime's box shows up here the moment a future grows past
//! the threshold. The measured sizes, over this transport and the default
//! one, are stated beside the bounds that pin them, in
//! `tests/static_assertions.rs::the_future_of_every_call_stays_small`.
//!
//! One profiler exists per process, so all of it runs in a single test.

mod support;

use std::{
    convert::Infallible,
    future::{Ready, ready},
    mem::size_of_val,
    pin::pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, Uri};
use http_body_util::BodyExt as _;
use tower_service::Service;
use typesafe_sdk::{
    __internals as sdk, Body, Choice, ClientBuilder, Noul, PreparedQuestions, Questions,
    RetryPolicy, Score, response::Answers,
};

use crate::support::measure_min;

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `RESULT` of `tests/test_clients.py:42-56`, as the upstream test sends it.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// The per-call budget under the default retry policy.
const MAX_BLOCKS: u64 = 12;

/// The same call when it cannot retry, so no body is retained.
const MAX_BLOCKS_WITHOUT_RETRY: u64 = 11;

/// A transport that answers every request with the fixture, allocating the
/// same blocks whatever the request: one response header.
#[derive(Debug, Clone, Copy)]
struct InMemory;

impl Service<Request<Body>> for InMemory {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        drop(request);
        let mut response = Response::new(Body::from(Bytes::from_static(RESULT)));
        response.headers_mut().insert("x-typesafe-request-id", HeaderValue::from_static("req-1"));
        ready(Ok(response))
    }
}

/// The three questions the fixture answers.
fn questions() -> PreparedQuestions {
    Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"))
        .prepare()
        .expect("the questions prepare")
}

/// Calls the transport directly with `request` and reads the response the
/// way a caller of it would: frame by frame, keeping nothing.
async fn call_directly(request: Request<Body>) -> StatusCode {
    let response = InMemory.call(request).await.unwrap_or_else(|never| match never {});
    let (parts, mut body) = response.into_parts();
    while let Some(frame) = body.frame().await {
        drop(frame.unwrap_or_else(|never| match never {}));
    }
    parts.status
}

/// A request like the one the SDK sends, built outside the measurement.
fn prebuilt_request() -> Request<Body> {
    let mut request = Request::new(Body::from(Bytes::from_static(b"{\"state\":\"x\"}")));
    *request.method_mut() = Method::POST;
    *request.uri_mut() = Uri::from_static("http://127.0.0.1:9/v1/systemone");
    request
}

#[test]
fn a_whole_call_costs_its_own_steps_and_nothing_else() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the runtime builds");
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build_with_service(InMemory)
        .expect("the client builds");
    let questions = questions();
    let state = "x".repeat(1024);

    let call = || {
        let call = pin!(client.system_one(state.as_str(), &questions).send());
        runtime.block_on(call).expect("the call succeeds")
    };
    let untimed = || {
        let call = pin!(client.system_one(state.as_str(), &questions).no_timeout().send());
        runtime.block_on(call).expect("the call succeeds")
    };
    let once = || {
        let request = client
            .system_one(state.as_str(), &questions)
            .retry(RetryPolicy::default().max_retries(0));
        runtime.block_on(pin!(request.send())).expect("the call succeeds")
    };
    let unpinned = || {
        runtime
            .block_on(client.system_one(state.as_str(), &questions).send())
            .expect("the call succeeds")
    };
    let unpinned_once = || {
        let request = client
            .system_one(state.as_str(), &questions)
            .retry(RetryPolicy::default().max_retries(0));
        runtime.block_on(request.send()).expect("the call succeeds")
    };
    let call_size = size_of_val(&client.system_one(state.as_str(), &questions).send());
    let list_size = size_of_val(&client.models().list().send());

    // Warm-ups: the encode scratch, the promoted `Bytes` of the endpoint and
    // the header values, the timer wheel, the event callsites.
    drop(call());
    drop(untimed());
    drop(once());
    drop(unpinned());
    drop(unpinned_once());
    assert_eq!(runtime.block_on(pin!(call_directly(prebuilt_request()))), StatusCode::OK);

    let (whole, response) = measure_min("whole call", || (), |()| call());
    let (whole_untimed, untimed_response) =
        measure_min("whole call, no deadline", || (), |()| untimed());
    let (whole_once, once_response) = measure_min("whole call, no retry", || (), |()| once());
    let (whole_unpinned, unpinned_response) =
        measure_min("whole call, unpinned", || (), |()| unpinned());
    let (whole_unpinned_once, unpinned_once_response) =
        measure_min("whole call, unpinned, no retry", || (), |()| unpinned_once());
    let (direct, status) = measure_min("transport called directly", prebuilt_request, |request| {
        runtime.block_on(pin!(call_directly(request)))
    });
    assert_eq!(status, StatusCode::OK);
    assert_eq!(response.answers().len(), 3);
    assert_eq!(untimed_response.answers(), response.answers());
    assert_eq!(once_response.answers(), response.answers());
    assert_eq!(unpinned_response.answers(), response.answers());
    assert_eq!(unpinned_once_response.answers(), response.answers());
    drop((response, untimed_response, once_response, unpinned_response, unpinned_once_response));

    // The steps on their own, through the same code the call runs.
    let (encode, body) = measure_min(
        "encode",
        || (),
        |()| {
            sdk::encode_body(|buffer| {
                buffer.extend_from_slice(br#"{"state":"#);
                sdk::encode_into(buffer, state.as_str())?;
                buffer.extend_from_slice(br#","model":"jev-latest","questions":"#);
                buffer.extend_from_slice(b"{}");
                buffer.push(b'}');
                Ok(())
            })
            .expect("the body encodes")
        },
    );
    drop(body);
    let mut headers = HeaderMap::with_capacity(6);
    for name in ["authorization", "accept", "user-agent", "x-typesafe-sdk", "x-typesafe-runtime"] {
        headers.insert(name, HeaderValue::from_static("value"));
    }
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    // The first clone moves the names' bytes into shared storage, once; the
    // client's own map went through that during the warm-up call.
    drop(headers.clone());
    let (header_clone, cloned) = measure_min("header map clone", || (), |()| headers.clone());
    drop(cloned);
    let fixture = Bytes::from_static(RESULT);
    drop(sdk::decode_system_one::<Answers>(fixture.clone(), StatusCode::OK, HeaderMap::new(), 3));
    let (decode, decoded) = measure_min(
        "decode",
        || fixture.clone(),
        |fixture| sdk::decode_system_one::<Answers>(fixture, StatusCode::OK, HeaderMap::new(), 3),
    );
    drop(decoded);

    let sdk_blocks = whole.blocks - direct.blocks;
    let sdk_untimed = whole_untimed.blocks - direct.blocks;
    let sdk_once = whole_once.blocks - direct.blocks;
    println!("whole call               blocks={:>3} bytes={:>6}", whole.blocks, whole.bytes);
    println!(
        "whole call, no deadline  blocks={:>3} bytes={:>6}",
        whole_untimed.blocks, whole_untimed.bytes
    );
    println!(
        "whole call, no retry     blocks={:>3} bytes={:>6}",
        whole_once.blocks, whole_once.bytes
    );
    println!("transport called directly blocks={:>3} bytes={:>6}", direct.blocks, direct.bytes);
    println!(
        "SDK's own                blocks={sdk_blocks:>3} (budget {MAX_BLOCKS}); without a deadline {sdk_untimed}"
    );
    println!("SDK's own, no retry      blocks={sdk_once:>3} (budget {MAX_BLOCKS_WITHOUT_RETRY})");
    println!(
        "  futures                system_one send {call_size} bytes, models send {list_size} bytes"
    );
    println!(
        "unpinned, runtime's box  blocks={:>3} (default policy), {:>3} (no retry); not asserted",
        whole_unpinned.blocks - direct.blocks,
        whole_unpinned_once.blocks - direct.blocks
    );
    println!("  encode                 blocks={:>3} bytes={:>6}", encode.blocks, encode.bytes);
    println!(
        "  header map clone       blocks={:>3} bytes={:>6}",
        header_clone.blocks, header_clone.bytes
    );
    println!("  decode                 blocks={:>3} bytes={:>6}", decode.blocks, decode.bytes);
    println!(
        "  retained retry body    blocks={:>3} bytes={:>6}",
        i128::from(whole.blocks) - i128::from(whole_once.blocks),
        i128::from(whole.bytes) - i128::from(whole_once.bytes)
    );
    let itemized = i128::from(encode.blocks + header_clone.blocks + decode.blocks);
    println!(
        "  the rest, no retry     blocks={:>3} (request assembly, frame queue, deadline)",
        i128::from(sdk_once) - itemized
    );

    assert!(
        sdk_blocks <= MAX_BLOCKS,
        "a call costs {sdk_blocks} blocks of its own, over the budget of {MAX_BLOCKS}"
    );
    assert!(
        sdk_once <= MAX_BLOCKS_WITHOUT_RETRY,
        "a call that cannot retry costs {sdk_once} blocks of its own, over the budget of \
         {MAX_BLOCKS_WITHOUT_RETRY}"
    );
}
