//! The Gemini provider behind the client: the recorded exchanges replayed,
//! the retry budget, the corrective loop, and what never reaches an error, a
//! trace or a log. Every request goes to a loopback server, and every
//! provider is built with a made-up key and that server's address.

#[path = "support/cassette.rs"]
mod cassette;
#[path = "support/expected.rs"]
mod expected;
#[cfg(feature = "tracing")]
#[path = "support/recorder.rs"]
mod recorder;

use std::{
    error::Error as StdError,
    fmt,
    future::{Ready, ready},
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use decision_model_adapter::{
    AnswerMode, Answers, AttemptTrace, Client, ClientBuilder, Error, ErrorKind, GeminiProvider,
    Message, Noul, PreparedQuestions, Provider, ProviderCall, Questions, Response, RetryPolicy,
    Role, Schema, StructuredOutputs,
    decision_model_sdk::{self, ApiErrorKind},
};
use http::{HeaderValue, StatusCode, header::LOCATION};
use http_body_util::Full;
use serde_json::{Value, json};
use test_support::{
    Protocol, RecordedRequest, RefusingPort, SilentServer, TestServer, json_response,
};

use tower_service::Service;

use crate::cassette::{CASSETTES, Cassette};
#[cfg(feature = "tracing")]
use crate::recorder::{Recorder, install};

/// The made-up key every provider of this file is built with.
const KEY: &str = "sk-test-0123456789abcdef";

/// The model of the tests that replay no recording.
const MODEL: &str = "gemini-3.8-flash";

/// A text placed in the state that no error, `Debug` or event may repeat.
const SENTINEL: &str = "SENTINEL-7f3a";

/// The state upstream's transport tests ask about.
const STATE: &str = "A delightful book.";

/// A reply's text that answers [`positive`] in discrete mode.
const POSITIVE: &str = r#"{"answers":{"positive":true}}"#;

/// The body upstream's retry test answers a 503 with.
const UNAVAILABLE: &str = r#"{"error":{"message":"unavailable"}}"#;

/// The one question upstream's transport tests ask.
fn positive() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .prepare()
        .expect("one noul is a valid question set")
}

/// Upstream's `_interaction_payload` of `tests/test_gemini_transports.py`.
fn interaction(text: &str, status: &str) -> String {
    json!({
        "id": "interaction-test",
        "status": status,
        "model": MODEL,
        "steps": [{"type": "model_output", "content": [{"type": "text", "text": text}]}],
        "usage": {"total_input_tokens": 12, "total_output_tokens": 7, "total_tokens": 19},
    })
    .to_string()
}

/// A server that answers every request with `status` and `body`.
async fn answering(status: StatusCode, body: impl Into<Bytes>) -> TestServer {
    let body = body.into();
    TestServer::start(Protocol::Http1, move |_| {
        let body = body.clone();
        async move { json_response(status, body) }
    })
    .await
    .expect("a loopback server")
}

/// A provider of `model` with the made-up key that sends to `base_url`.
fn gemini_model(model: &str, base_url: &str, timeout: Duration) -> Arc<GeminiProvider> {
    let provider = GeminiProvider::builder(model)
        .api_key(KEY)
        .base_url(base_url)
        .timeout(timeout)
        .build()
        .expect("a provider");
    Arc::new(provider)
}

/// A provider of [`MODEL`] that sends to `base_url`, with a deadline no
/// loopback exchange reaches.
fn gemini(base_url: &str) -> Arc<GeminiProvider> {
    gemini_model(MODEL, base_url, Duration::from_secs(30))
}

/// A client that asks for one value per question, as upstream's transport
/// tests do.
fn discrete(structured: StructuredOutputs) -> ClientBuilder {
    Client::builder(structured, AnswerMode::Discrete)
}

/// Upstream's `RetryPolicy(max_retries=n, backoff_initial=0)`.
fn retries(max_retries: u32) -> RetryPolicy {
    RetryPolicy::new()
        .max_retries(max_retries)
        .backoff_initial(Duration::ZERO)
        .backoff_jitter(0.0)
        .expect("a jitter of zero is valid")
}

/// The JSON body of a recorded request.
fn body_of(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).expect("a JSON body")
}

/// `json`, the text of one JSON value, parsed.
fn parsed(json: Option<&str>) -> Value {
    serde_json::from_str(json.expect("recorded JSON")).expect("valid JSON")
}

/// The SDK error a provider failure carries.
fn provider_failure(error: &Error) -> &decision_model_sdk::Error {
    match error.kind() {
        ErrorKind::Provider(error) => error,
        other => panic!("expected a provider failure, got {other:?}"),
    }
}

