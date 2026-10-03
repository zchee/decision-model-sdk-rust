//! The client against a real loopback server: ports of the upstream
//! `tests/test_clients.py`, case by case, over HTTP/1.1, h2c and HTTP/2 over
//! TLS where the protocol can matter.
//!
//! Every upstream test is named in the doc comment of the Rust test that
//! ports it. A case the README's deviation table lists is tested for the Rust
//! behaviour, and the comment names the row.

use std::{
    error::Error as StdError,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use decision_model_sdk::{
    ApiError, ApiErrorKind, Body, Choice, Client, ClientBuilder, Content, Error, ErrorKind, Noul,
    PreparedQuestions, Questions, RawQuestion, RetryPolicy, Score,
};
use http::{Request, Response, StatusCode, Uri, header::HOST};
use hyper::body::Incoming;
use hyper_util::{
    client::legacy::{self, connect::HttpConnector},
    rt::TokioExecutor,
};
use serde_json::json;
use test_support::{
    Protocol, RecordedRequest, RefusingPort, TestServer, json_response, raw_server,
};
use tokio::sync::Notify;
use tower_service::Service;

#[cfg(feature = "tracing")]
#[path = "support/recorder.rs"]
mod recorder;
#[path = "../src/rendering_tests.rs"]
mod rendering_tests;

use rendering_tests::assert_printable;

// ------------------------------------------------------------- fixtures

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

#[path = "support/answering.rs"]
mod answering;
#[path = "support/loopback_builder.rs"]
mod loopback_builder;

use answering::answering;
use loopback_builder::loopback_builder;

/// A builder for a client of `server`, with a default model.
fn builder_for(server: &TestServer, protocol: Protocol) -> ClientBuilder {
    loopback_builder(server, protocol)
        .default_model("jev-latest")
        // One attempt per call, as upstream's `clients` fixture builds them
        // (`tests/conftest.py:34-35`); retries are tested in `tests/retry.rs`.
        .retry(RetryPolicy::default().max_retries(0))
}

fn client_for(server: &TestServer, protocol: Protocol) -> Client {
    builder_for(server, protocol).build().expect("the client builds")
}

/// One question, raw, as most upstream cases ask.
fn one_raw_question() -> PreparedQuestions {
    Questions::new()
        .raw("q", RawQuestion::new("noul").field("instructions", "?"))
        .prepare()
        .expect("the question prepares")
}

fn body_text(request: &RecordedRequest) -> &str {
    std::str::from_utf8(&request.body).expect("the SDK sends UTF-8")
}

/// The API error `error` must be.
fn api_error(error: &Error) -> &ApiError {
    match error.kind() {
        ErrorKind::Api(api) => api,
        other => panic!("expected an API error, got {other:?}: {error}"),
    }
}

// --------------------------------------------------------- round trip

/// Upstream `test_round_trip[dataclass|raw|mixed]`: the body, the endpoint,
/// the content type, and every answer of the fixture - over each protocol.
/// Also upstream `test_response_carries_request_id`: the response's
/// `x-typesafe-request-id` is the call's request id.
#[tokio::test]
async fn round_trip_sends_the_body_and_decodes_every_answer_kind() {
    let raw = Questions::new()
        .raw("spam", RawQuestion::new("noul").field("instructions", "Spam?"))
        .raw(
            "tone",
            RawQuestion::new("choice")
                .field("instructions", "Tone?")
                .field("criteria", json!({"friendly": null, "hostile": null})),
        )
        .raw(
            "quality",
            RawQuestion::new("score")
                .field("instructions", "Quality?")
                .field("criteria", ["bad", "ok", "great"]),
        );
    let typed = Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"));
    let mixed = Questions::new()
        .raw("spam", RawQuestion::new("noul").field("instructions", "Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"));
    let forms = [
        ("dataclass", typed.prepare().expect("prepares")),
        ("raw", raw.prepare().expect("prepares")),
        ("mixed", mixed.prepare().expect("prepares")),
    ];
    let expected_body = concat!(
        r#"{"state":{"document":"Hello "#,
        "\u{1f30d}",
        r#""},"model":"jev-latest","questions":{"spam":{"type":"noul","instructions":"Spam?"},"#,
        r#""tone":{"type":"choice","instructions":"Tone?","criteria":{"friendly":null,"hostile":null}},"#,
        r#""quality":{"type":"score","instructions":"Quality?","criteria":["bad","ok","great"]}}}"#,
    );
    let state = json!({"document": "Hello \u{1f30d}"});

    for protocol in Protocol::ALL {
        for (form, questions) in &forms {
            let server = TestServer::start(protocol, |_| async {
                let mut response = json_response(StatusCode::OK, RESULT);
                response
                    .headers_mut()
                    .insert("x-typesafe-request-id", "req-42".parse().expect("valid"));
                response
            })
            .await
            .expect("the test server starts");
            let result = client_for(&server, protocol)
                .system_one(&state, questions)
                .send()
                .await
                .unwrap_or_else(|error| panic!("{protocol:?} {form}: {error}"));

            let [request] = &server.requests()[..] else {
                panic!("{protocol:?} {form}: exactly one request expected")
            };
            assert_eq!(request.method, "POST");
            assert_eq!(request.uri.path(), "/v1/systemone", "{protocol:?} {form}");
            assert_eq!(body_text(request), expected_body, "{protocol:?} {form}");
            assert_eq!(request.header_values("content-type"), ["application/json"]);

            assert_eq!(result.model(), "jev-latest");
            assert_eq!(result.usage().input_tokens(), Some(12));
            assert_eq!(result.usage().output_tokens(), Some(3));
            let answers = result.answers();
            assert_eq!(answers.names().collect::<Vec<_>>(), ["spam", "tone", "quality"]);
            assert_eq!(answers.noul("spam").map(|answer| answer.noul()), Some(0.98));
            let tone = answers.choice("tone").expect("a choice answer");
            assert_eq!(tone.choice(), "friendly");
            assert_eq!(tone.confidence(), 0.9);
            assert_eq!(
                tone.probabilities().collect::<Vec<_>>(),
                [("friendly", 0.9), ("hostile", 0.1)]
            );
            let quality = answers.score("quality").expect("a score answer");
            assert_eq!(quality.score(), 1.7);
            assert_eq!(quality.confidence(), 0.8);
            let legend: Vec<_> =
                quality.legend().map(|(level, text)| (level, text.as_text())).collect();
            assert_eq!(legend, [(0, Some("bad")), (1, Some("ok")), (2, Some("great"))]);
            assert_eq!(quality.probabilities().collect::<Vec<_>>(), [(0, 0.1), (1, 0.1), (2, 0.8)]);
            assert_eq!(result.meta().status(), StatusCode::OK);
            assert_eq!(result.meta().request_id(), Some("req-42"), "{protocol:?} {form}");
            assert_eq!(&result.meta().raw_body()[..], RESULT);
        }
    }
}

/// Upstream `test_extra_body_shallow_override`: last write wins, `model` is
/// replaced where it stands, `null` is sent. Member order is the wire order.
#[tokio::test]
async fn extra_body_shallow_override() {
    let server = answering(Protocol::Http1, StatusCode::OK, RESULT).await;
    let questions = one_raw_question();
    client_for(&server, Protocol::Http1)
        .system_one("hi", &questions)
        .model("call-model")
        .extra_body("model", "override-model")
        .extra_body("beam_width", &4)
        .extra_body("nullable", &None::<u8>)
        .send()
        .await
        .expect("the call succeeds");

    assert_eq!(
        body_text(&server.requests()[0]),
        r#"{"state":"hi","model":"override-model","questions":{"q":{"type":"noul","instructions":"?"}},"beam_width":4,"nullable":null}"#
    );
}

/// A value whose `Serialize` fails, as upstream's `object()` does.
struct Unserializable;

impl serde::Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("an object() the encoder cannot write"))
    }
}

/// Upstream `test_unserializable_request_body_raises`.
#[tokio::test]
async fn unserializable_request_body_raises_before_the_network() {
    let server = answering(Protocol::Http1, StatusCode::OK, RESULT).await;
    let questions = one_raw_question();
    let error = client_for(&server, Protocol::Http1)
        .system_one("x", &questions)
        .extra_body("bad", &Unserializable)
        .send()
        .await
        .expect_err("the body cannot be encoded");

    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    let prefix = r#"The request body could not be encoded as JSON: the extra member "bad": "#;
    let rendered = error.to_string();
    let reason = rendered.strip_prefix(prefix).unwrap_or_else(|| panic!("{rendered}"));
    assert!(reason.contains("an object() the encoder cannot write"), "{rendered}");
    assert_eq!(server.request_count(), 0, "an unencodable body reached the network");
}

/// Upstream `test_raw_question_passthrough`: fields the SDK does not model
/// travel as written.
#[tokio::test]
async fn raw_question_passthrough() {
    let server = answering(Protocol::Http1, StatusCode::OK, RESULT).await;
    let questions = Questions::new()
        .raw(
            "q",
            RawQuestion::new("noul")
                .field("instructions", "Spam?")
                .field("weight", 3)
                .field("nested", json!({"k": null})),
        )
        .raw(
            "choice",
            RawQuestion::new("choice").field("criteria", json!({"a": null})).field("weight", 2),
        )
        .raw("score", RawQuestion::new("score").field("criteria", ["good"]).field("weight", 1))
        .prepare()
        .expect("the questions prepare");
    client_for(&server, Protocol::Http1)
        .system_one("hi", &questions)
        .send()
        .await
        .expect("the call succeeds");

    assert_eq!(
        body_text(&server.requests()[0]),
        concat!(
            r#"{"state":"hi","model":"jev-latest","questions":{"#,
            r#""q":{"type":"noul","instructions":"Spam?","weight":3,"nested":{"k":null}},"#,
            r#""choice":{"type":"choice","criteria":{"a":null},"weight":2},"#,
            r#""score":{"type":"score","criteria":["good"],"weight":1}}}"#,
        )
    );
}

