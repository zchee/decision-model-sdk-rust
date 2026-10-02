//! The Anthropic provider against recorded and scripted HTTP exchanges.
//!
//! Every test asks through a [`Client`] that holds the provider as an
//! explicit instance, built with a made-up key and the base URL of a server
//! on the loopback interface. No test reads the process environment and no
//! request leaves the machine.

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
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header::LOCATION};
use http_body_util::Full;
use serde_json::{Value, json};
use system_one_adapter::{
    AnswerMode, Answers, AnthropicProvider, AnthropicProviderBuilder, AttemptTrace, Client,
    ClientBuilder, Error, ErrorKind, Message, Noul, PreparedQuestions, Provider, ProviderCall,
    Questions, Response, RetryPolicy, Role, Schema, StructuredOutputs, Trace,
};
use test_support::{Protocol, RefusingPort, SilentServer, TestServer, json_response};
use tower_service::Service;
use typesafe_sdk::{Body, BoxError, ErrorKind as SdkErrorKind};

use crate::cassette::Cassette;

/// The made-up key every provider here is built with. No test may find it in
/// an error, a trace, a `Debug` rendering or an event.
const KEY: &str = "sk-test-0123456789abcdef";

/// The model the recorded cases ask.
const MODEL: &str = "claude-haiku-4-5";

/// The document of upstream's provider tests.
const DOCUMENT: &str = "A delightful book.";

/// The reply that answers [`positive`] in discrete mode.
const ANSWER: &str = r#"{"answers":{"positive":true}}"#;

/// The text placed in a state that no rendering may show.
const SENTINEL: &str = "SENTINEL-7f3a";

/// The one question of upstream's provider tests.
fn positive() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .prepare()
        .expect("one noul is a valid question set")
}

/// A builder with the made-up key and a five-second deadline, pointed at
/// `base_url`.
fn builder(base_url: &str) -> AnthropicProviderBuilder {
    AnthropicProvider::builder(MODEL)
        .api_key(KEY)
        .base_url(base_url)
        .timeout(Duration::from_secs(5))
}

fn provider(base_url: &str) -> Arc<AnthropicProvider> {
    Arc::new(builder(base_url).build().expect("the provider builds"))
}

fn structured_outputs(structured: bool) -> StructuredOutputs {
    if structured { StructuredOutputs::Native } else { StructuredOutputs::Prompted }
}

/// Upstream's `RetryPolicy(max_retries=n, backoff_initial=0)`.
fn retries(max_retries: u32) -> RetryPolicy {
    RetryPolicy::new()
        .max_retries(max_retries)
        .backoff_initial(Duration::ZERO)
        .backoff_jitter(0.0)
        .expect("a jitter of zero is valid")
}