/// How often `needle` occurs in `text`.
fn occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// `error` and every link of its `source()` chain.
fn chain<'a>(error: &'a (dyn StdError + 'static)) -> Vec<&'a (dyn StdError + 'static)> {
    let mut links = Vec::new();
    let mut link = Some(error);
    while let Some(current) = link {
        links.push(current);
        link = current.source();
    }
    links
}

// ------------------------------------------------- upstream's transport tests

#[tokio::test]
// Upstream: tests/test_gemini_transports.py::test_gemini_transport_preserves_corrections_and_usage
async fn gemini_transport_preserves_corrections_and_usage() {
    let malformed = r#"{"answers":"#;
    for structured in [StructuredOutputs::Prompted, StructuredOutputs::Native] {
        let server = TestServer::start_nth(Protocol::Http1, move |nth, _| {
            let text = if nth == 1 { malformed } else { POSITIVE };
            json_response(StatusCode::OK, interaction(text, "completed"))
        })
        .await
        .expect("a loopback server");
        let client = discrete(structured)
            .n_retry_malformed_structure(1)
            .provider_instance(gemini(server.base_url()))
            .build()
            .expect("a client");
        let questions = positive();

        let response = client.system_one(STATE, &questions).send().await.expect("an answer");

        let serialized = serde_json::to_value(&response).expect("a response serializes");
        assert_eq!(serialized["answers"]["positive"]["noul"], json!(1.0));
        let usage = response.usage();
        assert_eq!(
            (
                usage.input_tokens(),
                usage.output_tokens(),
                usage.input_tokens_total(),
                usage.output_tokens_total(),
                usage.n_retries(),
                usage.n_retries_malformed_structure(),
            ),
            (Some(12), Some(7), Some(24), Some(14), 0, 1)
        );
        let requests: Vec<Value> = server.requests().iter().map(body_of).collect();
        let attempts = response.debug().attempts();
        assert_eq!(attempts.len(), 2);
        let traced: Vec<Value> = attempts.iter().map(|attempt| parsed(attempt.request())).collect();
        assert_eq!(traced, requests);
        assert_eq!(requests[0]["store"], json!(false));
        let system = requests[0]["system_instruction"].as_str().expect("a system instruction");
        assert!(system.starts_with("Evaluate every question"), "{system}");
        if structured == StructuredOutputs::Native {
            let answers = &requests[0]["response_format"]["schema"]["$defs"]["TypeSafeAnswers"];
            assert!(answers["properties"]["positive"].is_object(), "{answers}");
        } else {
            assert_eq!(requests[0].get("response_format"), None);
        }
        let input = requests[1]["input"].as_array().expect("the steps");
        assert_eq!(
            input[input.len() - 2],
            json!({"type": "model_output", "content": [{"type": "text", "text": malformed}]})
        );
        let correction = &input[input.len() - 1];
        assert_eq!(correction["type"], json!("user_input"));
        let correction = correction["content"][0]["text"].as_str().expect("the correction");
        assert!(correction.contains("previous response did not match"), "{correction}");
    }
}

#[tokio::test]
// Upstream: tests/test_gemini_transports.py::test_gemini_incomplete_http_response_is_not_an_answer
async fn gemini_incomplete_response_is_not_an_answer() {
    let server = answering(StatusCode::OK, interaction(POSITIVE, "incomplete")).await;
    let client = discrete(StructuredOutputs::Native)
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("not an answer");

    assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)), "{error:?}");
    assert_eq!(error.to_string(), "Gemini did not answer: status incomplete");
    let attempts = error.debug().expect("a trace").attempts();
    assert_eq!(attempts.len(), 1);
    // The refused reply stays readable in the trace.
    assert_eq!(parsed(attempts[0].response())["status"], json!("incomplete"));
    assert_eq!(attempts[0].finish_reason(), Some("incomplete"));
    assert_eq!(attempts[0].error_type(), Some("NonAnswer"));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
// Upstream: tests/test_gemini_transports.py::test_gemini_transport_errors_obey_retry_budget
async fn gemini_transport_errors_obey_the_budget() {
    let refusing = RefusingPort::new().await.expect("a port nothing listens on");
    let silent = SilentServer::start().await.expect("a server that never answers");
    let silent_url = format!("http://{}", silent.addr());

    for budget in [0_u32, 1] {
        let attempts_made = budget as usize + 1;
        let build = |provider: Arc<GeminiProvider>| {
            discrete(StructuredOutputs::Prompted)
                .retry(retries(budget))
                .provider_instance(provider)
                .build()
                .expect("a client")
        };

        // No connection.
        let client = build(gemini(refusing.base_url()));
        let error = client.system_one(STATE, &positive()).send().await.expect_err("no server");
        let failure = provider_failure(&error);
        assert!(matches!(failure.kind(), decision_model_sdk::ErrorKind::Connection), "{failure:?}");
        let attempts = error.debug().expect("a trace").attempts();
        assert_eq!(attempts.len(), attempts_made);
        assert!(attempts.iter().all(|attempt| attempt.error_type() == Some("Connection")));

        // No answer before the deadline.
        let accepted = silent.accepted_connections();
        let client = build(gemini_model(MODEL, &silent_url, Duration::from_millis(100)));
        let error = client.system_one(STATE, &positive()).send().await.expect_err("no answer");
        let failure = provider_failure(&error);
        assert!(
            matches!(failure.kind(), decision_model_sdk::ErrorKind::Timeout { .. }),
            "{failure:?}"
        );
        let attempts = error.debug().expect("a trace").attempts();
        assert_eq!(attempts.len(), attempts_made);
        assert!(attempts.iter().all(|attempt| attempt.error_type() == Some("Timeout")));
        assert!(silent.accepted_connections() > accepted);
    }
}