/// Upstream `test_question_schema_validation_is_left_to_api`: a raw question
/// the API will refuse is sent as written, and the 422 comes back mapped.
#[tokio::test]
async fn question_schema_validation_is_left_to_api() {
    let server = TestServer::start(Protocol::Http1, |_| async {
        json_response(StatusCode::UNPROCESSABLE_ENTITY, r#"{"detail":"Invalid question"}"#)
    })
    .await
    .expect("the test server starts");
    let rows = [
        (RawQuestion::new("noul").field("instructions", 1), r#"{"type":"noul","instructions":1}"#),
        (
            RawQuestion::new("choice").field("criteria", ["invalid", "shape"]),
            r#"{"type":"choice","criteria":["invalid","shape"]}"#,
        ),
    ];
    for (index, (question, sent)) in rows.into_iter().enumerate() {
        let questions = Questions::new().raw("q", question).prepare().expect("it prepares");
        let error = client_for(&server, Protocol::Http1)
            .system_one("x", &questions)
            .send()
            .await
            .expect_err("the server refuses it");

        assert_eq!(api_error(&error).kind(), ApiErrorKind::UnprocessableEntity);
        assert_eq!(
            error.to_string(),
            format!("POST {}/v1/systemone: 422 Invalid question", server.base_url())
        );
        let body = body_text(&server.requests()[index]).to_owned();
        assert!(body.ends_with(&format!(r#""questions":{{"q":{sent}}}}}"#)), "{body}");
    }
}

/// Upstream `test_rich_descriptions`: structured content in the questions and
/// in the answer's legend.
#[tokio::test]
async fn rich_descriptions() {
    const ANSWER: &[u8] = include_bytes!("fixtures/structured-legend.json");
    let server = answering(Protocol::Http1, StatusCode::OK, ANSWER).await;
    let criteria = json!({"summary": "duplicated", "examples": ["charged twice"]});
    let structured = || Content::json(&criteria).expect("an object is content");
    let questions = Questions::new()
        .raw(
            "duplicate",
            RawQuestion::new("noul")
                .field("instructions", json!({"question": "Duplicate?"}))
                .field("criteria", json!({"true": criteria})),
        )
        .choice(
            "team",
            Choice::new(["billing", "other"]).option("billing", structured()).instructions("Team?"),
        )
        .score("risk", Score::new([structured()]).instructions("Risk?"))
        .prepare()
        .expect("the questions prepare");
    let result = client_for(&server, Protocol::Http1)
        .system_one("a ticket", &questions)
        .model("custom")
        .send()
        .await
        .expect("the call succeeds");

    let legend = result.answers().score("risk").and_then(|risk| risk.description(0));
    let decoded: serde_json::Value =
        legend.and_then(Content::as_json).expect("a structured legend").decode().expect("JSON");
    assert_eq!(decoded, criteria);
    // `serde_json::json!` sorts keys, so the object is written in that order.
    assert_eq!(
        body_text(&server.requests()[0]),
        concat!(
            r#"{"state":"a ticket","model":"custom","questions":{"#,
            r#""duplicate":{"type":"noul","instructions":{"question":"Duplicate?"},"criteria":{"true":{"examples":["charged twice"],"summary":"duplicated"}}},"#,
            r#""team":{"type":"choice","instructions":"Team?","criteria":{"billing":{"examples":["charged twice"],"summary":"duplicated"},"other":null}},"#,
            r#""risk":{"type":"score","instructions":"Risk?","criteria":[{"examples":["charged twice"],"summary":"duplicated"}]}}}"#,
        )
    );
}

/// Upstream `test_validation_before_network`: questions that cannot be sent
/// never become a request, because they cannot become a `PreparedQuestions`.
#[test]
fn validation_before_network() {
    let empty = Questions::new().prepare().expect_err("no questions");
    assert!(matches!(empty.kind(), ErrorKind::InvalidRequest));
    assert_eq!(empty.to_string(), "At least one question is required.");

    let no_levels = Questions::new()
        .score("rating", Score::new(Vec::<&str>::new()).instructions("?"))
        .prepare()
        .expect_err("a score with no levels");
    assert!(matches!(no_levels.kind(), ErrorKind::InvalidRequest));
    assert!(no_levels.to_string().contains(r#""rating" has no criteria"#), "{no_levels}");
}

// ---------------------------------------------------------- API errors

/// Upstream `test_error_mapping`: every status to its class, with body,
/// request id, headers, message and `Retry-After`, over each protocol.
#[tokio::test]
async fn error_mapping() {
    let rows = [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::PermissionDenied),
        (404, ApiErrorKind::NotFound),
        (422, ApiErrorKind::UnprocessableEntity),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::InternalServer),
        (503, ApiErrorKind::InternalServer),
        (408, ApiErrorKind::Other),
        (409, ApiErrorKind::Other),
        (302, ApiErrorKind::Other),
    ];
    let body = r#"{"detail":{"message":"Server explanation"}}"#;
    for protocol in Protocol::ALL {
        for (status, kind) in rows {
            let server = TestServer::start(protocol, move |_| async move {
                let mut response =
                    json_response(StatusCode::from_u16(status).expect("valid"), body);
                let headers = response.headers_mut();
                headers.insert("x-typesafe-request-id", "req_123".parse().expect("valid"));
                headers.insert("retry-after-ms", "125".parse().expect("valid"));
                response
            })
            .await
            .expect("the test server starts");

            let error =
                client_for(&server, protocol).models().list().send().await.expect_err("refused");
            let api = api_error(&error);
            assert_eq!(api.kind(), kind, "{protocol:?} {status}");
            assert_eq!(api.status().as_u16(), status);
            assert_eq!(api.body(), body.as_bytes());
            assert_eq!(api.request_id(), Some("req_123"));
            assert_eq!(api.headers()["retry-after-ms"], "125");
            assert_eq!(api.retry_after(), Some(Duration::from_millis(125)));
            assert_eq!(
                error.to_string(),
                format!(
                    "GET {}/v1/models: {status} Server explanation (request_id=req_123)",
                    server.base_url()
                )
            );
            assert_eq!(server.request_count(), 1, "a failure status is not retried here");
        }
    }
}

/// Upstream `test_error_messages`: where the message is read from.
#[tokio::test]
async fn error_messages() {
    let rows: [(&str, &str); 8] = [
        (r#"{"error":"error","message":"message","detail":"detail"}"#, "error"),
        (r#"{"error":{"message":"nested error"},"message":"message"}"#, "nested error"),
        (r#"{"message":"message","detail":"detail"}"#, "message"),
        (r#"{"detail":"detail"}"#, "detail"),
        (r#"{"detail":{"message":"nested detail"}}"#, "nested detail"),
        (
            r#"{"detail":[{"loc":["body","questions","q","score","criteria",0],"msg":"Invalid"},{"msg":"Missing"},{}]}"#,
            "questions.q.score.criteria.0: Invalid; Missing",
        ),
        ("plain text", "plain text"),
        (r#"{"unexpected": true}"#, r#"{"unexpected":true}"#),
    ];
    for (body, message) in rows {
        let server = TestServer::start(Protocol::Http1, move |_| async move {
            json_response(StatusCode::BAD_REQUEST, body)
        })
        .await
        .expect("the test server starts");
        let error =
            client_for(&server, Protocol::Http1).models().list().send().await.expect_err("refused");
        assert_eq!(api_error(&error).kind(), ApiErrorKind::BadRequest);
        assert_eq!(
            error.to_string(),
            format!("GET {}/v1/models: 400 {message}", server.base_url()),
            "body {body}"
        );
    }
}

/// README deviation row "Server messages are used verbatim and uncut": what a
/// server puts in an error body or its request id reaches `Display` and
/// `Debug` escaped and cut, over each protocol; the body and the header stay
/// whole behind their accessors.
#[tokio::test]
async fn server_text_in_an_api_error_is_escaped_and_cut() {
    let hostile = serde_json::to_string("a\nb\u{1b}[31mRED\u{202e}X\u{0}").expect("serializes");
    let long_id = "r".repeat(6000);
    let huge = "y".repeat(1 << 20);
    let rows: [(String, &str, String); 4] = [
        (
            format!(r#"{{"error":{hostile}}}"#),
            "req\tlog",
            r"a\nb\u{1b}[31mRED\u{202e}X\u{0} (request_id=req\tlog)".to_owned(),
        ),
        (
            format!(r#"{{"message":"{huge}"}}"#),
            &long_id,
            format!("{}\u{2026} (request_id={}\u{2026})", "y".repeat(200), "r".repeat(128)),
        ),
        (
            "oops\u{1b}[2J\nline2".to_owned(),
            "req-text",
            r"oops\u{1b}[2J\nline2 (request_id=req-text)".to_owned(),
        ),
        (
            format!(
                r#"{{"detail":[{{"loc":["body",{}],"msg":{}}}]}}"#,
                serde_json::to_string("a\nb").expect("serializes"),
                serde_json::to_string("m\u{1b}n").expect("serializes")
            ),
            "req-list",
            r"a\nb: m\u{1b}n (request_id=req-list)".to_owned(),
        ),
    ];
    for protocol in Protocol::ALL {
        for (body, id, shown) in &rows {
            let (answer, header) = (Bytes::from(body.clone()), id.to_string());
            let server = TestServer::start(protocol, move |_| {
                let (answer, header) = (answer.clone(), header.clone());
                async move {
                    let mut response = json_response(StatusCode::INTERNAL_SERVER_ERROR, answer);
                    response
                        .headers_mut()
                        .insert("x-typesafe-request-id", header.parse().expect("a valid value"));
                    response
                }
            })
            .await
            .expect("the test server starts");

            let error =
                client_for(&server, protocol).models().list().send().await.expect_err("refused");
            let rendered = error.to_string();
            assert_eq!(
                rendered,
                format!("GET {}/v1/models: 500 {shown}", server.base_url()),
                "{protocol:?}"
            );
            let api = api_error(&error);
            for text in
                [rendered.clone(), format!("{error:?}"), api.to_string(), format!("{api:?}")]
            {
                assert_printable(&text);
            }
            // The endpoint is the SDK's; after it, 4 characters of status,
            // at most 201 of message and 14 + 129 of request id.
            let endpoint = format!("GET {}/v1/models: ", server.base_url());
            assert!(
                rendered.chars().count() <= endpoint.len() + 4 + 201 + 14 + 129,
                "{protocol:?}: {} characters",
                rendered.chars().count()
            );
            assert_eq!(api.body(), body.as_bytes(), "{protocol:?}: the body is kept whole");
            assert_eq!(api.body_text(), body.as_str());
            assert_eq!(api.request_id(), Some(*id), "{protocol:?}: the id is kept whole");
        }
    }
}

// ------------------------------------------------------ transport errors

/// A port at which every connection is refused, and which no test server of
/// this run can be given.
async fn refusing_port() -> RefusingPort {
    RefusingPort::new().await.unwrap_or_else(|error| panic!("{error}"))
}

/// Asserts that `error` is a connection error whose message is its cause's
/// chain of messages, and returns that message.
fn connection_message(error: &Error) -> String {
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    let chain: Vec<String> =
        std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| {
            (*cause).source()
        })
        .map(ToString::to_string)
        .collect();
    let rendered = error.to_string();
    assert_eq!(rendered, format!("Connection error: {}", chain.join(": ")));
    rendered
}

/// The first cause under `error` of type `T`.
fn cause<T: StdError + 'static>(error: &Error) -> Option<&T> {
    std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| (*cause).source())
        .find_map(|cause| cause.downcast_ref::<T>())
}

/// Upstream `test_transport_errors[ConnectError, ReadError,
/// RemoteProtocolError]`: every one is a connection error with the
/// transport's own error as its cause.
#[tokio::test]
async fn transport_errors_are_connection_errors_with_their_cause() {
    let client = |url: &str| {
        ClientBuilder::new().api_key("test-key").base_url(url).build().expect("the client builds")
    };

    // Nothing listening.
    let refused = refusing_port().await;
    let error = client(refused.base_url()).models().list().send().await.expect_err("refused");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (Connect): "), "{message}");
    let refused = cause::<std::io::Error>(&error).expect("an I/O error under it");
    assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);

    // The response ends before its body does.
    let url = format!(
        "http://{}",
        raw_server(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\n{\"models\"")
            .await
            .expect("a loopback port")
    );
    let error = client(&url).models().list().send().await.expect_err("a truncated body");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: "), "{message}");
    assert!(cause::<hyper::Error>(&error).is_some(), "{error:?}");

    // Not HTTP at all.
    let url = format!(
        "http://{}",
        raw_server(b"SSH-2.0-OpenSSH_9.9\r\n\r\n").await.expect("a loopback port")
    );
    let error = client(&url).models().list().send().await.expect_err("not HTTP");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (SendRequest): "), "{message}");
    assert!(cause::<hyper::Error>(&error).is_some_and(hyper::Error::is_parse), "{error:?}");

    // Upstream's `LocalProtocolError`: the transport fails before anything is
    // sent, which here is `poll_ready`.
    let error = ClientBuilder::new()
        .api_key("test-key")
        .base_url("https://api.typesafe.ai")
        .retry(RetryPolicy::default().max_retries(0))
        .build_with_service(NeverReady)
        .expect("the client builds")
        .models()
        .list()
        .send()
        .await
        .expect_err("the transport is never ready");
    assert_eq!(connection_message(&error), "Connection error: failed");
    let cause = error.source().expect("the transport's error is the cause");
    assert_eq!(cause.to_string(), "failed");
    assert!(cause.downcast_ref::<std::io::Error>().is_some(), "{error:?}");
}

/// A transport that is never ready to take a request.
#[derive(Debug, Clone, Copy)]
struct NeverReady;

impl Service<Request<Body>> for NeverReady {
    type Response = Response<Body>;
    type Error = std::io::Error;
    type Future = std::future::Ready<Result<Response<Body>, std::io::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Err(std::io::Error::other("failed")))
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        unreachable!("the client calls a service only once it is ready")
    }
}

/// A transport error that says whatever it likes: a newline, an ANSI
/// colour, a right-to-left override, and 100,000 characters.
#[derive(Debug)]
struct Loud;

impl std::fmt::Display for Loud {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "bad\nline \u{1b}[31mred \u{202e}rtl {}", "x".repeat(100_000))
    }
}

impl StdError for Loud {}

/// A transport that fails every request with [`Loud`].
#[derive(Debug, Clone, Copy)]
struct Failing;

impl Service<Request<Body>> for Failing {
    type Response = Response<Body>;
    type Error = Loud;
    type Future = std::future::Ready<Result<Response<Body>, Loud>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Loud>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        std::future::ready(Err(Loud))
    }
}

/// Text a caller's own transport puts in its error reaches the message
/// escaped and cut 200 characters after `Connection error: `; the error
/// itself, whole, is the cause.
#[tokio::test]
async fn a_transport_error_of_any_text_is_escaped_and_cut_in_the_message() {
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("https://api.typesafe.ai")
        .build_with_service(Failing)
        .expect("the client builds");
    let error = client.models().list().send().await.expect_err("the transport fails");

    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    let rendered = error.to_string();
    assert_eq!(
        rendered,
        format!(
            "Connection error: bad\\nline \\u{{1b}}[31mred \\u{{202e}}rtl {}\u{2026}",
            "x".repeat(164)
        )
    );
    // 18 characters of prefix, 200 of the transport's text, the mark.
    assert_eq!(rendered.chars().count(), 18 + 200 + 1);
    for shown in [rendered.clone(), format!("{rendered:?}")] {
        assert_printable(&shown);
    }
    let cause = error.source().and_then(|cause| cause.downcast_ref::<Loud>());
    assert_eq!(cause.map(ToString::to_string), Some(Loud.to_string()));
}

// ------------------------------------------------ credentials in errors

/// Where a failing [`Echo`] fails.
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// `poll_ready`, before there is a request.
    PollReady,
    /// The call itself.
    Call,
    /// Reading the response body.
    Body,
    /// The call, with an `io::Error` of kind `TimedOut` around the error.
    TimedOut,
}

/// Where the credential appears in the error [`Echo`] fails with.
#[derive(Debug, Clone, Copy)]
enum Placement {
    /// In the `Display` of the top link and of a source below it.
    Source,
    /// Only in the top link's `{:?}`.
    Debug,
    /// Only in the top link's `{:#?}`.
    AlternateDebug,
}

/// The error [`Echo`] fails with. Its `Debug` prints the fields, plus
/// `detail` in the form its placement asks for.
struct EchoError {
    display: String,
    detail: Option<(Placement, String)>,
    source: Option<Box<EchoError>>,
}

impl std::fmt::Display for EchoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl std::fmt::Debug for EchoError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let alternate = formatter.alternate();
        let mut shown = formatter.debug_struct("EchoError");
        shown.field("display", &self.display);
        match &self.detail {
            Some((Placement::Debug, detail)) if !alternate => {
                shown.field("detail", detail);
            }
            Some((Placement::AlternateDebug, detail)) if alternate => {
                shown.field("detail", detail);
            }
            _ => {}
        }
        shown.field("source", &self.source).finish()
    }
}

impl StdError for EchoError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as &(dyn StdError + 'static))
    }
}

/// A transport that fails at its [`Stage`] with an error that prints the
/// request's `Authorization` value as `Bytes` `Debug` does, and a source that
/// prints the credential after the scheme and the `x-client-secret` value,
/// as upstream's `test_transport_errors_do_not_expose_credentials` does.
/// Before there is a request it prints the values it was built with.
#[derive(Clone)]
struct Echo {
    stage: Stage,
    placement: Placement,
    authorization: String,
    secret: String,
    /// Invocations of the step that fails.
    calls: Arc<AtomicUsize>,
    /// The `Display` of the last error failed with, and of its source.
    last: Arc<Mutex<String>>,
}

impl Echo {
    fn error(&self, authorization: &[u8], secret: &[u8]) -> Box<dyn StdError + Send + Sync> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let authorization_text = String::from_utf8_lossy(authorization);
        let credential = authorization_text.split_once(' ').map_or("", |(_, rest)| rest);
        let inner = format!(
            "Rejected authorization: {credential}; provider: {}",
            String::from_utf8_lossy(secret)
        );
        let inner_text = inner.clone();
        let echoed = format!("Illegal header value {:?}", Bytes::copy_from_slice(authorization));
        let error = match self.placement {
            Placement::Source => EchoError {
                display: echoed,
                detail: None,
                source: Some(Box::new(EchoError { display: inner, detail: None, source: None })),
            },
            placement => EchoError {
                display: String::from("Illegal header value"),
                detail: Some((placement, format!("{echoed}: {inner}"))),
                source: None,
            },
        };
        let mut last = self.last.lock().expect("not poisoned");
        *last = format!("{error} / {error:?} / {error:#?} / {inner_text}");
        drop(last);
        match self.stage {
            Stage::TimedOut => Box::new(std::io::Error::new(std::io::ErrorKind::TimedOut, error)),
            _ => Box::new(error),
        }
    }
}

