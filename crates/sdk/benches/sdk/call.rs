//! B5: one whole System One call through an in-memory transport, against
//! its floor and against a naive client on the same transport.
//!
//! - `sdk`: `client.system_one(state, &questions).send()` with the default
//!   deadline and retry policy - encode, retained retry body, header map,
//!   request, readiness, the call, reading the body, decoding it.
//! - `floor`: the transport called directly with a request built beforehand,
//!   its response read frame by frame and dropped. No client can cost less.
//! - `naive`: comparator (A), `benches/support/naive.rs`.
//!
//! The state is 1 KB of text and the response the three-answer fixture, the
//! scenario the allocation budget of a call is stated on. `sdk_20` and
//! `naive_20` ask twenty questions and read the twenty-answer document of
//! `decode`, whose scores have five levels.
//!
//! How this can mislead: the transport answers at once, so everything the
//! network costs - which in production is four orders of magnitude more than
//! all of this - is absent by design; B6 has a loopback socket. Every future
//! is pinned on the stack before the runtime drives it, as a caller that
//! awaits inside a task has it; handing a large future to `block_on` by value
//! would add the runtime's own box in a debug build, which is not the SDK's
//! cost.

use std::pin::pin;

use bytes::Bytes;
use divan::{Bencher, black_box};
use http::{Method, Request, StatusCode, Uri};
use http_body_util::BodyExt as _;
use tower_service::Service as _;
use typesafe_sdk::{Body, Choice, Noul, PreparedQuestions, Questions, Score};

use crate::{
    MODEL, QUESTIONS_JSON,
    decode::TWENTY,
    naive::NaiveClient,
    service::{InMemory, client, runtime},
    support::{RESULT, questions, text},
};

#[divan::bench]
fn sdk(bencher: Bencher<'_, '_>) {
    let runtime = runtime();
    let client = client(InMemory::ok(RESULT));
    let questions = questions();
    let state = text(1 << 10);
    let call = || {
        let call = pin!(client.system_one(black_box(state.as_str()), &questions).send());
        runtime.block_on(call).expect("the call succeeds")
    };
    assert_eq!(call().answers().len(), 3);
    bencher.bench_local(call);
}

/// The twenty questions `TWENTY` answers: a noul, a choice of four options
/// and a score of five levels, in turn.
fn twenty_questions() -> PreparedQuestions {
    let mut questions = Questions::new();
    for index in 0..20 {
        let name = format!("q{index}");
        questions = match index % 3 {
            0 => questions.noul(name, Noul::new().instructions("Is this about billing?")),
            1 => questions.choice(
                name,
                Choice::new(["billing", "shipping", "account", "other"]).instructions("Topic?"),
            ),
            _ => questions.score(
                name,
                Score::new(["none", "low", "medium", "high", "critical"]).instructions("Urgency?"),
            ),
        };
    }
    questions.prepare().expect("the questions prepare")
}

/// [`twenty_questions`] in the shape the API takes them, for the naive
/// client.
fn twenty_questions_json() -> String {
    let questions = (0..20)
        .map(|index| {
            let question = match index % 3 {
                0 => r#"{"type":"noul","instructions":"Is this about billing?"}"#,
                1 => {
                    r#"{"type":"choice","instructions":"Topic?","criteria":{"billing":null,"shipping":null,"account":null,"other":null}}"#
                }
                _ => {
                    r#"{"type":"score","instructions":"Urgency?","criteria":["none","low","medium","high","critical"]}"#
                }
            };
            format!(r#""q{index}":{question}"#)
        })
        .collect::<Vec<_>>();
    format!("{{{}}}", questions.join(","))
}

#[divan::bench]
fn sdk_20(bencher: Bencher<'_, '_>) {
    let runtime = runtime();
    let client = client(InMemory::ok(TWENTY.as_slice()));
    let questions = twenty_questions();
    let state = text(1 << 10);
    let call = || {
        let call = pin!(client.system_one(black_box(state.as_str()), &questions).send());
        runtime.block_on(call).expect("the call succeeds")
    };
    assert_eq!(call().answers().len(), 20);
    bencher.bench_local(call);
}

#[divan::bench]
fn naive_20(bencher: Bencher<'_, '_>) {
    let runtime = runtime();
    let client = NaiveClient::new(
        "http://127.0.0.1:9",
        "bench-key",
        MODEL,
        twenty_questions_json().as_bytes(),
    );
    let state = text(1 << 10);
    let call = || {
        let mut service = InMemory::ok(TWENTY.as_slice());
        runtime
            .block_on(pin!(client.call(&mut service, black_box(state.as_str()))))
            .expect("the call succeeds")
    };
    assert_eq!(call().answers.len(), 20);
    bencher.bench_local(call);
}

#[divan::bench]
fn floor(bencher: Bencher<'_, '_>) {
    let runtime = runtime();
    let request = || {
        let mut request = Request::new(Body::from(Bytes::from_static(b"{\"state\":\"x\"}")));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = Uri::from_static("http://127.0.0.1:9/v1/systemone");
        request
    };
    let call = |request: Request<Body>| {
        runtime.block_on(pin!(async {
            let response =
                InMemory::ok(RESULT).call(request).await.unwrap_or_else(|never| match never {});
            let (parts, mut body) = response.into_parts();
            while let Some(frame) = body.frame().await {
                drop(frame.unwrap_or_else(|never| match never {}));
            }
            parts.status
        }))
    };
    assert_eq!(call(request()), StatusCode::OK);
    bencher.with_inputs(request).bench_local_values(call);
}

#[divan::bench]
fn naive(bencher: Bencher<'_, '_>) {
    let runtime = runtime();
    let client = NaiveClient::new("http://127.0.0.1:9", "bench-key", MODEL, QUESTIONS_JSON);
    let state = text(1 << 10);
    let call = || {
        let mut service = InMemory::ok(RESULT);
        runtime
            .block_on(pin!(client.call(&mut service, black_box(state.as_str()))))
            .expect("the call succeeds")
    };
    assert_eq!(call().answers.len(), 3);
    bencher.bench_local(call);
}