/// Upstream's `test_retry_policy_controls_http_attempts` for one budget: a
/// server that always answers 503 is asked `budget + 1` times, and each
/// attempt is traced with its request, no response and its error.
async fn budget_controls_the_attempts(budget: u32) {
    let server = answering(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE).await;
    let client = discrete(StructuredOutputs::Prompted)
        .retry(retries(budget))
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("always 503");

    let decision_model_sdk::ErrorKind::Api(api) = provider_failure(&error).kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.kind(), ApiErrorKind::InternalServer);
    let attempts_made = budget as usize + 1;
    assert_eq!(server.request_count(), attempts_made);
    let trace = error.debug().expect("a trace");
    let attempts = trace.attempts();
    assert_eq!(attempts.len(), attempts_made);
    let requests: Vec<Value> = server.requests().iter().map(body_of).collect();
    let traced: Vec<Value> = attempts.iter().map(|attempt| parsed(attempt.request())).collect();
    assert_eq!(traced, requests);
    for attempt in attempts {
        assert_eq!(attempt.response(), None);
        assert_eq!(attempt.error_type(), Some("Api"));
        let text = attempt.error().expect("the attempt's error");
        assert!(text.contains("unavailable"), "{text}");
    }
    let serialized = serde_json::to_value(trace).expect("a trace serializes");
    assert_eq!(serialized["llm_attempts"].as_array().map(Vec::len), Some(attempts_made));
    assert_eq!(serialized["llm_attempts"][0]["llm_response"], Value::Null);
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_zero_is_one_request() {
    budget_controls_the_attempts(0).await;
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_one_is_two_requests() {
    budget_controls_the_attempts(1).await;
}

// ----------------------------------------------------------------- the key

/// One failure per way a call or a build can fail, each named. Every
/// provider holds the made-up key; the two build failures hold it in the
/// very value that is refused.
async fn failures() -> Vec<(&'static str, Error)> {
    let questions = positive();
    let ask = async |provider: Arc<GeminiProvider>| {
        discrete(StructuredOutputs::Native)
            .provider_instance(provider)
            .build()
            .expect("a client")
            .system_one(STATE, &questions)
            .send()
            .await
            .expect_err("the call fails")
    };

    let unavailable = answering(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE).await;
    let status = ask(gemini(unavailable.base_url())).await;
    assert!(matches!(provider_failure(&status).kind(), decision_model_sdk::ErrorKind::Api(_)));

    let refusing = RefusingPort::new().await.expect("a port nothing listens on");
    let connect = ask(gemini(refusing.base_url())).await;
    assert!(matches!(provider_failure(&connect).kind(), decision_model_sdk::ErrorKind::Connection));

    let silent = SilentServer::start().await.expect("a server that never answers");
    let silent_url = format!("http://{}", silent.addr());
    let timeout = ask(gemini_model(MODEL, &silent_url, Duration::from_millis(100))).await;
    assert!(matches!(
        provider_failure(&timeout).kind(),
        decision_model_sdk::ErrorKind::Timeout { .. }
    ));

    let failed = answering(StatusCode::OK, interaction(POSITIVE, "failed")).await;
    let non_answer = ask(gemini(failed.base_url())).await;
    assert!(matches!(non_answer.kind(), ErrorKind::NonAnswer(_)));

    let wrong = answering(StatusCode::OK, interaction("not the answers", "completed")).await;
    let malformed = ask(gemini(wrong.base_url())).await;
    assert!(matches!(malformed.kind(), ErrorKind::MalformedStructure));

    let build = |key: &str, base_url: &str| {
        let error = GeminiProvider::builder(MODEL)
            .api_key(key)
            .base_url(base_url)
            .build()
            .expect_err("the builder is refused");
        assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
        error
    };
    let base_url = build(KEY, &format!("http://[{KEY}/v1"));
    let header_byte = build(&format!("{KEY}\n"), "http://127.0.0.1:1");

    vec![
        ("status", status),
        ("connect", connect),
        ("timeout", timeout),
        ("non-answer", non_answer),
        ("malformed structure", malformed),
        ("base URL", base_url),
        ("header byte", header_byte),
    ]
}

#[tokio::test]
async fn key_never_printed_by_an_error_s_display() {
    for (case, error) in failures().await {
        for link in chain(&error) {
            let text = link.to_string();
            assert_eq!(occurrences(&text, KEY), 0, "{case}: {text}");
        }
    }
}

#[tokio::test]
async fn key_never_printed_by_debug() {
    for (case, error) in failures().await {
        for link in chain(&error) {
            for text in [format!("{link:?}"), format!("{link:#?}")] {
                assert_eq!(occurrences(&text, KEY), 0, "{case}: {text}");
            }
        }
    }

    // Everything that holds the provider, and the record of an attempt it
    // made.
    let server = answering(StatusCode::OK, interaction(POSITIVE, "completed")).await;
    let builder = GeminiProvider::builder(MODEL).api_key(KEY).base_url(server.base_url());
    let builder_text = format!("{builder:?} {builder:#?}");
    let provider = Arc::new(builder.build().expect("a provider"));
    let client_builder = discrete(StructuredOutputs::Native).provider_instance(provider.clone());
    let client_builder_text = format!("{client_builder:?} {client_builder:#?}");
    let client = client_builder.build().expect("a client");
    let questions = positive();
    let request = client.system_one(STATE, &questions).provider_instance(provider.clone());
    let request_text = format!("{request:?} {request:#?}");

    let schema = Schema::from_json(r#"{"type":"object"}"#).expect("a schema");
    let messages = [Message::new(Role::User, STATE)];
    let mut trace = AttemptTrace::default();
    provider
        .request(ProviderCall::new(&messages, &schema, true, &mut trace))
        .await
        .expect("the exchange succeeds")
        .expect("an answer");
    assert!(trace.response().is_some(), "the attempt was recorded");

    let texts = [
        ("the provider", format!("{provider:?} {provider:#?}")),
        ("its builder", builder_text),
        ("a client builder", client_builder_text),
        ("a client", format!("{client:?} {client:#?}")),
        ("a request", request_text),
        ("an attempt's record", format!("{trace:?} {trace:#?}")),
    ];
    for (what, text) in &texts {
        assert_eq!(occurrences(text, KEY), 0, "{what}: {text}");
    }
    // The texts are about this provider: each of the first five names it.
    for (what, text) in &texts[..5] {
        assert!(text.contains("Gemini"), "{what}: {text}");
    }
}

#[tokio::test]
async fn key_never_printed_in_the_trace() {
    let mut traces = 0;
    for (case, error) in failures().await {
        let Some(trace) = error.debug() else { continue };
        let text = serde_json::to_string(trace).expect("a trace serializes");
        assert_eq!(occurrences(&text, KEY), 0, "{case}: {text}");
        // The trace is the real one: it holds the request that was sent.
        assert!(text.contains(r#""api":"interactions""#), "{case}: {text}");
        assert!(text.contains(STATE), "{case}: {text}");
        traces += 1;
    }
    assert_eq!(traces, 5, "every failed call carries its trace");

    let server = answering(StatusCode::OK, interaction(POSITIVE, "completed")).await;
    let client = discrete(StructuredOutputs::Native)
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");
    let response = client.system_one(STATE, &positive()).send().await.expect("an answer");
    let text = serde_json::to_string(&response).expect("a response serializes");
    assert_eq!(occurrences(&text, KEY), 0, "{text}");
    assert!(text.contains("interaction-test"), "the vendor's reply is in the trace: {text}");
    // The key did travel: the server received it in its header.
    assert_eq!(server.requests()[0].header_values("x-goog-api-key"), [KEY]);
}

/// The lines of `target` among `lines`, without the target.
#[cfg(feature = "tracing")]
fn of_target(lines: &[String], target: &str) -> Vec<String> {
    let prefix = format!("{target} ");
    lines.iter().filter_map(|line| line.strip_prefix(&prefix)).map(str::to_owned).collect()
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_in_the_events() {
    let events = Recorder::default();
    let _installed = install(&events);

    let failed = failures().await.len();
    let server = answering(StatusCode::OK, interaction(POSITIVE, "completed")).await;
    let client = discrete(StructuredOutputs::Native)
        .retry(retries(1))
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");
    client.system_one(STATE, &positive()).send().await.expect("an answer");

    // Every target at every level: the adapter's, the SDK's and the HTTP
    // stack's.
    let all = events.all();
    for line in &all {
        assert_eq!(occurrences(line, KEY), 0, "{line}");
    }
    // The events are the real ones: the five failed calls and the answered
    // one each made at least one exchange, and each exchange is one `DEBUG`
    // event of the adapter that names the endpoint.
    let adapter = events.at(tracing::Level::DEBUG);
    let exchanges = adapter.iter().filter(|line| line.contains("/v1beta/interactions")).count();
    assert!(exchanges > failed - 2, "{all:#?}");
    assert!(adapter.iter().any(|line| line.contains("status=503")), "{adapter:#?}");
    // Other crates logged as well, so their lines were searched too.
    assert!(all.iter().any(|line| !line.starts_with("decision_model_adapter")), "{all:#?}");
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_by_the_retry_line_when_the_key_is_in_the_path() {
    let events = Recorder::default();
    let _installed = install(&events);
    let server = TestServer::start_nth(Protocol::Http1, |nth, _| {
        if nth == 1 {
            json_response(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE)
        } else {
            json_response(StatusCode::OK, interaction(POSITIVE, "completed"))
        }
    })
    .await
    .expect("a loopback server");
    // A caller that puts its key into the base URL's path.
    let base_url = format!("{}/{KEY}/v1", server.base_url());
    let client = discrete(StructuredOutputs::Native)
        .retry(retries(1))
        .provider_instance(gemini(&base_url))
        .build()
        .expect("a client");

    let response = client.system_one(STATE, &positive()).send().await.expect("the retry answers");

    assert_eq!(response.usage().n_retries(), 1);
    // The requests did go to the caller's path.
    let paths: Vec<String> =
        server.requests().iter().map(|request| request.uri.path().to_owned()).collect();
    let wire_path = format!("/{KEY}/v1/v1beta/interactions");
    assert_eq!(paths, [wire_path.clone(), wire_path]);

    let all = events.all();
    let retry_lines: Vec<&str> =
        all.iter().map(String::as_str).filter(|line| line.contains(" retry ")).collect();
    assert_eq!(
        retry_lines,
        [format!(
            "decision_model_sdk message=POST {}/v1beta/interactions retry 1",
            server.base_url()
        )]
    );
    // Neither the adapter's events nor the SDK's name the caller's prefix.
    for target in ["decision_model_adapter", "decision_model_sdk"] {
        let of_target = of_target(&all, target);
        assert!(!of_target.is_empty(), "{target} logged nothing");
        for line in of_target {
            assert_eq!(occurrences(&line, KEY), 0, "{target}: {line}");
        }
    }
    assert_eq!(occurrences(&serde_json::to_string(response.debug()).expect("a trace"), KEY), 0);
}

// ------------------------------------------------ a failure response's body

/// Every rendering of `error` and its chain, and its serialized trace.
fn renderings(error: &Error) -> Vec<String> {
    let mut texts = Vec::new();
    for link in chain(error) {
        texts.extend([link.to_string(), format!("{link:?}"), format!("{link:#?}")]);
    }
    texts.push(serde_json::to_string(error.debug().expect("a trace")).expect("serializes"));
    texts
}

#[tokio::test]
async fn a_failure_body_that_repeats_the_key_is_not_shown() {
    let echo = json!({"error": {"message": format!("API key not valid: {KEY}")}}).to_string();
    let server = answering(StatusCode::BAD_REQUEST, echo).await;
    let client = discrete(StructuredOutputs::Native)
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("a 400");

    let failure = provider_failure(&error);
    let decision_model_sdk::ErrorKind::Api(api) = failure.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        api.message(),
        "The response's body and headers are not shown, because showing them could reveal the API key."
    );
    assert!(failure.to_string().contains(api.message()), "{failure}");
    for text in renderings(&error) {
        assert_eq!(occurrences(&text, KEY), 0, "{text}");
    }
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_failure_body_over_the_limit_is_reported_without_it() {
    let large = json!({"error": {"message": "x".repeat(4096)}}).to_string();
    let server = answering(StatusCode::BAD_REQUEST, large).await;
    let provider = GeminiProvider::builder(MODEL)
        .api_key(KEY)
        .base_url(server.base_url())
        .max_response_bytes(256)
        .build()
        .expect("a provider");
    let client = discrete(StructuredOutputs::Native)
        .provider_instance(Arc::new(provider))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("a 400");

    let failure = provider_failure(&error);
    let decision_model_sdk::ErrorKind::Api(api) = failure.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::BAD_REQUEST);
    assert_eq!(api.message(), "The response body was larger than the limit and is not shown.");
    assert!(failure.to_string().contains(api.message()), "{failure}");
    for text in renderings(&error) {
        assert_eq!(occurrences(&text, KEY), 0, "{text}");
        assert_eq!(occurrences(&text, "xxxxxxxx"), 0, "{text}");
    }
    assert_eq!(server.request_count(), 1);
}

// ------------------------------------------------------ a caller's service

/// A caller's service that fails every call with an error whose message and
/// `Debug` spell the request's headers, the key's among them.
#[derive(Clone, Default)]
struct Leaking {
    /// The text of every error it returned.
    dumps: Arc<Mutex<Vec<String>>>,
}

/// The error of [`Leaking`]: the request's headers as text.
#[derive(Debug)]
struct HeaderDump {
    headers: String,
}

impl fmt::Display for HeaderDump {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the request with the headers [{}] failed", self.headers)
    }
}

impl StdError for HeaderDump {}

impl Service<http::Request<decision_model_sdk::Body>> for Leaking {
    type Response = http::Response<Full<Bytes>>;
    type Error = HeaderDump;
    type Future = Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<decision_model_sdk::Body>) -> Self::Future {
        // `to_str` reads a header value whether or not it is marked
        // sensitive: what a careless service would log.
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| format!("{name}: {}", value.to_str().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join(", ");
        self.dumps.lock().expect("not poisoned").push(headers.clone());
        ready(Err(HeaderDump { headers }))
    }
}

#[tokio::test]
async fn foreign_service_error_holding_the_headers_never_shows_the_key() {
    let service = Leaking::default();
    let provider = GeminiProvider::builder(MODEL)
        .api_key(KEY)
        .base_url("http://127.0.0.1:1")
        .build_with_service(service.clone())
        .expect("a provider over the caller's service");
    let client = discrete(StructuredOutputs::Native)
        .retry(retries(1))
        .provider_instance(Arc::new(provider))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("the service fails");

    // The service did receive the key and did put it into each error.
    let dumps = service.dumps.lock().expect("not poisoned").clone();
    assert_eq!(dumps.len(), 2, "the failure is retried once");
    for dump in &dumps {
        assert!(dump.contains(&format!("x-goog-api-key: {KEY}")), "{dump}");
    }

    let failure = provider_failure(&error);
    assert!(matches!(failure.kind(), decision_model_sdk::ErrorKind::Connection), "{failure:?}");
    // `source()` starts below the SDK's error, which `kind()` gives: the
    // service's error is not kept as the SDK error's cause either.
    let links = chain(&error);
    assert_eq!(links.len(), 1, "the service's error is not kept as a cause");
    for link in links.into_iter().chain([failure as &(dyn StdError + 'static)]) {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
            assert_eq!(occurrences(&text, "x-goog-api-key"), 0, "{text}");
        }
    }
    let trace = error.debug().expect("a trace");
    assert_eq!(trace.attempts().len(), 2);
    let serialized = serde_json::to_string(trace).expect("a trace serializes");
    assert_eq!(occurrences(&serialized, KEY), 0, "{serialized}");
    assert_eq!(occurrences(&format!("{trace:?} {trace:#?}"), KEY), 0);
}

// ---------------------------------------- a success reply that repeats the key

/// The non-answer of a success reply that the key search has a hit for.
const KEY_NOT_SHOWN: &str = "Gemini did not answer: the reason is not shown, because showing it \
                             could reveal the API key.";

/// [`KEY`] as a JSON string can spell it, with its last character as a
/// `\u` escape. The escape is built here from a backslash character, so no
/// tool that writes this file can turn it back into the letter.
fn key_with_an_escape() -> String {
    let last = KEY.chars().next_back().expect("a key is not empty");
    let kept = &KEY[..KEY.len() - last.len_utf8()];
    format!("{kept}{}u{:04x}", '\\', u32::from(last))
}

/// One call to a server that answers every request with `body`.
async fn key_echo(body: String) -> (Result<Response<Answers>, Error>, TestServer) {
    let server = answering(StatusCode::OK, body).await;
    let client = discrete(StructuredOutputs::Prompted)
        .retry(retries(2))
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("the client builds");
    let result = client.system_one(STATE, &positive()).send().await;
    (result, server)
}

/// `error` is the non-answer [`KEY_NOT_SHOWN`] of the one attempt `server`
/// was asked for, its recorded stop reason is cleared, [`KEY`] occurs in
/// none of the error's renderings, the attempt's error, `Debug` or
/// serialized `debug_info`, and the recorded response still holds
/// `received`, the reply's own spelling of the key.
#[track_caller]
fn assert_key_not_shown(error: &Error, server: &TestServer, received: &str) {
    assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)), "{error:?}");
    assert_eq!(error.to_string(), KEY_NOT_SHOWN);
    assert_eq!(server.request_count(), 1, "a non-answer is not asked for again");
    let mut texts = Vec::new();
    for link in chain(error) {
        texts.extend([link.to_string(), format!("{link:?}"), format!("{link:#?}")]);
    }

    let trace = error.debug().expect("the error carries a trace");
    assert_eq!(trace.attempts().len(), 1);
    let attempt = &trace.attempts()[0];
    assert_eq!(attempt.error(), Some(KEY_NOT_SHOWN));
    assert_eq!(attempt.error_type(), Some("NonAnswer"));
    assert_eq!(attempt.finish_reason(), None);
    let serialized = serde_json::to_string(attempt).expect("the attempt serializes");
    let value = parsed(Some(&serialized));
    let debug_info = &value["debug_info"];
    assert_eq!(debug_info.get("finish_reason"), Some(&Value::Null), "{debug_info}");
    assert_eq!(debug_info["error"], KEY_NOT_SHOWN);
    texts.extend([
        format!("{attempt:?}"),
        format!("{attempt:#?}"),
        format!("{trace:?}"),
        format!("{trace:#?}"),
        debug_info.to_string(),
    ]);
    for text in &texts {
        assert_eq!(occurrences(text, KEY), 0, "{text}");
    }

    // The recorded response is kept as received, the key included.
    let response = attempt.response().expect("the response is recorded");
    assert!(occurrences(response, received) > 0, "{response}");
    assert!(occurrences(&serialized, received) > 0, "{serialized}");
}

#[tokio::test]
async fn key_echo_in_the_interaction_status() {
    let (result, server) = key_echo(interaction(POSITIVE, KEY)).await;

    let error = result.expect_err("a status that is not completed");
    assert_key_not_shown(&error, &server, KEY);
}

#[tokio::test]
async fn key_echo_escaped_in_the_status() {
    let spelled = key_with_an_escape();
    let body = interaction(POSITIVE, KEY).replace(KEY, &spelled);
    assert_eq!(occurrences(&body, KEY), 0, "{body}");

    let (result, server) = key_echo(body).await;

    let error = result.expect_err("a status that is not completed");
    assert_key_not_shown(&error, &server, &spelled);
}

#[tokio::test]
async fn key_echo_in_an_unread_member_is_returned() {
    // The key in members no reader reads, beside a valid answer: an answer
    // is never searched, and the trace keeps the body as received.
    let mut payload: Value =
        serde_json::from_str(&interaction(POSITIVE, "completed")).expect("JSON");
    payload["id"] = json!(KEY);
    payload["note"] = json!({"text": KEY});

    let (result, server) = key_echo(payload.to_string()).await;

    let response = result.expect("an answer");
    assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
    assert_eq!(server.request_count(), 1);
    let attempt = &response.debug().attempts()[0];
    assert_eq!(attempt.finish_reason(), Some("completed"));
    assert_eq!(attempt.error(), None);
    let recorded = attempt.response().expect("the response is recorded");
    assert_eq!(occurrences(recorded, KEY), 2, "{recorded}");
}

// ------------------------------------------------------- the caller's data

#[tokio::test]
async fn user_data_not_printed_by_debug_display_or_an_event() {
    #[cfg(feature = "tracing")]
    let events = Recorder::default();
    #[cfg(feature = "tracing")]
    let _installed = install(&events);
    let state = json!({"review": format!("A delightful book. {SENTINEL}")});
    let questions = positive();

    // An answered call.
    let server = answering(StatusCode::OK, interaction(POSITIVE, "completed")).await;
    let client = discrete(StructuredOutputs::Native)
        .provider_instance(gemini(server.base_url()))
        .build()
        .expect("a client");
    let response = client.system_one(&state, &questions).send().await.expect("an answer");
    // The sentinel was sent and is in the serialized trace, where the caller
    // asks for it.
    assert!(server.requests()[0].body.windows(SENTINEL.len()).any(|w| w == SENTINEL.as_bytes()));
    assert!(serde_json::to_string(&response).expect("serializes").contains(SENTINEL));
    let mut texts = vec![
        ("the response", format!("{response:?} {response:#?}")),
        ("its trace", format!("{:?} {:#?}", response.debug(), response.debug())),
    ];
    for attempt in response.debug().attempts() {
        texts.push(("an attempt", format!("{attempt:?} {attempt:#?}")));
    }

    // A failed call, whose vendor body does not repeat the request.
    let unavailable = answering(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE).await;
    let client = discrete(StructuredOutputs::Native)
        .retry(retries(1))
        .provider_instance(gemini(unavailable.base_url()))
        .build()
        .expect("a client");
    let error = client.system_one(&state, &questions).send().await.expect_err("always 503");
    assert_eq!(unavailable.request_count(), 2);
    for link in chain(&error) {
        texts.push(("the error's Display", link.to_string()));
        texts.push(("the error's Debug", format!("{link:?} {link:#?}")));
    }
    let trace = error.debug().expect("a trace");
    assert!(serde_json::to_string(trace).expect("serializes").contains(SENTINEL));
    texts.push(("the error's trace", format!("{trace:?} {trace:#?}")));
    for attempt in trace.attempts() {
        texts.push(("a failed attempt", format!("{attempt:?} {attempt:#?}")));
    }

    for (what, text) in &texts {
        assert_eq!(occurrences(text, SENTINEL), 0, "{what}: {text}");
    }
    #[cfg(feature = "tracing")]
    {
        let all = events.all();
        for target in ["decision_model_adapter", "decision_model_sdk"] {
            assert!(!of_target(&all, target).is_empty(), "{target} logged nothing");
        }
        for line in &all {
            assert_eq!(occurrences(line, SENTINEL), 0, "{line}");
        }
    }
}

// ------------------------------------------------- redirects and base URLs

#[tokio::test]
async fn redirect_not_followed_to_another_host() {
    let target = answering(StatusCode::OK, interaction(POSITIVE, "completed")).await;
    let location = HeaderValue::from_str(&format!("{}/v1beta/interactions", target.base_url()))
        .expect("a header value");
    let redirecting = TestServer::start(Protocol::Http1, move |_| {
        let mut response = json_response(StatusCode::FOUND, "{}");
        response.headers_mut().insert(LOCATION, location.clone());
        async move { response }
    })
    .await
    .expect("a loopback server");
    let client = discrete(StructuredOutputs::Native)
        .retry(retries(1))
        .provider_instance(gemini(redirecting.base_url()))
        .build()
        .expect("a client");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("a 302");

    let decision_model_sdk::ErrorKind::Api(api) = provider_failure(&error).kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::FOUND);
    assert_eq!(redirecting.request_count(), 1);
    assert_eq!(target.request_count(), 0);
}

/// The text of the `Config` error a builder with `base_url` is refused with,
/// which repeats no part of the URL.
fn refused_base_url(base_url: &str) -> String {
    let error = GeminiProvider::builder(MODEL)
        .api_key(KEY)
        .base_url(base_url)
        .build()
        .expect_err("the base URL is refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    for link in chain(&error) {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            for part in [base_url, "proxy.example", "hunter2", KEY] {
                assert_eq!(occurrences(&text, part), 0, "{part}: {text}");
            }
        }
    }
    error.to_string()
}