/// A response body that fails on its first read.
struct EchoBody(Option<Box<dyn StdError + Send + Sync>>);

impl http_body::Body for EchoBody {
    type Data = Bytes;
    type Error = Box<dyn StdError + Send + Sync>;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        Poll::Ready(self.0.take().map(Err))
    }
}

impl Service<Request<Body>> for Echo {
    type Response = Response<EchoBody>;
    type Error = Box<dyn StdError + Send + Sync>;
    type Future = std::future::Ready<Result<Response<EchoBody>, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.stage {
            Stage::PollReady => {
                Poll::Ready(Err(self.error(self.authorization.as_bytes(), self.secret.as_bytes())))
            }
            _ => Poll::Ready(Ok(())),
        }
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let headers = request.headers();
        let error =
            self.error(headers["authorization"].as_bytes(), headers["x-client-secret"].as_bytes());
        std::future::ready(match self.stage {
            Stage::Body => Ok(Response::new(EchoBody(Some(error)))),
            _ => Err(error),
        })
    }
}

/// The two credentials upstream's test uses: a plain one, and one whose
/// quote, double quote and backslash every escaping form writes differently.
const CREDENTIALS: [&str; 2] = ["ts_live_private", "ts_live_quo'te\"slash\\tail"];

/// The value of the secret default header in these tests.
const PROVIDER_SECRET: &str = "provider-credential";