/// The client of upstream's non-answer tests: discrete answers, two
/// corrective retries and a retry budget of two, so that a request which is
/// wrongly retried shows as a second recorded request.
fn client(structured: bool) -> ClientBuilder {
    Client::builder(structured_outputs(structured), AnswerMode::Discrete)
        .n_retry_malformed_structure(2)
        .retry(retries(2))
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

/// A Messages reply with `stop_reason` and `content`, and the token counts
/// 12 and 7.
fn reply(stop_reason: Option<&str>, content: Value) -> Value {
    json!({
        "id": "message-test",
        "type": "message",
        "role": "assistant",
        "model": "test-model",
        "stop_reason": stop_reason,
        "content": content,
        "usage": {"input_tokens": 12, "output_tokens": 7},
    })
}

/// The content of a reply that answers [`positive`].
fn answer_content() -> Value {
    json!([{"type": "text", "text": ANSWER}])
}

fn parsed(json: &str) -> Value {
    serde_json::from_str(json).expect("JSON")
}

/// Every request `server` recorded, each body as parsed JSON.
fn recorded_bodies(server: &TestServer) -> Vec<Value> {
    server
        .requests()
        .iter()
        .map(|request| serde_json::from_slice(&request.body).expect("a JSON body"))
        .collect()
}

/// How often `needle` occurs in `text`.
fn occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// The links of `error`'s `source()` chain, `error` first.
fn chain<'a>(error: &'a (dyn StdError + 'static)) -> Vec<&'a (dyn StdError + 'static)> {
    // `|&link|` copies the reference out, so the next link borrows from the
    // error and not from the iterator's own slot.
    std::iter::successors(Some(error), |&link| link.source()).collect()
}

// ------------------------------------------------- AC-P3: the stop reasons

/// One case of upstream's stop-reason test: the vendor answers 200 with
/// `stop_reason`; the reply is an answer for `end_turn`, `stop_sequence` and
/// no reason, and a non-answer that spends no retry for every other one.
async fn stop_reason_case(stop_reason: Option<&str>, structured: bool) {
    let content = if stop_reason == Some("refusal") { json!([]) } else { answer_content() };
    let payload = reply(stop_reason, content);
    let server = answering(StatusCode::OK, payload.to_string()).await;
    let client = client(structured)
        .provider_instance(provider(server.base_url()))
        .build()
        .expect("the client builds");
    let questions = positive();
    let succeeded = matches!(stop_reason, None | Some("end_turn" | "stop_sequence"));

    let outcome = client.system_one(DOCUMENT, &questions).send().await;

    let debug = match &outcome {
        Ok(response) => {
            assert!(succeeded, "{stop_reason:?} is not an answer");
            assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
            response.debug()
        }
        Err(error) => {
            assert!(!succeeded, "{stop_reason:?} is an answer: {error}");
            assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)), "{error:?}");
            let reason = stop_reason.expect("a failing case names its reason");
            let message = error.to_string();
            assert!(
                message.starts_with(&format!("Anthropic did not answer: stop reason {reason}")),
                "{message}"
            );
            assert_eq!(message.contains("raise max_tokens"), reason == "max_tokens", "{message}");
            let debug = error.debug().expect("a failed attempt leaves a trace");
            assert!(debug.retry_reasons().is_empty(), "a non-answer spends no retry");
            debug
        }
    };

    let requests = recorded_bodies(&server);
    assert_eq!(requests.len(), 1, "exactly one request");
    assert_eq!(debug.attempts().len(), 1, "exactly one attempt");
    let attempt = &debug.attempts()[0];
    assert_eq!(parsed(attempt.request().expect("a recorded request")), requests[0]);
    assert_eq!(requests[0].get("output_config").is_some(), structured);
    assert_eq!(attempt.api(), Some("messages"));
    let response = parsed(attempt.response().expect("the raw response"));
    assert_eq!(response, payload);
    assert_eq!(response["stop_reason"], json!(stop_reason));
    assert_eq!(attempt.finish_reason(), stop_reason);
    assert_eq!(attempt.error().is_some(), !succeeded);
    assert_eq!(attempt.error_type(), (!succeeded).then_some("NonAnswer"));
    serde_json::to_string(debug).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_end_turn_native() {
    stop_reason_case(Some("end_turn"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_end_turn_prompted() {
    stop_reason_case(Some("end_turn"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_stop_sequence_native() {
    stop_reason_case(Some("stop_sequence"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_stop_sequence_prompted() {
    stop_reason_case(Some("stop_sequence"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_no_stop_reason_native() {
    stop_reason_case(None, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_no_stop_reason_prompted() {
    stop_reason_case(None, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_refusal_native() {
    stop_reason_case(Some("refusal"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_refusal_prompted() {
    stop_reason_case(Some("refusal"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_model_context_window_exceeded_native() {
    stop_reason_case(Some("model_context_window_exceeded"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_model_context_window_exceeded_prompted() {
    stop_reason_case(Some("model_context_window_exceeded"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_pause_turn_native() {
    stop_reason_case(Some("pause_turn"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_pause_turn_prompted() {
    stop_reason_case(Some("pause_turn"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_tool_use_native() {
    stop_reason_case(Some("tool_use"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_tool_use_prompted() {
    stop_reason_case(Some("tool_use"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_unknown_native() {
    stop_reason_case(Some("unknown"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_unknown_prompted() {
    stop_reason_case(Some("unknown"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_max_tokens_native() {
    stop_reason_case(Some("max_tokens"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_anthropic_nonanswers
async fn nonanswer_max_tokens_prompted() {
    stop_reason_case(Some("max_tokens"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_anthropic_output_limit
async fn a_reply_cut_at_the_output_limit_is_not_asked_for_again() {
    for structured in [false, true] {
        for truncated in [false, true] {
            let stop_reason = if truncated { "max_tokens" } else { "end_turn" };
            let payload = reply(Some(stop_reason), answer_content());
            let server = answering(StatusCode::OK, payload.to_string()).await;
            let provider = builder(server.base_url()).max_tokens(8192).build().expect("it builds");
            let client = client(structured)
                .provider_instance(Arc::new(provider))
                .build()
                .expect("the client builds");
            let questions = positive();

            let outcome = client.system_one(DOCUMENT, &questions).send().await;

            let debug = match &outcome {
                Ok(response) => {
                    assert!(!truncated);
                    assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
                    response.debug()
                }
                Err(error) => {
                    // Even valid JSON must not hide a reply that was cut
                    // short, and neither retry budget is spent on it.
                    assert!(truncated, "{error}");
                    assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)), "{error:?}");
                    let message = error.to_string();
                    assert!(message.contains("stop reason max_tokens"), "{message}");
                    assert!(message.contains("raise max_tokens"), "{message}");
                    error.debug().expect("a failed attempt leaves a trace")
                }
            };
            let requests = recorded_bodies(&server);
            assert_eq!(requests.len(), 1, "exactly one request");
            assert_eq!(requests[0]["max_tokens"], 8192);
            assert_eq!(debug.attempts().len(), 1);
            let attempt = &debug.attempts()[0];
            assert_eq!(parsed(attempt.request().expect("a recorded request")), requests[0]);
            let response = parsed(attempt.response().expect("the raw response"));
            assert_eq!(response["content"][0]["text"], ANSWER);
            assert_eq!(attempt.finish_reason(), Some(stop_reason));
            assert_eq!(attempt.error().is_some(), truncated);
        }
    }
}

// ------------------------------------------------ AC-P2: the retry budget

/// A vendor that answers 503 every time is asked `budget + 1` times, and
/// every attempt is in the error's trace.
async fn budget_case(budget: u32) {
    let server =
        answering(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"unavailable"}}"#).await;
    let client = Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete)
        .provider_instance(provider(server.base_url()))
        .retry(retries(budget))
        .build()
        .expect("the client builds");
    let questions = positive();

    let error =
        client.system_one(DOCUMENT, &questions).send().await.expect_err("every attempt fails");

    let ErrorKind::Provider(failure) = error.kind() else {
        panic!("expected a provider failure, got {error:?}");
    };
    let SdkErrorKind::Api(api) = failure.kind() else {
        panic!("expected an API error, got {failure:?}");
    };
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);

    let expected = usize::try_from(budget).expect("a small budget") + 1;
    let requests = recorded_bodies(&server);
    assert_eq!(server.request_count(), expected);
    let debug = error.debug().expect("the failed attempts leave a trace");
    assert_eq!(debug.attempts().len(), expected);
    for (attempt, request) in debug.attempts().iter().zip(&requests) {
        assert_eq!(&parsed(attempt.request().expect("a recorded request")), request);
        assert_eq!(attempt.response(), None);
        assert_eq!(attempt.finish_reason(), None);
        assert_eq!(attempt.error_type(), Some("Api"));
        let message = attempt.error().expect("the attempt failed");
        assert!(message.contains("unavailable"), "{message}");
    }
    serde_json::to_string(debug).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_zero_sends_one_request() {
    budget_case(0).await;
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_one_sends_two_requests() {
    budget_case(1).await;
}

// ------------------------------------- AC-X4, AC-P5, AC-X5b: the cassettes

/// The recorded case `name` asked again: the server answers with the
/// cassette's response, the client is set as the case's name says.
struct Replay {
    cassette: Cassette,
    server: TestServer,
    response: Response<Answers>,
}

impl Replay {
    async fn run(name: &str) -> Self {
        assert!(cassette::CASSETTES.contains(&name), "`{name}` is not a listed cassette");
        assert!(name.ends_with("-anthropic]"), "`{name}` is not an Anthropic case");
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        let (status, body) = (cassette.response.status, cassette.response.body.clone());
        let server = answering(status, body).await;

        let structured = if name.contains("-native-") {
            StructuredOutputs::Native
        } else {
            assert!(name.contains("-prompted-"), "`{name}` names no output mode");
            StructuredOutputs::Prompted
        };
        let mode = if name.contains("[probabilities-") {
            AnswerMode::Probabilities
        } else {
            assert!(name.contains("[discrete-"), "`{name}` names no answer mode");
            AnswerMode::Discrete
        };
        let (state, questions) = if name.starts_with("test_live_responses_match_reference_shape[") {
            (expected::STATE, expected::questions())
        } else {
            (expected::CONTEXT_PROBE_STATE, expected::context_probe_questions())
        };
        let client = Client::builder(structured, mode)
            .provider_instance(provider(server.base_url()))
            .build()
            .expect("the client builds");

        let response = client
            .system_one(state, &questions)
            .send()
            .await
            .unwrap_or_else(|error| panic!("`{name}` does not replay: {error}"));
        Self { cassette, server, response }
    }
}

/// AC-X4 and AC-P5 for the cassette `name`: one request, equal to the
/// recorded one in method, path, query and JSON body; the answers upstream's
/// test asserts; and, for a case with an expected response, a response equal
/// to it under upstream's field rules.
async fn replay_case(name: &str) {
    let Replay { cassette, server, response } = Replay::run(name).await;

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    let request = &requests[0];
    if let Err(mismatch) = cassette.matches(&request.method, &request.uri, &request.body) {
        panic!("{mismatch}");
    }
    assert_eq!(request.uri.path(), "/v1/messages");
    assert_eq!(request.header_values("anthropic-version"), ["2023-06-01"]);
    assert_eq!(response.model(), MODEL);

    let answers = response.answers();
    if expected::EXPECTED.contains(&name) {
        let positive = answers.noul("positive").expect("a noul").noul();
        let rating = answers.score("rating").expect("a score").probability(4);
        let genre = answers.choice("genre").expect("a choice").probability("fiction");
        assert!(positive > 0.9, "positive: {positive}");
        assert!(rating.is_some_and(|probability| probability > 0.9), "rating: {rating:?}");
        assert!(genre.is_some_and(|probability| probability > 0.9), "genre: {genre:?}");

        let found = serde_json::to_value(&response).expect("the response serializes");
        if let Err(difference) = expected::compare(&found, &expected::read(name)) {
            panic!("`{name}` differs from upstream's response {difference}");
        }
    } else {
        for (question, choice) in
            [("instruction_probe", "marker_tor"), ("criteria_probe", "route_7q")]
        {
            let answer = answers.choice(question).expect("a choice");
            assert_eq!(answer.choice(), choice, "{question}");
            let probability = answer.probability(choice);
            assert!(probability.is_some_and(|p| p > 0.9), "{question}: {probability:?}");
        }
    }
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_prompted() {
    replay_case("test_live_responses_match_reference_shape[probabilities-prompted-anthropic]")
        .await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_native() {
    replay_case("test_live_responses_match_reference_shape[probabilities-native-anthropic]").await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_prompted() {
    replay_case("test_live_responses_match_reference_shape[discrete-prompted-anthropic]").await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_native() {
    replay_case("test_live_responses_match_reference_shape[discrete-native-anthropic]").await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_prompted() {
    replay_case(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-anthropic]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_native() {
    replay_case(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-native-anthropic]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_prompted() {
    replay_case(
        "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-anthropic]",
    )
    .await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_native() {
    replay_case(
        "test_live_models_follow_question_instructions_and_criteria[discrete-native-anthropic]",
    )
    .await;
}

/// AC-X5b for the prompted cassette `name`: the system string and the user
/// string the client sent are the recorded ones, byte for byte.
async fn prompt_case(name: &str) {
    let Replay { cassette, server, response } = Replay::run(name).await;
    let recorded = &cassette.request.body;
    let recorded_system = recorded["system"].as_str().expect("a recorded system string");
    let recorded_messages = recorded["messages"].as_array().expect("recorded messages");
    assert_eq!(recorded_messages.len(), 1, "one user turn is recorded");
    let recorded_user = recorded_messages[0]["content"].as_str().expect("a recorded user string");
    assert!(recorded.get("output_config").is_none(), "`{name}` is not a prompted case");

    let sent = recorded_bodies(&server);
    assert_eq!(sent.len(), 1, "exactly one request");
    let sent_system = sent[0]["system"].as_str().expect("a system string");
    let sent_user = sent[0]["messages"][0]["content"].as_str().expect("a user string");
    assert_eq!(sent_system.as_bytes(), recorded_system.as_bytes());
    assert_eq!(sent_user.as_bytes(), recorded_user.as_bytes());
    assert_eq!(sent[0]["messages"][0]["role"], "user");
    assert_eq!(sent[0]["messages"].as_array().map(Vec::len), Some(1));

    // The trace holds the same two strings as the messages of the attempt.
    let messages = response.debug().attempts()[0].messages();
    let texts: Vec<(Role, &str)> =
        messages.iter().map(|message| (message.role(), message.content())).collect();
    assert_eq!(texts, [(Role::System, recorded_system), (Role::User, recorded_user)]);
}

#[tokio::test]
async fn prompt_bytes_reference_shape_probabilities() {
    prompt_case("test_live_responses_match_reference_shape[probabilities-prompted-anthropic]")
        .await;
}

#[tokio::test]
async fn prompt_bytes_reference_shape_discrete() {
    prompt_case("test_live_responses_match_reference_shape[discrete-prompted-anthropic]").await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_probabilities() {
    prompt_case(
        "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-anthropic]",
    )
    .await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_discrete() {
    prompt_case(
        "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-anthropic]",
    )
    .await;
}

// ------------------------------------------------ AC-P11: transport rules

#[tokio::test]
async fn redirect_not_followed() {
    let target =
        answering(StatusCode::OK, reply(Some("end_turn"), answer_content()).to_string()).await;
    let location = HeaderValue::from_str(&format!("{}/v1/messages", target.base_url()))
        .expect("a URL is a header value");
    let server = TestServer::start(Protocol::Http1, move |_| {
        let location = location.clone();
        async move {
            let mut response = json_response(StatusCode::FOUND, "{}");
            response.headers_mut().insert(LOCATION, location);
            response
        }
    })
    .await
    .expect("a loopback server");
    let client = client(false)
        .provider_instance(provider(server.base_url()))
        .build()
        .expect("the client builds");
    let questions = positive();

    let error = client.system_one(DOCUMENT, &questions).send().await.expect_err("a redirect");

    let ErrorKind::Provider(failure) = error.kind() else {
        panic!("expected a provider failure, got {error:?}");
    };
    let SdkErrorKind::Api(api) = failure.kind() else {
        panic!("expected an API error, got {failure:?}");
    };
    assert_eq!(api.status(), StatusCode::FOUND);
    assert_eq!(server.request_count(), 1, "the redirect is neither followed nor retried");
    assert_eq!(target.request_count(), 0, "no request reached the second host");
}

/// A base URL the shared rules refuse is a `Config` error at `build()`, and
/// no rendering of the error repeats the URL or one of its `parts`.
#[track_caller]
fn refused_base_url(base_url: &str, parts: &[&str], expected: &str) {
    let error = builder(base_url).build().expect_err("the base URL is refused");

    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert_eq!(error.to_string(), expected);
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert_eq!(occurrences(&text, base_url), 0, "{text}");
        for part in parts {
            assert_eq!(occurrences(&text, part), 0, "{text}");
        }
    }
}

#[test]
fn base_url_refused_with_userinfo() {
    refused_base_url(
        "https://alice:hunter2@proxy.example/v1",
        &["alice", "hunter2", "proxy.example"],
        "The base URL must not carry credentials; pass the API key on its own instead.",
    );
}

#[test]
fn base_url_refused_with_a_fragment() {
    refused_base_url(
        "https://proxy.example/v1#token-9f2",
        &["token-9f2", "proxy.example"],
        "The base URL must not carry a fragment ('#...').",
    );
}

#[test]
fn base_url_refused_with_a_query() {
    refused_base_url(
        "https://proxy.example/v1?api_key=k-77",
        &["api_key", "k-77", "proxy.example"],
        "The base URL must not carry a query ('?...').",
    );
}

// ------------------------------------------- AC-P10: a caller's own service

/// A caller's service that fails every call with an error whose message and
/// `Debug` hold the request's headers, the key among them.
#[derive(Clone)]
struct Leaking;

/// The error of [`Leaking`]: `Display` and `Debug` both print the headers.
#[derive(Debug)]
struct Leaked {
    headers: String,
}

impl fmt::Display for Leaked {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the proxy refused the request with headers {}", self.headers)
    }
}

impl StdError for Leaked {}

impl Leaked {
    /// The headers of `request` as text. A sensitive value prints as
    /// `Sensitive` through `Debug`, so a service that wants the text reads
    /// the bytes, as this one does.
    fn of(request: &Request<Body>) -> Self {
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| format!("{name}: {}", String::from_utf8_lossy(value.as_bytes())))
            .collect::<Vec<_>>()
            .join("; ");
        Self { headers }
    }
}

impl Service<Request<Body>> for Leaking {
    type Response = http::Response<Full<Bytes>>;
    type Error = BoxError;
    type Future = Ready<Result<Self::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ready(Err(Box::new(Leaked::of(&request))))
    }
}

#[tokio::test]
async fn foreign_service_error_holding_the_headers_shows_no_key() {
    // The service's error does hold the key, so the check below is not
    // vacuous.
    let mut request = Request::new(Body::from(Bytes::new()));
    request.headers_mut().insert("x-api-key", HeaderValue::from_static(KEY));
    let leaked = Leaked::of(&request);
    assert_eq!(occurrences(&leaked.to_string(), KEY), 1);
    assert_eq!(occurrences(&format!("{leaked:?}"), KEY), 1);

    let provider =
        builder("https://proxy.example").build_with_service(Leaking).expect("the provider builds");
    let client =
        client(true).provider_instance(Arc::new(provider)).build().expect("the client builds");
    let questions = positive();

    let error = client
        .system_one(DOCUMENT, &questions)
        .send()
        .await
        .expect_err("the service fails every call");

    let ErrorKind::Provider(failure) = error.kind() else {
        panic!("expected a provider failure, got {error:?}");
    };
    assert!(matches!(failure.kind(), SdkErrorKind::Connection), "{failure:?}");
    let links = chain(&error);
    assert!(links.len() >= 2, "the SDK's error is the source");
    for link in links {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
            assert_eq!(occurrences(&text, "x-api-key"), 0, "{text}");
        }
    }
    // The connection failure was retried twice, and no attempt's record
    // holds the key either.
    let debug = error.debug().expect("the failed attempts leave a trace");
    assert_eq!(debug.attempts().len(), 3);
    let trace = serde_json::to_string(debug).expect("the trace serializes");
    assert_eq!(occurrences(&trace, KEY), 0);
}

// ------------------------- a failure response the HTTP module does not show

/// What the two cases below share: one call through a client without a
/// retry budget fails with an API error of `status` whose text is `status`
/// and then the fixed `sentence`; one request was made; and the key is in no
/// rendering of the error, of a link below it, or of the serialized trace.
async fn assert_fixed_failure_text(
    server: &TestServer,
    provider: AnthropicProvider,
    status: StatusCode,
    sentence: &str,
) {
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(Arc::new(provider))
        .build()
        .expect("the client builds");
    let questions = positive();

    let error = client.system_one(DOCUMENT, &questions).send().await.expect_err("a failure status");

    let ErrorKind::Provider(failure) = error.kind() else {
        panic!("expected a provider failure, got {error:?}");
    };
    let SdkErrorKind::Api(api) = failure.kind() else {
        panic!("expected an API error, got {failure:?}");
    };
    assert_eq!(api.status(), status);
    assert_eq!(api.message(), sentence);
    assert_eq!(error.to_string(), format!("{} {sentence}", status.as_u16()));
    assert_eq!(server.request_count(), 1);
    for link in chain(&error) {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
        }
    }
    let debug = error.debug().expect("the failed attempt leaves a trace");
    assert_eq!(debug.attempts().len(), 1);
    assert_eq!(debug.attempts()[0].error(), Some(error.to_string().as_str()));
    assert_eq!(debug.attempts()[0].response(), None);
    let trace = serde_json::to_string(debug).expect("the trace serializes");
    assert_eq!(occurrences(&trace, KEY), 0, "the serialized trace holds the key");
}

#[tokio::test]
async fn a_failure_body_over_the_size_limit_is_reported_with_a_fixed_text() {
    // 65 bytes of JSON against a limit of 64.
    let body = format!("\"{}\"", "x".repeat(63));
    assert_eq!(body.len(), 65);
    let server = answering(StatusCode::BAD_GATEWAY, body).await;
    let provider =
        builder(server.base_url()).max_response_bytes(64).build().expect("the provider builds");

    assert_fixed_failure_text(
        &server,
        provider,
        StatusCode::BAD_GATEWAY,
        "The response body was larger than the limit and is not shown.",
    )
    .await;
}

#[tokio::test]
async fn a_failure_body_that_repeats_the_key_is_not_shown() {
    // A gateway that quotes the header it refused.
    let body = json!({
        "type": "error",
        "error": {"type": "authentication_error", "message": format!("invalid x-api-key: {KEY}")},
    })
    .to_string();
    assert_eq!(occurrences(&body, KEY), 1);
    let server = answering(StatusCode::UNAUTHORIZED, body).await;
    let provider = builder(server.base_url()).build().expect("the provider builds");

    assert_fixed_failure_text(
        &server,
        provider,
        StatusCode::UNAUTHORIZED,
        "The response's body and headers are not shown, \
         because showing them could reveal the API key.",
    )
    .await;
}

// ------------------------------------------- AC-P4: the key is never printed

/// One error of every path a call or a build can fail on, each named.
async fn error_paths() -> Vec<(&'static str, Error)> {
    let mut errors = Vec::new();
    let ask = async |provider: Arc<AnthropicProvider>| {
        Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
            .provider_instance(provider)
            .build()
            .expect("the client builds")
            .system_one(DOCUMENT, &positive())
            .send()
            .await
    };

    let unauthorized = answering(
        StatusCode::UNAUTHORIZED,
        r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
    )
    .await;
    errors.push(("status", ask(provider(unauthorized.base_url())).await.expect_err("a 401")));

    let refusing = RefusingPort::new().await.expect("a refusing port");
    errors.push(("connect", ask(provider(refusing.base_url())).await.expect_err("no listener")));

    let silent = SilentServer::start().await.expect("a silent server");
    let impatient = builder(&format!("http://{}", silent.addr()))
        .timeout(Duration::from_millis(100))
        .build()
        .expect("the provider builds");
    errors.push(("timeout", ask(Arc::new(impatient)).await.expect_err("no answer in time")));

    let refusal = answering(StatusCode::OK, reply(Some("refusal"), json!([])).to_string()).await;
    errors.push(("non-answer", ask(provider(refusal.base_url())).await.expect_err("a refusal")));

    let malformed = answering(
        StatusCode::OK,
        reply(Some("end_turn"), json!([{"type": "text", "text": r#"{"answers":{}}"#}])).to_string(),
    )
    .await;
    errors.push((
        "malformed structure",
        ask(provider(malformed.base_url())).await.expect_err("no answer for the question"),
    ));

    // A base URL that holds the key and does not parse, and a key that
    // cannot be a header value.
    errors.push((
        "config: base URL",
        builder(&format!("http://127.0.0.1:1/{KEY} /v1")).build().expect_err("a space in a URL"),
    ));
    errors.push((
        "config: key",
        AnthropicProvider::builder(MODEL)
            .api_key(format!("{KEY}\n{KEY}"))
            .base_url("http://127.0.0.1:1")
            .build()
            .expect_err("a line feed in a key"),
    ));

    let kinds: Vec<&str> = errors
        .iter()
        .map(|(_, error)| match error.kind() {
            ErrorKind::Provider(failure) => match failure.kind() {
                SdkErrorKind::Api(_) => "api",
                SdkErrorKind::Connection => "connection",
                SdkErrorKind::Timeout { .. } => "timeout",
                _ => "another provider failure",
            },
            ErrorKind::NonAnswer(_) => "non-answer",
            ErrorKind::MalformedStructure => "malformed structure",
            ErrorKind::Config => "config",
            _ => "another kind",
        })
        .collect();
    assert_eq!(
        kinds,
        ["api", "connection", "timeout", "non-answer", "malformed structure", "config", "config"],
        "every error path is reached"
    );
    errors
}

#[tokio::test]
async fn key_never_printed_in_an_error_display() {
    for (path, error) in error_paths().await {
        for link in chain(&error) {
            let text = link.to_string();
            assert_eq!(occurrences(&text, KEY), 0, "{path}: {text}");
        }
    }
}

#[tokio::test]
async fn key_never_printed_in_an_error_debug() {
    for (path, error) in error_paths().await {
        for link in chain(&error) {
            for text in [format!("{link:?}"), format!("{link:#?}")] {
                assert_eq!(occurrences(&text, KEY), 0, "{path}: {text}");
            }
        }
    }
}

#[tokio::test]
async fn key_never_printed_in_a_trace_or_a_debug_of_what_holds_it() {
    // The trace of every failed call.
    let mut traces = 0;
    for (path, error) in error_paths().await {
        if let Some(debug) = error.debug() {
            traces += 1;
            let trace = serde_json::to_string(debug).expect("the trace serializes");
            assert_eq!(occurrences(&trace, KEY), 0, "{path}: {trace}");
            assert_eq!(occurrences(&format!("{debug:?}"), KEY), 0, "{path}");
        }
    }
    assert_eq!(traces, 5, "the five failed calls leave a trace, the two failed builds none");

    // The response of a call that succeeds, whole.
    let server =
        answering(StatusCode::OK, reply(Some("end_turn"), answer_content()).to_string()).await;
    let builder = builder(server.base_url());
    let builder_text = format!("{builder:?} {builder:#?}");
    let provider = Arc::new(builder.build().expect("the provider builds"));
    let client_builder = client(true).provider_instance(provider.clone());
    let client_builder_text = format!("{client_builder:?} {client_builder:#?}");
    let client = client_builder.build().expect("the client builds");
    let questions = positive();
    let request = client.system_one(DOCUMENT, &questions).provider_instance(provider.clone());
    let request_text = format!("{request:?} {request:#?}");
    let response = request.send().await.expect("an answer");
    let serialized = serde_json::to_string(&response).expect("the response serializes");
    assert!(serialized.contains(DOCUMENT), "the trace holds the document");

    // One attempt made outside the client, for its own record.
    let schema = Schema::from_json(r#"{"type":"object"}"#).expect("a JSON object");
    let messages = [Message::new(Role::User, DOCUMENT)];
    let mut attempt = AttemptTrace::default();
    provider
        .request(ProviderCall::new(&messages, &schema, false, &mut attempt))
        .await
        .expect("an exchange")
        .expect("an answer");
    assert!(attempt.request().is_some() && attempt.response().is_some());

    assert_eq!(server.request_count(), 2);
    assert_eq!(server.requests()[0].header_values("x-api-key"), [KEY], "the key was sent");
    for (what, text) in [
        ("the response", serialized),
        ("the response's Debug", format!("{response:?} {response:#?}")),
        ("the provider builder", builder_text),
        ("the provider", format!("{provider:?} {provider:#?}")),
        ("the client builder", client_builder_text),
        ("the client", format!("{client:?} {client:#?}")),
        ("the request", request_text),
        ("the attempt's record", format!("{attempt:?} {attempt:#?}")),
    ] {
        assert!(!text.is_empty(), "{what}");
        assert_eq!(occurrences(&text, KEY), 0, "{what}: {text}");
    }
}

/// The lines of `lines` whose target is `target`: a recorded line starts
/// with its target.
#[cfg(feature = "tracing")]
fn of_target<'a>(lines: &'a [String], target: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter(|line| line.strip_prefix(target).is_some_and(|rest| rest.starts_with(' ')))
        .map(String::as_str)
        .collect()
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_in_an_event_of_any_target() {
    let recorded = recorder::Recorder::default();
    let _installed = recorder::install(&recorded);

    // Every error path, and an answer over HTTP/1.1.
    let paths = error_paths().await.len();
    let plain =
        answering(StatusCode::OK, reply(Some("end_turn"), answer_content()).to_string()).await;
    let questions = positive();
    client(true)
        .provider_instance(provider(&format!("{}/tenant-4b1e", plain.base_url())))
        .build()
        .expect("the client builds")
        .system_one(DOCUMENT, &questions)
        .send()
        .await
        .expect("an answer over HTTP/1.1");

    // An answer over HTTP/2 with TLS, where the header travels through
    // HPACK and h2 logs its frames.
    let body = reply(Some("end_turn"), answer_content()).to_string();
    let secure = TestServer::start(Protocol::Http2Tls, move |_| {
        let body = body.clone();
        async move { json_response(StatusCode::OK, body) }
    })
    .await
    .expect("a TLS server");
    let root = secure.certificate_der().expect("a TLS server has a certificate").to_vec();
    let over_tls = builder(&format!("{}/tenant-4b1e", secure.base_url()))
        .add_root_certificate(root)
        .build()
        .expect("the provider builds");
    client(true)
        .provider_instance(Arc::new(over_tls))
        .build()
        .expect("the client builds")
        .system_one(DOCUMENT, &questions)
        .send()
        .await
        .expect("an answer over HTTP/2");
    assert_eq!(secure.requests()[0].version, http::Version::HTTP_2);
    assert_eq!(secure.requests()[0].header_values("x-api-key"), [KEY], "the key was sent");
    assert_eq!(plain.requests()[0].header_values("x-api-key"), [KEY], "the key was sent");

    let lines = recorded.all();
    for line in &lines {
        assert_eq!(occurrences(line, KEY), 0, "{line}");
        assert_eq!(occurrences(line, "tenant-4b1e"), 0, "{line}");
    }
    // The capture is not empty: the adapter logged one exchange per request
    // that reached the transport and one line per attempt.
    let adapter = recorded.at(tracing::Level::DEBUG);
    let exchanges = adapter.iter().filter(|line| line.contains(" method=POST ")).count();
    let attempts = adapter.iter().filter(|line| line.contains(" attempt=")).count();
    assert_eq!(paths, 7);
    assert_eq!(exchanges, 7, "five failed calls and two answers: {adapter:#?}");
    assert_eq!(attempts, 7, "{adapter:#?}");
    assert!(lines.len() > adapter.len(), "other targets log too: {} lines", lines.len());
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_in_the_retry_line_of_a_base_url_that_holds_it() {
    let recorded = recorder::Recorder::default();
    let _installed = recorder::install(&recorded);
    let body = reply(Some("end_turn"), answer_content()).to_string();
    let server = TestServer::start_nth(Protocol::Http1, move |served, _| match served {
        1 => {
            json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"unavailable"}}"#)
        }
        _ => json_response(StatusCode::OK, body.clone()),
    })
    .await
    .expect("a loopback server");
    // The key as a path segment of the base URL.
    let base_url = format!("{}/{KEY}/v1", server.base_url());
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(provider(&base_url))
        .retry(retries(1))
        .build()
        .expect("the client builds");
    let questions = positive();

    let response = client.system_one(DOCUMENT, &questions).send().await.expect("the retry answers");

    assert_eq!(response.usage().n_retries(), 1);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].uri.path(), format!("/{KEY}/v1/v1/messages"), "the prefix is kept");

    let port = server.addr().port();
    let lines = recorded.all();
    let sdk = of_target(&lines, "typesafe_sdk");
    let retries: Vec<&str> = sdk.iter().copied().filter(|line| line.contains(" retry ")).collect();
    assert_eq!(retries.len(), 1, "exactly one retry line: {sdk:#?}");
    assert!(
        retries[0].contains(&format!("POST http://127.0.0.1:{port}/v1/messages retry 1")),
        "the line names the vendor's fixed path: {}",
        retries[0]
    );
    for line in &lines {
        assert_eq!(occurrences(line, KEY), 0, "{line}");
    }
    // The key in the trace's bodies is not this test's subject: no body
    // holds a header or a URL.
    let trace = serde_json::to_string(response.debug()).expect("the trace serializes");
    assert_eq!(occurrences(&trace, KEY), 0);
}

// ---------------------------------------- a success reply that repeats the key

/// The non-answer of a success reply that the key search has a hit for.
const KEY_NOT_SHOWN: &str = "Anthropic did not answer: the reason is not shown, because showing it \
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
    let client = client(false)
        .provider_instance(provider(server.base_url()))
        .build()
        .expect("the client builds");
    let result = client.system_one(DOCUMENT, &positive()).send().await;
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
    let value = parsed(&serialized);
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
async fn key_echo_in_the_stop_reason() {
    let (result, server) = key_echo(reply(Some(KEY), answer_content()).to_string()).await;

    let error = result.expect_err("a stop reason that is not an answer");
    assert_key_not_shown(&error, &server, KEY);
}

#[tokio::test]
async fn key_echo_escaped_in_the_stop_reason() {
    let spelled = key_with_an_escape();
    let body = reply(Some(KEY), answer_content()).to_string().replace(KEY, &spelled);
    assert_eq!(occurrences(&body, KEY), 0, "{body}");

    let (result, server) = key_echo(body).await;

    let error = result.expect_err("a stop reason that is not an answer");
    assert_key_not_shown(&error, &server, &spelled);
}

#[tokio::test]
async fn key_echo_in_an_unread_member_is_returned() {
    // The key in members no reader reads, beside a valid answer: an answer
    // is never searched, and the trace keeps the body as received.
    let mut payload = reply(
        Some("end_turn"),
        json!([{"type": "thinking", "thinking": KEY}, {"type": "text", "text": ANSWER}]),
    );
    payload["id"] = json!(KEY);

    let (result, server) = key_echo(payload.to_string()).await;

    let response = result.expect("an answer");
    assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
    assert_eq!(server.request_count(), 1);
    let attempt = &response.debug().attempts()[0];
    assert_eq!(attempt.finish_reason(), Some("end_turn"));
    assert_eq!(attempt.error(), None);
    let recorded = attempt.response().expect("the response is recorded");
    assert_eq!(occurrences(recorded, KEY), 2, "{recorded}");
}

// ------------------------------------------------- AC-P9: the caller's data

/// The `Debug` renderings of a trace and of each of its attempts.
fn trace_renderings(debug: &Trace) -> Vec<String> {
    let mut texts = vec![format!("{debug:?}"), format!("{debug:#?}")];
    texts.extend(debug.attempts().iter().map(|attempt| format!("{attempt:?} {attempt:#?}")));
    texts
}

#[tokio::test]
async fn user_data_not_printed() {
    #[cfg(feature = "tracing")]
    let recorded = recorder::Recorder::default();
    #[cfg(feature = "tracing")]
    let _installed = recorder::install(&recorded);

    let state = json!({"review": format!("A delightful book. {SENTINEL}"), "stars": 5});
    let questions = positive();

    // A call that answers.
    let server =
        answering(StatusCode::OK, reply(Some("end_turn"), answer_content()).to_string()).await;
    let response = client(true)
        .provider_instance(provider(server.base_url()))
        .build()
        .expect("the client builds")
        .system_one(&state, &questions)
        .send()
        .await
        .expect("an answer");
    // The state did go to the vendor and is in the serialized trace, so the
    // checks below are not vacuous.
    assert_eq!(occurrences(&String::from_utf8_lossy(&server.requests()[0].body), SENTINEL), 1);
    let serialized = serde_json::to_string(response.debug()).expect("the trace serializes");
    assert!(occurrences(&serialized, SENTINEL) >= 1);
    let mut texts = vec![format!("{response:?}"), format!("{response:#?}")];
    texts.extend(trace_renderings(response.debug()));

    // A call that fails with a body that does not repeat the state.
    let failing =
        answering(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"unavailable"}}"#).await;
    let error = client(true)
        .provider_instance(provider(failing.base_url()))
        .build()
        .expect("the client builds")
        .system_one(&state, &questions)
        .send()
        .await
        .expect_err("every attempt fails");
    assert_eq!(failing.request_count(), 3);
    assert_eq!(occurrences(&String::from_utf8_lossy(&failing.requests()[0].body), SENTINEL), 1);
    let debug = error.debug().expect("the failed attempts leave a trace");
    assert!(occurrences(&serde_json::to_string(debug).expect("it serializes"), SENTINEL) >= 3);
    texts.push(error.to_string());
    for link in chain(&error) {
        texts.extend([link.to_string(), format!("{link:?}"), format!("{link:#?}")]);
    }
    texts.extend(trace_renderings(debug));

    for text in &texts {
        assert_eq!(occurrences(text, SENTINEL), 0, "{text}");
    }

    #[cfg(feature = "tracing")]
    {
        let lines = recorded.all();
        assert!(!recorded.at(tracing::Level::DEBUG).is_empty(), "the adapter logged the exchanges");
        assert!(!of_target(&lines, "typesafe_sdk").is_empty(), "the SDK logged the retries");
        for line in &lines {
            assert_eq!(occurrences(line, SENTINEL), 0, "{line}");
        }
    }
}