#[test]
fn base_url_refused_with_userinfo() {
    assert_eq!(
        refused_base_url("https://user:hunter2@proxy.example/v1"),
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
}

#[test]
fn base_url_refused_with_a_fragment() {
    assert_eq!(
        refused_base_url("https://proxy.example/v1#hunter2"),
        "The base URL must not carry a fragment ('#...')."
    );
}

#[test]
fn base_url_refused_with_a_query() {
    assert_eq!(
        refused_base_url("https://proxy.example/v1?token=hunter2"),
        "The base URL must not carry a query ('?...')."
    );
}

// ------------------------------------------------------------ the cassettes

/// The test of upstream that recorded the four cassettes with an expected
/// response.
const REFERENCE_SHAPE: &str = "test_live_responses_match_reference_shape";

/// The names of the recordings of this vendor.
fn gemini_names(names: &[&'static str]) -> Vec<&'static str> {
    names.iter().copied().filter(|name| name.ends_with("-gemini]")).collect()
}

/// One recording replayed through the client.
struct Replay {
    cassette: Cassette,
    /// The one request the server received.
    request: RecordedRequest,
    response: Response<Answers>,
}

/// Replays the cassette `name`: a server answers with the recorded response,
/// and a client set up as the name says asks the recorded test's questions
/// of a provider pointed at that server. The server must have received one
/// request, equal to the recorded one in method, path and JSON body.
async fn replay(name: &str) -> Replay {
    let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
    let (status, body) = (cassette.response.status, cassette.response.body.clone());
    let server = answering(status, body).await;
    let model = cassette.request.body["model"].as_str().expect("the recorded model");
    let structured = match (name.contains("-native-"), name.contains("-prompted-")) {
        (true, false) => StructuredOutputs::Native,
        (false, true) => StructuredOutputs::Prompted,
        _ => panic!("`{name}` names no structured-output mode"),
    };
    let answer_mode = match (name.contains("[probabilities-"), name.contains("[discrete-")) {
        (true, false) => AnswerMode::Probabilities,
        (false, true) => AnswerMode::Discrete,
        _ => panic!("`{name}` names no answer mode"),
    };
    let client = Client::builder(structured, answer_mode)
        .provider_instance(gemini_model(model, server.base_url(), Duration::from_secs(30)))
        .build()
        .expect("a client");

    let response = if name.starts_with(REFERENCE_SHAPE) {
        client.system_one(expected::STATE, &expected::questions()).send().await
    } else {
        let questions = expected::context_probe_questions();
        client.system_one(expected::CONTEXT_PROBE_STATE, &questions).send().await
    }
    .unwrap_or_else(|error| panic!("`{name}` is not answered: {error}"));

    let mut requests = server.requests();
    assert_eq!(requests.len(), 1, "`{name}`: exactly one request");
    let request = requests.remove(0);
    cassette
        .matches(&request.method, &request.uri, &request.body)
        .unwrap_or_else(|mismatch| panic!("{mismatch}"));
    assert_eq!(body_of(&request), cassette.request.body);
    assert_eq!(response.model(), model);
    Replay { cassette, request, response }
}

/// Replays `name` and compares the response with the one upstream recorded
/// for the same test case.
async fn replay_against_the_reference(name: &str) {
    let replayed = replay(name).await;

    let found = serde_json::to_value(&replayed.response).expect("a response serializes");
    expected::compare(&found, &expected::read(name))
        .unwrap_or_else(|difference| panic!("`{name}`: {difference}"));
    assert_eq!(
        replayed.response.debug().attempts()[0].provider(),
        "decision_model_adapter::GeminiProvider"
    );
}

/// Replays `name` and checks the two answers that only a model which saw
/// the instructions and the criteria gives.
async fn replay_the_context_probe(name: &str) {
    let replayed = replay(name).await;

    let found = serde_json::to_value(&replayed.response).expect("a response serializes");
    for (question, choice) in [("instruction_probe", "marker_tor"), ("criteria_probe", "route_7q")]
    {
        let answer = &found["answers"][question];
        assert_eq!(answer["choice"], json!(choice), "`{name}`: {answer}");
        let probability = answer["probabilities"][choice].as_f64().expect("a probability");
        assert!(probability > 0.9, "`{name}`: {answer}");
    }
}

/// Replays the prompted cassette `name` and holds the two prompts the client
/// wrote against the recorded ones, byte for byte: as the client handed them
/// to the provider, and as the provider put them on the wire.
async fn prompts_equal_the_recorded_bytes(name: &str) {
    assert!(name.contains("-prompted-"), "`{name}` is not a prompted recording");
    let replayed = replay(name).await;

    let recorded = &replayed.cassette.request.body;
    let recorded_system = recorded["system_instruction"].as_str().expect("the recorded system");
    let recorded_user =
        recorded["input"][0]["content"][0]["text"].as_str().expect("the recorded user text");
    assert_eq!(recorded["input"].as_array().map(Vec::len), Some(1));

    let messages = replayed.response.debug().attempts()[0].messages();
    assert_eq!(messages.len(), 2);
    assert_eq!((messages[0].role(), messages[1].role()), (Role::System, Role::User));
    assert_eq!(messages[0].content().as_bytes(), recorded_system.as_bytes());
    assert_eq!(messages[1].content().as_bytes(), recorded_user.as_bytes());

    let sent = body_of(&replayed.request);
    assert_eq!(
        sent["system_instruction"].as_str().map(str::as_bytes),
        Some(recorded_system.as_bytes())
    );
    assert_eq!(
        sent["input"][0]["content"][0]["text"].as_str().map(str::as_bytes),
        Some(recorded_user.as_bytes())
    );
    // Prompted mode: the schema is in the system prompt, not beside it.
    assert_eq!(sent.get("response_format"), None);
    assert!(recorded_system.contains("TypeSafeAnswers"), "the schema is in the prompt");
}

#[test]
fn the_gemini_recordings_are_eight_and_four_have_a_reference() {
    let cassettes = gemini_names(&CASSETTES);
    assert_eq!(cassettes.len(), 8);
    let references = gemini_names(&expected::EXPECTED);
    let with_reference: Vec<&str> =
        cassettes.iter().copied().filter(|name| name.starts_with(REFERENCE_SHAPE)).collect();
    assert_eq!(references, with_reference);
    assert_eq!(references.len(), 4);
    for name in cassettes {
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(cassette.response.status, StatusCode::OK, "{name}");
    }
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_prompted() {
    replay_against_the_reference(
        "test_live_responses_match_reference_shape[probabilities-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_native() {
    replay_against_the_reference(
        "test_live_responses_match_reference_shape[probabilities-native-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_prompted() {
    replay_against_the_reference(
        "test_live_responses_match_reference_shape[discrete-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_native() {
    replay_against_the_reference(
        "test_live_responses_match_reference_shape[discrete-native-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_prompted() {
    replay_the_context_probe(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_native() {
    replay_the_context_probe(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-native-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_prompted() {
    replay_the_context_probe(
        "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_native() {
    replay_the_context_probe(
        "test_live_models_follow_question_instructions_and_criteria[discrete-native-gemini]",
    )
    .await;
}

#[tokio::test]
async fn prompt_bytes_reference_shape_probabilities() {
    prompts_equal_the_recorded_bytes(
        "test_live_responses_match_reference_shape[probabilities-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn prompt_bytes_reference_shape_discrete() {
    prompts_equal_the_recorded_bytes(
        "test_live_responses_match_reference_shape[discrete-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_probabilities() {
    prompts_equal_the_recorded_bytes(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-gemini]",
    )
    .await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_discrete() {
    prompts_equal_the_recorded_bytes(
        "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-gemini]",
    )
    .await;
}