/// Every form in which `secret` could be printed: as it is, and as `{:?}` of
/// a `str`, `escape_debug`, `{:?}` of a `HeaderValue` and of `Bytes`, and a
/// JSON string write it, without their quotes; and each of those once more
/// as `{:?}` of a `str` writes it, as a derived `Debug` prints a `String`
/// field holding it.
fn printed_forms(secret: &str) -> Vec<String> {
    let debug = format!("{secret:?}");
    let header = format!("{:?}", http::HeaderValue::from_str(secret).expect("a header value"));
    let bytes = format!("{:?}", Bytes::copy_from_slice(secret.as_bytes()));
    let json = serde_json::to_string(secret).expect("a string encodes");
    let forms = vec![
        secret.to_owned(),
        debug[1..debug.len() - 1].to_owned(),
        secret.escape_debug().to_string(),
        header[1..header.len() - 1].to_owned(),
        bytes[2..bytes.len() - 1].to_owned(),
        json[1..json.len() - 1].to_owned(),
    ];
    let again: Vec<String> = forms
        .iter()
        .map(|form| {
            let quoted = format!("{form:?}");
            quoted[1..quoted.len() - 1].to_owned()
        })
        .collect();
    forms.into_iter().chain(again).collect()
}

/// Every rendering of `error` a caller can reach: `Display`, `{:?}` and
/// `{:#?}` of it and of every link of its `source()` chain.
fn every_rendering(error: &Error) -> Vec<String> {
    let mut renderings = vec![error.to_string(), format!("{error:?}"), format!("{error:#?}")];
    for link in
        std::iter::successors(error.source(), |cause: &&(dyn StdError + 'static)| (*cause).source())
    {
        renderings.extend([link.to_string(), format!("{link:?}"), format!("{link:#?}")]);
    }
    renderings
}

/// Asserts that no printed form of any of `secrets` is in any rendering of
/// `error`.
#[track_caller]
fn assert_no_credential(error: &Error, secrets: &[&str], case: &str) {
    for rendering in every_rendering(error) {
        for secret in secrets {
            for form in printed_forms(secret) {
                assert!(
                    !rendering.contains(&form),
                    "{case}: {form:?} of {secret:?} in {rendering}"
                );
            }
        }
    }
}

/// The client of these tests: the credential as its key, a secret default
/// header, and three attempts with no wait between them.
fn echo_client(echo: Echo, credential: &str) -> Client<Echo> {
    ClientBuilder::new()
        .api_key(credential)
        .base_url("https://api.typesafe.ai")
        .default_header("x-client-secret", PROVIDER_SECRET)
        .retry(
            RetryPolicy::default()
                .max_retries(2)
                .backoff_initial(Duration::ZERO)
                .backoff_max(Duration::ZERO),
        )
        .build_with_service(echo)
        .expect("the client builds")
}

/// Fails through an [`Echo`] and returns the error and the echo.
async fn echo_failure(stage: Stage, placement: Placement, credential: &str) -> (Error, Echo) {
    let echo = Echo {
        stage,
        placement,
        authorization: format!("Bearer {credential}"),
        secret: PROVIDER_SECRET.to_owned(),
        calls: Arc::default(),
        last: Arc::default(),
    };
    let error = echo_client(echo.clone(), credential)
        .models()
        .list()
        .send()
        .await
        .expect_err("the transport fails");
    (error, echo)
}

const STAGES: [Stage; 4] = [Stage::PollReady, Stage::Call, Stage::Body, Stage::TimedOut];

const PLACEMENTS: [Placement; 3] = [Placement::Source, Placement::Debug, Placement::AlternateDebug];

/// Upstream `test_transport_errors_do_not_expose_credentials`: a transport
/// whose error prints the request's credentials, at every stage an attempt
/// can fail at, never lets one reach the connection error - its message, its
/// `Debug`, or any link of its chain - in any form, while the transport's
/// own error still holds it. The chain is then a redacted copy, which cannot
/// be downcast to the transport's type.
#[tokio::test]
async fn transport_errors_never_expose_a_credential() {
    for stage in STAGES {
        for placement in PLACEMENTS {
            for credential in CREDENTIALS {
                let case = format!("{stage:?} {placement:?} {credential:?}");
                let (error, echo) = echo_failure(stage, placement, credential).await;

                assert!(matches!(error.kind(), ErrorKind::Connection), "{case}: {error:?}");
                assert_eq!(echo.calls.load(Ordering::SeqCst), 3, "{case}: three attempts");
                let last = echo.last.lock().expect("not poisoned").clone();
                assert!(last.contains(credential), "{case}: the transport's own text: {last}");
                assert!(last.contains(PROVIDER_SECRET), "{case}: {last}");
                match placement {
                    Placement::Source => {
                        assert_eq!(
                            error.to_string(),
                            "Connection error: Illegal header value b\"***\": \
                             Rejected authorization: ***; provider: ***",
                            "{case}"
                        );
                        let second = error
                            .source()
                            .and_then(StdError::source)
                            .unwrap_or_else(|| panic!("{case}: two links: {error:?}"));
                        assert_eq!(
                            second.to_string(),
                            "Rejected authorization: ***; provider: ***",
                            "{case}"
                        );
                    }
                    Placement::Debug | Placement::AlternateDebug => {
                        assert_eq!(
                            error.to_string(),
                            "Connection error: Illegal header value",
                            "{case}"
                        );
                        let debug = format!("{error:?}");
                        let alternate = format!("{error:#?}");
                        let shown =
                            if matches!(placement, Placement::Debug) { &debug } else { &alternate };
                        assert!(
                            shown.contains("Rejected authorization: ***; provider: ***"),
                            "{case}: the redacted detail in {shown}"
                        );
                        assert!(
                            shown.contains(concat!(
                                r#"detail: "Illegal header value b\"***\": "#,
                                r#"Rejected authorization: ***; provider: ***""#,
                            )),
                            "{case}: the whole detail redacted in {shown}"
                        );
                        // The detail is printed through `{:?}` a second time, so
                        // the `Bytes` form of the `Authorization` value is escaped
                        // twice; that form must be gone from both renderings.
                        let twice = format!(
                            "{:?}",
                            format!("{:?}", Bytes::from(format!("Bearer {credential}")))
                        );
                        let twice = twice.trim_matches('"');
                        for rendering in [&debug, &alternate] {
                            assert!(!rendering.contains(twice), "{case}: {twice} in {rendering}");
                        }
                    }
                }
                let top = error.source().unwrap_or_else(|| panic!("{case}: a cause: {error:?}"));
                assert!(top.downcast_ref::<EchoError>().is_none(), "{case}: {top:?}");
                assert!(top.downcast_ref::<std::io::Error>().is_none(), "{case}: {top:?}");
                assert_no_credential(
                    &error,
                    &[credential, &format!("Bearer {credential}"), PROVIDER_SECRET],
                    &case,
                );
            }
        }
    }
}

/// A TLS server whose certificate the client was not told to trust.
#[tokio::test]
async fn an_untrusted_certificate_is_a_connection_error_naming_the_certificate() {
    let server = answering(Protocol::Http2Tls, StatusCode::OK, br#"{"models":[]}"#).await;
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url(server.base_url())
        .build()
        .expect("builds");

    let error = client.models().list().send().await.expect_err("the certificate is not trusted");
    let message = connection_message(&error);
    assert!(message.starts_with("Connection error: client error (Connect): "), "{message}");
    // `io::Error::source` skips the error it wraps, and the connector wraps
    // the TLS error in two of them, so it is reached through `get_ref`.
    let mut wrapped = cause::<std::io::Error>(&error).and_then(|io| io.get_ref());
    while let Some(io) = wrapped.and_then(|inner| inner.downcast_ref::<std::io::Error>()) {
        wrapped = io.get_ref();
    }
    let tls = wrapped
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .unwrap_or_else(|| panic!("the TLS error under {error:#?}"));
    assert!(matches!(tls, rustls::Error::InvalidCertificate(_)), "{tls:?}");
    assert_eq!(server.request_count(), 0);
}

/// A server whose handler holds every response until the test releases it,
/// and says when a request has arrived.
struct Held {
    server: TestServer,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
}

async fn held(protocol: Protocol, body: &'static [u8]) -> Held {
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (on_arrival, on_release) = (Arc::clone(&arrived), Arc::clone(&release));
    let server = TestServer::start(protocol, move |_| {
        let (arrived, release) = (Arc::clone(&on_arrival), Arc::clone(&on_release));
        async move {
            arrived.notify_one();
            release.notified().await;
            json_response(StatusCode::OK, body)
        }
    })
    .await
    .expect("the test server starts");
    Held { server, arrived, release }
}

/// Upstream `test_transport_errors[ConnectTimeout, ReadTimeout]`: an attempt
/// past its deadline is a timeout carrying that deadline. (README deviation
/// row "Timeout per httpx2 phase; `httpx2.Timeout` objects": one deadline covers
/// the whole attempt, not each httpx2 phase.)
#[tokio::test]
async fn an_attempt_past_its_deadline_is_a_timeout_with_that_deadline() {
    for protocol in Protocol::ALL {
        let held = held(protocol, br#"{"models":[]}"#).await;
        let error = client_for(&held.server, protocol)
            .models()
            .list()
            .timeout(Duration::from_millis(125))
            .send()
            .await
            .expect_err("the response is held back");

        assert!(
            matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_millis(125)),
            "{protocol:?}: {error:?}"
        );
        assert_eq!(error.to_string(), "Request timed out (timeout=0.125s).");
        assert!(error.source().is_none());
        assert_eq!(held.server.request_count(), 1, "{protocol:?}: the request reached the server");
        held.release.notify_waiters();
    }
}

/// Upstream `test_system_one_timeout_override` (and `test_invalid_timeout`'s
/// per-call half): a call's deadline replaces the client's for that call
/// only, `no_timeout` removes it, and zero is refused before the network.
#[tokio::test]
async fn system_one_timeout_override() {
    let held = held(Protocol::Http1, RESULT).await;
    let client = builder_for(&held.server, Protocol::Http1)
        .timeout(Duration::from_millis(100))
        .build()
        .expect("builds");
    let questions = one_raw_question();

    // The client's own deadline fails a held call.
    let error = client.system_one("hello", &questions).send().await.expect_err("held past 100 ms");
    assert!(
        matches!(error.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_millis(100)),
        "{error:?}"
    );
    held.release.notify_waiters();

    // A longer per-call deadline, and no deadline, outlast a hold three times
    // as long as the client's deadline, which that deadline would not allow.
    for (label, request) in [
        ("timeout(5s)", client.system_one("hello", &questions).timeout(Duration::from_secs(5))),
        ("no_timeout()", client.system_one("hello", &questions).no_timeout()),
    ] {
        let release = async {
            held.arrived.notified().await;
            tokio::time::sleep(Duration::from_millis(300)).await;
            held.release.notify_one();
        };
        let (response, ()) = tokio::join!(request.send(), release);
        let response = response.unwrap_or_else(|error| panic!("{label}: {error}"));
        assert_eq!(response.model(), "jev-latest", "{label}");
    }

    let error = client
        .system_one("hello", &questions)
        .timeout(Duration::ZERO)
        .send()
        .await
        .expect_err("a zero deadline");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    assert_eq!(error.to_string(), "timeout must be a positive, finite number of seconds.");
    assert_eq!(held.server.request_count(), 3, "the zero deadline never reached the network");
}

// ------------------------------------------------------- response limit

/// The 16 MiB cap of README deviation row "No response size limit", set
/// lower: a body over it is not read past it, whatever says how long it is.
#[tokio::test]
async fn a_response_over_the_limit_is_refused() {
    let big = Bytes::from(vec![b' '; 4096]);
    for protocol in [Protocol::Http1, Protocol::H2c] {
        // A success response whose declared length is over the limit: its
        // own kind, naming the limit, with no cause.
        let server = answering(protocol, StatusCode::OK, big.clone()).await;
        let client =
            builder_for(&server, protocol).max_response_bytes(1024).build().expect("builds");
        let error = client.models().list().send().await.expect_err("over the limit");
        assert_too_large(&error, 1024);

        // A failure response keeps its status and headers, and no body.
        let failing = big.clone();
        let server = TestServer::start(protocol, move |_| {
            let body = failing.clone();
            async move {
                let mut response = json_response(StatusCode::SERVICE_UNAVAILABLE, body);
                response.headers_mut().insert("retry-after", "3".parse().expect("valid"));
                response
            }
        })
        .await
        .expect("the test server starts");
        let client =
            builder_for(&server, protocol).max_response_bytes(1024).build().expect("builds");
        let error = client.models().list().send().await.expect_err("over the limit");
        let api = api_error(&error);
        assert_eq!(api.kind(), ApiErrorKind::InternalServer);
        assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api.body(), b"");
        assert_eq!(api.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(
            error.to_string(),
            format!(
                "GET {}/v1/models: 503 The response body exceeded the limit of 1024 bytes and was not read.",
                server.base_url()
            )
        );
    }

    // A body at the limit is read.
    let at_limit: &'static [u8] = br#"{"models":[]}"#;
    let server = answering(Protocol::Http1, StatusCode::OK, at_limit).await;
    let client = builder_for(&server, Protocol::Http1)
        .max_response_bytes(at_limit.len())
        .build()
        .expect("builds");
    let response = client.models().list().send().await.expect("exactly at the limit");
    assert!(response.models().is_empty());
}

/// The error a success response over `limit` must be.
fn assert_too_large(error: &Error, limit: usize) {
    assert!(
        matches!(error.kind(), ErrorKind::ResponseTooLarge { limit: at } if *at == limit),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        format!("The response body exceeded the limit of {limit} bytes and was not read.")
    );
    assert!(error.source().is_none(), "{error:?}");
}

/// A `content-length` over the limit is refused before a byte of the body is
/// read - even when the server then sends far less than it declared.
#[tokio::test]
async fn a_declared_length_over_the_limit_is_refused_whatever_follows_it() {
    let url = format!(
        "http://{}",
        raw_server(b"HTTP/1.1 200 OK\r\ncontent-length: 5000\r\n\r\n{\"models\":[]}")
            .await
            .expect("a loopback port")
    );
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url(url)
        .max_response_bytes(1024)
        .build()
        .expect("builds");
    let error = client.models().list().send().await.expect_err("declared over the limit");
    assert_too_large(&error, 1024);
}

/// A body with no declared length is cut off by the limit as it streams.
#[tokio::test]
async fn a_streamed_response_over_the_limit_is_refused_as_it_arrives() {
    // One chunk of 0x400 = 1024 bytes, and no `content-length`.
    let reply = format!(
        "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n400\r\n{}\r\n0\r\n\r\n",
        " ".repeat(1024)
    );
    let url = format!("http://{}", raw_server(reply).await.expect("a loopback port"));
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url(url)
        .max_response_bytes(100)
        .build()
        .expect("builds");
    let error = client.models().list().send().await.expect_err("over the limit");
    assert_too_large(&error, 100);
}

// -------------------------------------------------------------- headers

/// A transport that forwards every request to a loopback server over plain
/// HTTP/1.1, keeping the host it was asked for in `Host`: how a request to a
/// host this test cannot resolve still reaches a real server.
#[derive(Clone)]
struct Forward {
    inner: legacy::Client<HttpConnector, Body>,
    /// The server's `host:port`.
    target: Uri,
    /// Every URI the SDK handed over, in order.
    asked: Arc<Mutex<Vec<(http::Method, Uri)>>>,
}

impl Forward {
    fn to(server: &TestServer) -> Self {
        Self {
            inner: legacy::Client::builder(TokioExecutor::new()).build_http(),
            target: server.base_url().parse().expect("a URL"),
            asked: Arc::default(),
        }
    }

    fn asked(&self) -> Vec<(http::Method, Uri)> {
        self.asked.lock().expect("not poisoned").clone()
    }
}

impl Service<Request<Body>> for Forward {
    type Response = Response<Incoming>;
    type Error = legacy::Error;
    type Future = legacy::ResponseFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), legacy::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<Body>) -> Self::Future {
        let asked = request.uri().clone();
        self.asked.lock().expect("not poisoned").push((request.method().clone(), asked.clone()));
        let authority = asked.authority().expect("the SDK sends an absolute URI").as_str();
        request.headers_mut().insert(HOST, authority.parse().expect("a host is a header value"));
        let mut parts = asked.into_parts();
        parts.scheme = Some(http::uri::Scheme::HTTP);
        parts.authority = self.target.authority().cloned();
        *request.uri_mut() = Uri::from_parts(parts).expect("the parts came from a URI");
        self.inner.request(request)
    }
}

/// The five headers upstream protects, each set to something wrong.
const PROTECTED: [(&str, &str); 5] = [
    ("authorization", "injected-secret"),
    ("accept", "text/plain"),
    ("user-agent", "wrong"),
    ("x-typesafe-sdk", "wrong"),
    ("x-typesafe-runtime", "wrong"),
];

/// A server answering the fixture with a credential and a request id among
/// its headers.
async fn logging_server(protocol: Protocol) -> TestServer {
    TestServer::start(protocol, |_| async {
        let mut response = json_response(StatusCode::OK, RESULT);
        let headers = response.headers_mut();
        headers.insert("set-cookie", "response-secret".parse().expect("valid"));
        headers.insert("x-typesafe-request-id", "req_log".parse().expect("valid"));
        response
    })
    .await
    .expect("the test server starts")
}

/// `builder` with every client header upstream's case sets.
fn upstream_builder(builder: ClientBuilder) -> ClientBuilder {
    let mut builder = builder.timeout(Duration::from_secs(7));
    for (name, value) in PROTECTED {
        builder = builder.default_header(name, value);
    }
    builder
        .default_header("X-Team", "default")
        .default_header("X-Default", "kept")
        .default_header("X-API-Key", "key-secret")
        .default_header("cookie", "cookie-secret")
}

/// Sets every call header upstream's case sets and sends one request.
async fn send_upstream_headers<S: decision_model_sdk::HttpService>(client: &Client<S>) {
    let questions = one_raw_question();
    let mut request = client.system_one("hello", &questions).timeout(Duration::from_secs(2));
    for (name, value) in PROTECTED {
        request = request.header(name, value);
    }
    request
        .header("x-team", "call")
        .header("x-typesafe-retry-count", "99")
        .header("content-type", "wrong")
        .send()
        .await
        .expect("the call succeeds");
}

/// What the server must have recorded for [`send_upstream_headers`].
fn assert_upstream_headers(request: &RecordedRequest, context: &str) {
    let sdk = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    let runtime = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    assert_eq!(request.header_values("authorization"), ["Bearer test-key"], "{context}");
    assert_eq!(request.header_values("accept"), ["application/json"], "{context}");
    assert_eq!(request.header_values("user-agent"), [sdk.as_str()], "{context}");
    assert_eq!(request.header_values("x-typesafe-sdk"), [sdk.as_str()], "{context}");
    assert_eq!(request.header_values("x-typesafe-runtime"), [runtime.as_str()], "{context}");
    assert!(request.headers.get("x-typesafe-retry-count").is_none(), "{context}");
    assert_eq!(request.header_values("x-team"), ["call"], "{context}");
    assert_eq!(request.header_values("x-default"), ["kept"], "{context}");
    assert_eq!(request.header_values("content-type"), ["application/json"], "{context}");
    // A secret default is sent; it is kept out of logs, not off the wire.
    assert_eq!(request.header_values("x-api-key"), ["key-secret"], "{context}");
    assert_eq!(request.header_values("cookie"), ["cookie-secret"], "{context}");
}

/// Upstream `test_headers_timeout_and_logging` - the headers half, AC-F5:
/// the protected headers and `Content-Type` cannot be overridden, a caller's
/// `x-typesafe-retry-count: 99` is not sent, and a base URL of
/// `<root>/prefix///` sends to `<root>/prefix/v1/systemone`. Asserted on what
/// the server recorded, over each protocol.
#[tokio::test]
async fn headers_timeout_and_logging() {
    for protocol in Protocol::ALL {
        let server = logging_server(protocol).await;
        let builder =
            builder_for(&server, protocol).base_url(format!("{}/prefix///", server.base_url()));
        send_upstream_headers(&upstream_builder(builder).build().expect("the client builds")).await;

        let [request] = &server.requests()[..] else { panic!("{protocol:?}: one request") };
        assert_eq!(request.uri.path(), "/prefix/v1/systemone", "{protocol:?}");
        assert_upstream_headers(request, &format!("{protocol:?}"));
    }
}

/// AC-F5's URL exactly: `https://example.test/prefix///` sends to
/// `https://example.test/prefix/v1/systemone`. The host cannot be resolved
/// here, so a forwarding transport carries the request to the loopback
/// server, which records the path and the `Host` it was sent.
#[tokio::test]
async fn the_upstream_base_url_with_a_prefix_and_trailing_slashes() {
    let server = logging_server(Protocol::Http1).await;
    let forward = Forward::to(&server);
    let builder = ClientBuilder::new()
        .api_key("test-key")
        .default_model("jev-latest")
        .base_url("https://example.test/prefix///");
    let client =
        upstream_builder(builder).build_with_service(forward.clone()).expect("the client builds");
    send_upstream_headers(&client).await;

    assert_eq!(
        forward.asked(),
        [(http::Method::POST, Uri::from_static("https://example.test/prefix/v1/systemone"))]
    );
    let [request] = &server.requests()[..] else { panic!("one request") };
    assert_eq!(request.uri.path(), "/prefix/v1/systemone");
    assert_eq!(request.header_values("host"), ["example.test"]);
    assert_upstream_headers(request, "example.test");
}

/// A server answering a listing with no models and a System One call with
/// the fixture.
async fn listing_and_answering(protocol: Protocol) -> TestServer {
    TestServer::start(protocol, |request: RecordedRequest| async move {
        let body: &'static [u8] =
            if request.method == http::Method::POST { RESULT } else { br#"{"models":[]}"# };
        json_response(StatusCode::OK, body)
    })
    .await
    .expect("the test server starts")
}

/// Lists the models and asks one question, each call carrying `headers`,
/// and returns what the server recorded for the two.
async fn list_and_ask(
    server: &TestServer,
    client: &Client,
    headers: &[(&str, &str)],
) -> Vec<RecordedRequest> {
    let questions = one_raw_question();
    let mut list = client.models().list();
    let mut ask = client.system_one("hello", &questions);
    for (name, value) in headers {
        list = list.header(*name, *value);
        ask = ask.header(*name, *value);
    }
    list.send().await.expect("the listing succeeds");
    ask.send().await.expect("the call succeeds");
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one listing and one call");
    requests
}

/// Not in upstream, which always sends its own identifier alone (README
/// deviation row "`User-Agent` names the SDK alone; `X-TypeSafe-Runtime` is
/// always sent"): an application's product goes in front of the SDK's in
/// `User-Agent`, and `X-TypeSafe-SDK` and `X-TypeSafe-Runtime` are as they
/// are without it. Asserted on what the server received, over each
/// protocol, on a request with a body and one without.
#[tokio::test]
async fn an_application_product_is_received_in_front_of_the_sdk_identifier() {
    let sdk = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    let runtime = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    let user_agent = format!("ganja-code/0.1.0 {sdk}");
    for protocol in Protocol::ALL {
        let server = listing_and_answering(protocol).await;
        let client = builder_for(&server, protocol)
            .user_agent_product("ganja-code/0.1.0")
            .build()
            .expect("the client builds");
        for request in list_and_ask(&server, &client, &[]).await {
            let context = format!("{protocol:?}, {}", request.method);
            assert_eq!(request.header_values("user-agent"), [user_agent.as_str()], "{context}");
            assert_eq!(request.header_values("x-typesafe-sdk"), [sdk.as_str()], "{context}");
            assert_eq!(
                request.header_values("x-typesafe-runtime"),
                [runtime.as_str()],
                "{context}"
            );
        }
    }
}

/// With the runtime header switched off, no request carries
/// `X-TypeSafe-Runtime`, and `X-TypeSafe-SDK` and `User-Agent` are still the
/// SDK's (README deviation row "`User-Agent` names the SDK alone;
/// `X-TypeSafe-Runtime` is always sent"). Over each protocol, both kinds of
/// request.
#[tokio::test]
async fn with_the_runtime_header_off_the_server_receives_none() {
    let sdk = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    for protocol in Protocol::ALL {
        let server = listing_and_answering(protocol).await;
        let client = builder_for(&server, protocol)
            .send_runtime_header(false)
            .build()
            .expect("the client builds");
        for request in list_and_ask(&server, &client, &[]).await {
            let context =
                format!("{protocol:?}, {}, headers {:?}", request.method, request.headers);
            assert!(request.headers.get("x-typesafe-runtime").is_none(), "{context}");
            assert_eq!(request.header_values("x-typesafe-sdk"), [sdk.as_str()], "{context}");
            assert_eq!(request.header_values("user-agent"), [sdk.as_str()], "{context}");
            assert_eq!(request.header_values("authorization"), ["Bearer test-key"], "{context}");
        }
    }
}

/// The two settings do not loosen the protection of the SDK's headers: with
/// a product set and the runtime header off, a caller's default and per-call
/// `User-Agent` and `X-TypeSafe-Runtime` still do not get through. The
/// server receives the product's `User-Agent` once and no runtime header.
#[tokio::test]
async fn a_caller_header_cannot_stand_in_for_either_setting() {
    let sdk = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    let user_agent = format!("ganja-code/0.1.0 {sdk}");
    let caller = [("user-agent", "wrong"), ("x-typesafe-runtime", "x")];
    for protocol in Protocol::ALL {
        let server = listing_and_answering(protocol).await;
        let mut builder = builder_for(&server, protocol);
        for (name, value) in caller {
            builder = builder.default_header(name, value);
        }
        let client = builder
            .user_agent_product("ganja-code/0.1.0")
            .send_runtime_header(false)
            .build()
            .expect("the client builds");
        for request in list_and_ask(&server, &client, &caller).await {
            let context =
                format!("{protocol:?}, {}, headers {:?}", request.method, request.headers);
            assert_eq!(request.header_values("user-agent"), [user_agent.as_str()], "{context}");
            assert!(request.headers.get("x-typesafe-runtime").is_none(), "{context}");
            assert_eq!(request.header_values("x-typesafe-sdk"), [sdk.as_str()], "{context}");
        }
    }
}

/// The headers that frame a message or manage its connection, each with a
/// value that would change what the transport does if it were sent.
const TRANSPORT_OWNED: [(&str, &str); 8] = [
    ("content-length", "999"),
    ("transfer-encoding", "chunked"),
    ("connection", "close"),
    ("keep-alive", "timeout=5"),
    ("proxy-connection", "keep-alive"),
    ("te", "trailers"),
    ("trailer", "x-checksum"),
    ("upgrade", "websocket"),
];

/// Asserts that none of [`TRANSPORT_OWNED`] reached the server with the
/// caller's value, that `Content-Length` is the transport's own when it is
/// there at all, and that the caller's `Host` arrived.
fn assert_transport_owned_dropped(request: &RecordedRequest, context: &str) {
    let context =
        format!("{context}, {} {}, headers {:?}", request.method, request.uri, request.headers);
    for (name, _) in TRANSPORT_OWNED {
        if name == "content-length" {
            let length = request.body.len().to_string();
            let sent = request.header_values(name);
            assert!(
                sent.iter().all(|value| *value == length),
                "{context}: content-length {sent:?} is not the body's length {length}"
            );
            if request.method == http::Method::GET {
                assert!(sent.is_empty() || length == "0", "{context}: content-length {sent:?}");
            }
        } else {
            assert_eq!(request.header_values(name), Vec::<&str>::new(), "{context}: {name}");
        }
    }
    assert_eq!(request.header_values("host"), ["routed.example"], "{context}");
}

/// The framing and connection headers belong to the transport: set as a
/// client default or on a call, over each protocol, on a call with a body and
/// one without, none of them is sent and the call succeeds - a
/// `content-length: 999` included, which would otherwise fail an HTTP/2
/// stream and leave an HTTP/1.1 call waiting until its deadline. `host` is
/// sent as given; over HTTP/2 the `:authority` is still the base URL's.
#[tokio::test]
async fn framing_and_connection_headers_are_dropped_and_host_is_sent() {
    for protocol in Protocol::ALL {
        let server = listing_and_answering(protocol).await;
        let questions = one_raw_question();

        for set_on in ["default", "call"] {
            let context = format!("{protocol:?}, set as a {set_on} header");
            let mut builder = builder_for(&server, protocol).timeout(Duration::from_secs(5));
            if set_on == "default" {
                for (name, value) in TRANSPORT_OWNED {
                    builder = builder.default_header(name, value);
                }
                builder = builder.default_header("host", "routed.example");
            }
            let client = builder.build().expect("the client builds");

            let mut ask = client.system_one("hello", &questions);
            let mut list = client.models().list();
            if set_on == "call" {
                for (name, value) in TRANSPORT_OWNED {
                    ask = ask.header(name, value);
                    list = list.header(name, value);
                }
                ask = ask.header("host", "routed.example");
                list = list.header("host", "routed.example");
            }
            ask.send().await.unwrap_or_else(|error| panic!("{context}: the call failed: {error}"));
            list.send()
                .await
                .unwrap_or_else(|error| panic!("{context}: the listing failed: {error}"));
        }

        let requests = server.requests();
        assert_eq!(requests.len(), 4, "{protocol:?}: two calls per client, two clients");
        let authority = server.addr().to_string();
        for request in &requests {
            assert_transport_owned_dropped(request, &format!("{protocol:?}"));
            if protocol != Protocol::Http1 {
                assert_eq!(
                    request.uri.authority().map(|authority| authority.as_str()),
                    Some(authority.as_str()),
                    "{protocol:?}: the :authority comes from the base URL"
                );
            }
        }
    }
}

/// Upstream `test_http_client_settings`, as README deviation row
/// "`http_client=` or `transport=`, mutually exclusive" has it: a transport of
/// the caller's own carries every request, with the SDK's headers and the
/// call's.
#[tokio::test]
async fn a_custom_transport_carries_every_request_with_the_sdk_headers() {
    let server = listing_and_answering(Protocol::Http1).await;
    let forward = Forward::to(&server);
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("https://api.typesafe.ai")
        .default_header("x-sdk-default", "sdk")
        .default_header("x-call", "sdk")
        .build_with_service(forward.clone())
        .expect("the client builds");

    let models = client.models().list().header("x-call", "call").send().await.expect("listed");
    assert!(models.models().is_empty());
    let questions = one_raw_question();
    client.system_one("x", &questions).header("x-call", "call").send().await.expect("answered");

    let asked: Vec<_> =
        forward.asked().into_iter().map(|(method, uri)| format!("{method} {uri}")).collect();
    assert_eq!(
        asked,
        ["GET https://api.typesafe.ai/v1/models", "POST https://api.typesafe.ai/v1/systemone"]
    );
    for request in server.requests() {
        assert_eq!(request.header_values("authorization"), ["Bearer test-key"]);
        assert_eq!(request.header_values("accept"), ["application/json"]);
        assert_eq!(request.header_values("x-sdk-default"), ["sdk"]);
        assert_eq!(request.header_values("x-call"), ["call"]);
        let content_type =
            if request.method == http::Method::POST { vec!["application/json"] } else { vec![] };
        assert_eq!(request.header_values("content-type"), content_type, "{}", request.method);
    }
}

/// A transport that counts how many copies of it are alive.
struct Counted {
    forward: Forward,
    alive: Arc<AtomicUsize>,
}

impl Counted {
    fn new(forward: Forward, alive: &Arc<AtomicUsize>) -> Self {
        alive.fetch_add(1, Ordering::SeqCst);
        Self { forward, alive: Arc::clone(alive) }
    }
}

impl Clone for Counted {
    fn clone(&self) -> Self {
        Self::new(self.forward.clone(), &self.alive)
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.alive.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Service<Request<Body>> for Counted {
    type Response = Response<Incoming>;
    type Error = legacy::Error;
    type Future = legacy::ResponseFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), legacy::Error>> {
        self.forward.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        self.forward.call(request)
    }
}

/// Upstream `test_supplied_network_resources_closed`,
/// `test_owned_http_client_closed` and
/// `test_exceptional_context_closes_http_client`, as README deviation row
/// "`close()`, context managers, closing a supplied client" has them: a client
/// owns its transport by value, and dropping the last clone of the client
/// drops it - after success and after failure alike.
#[tokio::test]
async fn the_last_clone_of_a_client_drops_its_transport() {
    let server = answering(Protocol::Http1, StatusCode::OK, br#"{"models":[]}"#).await;
    let alive = Arc::new(AtomicUsize::new(0));
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://api.test")
        .build_with_service(Counted::new(Forward::to(&server), &alive))
        .expect("the client builds");
    let clone = client.clone();
    assert_eq!(alive.load(Ordering::SeqCst), 1, "a client clone shares the transport");

    clone.models().list().send().await.expect("listed");
    let failure = clone.models().list().timeout(Duration::ZERO).send().await;
    assert!(failure.is_err_and(|error| matches!(error.kind(), ErrorKind::InvalidRequest)));
    assert_eq!(alive.load(Ordering::SeqCst), 1, "each call's clone of the transport is gone");

    drop(client);
    assert_eq!(alive.load(Ordering::SeqCst), 1, "another clone still holds it");
    drop(clone);
    assert_eq!(alive.load(Ordering::SeqCst), 0, "the last clone dropped it");
}

/// Upstream `test_task_cancellation_closes_context` and
/// `test_cancellation_propagates`: dropping the future of a call in flight
/// cancels it - one request reached the server, nothing more is sent - and
/// the client goes on working.
#[tokio::test]
async fn dropping_a_call_in_flight_cancels_it() {
    for protocol in Protocol::ALL {
        let held = held(protocol, br#"{"models":[]}"#).await;
        let client = client_for(&held.server, protocol);
        {
            let call = client.models().list().no_timeout().send();
            tokio::pin!(call);
            tokio::select! {
                result = &mut call => panic!("{protocol:?}: the held call returned {result:?}"),
                () = held.arrived.notified() => {}
            }
        }
        assert_eq!(held.server.request_count(), 1, "{protocol:?}");

        held.release.notify_one();
        let release = async {
            held.arrived.notified().await;
            held.release.notify_one();
        };
        let (listed, ()) = tokio::join!(client.models().list().send(), release);
        assert!(listed.expect("the client still works").models().is_empty());
        assert_eq!(held.server.request_count(), 2, "{protocol:?}");
    }
}

// -------------------------------------------------------------- logging

/// The logging half of upstream `test_headers_timeout_and_logging` and
/// `test_secret_headers_redacted`: no credential reaches an event, whichever
/// side of the exchange carried it; the request id and the body do, the body
/// at `TRACE` only (README deviation row "DEBUG logs full bodies").
#[cfg(feature = "tracing")]
mod logging {
    use tracing::Level;

    use super::*;
    use crate::recorder::{Recorder, assert_timed, install};

    impl Recorder {
        fn text(&self) -> String {
            let events = self.0.lock().expect("not poisoned");
            events.iter().map(|(level, line)| format!("{level} {line}\n")).collect()
        }
    }

    #[tokio::test]
    async fn log_endpoint_host_false_prints_the_path_alone() {
        let recorder = Recorder::default();
        let _installed = install(&recorder);
        let server = TestServer::start_nth(Protocol::Http1, |attempt, request| {
            assert_eq!(request.uri.path(), "/prefix/v1/models");
            match attempt {
                1 => json_response(StatusCode::SERVICE_UNAVAILABLE, "{}"),
                2 => json_response(StatusCode::OK, r#"{"models":[]}"#),
                3 => json_response(StatusCode::NOT_FOUND, r#"{"message":"gone"}"#),
                other => panic!("unexpected attempt {other}"),
            }
        })
        .await
        .expect("the test server starts");
        let base = format!("{}/prefix", server.base_url());
        let client = builder_for(&server, Protocol::Http1)
            .base_url(&base)
            .log_endpoint_host(false)
            .retry(RetryPolicy::new().backoff_initial(Duration::ZERO))
            .build()
            .expect("the client builds");

        let response = client.models().list().send().await.expect("the retry succeeds");
        assert!(response.models().is_empty());
        let error = client.models().list().send().await.expect_err("the next call is not found");
        assert_eq!(error.to_string(), format!("GET {base}/v1/models: 404 gone"));
        let ErrorKind::Api(api) = error.kind() else { panic!("expected an API error: {error:?}") };
        assert_eq!(api.endpoint(), Some(format!("GET {base}/v1/models").as_str()));
        assert_eq!(server.request_count(), 3, "503, retry success, then 404");

        let info = recorder.at(Level::INFO);
        assert_eq!(info.len(), 4, "{info:#?}");
        assert_timed(&info[0], "message=GET /v1/models <- 503 in ", " (request -)");
        assert_eq!(info[1], "message=GET /v1/models retry 1");
        assert_timed(&info[2], "message=GET /v1/models <- 200 in ", " (request -)");
        assert_timed(&info[3], "message=GET /v1/models <- 404 in ", " (request -)");
        for level in [Level::INFO, Level::DEBUG, Level::TRACE] {
            let lines = recorder.at(level);
            assert!(!lines.is_empty(), "{level}: expected captured events");
            for line in lines {
                for hidden in ["127.0.0.1", "http://", "prefix"] {
                    assert!(!line.contains(hidden), "{level}: {hidden} reached {line}");
                }
                if level != Level::INFO {
                    assert!(line.contains("endpoint=/v1/models"), "{level}: {line}");
                }
            }
        }
    }

    /// Every event is recorded, hyper's included, so the secrets are checked
    /// against everything a subscriber would see.
    #[tokio::test]
    async fn no_credential_reaches_an_event_and_the_body_waits_for_trace() {
        let recorder = Recorder::default();
        let _installed = install(&recorder);

        let server = logging_server(Protocol::Http1).await;
        let builder = upstream_builder(builder_for(&server, Protocol::Http1));
        send_upstream_headers(&builder.build().expect("the client builds")).await;

        let text = recorder.text();
        for secret in
            ["test-key", "injected-secret", "key-secret", "cookie-secret", "response-secret"]
        {
            assert!(!text.contains(secret), "{secret} reached the events:\n{text}");
        }
        assert!(text.contains("***"), "{text}");
        assert!(text.contains("req_log"), "{text}");
        assert!(text.contains("x-default"), "{text}");

        let debug = recorder.at(Level::DEBUG);
        assert_eq!(debug.len(), 2, "one event each way: {debug:#?}");
        assert!(debug[0].contains("message=sending request"), "{}", debug[0]);
        assert!(debug[0].contains("method=POST"), "{}", debug[0]);
        assert!(
            debug[0].contains(&format!("endpoint={}/v1/systemone", server.base_url())),
            "{}",
            debug[0]
        );
        let sent = server.requests()[0].body.len();
        assert!(debug[0].contains(&format!("body_len={sent}")), "{}", debug[0]);
        assert!(debug[1].contains("message=received response"), "{}", debug[1]);
        assert!(debug[1].contains("status=200"), "{}", debug[1]);
        assert!(debug[1].contains("request_id=req_log "), "{}", debug[1]);
        assert!(debug.iter().all(|line| !line.contains("hello")), "a body at DEBUG: {debug:#?}");

        let trace = recorder.at(Level::TRACE);
        assert_eq!(trace.len(), 2, "{trace:#?}");
        assert!(trace[0].contains(r#"body={"state":"hello","#), "{}", trace[0]);
        assert!(trace[1].contains(r#"body={"model":"jev-latest","#), "{}", trace[1]);
    }

    /// The `INFO` line an attempt's single `INFO` event renders as, without
    /// the target.
    fn only_info_line(recorder: &Recorder) -> String {
        let lines = recorder.at(Level::INFO);
        let [line] = &lines[..] else { panic!("one INFO line expected: {lines:#?}") };
        line.strip_prefix("message=").unwrap_or_else(|| panic!("{line}")).to_owned()
    }

    /// Upstream `test_logger_level_controls_output`'s INFO summary, and its
    /// failure line: one `INFO` event per attempt, as upstream words it, for
    /// a response of any status and for an attempt that got none. No INFO or
    /// DEBUG event carries a body, a header value or an error's own message.
    #[tokio::test]
    async fn every_attempt_gets_one_info_line_and_no_body_reaches_info_or_debug() {
        // A success.
        let recorder = Recorder::default();
        let installed = install(&recorder);
        let server = logging_server(Protocol::Http1).await;
        let questions = one_raw_question();
        client_for(&server, Protocol::Http1)
            .system_one("secret-state", &questions)
            .send()
            .await
            .expect("answered");
        let base = server.base_url();
        assert_timed(
            &only_info_line(&recorder),
            &format!("POST {base}/v1/systemone <- 200 in "),
            " (request req_log)",
        );
        let quiet = [recorder.at(Level::INFO), recorder.at(Level::DEBUG)].concat();
        assert!(quiet.iter().all(|line| !line.contains("secret-state")), "{quiet:#?}");
        drop(installed);

        // A failure status, with a body that says something private.
        let recorder = Recorder::default();
        let installed = install(&recorder);
        let server = TestServer::start(Protocol::Http1, |_| async {
            json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"message":"secret-body"}"#)
        })
        .await
        .expect("the test server starts");
        let error =
            client_for(&server, Protocol::Http1).models().list().send().await.expect_err("503");
        assert_eq!(api_error(&error).message(), "secret-body");
        assert_timed(
            &only_info_line(&recorder),
            &format!("GET {}/v1/models <- 503 in ", server.base_url()),
            " (request -)",
        );
        let quiet = [recorder.at(Level::INFO), recorder.at(Level::DEBUG)].concat();
        assert!(quiet.iter().all(|line| !line.contains("secret-body")), "{quiet:#?}");
        drop(installed);

        // No response: a timeout, a refused connection, a body over the limit.
        let held = held(Protocol::Http1, br#"{"models":[]}"#).await;
        let refused = refusing_port().await;
        let big = answering(Protocol::Http1, StatusCode::OK, vec![b' '; 4096]).await;
        let cases: [(ClientBuilder, &str, &str); 3] = [
            (
                builder_for(&held.server, Protocol::Http1).timeout(Duration::from_millis(50)),
                held.server.base_url(),
                "timeout",
            ),
            (
                ClientBuilder::new()
                    .api_key("test-key")
                    .base_url(refused.base_url())
                    .retry(RetryPolicy::default().max_retries(0)),
                refused.base_url(),
                "connection error",
            ),
            (
                builder_for(&big, Protocol::Http1).max_response_bytes(1024),
                big.base_url(),
                "response too large",
            ),
        ];
        for (builder, base, word) in cases {
            let recorder = Recorder::default();
            let _installed = install(&recorder);
            let client = builder.build().expect("the client builds");
            let error = client.models().list().send().await.expect_err(word);
            let endpoint = format!("GET {base}/v1/models");
            assert_eq!(only_info_line(&recorder), format!("{endpoint} <- {word}"));
            // The DEBUG failure event names the kind by the same word and
            // carries no message: a connection error's message is the
            // transport's chain, which can hold text the server chose.
            let debug = recorder.at(Level::DEBUG);
            let failure = debug.last().expect("a DEBUG failure event");
            assert!(failure.contains(&format!("failure=\"{word}\"")), "{failure}");
            assert!(!failure.contains(&error.to_string()), "{failure}");
        }
        held.release.notify_waiters();
    }

    /// The events of a failing attempt, and a caller's own event holding the
    /// error it returned, carry no credential of the request in any form.
    #[tokio::test]
    async fn transport_errors_never_log_a_credential() {
        for stage in STAGES {
            for credential in CREDENTIALS {
                let case = format!("{stage:?} {credential:?}");
                let recorder = Recorder::default();
                let _installed = install(&recorder);

                let (error, _) = echo_failure(stage, Placement::Source, credential).await;
                tracing::error!(error = %error, detail = ?error);

                let text = recorder.text();
                assert!(text.contains("<- connection error"), "{case}:\n{text}");
                assert!(
                    text.contains("Rejected authorization: ***; provider: ***"),
                    "{case}:\n{text}"
                );
                for secret in [credential, &format!("Bearer {credential}"), PROVIDER_SECRET] {
                    for form in printed_forms(secret) {
                        assert!(!text.contains(&form), "{case}: {form:?} reached\n{text}");
                    }
                }
            }
        }
    }

    /// Upstream `test_secret_headers_redacted`, the nine spellings of a secret
    /// header, on the request (a client default) and on the response, for a
    /// success and a failure.
    #[tokio::test]
    async fn every_secret_header_spelling_is_redacted_both_ways() {
        let names = [
            "Authorization",
            "Proxy-Authorization",
            "X-API-Key",
            "API-Key",
            "Cookie",
            "Set-Cookie",
            "X-Access-Token",
            "X-Client-Secret",
            "x-MiXeD-ToKeN",
        ];
        for status in [StatusCode::OK, StatusCode::BAD_REQUEST] {
            for name in names {
                let recorder = Recorder::default();
                let _installed = install(&recorder);
                let server = TestServer::start(Protocol::Http1, move |_| async move {
                    let body: &'static [u8] = if status.is_success() {
                        br#"{"models":[]}"#
                    } else {
                        br#"{"message":"failure"}"#
                    };
                    let mut response = json_response(status, body);
                    let headers = response.headers_mut();
                    headers.insert(
                        http::HeaderName::from_bytes(name.as_bytes()).expect("valid"),
                        "response-credential".parse().expect("valid"),
                    );
                    headers.insert("x-visible", "response-visible".parse().expect("valid"));
                    response
                })
                .await
                .expect("the test server starts");
                let client = ClientBuilder::new()
                    .api_key("auth-credential")
                    .base_url(server.base_url())
                    .default_header(name, "request-credential")
                    .default_header("x-visible", "request-visible")
                    .build()
                    .expect("the client builds");
                let outcome = client.models().list().send().await;
                assert_eq!(outcome.is_ok(), status.is_success(), "{name} {status}");

                let text = recorder.text();
                assert!(text.contains("request-visible"), "{name} {status}:\n{text}");
                assert!(text.contains("response-visible"), "{name} {status}:\n{text}");
                assert!(text.contains("***"), "{name} {status}:\n{text}");
                for secret in ["auth-credential", "request-credential", "response-credential"] {
                    assert!(!text.contains(secret), "{name} {status}: {secret} reached\n{text}");
                }
            }
        }
    }
}
