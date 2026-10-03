//! Tests for the Anthropic provider: what its builder refuses, the request
//! it sends and the reply it reads, each exchange against a loopback server
//! or a scripted service. No test reads the process environment.

use std::{
    future::{Ready, ready},
    sync::Mutex,
    task::{Context, Poll},
};

use ::http::{Request, Response, StatusCode};
use decision_model_sdk::{Body, BoxError, ErrorKind as SdkErrorKind};
use http_body_util::Full;
use serde_json::{Value, json};
use test_support::{Protocol, TestServer, json_response};
use tower_service::Service;

use super::*;
use crate::error::ErrorKind;

/// The made-up key every test sends. No test may find it in a `Debug`
/// rendering or an error.
const KEY: &str = "sk-ant-test-4e1b7c9a2f60";

/// The schema upstream's request tests use.
const SCHEMA: &str = r#"{"type":"object","properties":{"answers":{"type":"object"}}}"#;

/// The reply upstream's output-limit test answers with.
const ANSWER: &str = r#"{"answers":{"positive":true}}"#;

fn schema() -> Schema {
    Schema::from_json(SCHEMA).expect("a JSON object")
}

/// The two messages upstream's request tests use.
fn messages() -> Vec<Message> {
    vec![Message::new(Role::System, "system prompt"), Message::new(Role::User, "the document")]
}

/// A builder with the made-up key and a five-second deadline, pointed at
/// `base_url`.
fn builder(base_url: &str) -> AnthropicProviderBuilder {
    AnthropicProvider::builder("claude-haiku-4-5")
        .api_key(KEY)
        .base_url(base_url)
        .timeout(Duration::from_secs(5))
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

/// A Messages reply with one text block holding [`ANSWER`], the token counts
/// 12 and 7, and `stop_reason`.
fn reply(stop_reason: Value) -> String {
    json!({
        "id": "message-test",
        "type": "message",
        "role": "assistant",
        "model": "test-model",
        "stop_reason": stop_reason,
        "content": [{"type": "text", "text": ANSWER}],
        "usage": {"input_tokens": 12, "output_tokens": 7},
    })
    .to_string()
}

/// What one call of the provider gave.
type Outcome = Result<Result<ProviderResult, NonAnswer>, decision_model_sdk::Error>;

/// One call of `provider` with [`messages`], and the trace it wrote.
async fn ask<S: HttpService>(
    provider: &AnthropicProvider<S>,
    structured: bool,
) -> (Outcome, AttemptTrace) {
    ask_with(provider, &messages(), structured).await
}

/// One call of `provider` with `messages`, and the trace it wrote.
async fn ask_with<S: HttpService>(
    provider: &AnthropicProvider<S>,
    messages: &[Message],
    structured: bool,
) -> (Outcome, AttemptTrace) {
    let schema = schema();
    let mut trace = AttemptTrace::default();
    let outcome =
        provider.request(ProviderCall::new(messages, &schema, structured, &mut trace)).await;
    (outcome, trace)
}

fn parsed(json: &str) -> Value {
    serde_json::from_str(json).expect("JSON")
}

/// The one request `server` recorded, as parsed JSON.
#[track_caller]
fn recorded_body(server: &TestServer) -> Value {
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    serde_json::from_slice(&requests[0].body).expect("a JSON body")
}

/// The `Config` error `builder` is refused with, after checking that no
/// rendering of it holds the key.
#[track_caller]
fn refused(builder: AnthropicProviderBuilder) -> String {
    let error = builder.build().expect_err("the builder is refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert!(!text.contains(KEY), "{text}");
    }
    error.to_string()
}

// ------------------------------------------------------------ the builder

#[test]
// Upstream: tests/test_provider_requests.py::test_anthropic_rejects_nonpositive_output_limit
fn a_zero_output_limit_is_refused() {
    assert_eq!(
        refused(builder("http://127.0.0.1:9").max_tokens(0)),
        "max_tokens must be greater than zero."
    );
    // The same builder with a positive bound is accepted.
    builder("http://127.0.0.1:9").max_tokens(1).build().expect("a positive bound");
}

#[test]
fn a_zero_deadline_or_size_limit_is_refused() {
    assert_eq!(
        refused(builder("http://127.0.0.1:9").timeout(Duration::ZERO)),
        "timeout must be greater than zero."
    );
    assert_eq!(
        refused(builder("http://127.0.0.1:9").max_response_bytes(0)),
        "max_response_bytes must be at least 1: every response carries a body."
    );
}

#[test]
fn a_key_that_cannot_be_sent_is_refused_without_being_repeated() {
    let error = AnthropicProvider::builder("claude-haiku-4-5")
        .api_key("sk-first\nsk-second")
        .base_url("http://127.0.0.1:9")
        .build()
        .expect_err("a line feed cannot be sent");
    assert!(matches!(error.kind(), ErrorKind::Config));
    for text in [error.to_string(), format!("{error:?}")] {
        assert!(!text.contains("sk-first") && !text.contains("sk-second"), "{text}");
    }

    assert_eq!(
        refused(AnthropicProvider::builder("m").api_key("").base_url("http://127.0.0.1:9")),
        "The API key is empty."
    );
}

#[test]
fn a_base_url_the_shared_rules_refuse_is_refused_without_being_repeated() {
    for (base_url, part) in [
        ("https://alice:hunter2@proxy.example", "hunter2"),
        ("https://proxy.example/v1?api_key=k-77", "k-77"),
        ("https://proxy.example/v1#token-9f2", "token-9f2"),
        ("proxy.example", "proxy.example"),
    ] {
        let message = refused(builder(base_url));
        assert!(!message.contains(part), "{message}");
        assert!(!message.contains("proxy.example"), "{message}");
    }
}

#[test]
fn a_provider_without_a_key_is_not_built() {
    // With the `internals` feature another test's replacement could be in
    // force in this process; an empty one of this test's own rules that out.
    // Without the feature a test build finds every variable unset.
    #[cfg(feature = "internals")]
    let _environment = crate::__internals::env::replace();

    let error = AnthropicProvider::builder("claude-haiku-4-5")
        .base_url("http://127.0.0.1:9")
        .build()
        .expect_err("no key was given and none is in the environment");
    assert!(matches!(error.kind(), ErrorKind::Config));
    assert_eq!(
        error.to_string(),
        "No Anthropic API key: pass one to api_key, \
         or set the ANTHROPIC_API_KEY environment variable."
    );
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn the_key_and_the_base_url_come_from_the_environment_when_not_given() {
    let server = answering(StatusCode::OK, reply(json!("end_turn"))).await;
    let other = answering(StatusCode::OK, reply(json!("end_turn"))).await;
    let environment = crate::__internals::env::replace();
    environment.set("ANTHROPIC_API_KEY", "sk-ant-from-the-environment");
    environment.set("ANTHROPIC_BASE_URL", server.base_url());

    // Neither given: both are read from the environment.
    let provider = AnthropicProvider::builder("claude-haiku-4-5").build().expect("it builds");
    ask(&provider, false).await.0.expect("an exchange").expect("an answer");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header_values("x-api-key"), ["sk-ant-from-the-environment"]);

    // Both given: the environment is not used.
    let explicit = builder(other.base_url()).build().expect("it builds");
    ask(&explicit, false).await.0.expect("an exchange").expect("an answer");
    assert_eq!(server.request_count(), 1);
    assert_eq!(other.requests()[0].header_values("x-api-key"), [KEY]);

    // The environment is read when the provider is built, not when it asks.
    environment.remove("ANTHROPIC_API_KEY");
    environment.remove("ANTHROPIC_BASE_URL");
    ask(&provider, false).await.0.expect("an exchange").expect("an answer");
    assert_eq!(server.request_count(), 2);

    // An empty variable counts as unset, and a base URL from the
    // environment is checked like any other.
    environment.set("ANTHROPIC_API_KEY", "");
    let error = AnthropicProvider::builder("m").build().expect_err("no key");
    assert!(error.to_string().starts_with("No Anthropic API key"), "{error}");
    environment.set("ANTHROPIC_API_KEY", "sk-ant-from-the-environment");
    environment.set("ANTHROPIC_BASE_URL", "https://proxy.example/v1?token=t-31");
    let error = AnthropicProvider::builder("m").build().expect_err("a query");
    assert_eq!(error.to_string(), "The base URL must not carry a query ('?...').");
}

#[cfg(feature = "internals")]
#[test]
fn the_base_url_is_the_vendors_when_none_is_given_or_set() {
    // An empty environment: no base URL is given and none is set. Nothing
    // is sent.
    let _environment = crate::__internals::env::replace();
    let provider =
        AnthropicProvider::builder("claude-haiku-4-5").api_key(KEY).build().expect("it builds");
    assert_eq!(
        provider.log_uri().expect("an endpoint").to_string(),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn debug_prints_the_model_the_host_and_the_api_only() {
    let base_url = format!("https://proxy.example:8443/{KEY}/tenant-4b1e");
    let builder = AnthropicProvider::builder("claude-haiku-4-5").api_key(KEY).base_url(&base_url);
    let builder_text = format!("{builder:?}");
    let provider = builder.build().expect("it builds");
    let provider_text = format!("{provider:?}");

    assert_eq!(
        provider_text,
        r#"AnthropicProvider { model: "claude-haiku-4-5", host: "proxy.example", api: "messages", .. }"#
    );
    assert_eq!(
        builder_text,
        "AnthropicProviderBuilder { model: \"claude-haiku-4-5\", api_key: true, base_url: true, \
         max_tokens: 4096, timeout: None, max_response_bytes: None, root_certificates: 0 }"
    );
    for text in [builder_text, provider_text, format!("{provider:#?}")] {
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("tenant-4b1e"), "{text}");
    }
}

/// A clone of a builder builds a provider with the same settings, and
/// prints no key either.
#[tokio::test]
async fn a_cloned_builder_builds_a_provider_with_the_same_settings() {
    let server = answering(StatusCode::OK, reply(json!("end_turn"))).await;
    let original = builder(&format!("{}/team-a/", server.base_url())).max_tokens(77);
    let copy = original.clone();

    let copy_text = format!("{copy:?} {copy:#?}");
    assert!(!copy_text.contains(KEY), "{copy_text}");
    assert_eq!(format!("{copy:?}"), format!("{original:?}"));
    let providers = [original.build().expect("it builds"), copy.build().expect("the clone builds")];
    assert_eq!(format!("{:?}", providers[1]), format!("{:?}", providers[0]));
    for provider in &providers {
        let (outcome, _) = ask(provider, true).await;
        outcome.expect("an exchange").expect("an answer");
    }

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.uri.path(), "/team-a/v1/messages");
        assert_eq!(request.header_values("x-api-key"), [KEY]);
        assert_eq!(parsed(std::str::from_utf8(&request.body).expect("UTF-8"))["max_tokens"], 77);
    }
    assert_eq!(requests[0].body, requests[1].body);
}

#[test]
fn the_names_are_the_model_and_the_public_type_path() {
    let provider = builder("http://127.0.0.1:9").build().expect("it builds");
    assert_eq!(provider.model_name(), "claude-haiku-4-5");
    assert_eq!(provider.type_name(), "decision_model_adapter::AnthropicProvider");
}

#[test]
fn a_builder_left_at_its_defaults_holds_the_shared_limits() {
    // With the `internals` feature another test's variables could be in
    // force in this process, a base URL among them; an empty replacement of
    // this test's own rules that out. Without the feature a unit-test build
    // finds no variable.
    #[cfg(feature = "internals")]
    let _environment = crate::__internals::env::replace();

    let provider =
        AnthropicProvider::builder("claude-haiku-4-5").api_key(KEY).build().expect("it builds");

    assert_eq!(provider.settings.limits, Limits::new(None, None).expect("the defaults are legal"));
}

#[test]
fn anthropic_names_its_endpoint_for_the_retry_line() {
    // The key as a path segment of the base URL, a prefix after it, and a
    // port that is not the scheme's own.
    let base_url = format!("http://127.0.0.1:8089/{KEY}/tenant-4b1e/");
    let provider = builder(&base_url).build().expect("it builds");
    let uri = provider.log_uri().expect("a built-in provider names its endpoint");

    assert_eq!(uri.path(), "/v1/messages");
    assert_eq!(uri.query(), None);
    let authority = uri.authority().expect("an authority").as_str();
    assert!(!authority.contains('@'), "{authority}");
    assert_eq!(uri.to_string(), "http://127.0.0.1:8089/v1/messages");
}

// ------------------------------------------------------------ the request

#[test]
// Upstream: tests/test_provider_requests.py::test_anthropic_request_puts_schema_in_output_config_when_structured
fn a_structured_request_puts_the_schema_in_output_config() {
    let schema = schema();
    let body = parsed(&request_body("claude-haiku-4-5", 4096, &messages(), Some(&schema)));

    assert_eq!(body["system"], "system prompt");
    assert_eq!(body["messages"], json!([{"role": "user", "content": "the document"}]));
    assert_eq!(body["max_tokens"], 4096);
    assert_eq!(
        body["output_config"],
        json!({"format": {"type": "json_schema", "schema": parsed(SCHEMA)}})
    );
    assert_eq!(
        body,
        json!({
            "model": "claude-haiku-4-5",
            "max_tokens": 4096,
            "system": "system prompt",
            "messages": [{"role": "user", "content": "the document"}],
            "output_config": {"format": {"type": "json_schema", "schema": parsed(SCHEMA)}},
        })
    );
}

#[test]
// Upstream: tests/test_provider_requests.py::test_anthropic_request_omits_output_config_when_prompted
fn a_prompted_request_has_no_output_config() {
    let body = parsed(&request_body("claude-haiku-4-5", 4096, &messages(), None));

    assert!(body.get("output_config").is_none(), "{body}");
    assert_eq!(
        body,
        json!({
            "model": "claude-haiku-4-5",
            "max_tokens": 4096,
            "system": "system prompt",
            "messages": [{"role": "user", "content": "the document"}],
        })
    );
}

#[test]
fn system_messages_are_joined_and_the_other_turns_keep_their_order() {
    let messages = [
        Message::new(Role::System, "first rule"),
        Message::new(Role::User, "the document"),
        Message::new(Role::Assistant, r#"{"answers":"#),
        Message::new(Role::System, "second rule"),
        Message::new(Role::User, "fix it"),
    ];
    let body = parsed(&request_body("m", 1, &messages, None));

    assert_eq!(body["system"], "first rule\n\nsecond rule");
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": "the document"},
            {"role": "assistant", "content": r#"{"answers":"#},
            {"role": "user", "content": "fix it"},
        ])
    );

    // Without a system message the member is still sent, empty.
    let body = parsed(&request_body("m", 1, &messages[1..2], None));
    assert_eq!(body["system"], "");
}

#[tokio::test]
async fn a_request_carries_the_key_the_version_and_the_recorded_body() {
    let server = answering(StatusCode::OK, reply(json!("end_turn"))).await;
    let provider = builder(&format!("{}/team-a/", server.base_url())).build().expect("it builds");

    for structured in [true, false] {
        let (outcome, trace) = ask(&provider, structured).await;
        outcome.expect("an exchange").expect("an answer");

        let requests = server.requests();
        let request = requests.last().expect("a request");
        assert_eq!(request.method, "POST");
        assert_eq!(request.uri.path(), "/team-a/v1/messages");
        assert_eq!(request.uri.query(), None);
        assert_eq!(request.header_values("x-api-key"), [KEY]);
        assert_eq!(request.header_values("anthropic-version"), ["2023-06-01"]);
        assert_eq!(request.header_values("content-type"), ["application/json"]);
        assert_eq!(request.header_values("accept"), ["application/json"]);
        assert!(request.header_values("authorization").is_empty());

        // The trace holds exactly the bytes that were sent.
        assert_eq!(trace.request().map(str::as_bytes), Some(&request.body[..]));
        assert_eq!(trace.api(), Some("messages"));
        let body: Value = serde_json::from_slice(&request.body).expect("a JSON body");
        assert_eq!(body.get("output_config").is_some(), structured);
    }
    assert_eq!(server.request_count(), 2);
}

/// The URI of one request a [`Canned`] service was asked, and the bytes of
/// its `x-api-key` header.
type Seen = (String, Vec<u8>);

/// A caller's service: it records what it is asked and answers with one
/// fixed reply.
#[derive(Clone)]
struct Canned {
    reply: Bytes,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Service<Request<Body>> for Canned {
    type Response = Response<Full<Bytes>>;
    type Error = BoxError;
    type Future = Ready<Result<Self::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let key = request.headers().get("x-api-key").map(|value| value.as_bytes().to_vec());
        self.seen
            .lock()
            .expect("the lock is not poisoned")
            .push((request.uri().to_string(), key.unwrap_or_default()));
        ready(Ok(Response::new(Full::new(self.reply.clone()))))
    }
}

#[tokio::test]
async fn a_callers_own_service_carries_the_request() {
    let service = Canned { reply: Bytes::from(reply(json!("end_turn"))), seen: Arc::default() };
    let seen = Arc::clone(&service.seen);
    let provider =
        builder("https://proxy.example/team-a").build_with_service(service).expect("it builds");

    // A clone asks through the same service.
    let (outcome, _) = ask(&provider.clone(), true).await;
    let result = outcome.expect("an exchange").expect("an answer");

    assert_eq!(result.text(), ANSWER);
    assert_eq!(
        *seen.lock().expect("the lock is not poisoned"),
        [("https://proxy.example/team-a/v1/messages".to_owned(), KEY.as_bytes().to_vec())]
    );
    // `Debug` asks nothing of the service's type.
    assert!(format!("{provider:?}").starts_with("AnthropicProvider {"));
}

#[tokio::test]
async fn a_provider_left_at_the_default_base_url_sends_to_anthropics_path() {
    let service = Canned { reply: Bytes::from(reply(json!("end_turn"))), seen: Arc::default() };
    let seen = Arc::clone(&service.seen);
    let provider = {
        // With the `internals` feature another test's variables could be in
        // force in this process; an empty replacement of this test's own
        // leaves the default as the only base URL. It is dropped before the
        // call: the environment is read when the provider is built.
        #[cfg(feature = "internals")]
        let _environment = crate::__internals::env::replace();
        AnthropicProvider::builder("claude-haiku-4-5")
            .api_key(KEY)
            .build_with_service(service)
            .expect("it builds")
    };

    let (outcome, _) = ask(&provider, true).await;

    assert_eq!(outcome.expect("an exchange").expect("an answer").text(), ANSWER);
    assert_eq!(
        *seen.lock().expect("the lock is not poisoned"),
        [("https://api.anthropic.com/v1/messages".to_owned(), KEY.as_bytes().to_vec())]
    );
}

#[test]
fn an_added_root_is_refused_with_a_callers_own_service() {
    let service = Canned { reply: Bytes::new(), seen: Arc::default() };
    let error = builder("https://proxy.example")
        .add_root_certificate(vec![0x30, 0x00])
        .build_with_service(service)
        .expect_err("the root would be ignored");

    assert!(matches!(error.kind(), ErrorKind::Config));
    assert_eq!(
        error.to_string(),
        "add_root_certificate configures the default transport, \
         which a provider built with build_with_service does not use."
    );
}

#[test]
fn an_added_root_that_is_not_a_certificate_is_refused() {
    let message = refused(builder("https://proxy.example").add_root_certificate(vec![0x30, 0x00]));
    assert!(message.starts_with("The TLS certificate verifier could not be built: "), "{message}");
}

// -------------------------------------------------------------- the reply

#[test]
// Upstream: tests/test_provider_requests.py::test_anthropic_result_joins_text_blocks_and_reads_usage
fn a_reply_joins_its_text_blocks_and_reads_the_usage() {
    let json = json!({
        "stop_reason": "end_turn",
        "content": [
            {"type": "text", "text": r#"{"answers":"#},
            {"type": "thinking", "text": "ignored"},
            {"type": "text", "text": " {}}"},
        ],
        "usage": {"input_tokens": 20, "output_tokens": 5},
    })
    .to_string();
    let mut trace = AttemptTrace::default();

    let result = read_reply(&json, &mut trace).expect("an answer");

    assert_eq!(result.text(), r#"{"answers": {}}"#);
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(20), Some(5)));
    assert_eq!(trace.response(), Some(json.as_str()));
    assert_eq!(trace.finish_reason(), Some("end_turn"));
}

#[test]
fn a_token_count_the_vendor_left_out_is_none() {
    for (usage, expected) in [
        (json!({"input_tokens": 3}), (Some(3), None)),
        (json!({"output_tokens": 4, "input_tokens": null}), (None, Some(4))),
        (json!({}), (None, None)),
        (Value::Null, (None, None)),
    ] {
        let json = json!({"content": [], "usage": usage}).to_string();
        let result = read_reply(&json, &mut AttemptTrace::default()).expect("an answer");
        assert_eq!((result.input_tokens(), result.output_tokens()), expected, "{json}");
        assert_eq!(result.text(), "");
    }

    // No `usage` member at all, and a block that is not text.
    let json = r#"{"content":[{"type":"tool_use","id":"t-1","input":{"text":7}}]}"#;
    let result = read_reply(json, &mut AttemptTrace::default()).expect("an answer");
    assert_eq!((result.text(), result.input_tokens(), result.output_tokens()), ("", None, None));
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_anthropic_output_limit
async fn a_reply_cut_at_the_output_limit_is_not_an_answer() {
    for structured in [false, true] {
        for truncated in [false, true] {
            let stop_reason = if truncated { "max_tokens" } else { "end_turn" };
            let server = answering(StatusCode::OK, reply(json!(stop_reason))).await;
            let provider = builder(server.base_url()).max_tokens(8192).build().expect("it builds");

            let (outcome, trace) = ask(&provider, structured).await;
            let outcome = outcome.expect("a success status is not an exchange failure");

            if truncated {
                // Even valid JSON must not hide a reply that was cut short.
                let message = outcome.expect_err("a cut reply").to_string();
                assert_eq!(
                    message,
                    "Anthropic did not answer: stop reason max_tokens, the reply was cut at the \
                     output token limit; raise max_tokens on the provider or ask fewer questions"
                );
            } else {
                let result = outcome.expect("an answer");
                assert_eq!(result.text(), ANSWER);
                assert_eq!((result.input_tokens(), result.output_tokens()), (Some(12), Some(7)));
            }
            let body = recorded_body(&server);
            assert_eq!(body["max_tokens"], 8192);
            assert_eq!(parsed(trace.request().expect("a recorded request")), body);
            let response = parsed(trace.response().expect("a recorded response"));
            assert_eq!(response["content"][0]["text"], ANSWER);
            assert_eq!(trace.finish_reason(), Some(stop_reason));
        }
    }
}

#[tokio::test]
async fn every_stop_reason_is_an_answer_or_names_itself() {
    // The reasons of upstream's stop-reason test; `Value::Null` is the
    // member sent as `null`.
    for (stop_reason, answered) in [
        (json!("end_turn"), true),
        (json!("stop_sequence"), true),
        (Value::Null, true),
        (json!("refusal"), false),
        (json!("model_context_window_exceeded"), false),
        (json!("pause_turn"), false),
        (json!("tool_use"), false),
        (json!("unknown"), false),
        (json!("max_tokens"), false),
    ] {
        let server = answering(StatusCode::OK, reply(stop_reason.clone())).await;
        let provider = builder(server.base_url()).build().expect("it builds");

        let (outcome, trace) = ask(&provider, true).await;
        let outcome = outcome.expect("a success status is not an exchange failure");

        assert_eq!(outcome.is_ok(), answered, "{stop_reason}");
        if let Err(not_answered) = &outcome {
            let message = not_answered.to_string();
            let reason = stop_reason.as_str().expect("a named reason");
            assert!(
                message.starts_with(&format!("Anthropic did not answer: stop reason {reason}")),
                "{message}"
            );
            assert!(!message.contains("positive"), "the reply's text is not quoted: {message}");
        }
        // Answered or not, the reply and its stop reason are in the trace.
        assert_eq!(server.request_count(), 1);
        assert_eq!(
            parsed(trace.response().expect("a recorded response"))["stop_reason"],
            stop_reason
        );
        assert_eq!(trace.finish_reason(), stop_reason.as_str());
    }

    // A reply without the member at all is an answer too.
    let mut trace = AttemptTrace::default();
    let json = json!({"content": [{"type": "text", "text": ANSWER}]}).to_string();
    assert_eq!(read_reply(&json, &mut trace).expect("an answer").text(), ANSWER);
    assert_eq!(trace.finish_reason(), None);
    assert_eq!(trace.response(), Some(json.as_str()));
}

#[test]
fn a_stop_reason_is_escaped_and_cut_before_it_is_printed() {
    let reason = format!("line one\nline two {}", "x".repeat(300));
    let json = json!({"stop_reason": reason, "content": []}).to_string();

    let message =
        read_reply(&json, &mut AttemptTrace::default()).expect_err("not an answer").to_string();

    assert!(message.starts_with(r"Anthropic did not answer: stop reason line one\nline two x"));
    assert!(!message.contains('\n'), "{message}");
    assert!(message.ends_with('\u{2026}'), "{message}");
    assert_eq!(message.chars().count(), "Anthropic did not answer: ".len() + 200 + 1);
}

/// An array whose members fill the reply by position, and every other JSON
/// value that is not an object, is not read as a reply.
#[test]
fn a_body_that_is_not_an_object_is_not_read_as_a_reply() {
    for body in [
        "[null,[]]",
        " \n[null,[],null]",
        r#"["end_turn",[{"type":"text","text":"SECRET-BODY-TEXT"}]]"#,
        "[]",
        "null",
        "7",
        r#""SECRET-BODY-TEXT""#,
    ] {
        let mut trace = AttemptTrace::default();

        let message = read_reply(body, &mut trace).expect_err("not a reply").to_string();

        assert_eq!(message, "Anthropic did not answer: a body that is not the vendor's reply");
        assert_eq!(trace.response().map(str::trim), Some(body.trim()), "{body}");
        assert_eq!(trace.finish_reason(), None, "{body}");
    }
}

#[tokio::test]
async fn a_success_body_that_is_not_a_messages_reply_is_not_an_answer() {
    for body in [
        r#"{"error":{"message":"SECRET-BODY-TEXT"}}"#,
        r#"["SECRET-BODY-TEXT"]"#,
        r#"{"content":"SECRET-BODY-TEXT"}"#,
        r#"{"content":[{"text":"SECRET-BODY-TEXT"}]}"#,
        r#"{"content":[{"type":"text","text":["SECRET-BODY-TEXT"]}]}"#,
        r#"{"content":[],"stop_reason":["SECRET-BODY-TEXT"]}"#,
        r#"{"content":[],"usage":{"input_tokens":"SECRET-BODY-TEXT"}}"#,
    ] {
        let server = answering(StatusCode::OK, body).await;
        let provider = builder(server.base_url()).build().expect("it builds");

        let (outcome, trace) = ask(&provider, false).await;
        let message = outcome
            .expect("a success status is not an exchange failure")
            .expect_err("not a Messages reply")
            .to_string();

        assert_eq!(
            message, "Anthropic did not answer: a body that is not the vendor's reply",
            "{body}"
        );
        // The body is in the trace, and only there.
        assert_eq!(trace.response(), Some(body));
        assert_eq!(trace.finish_reason(), None);
        assert_eq!(server.request_count(), 1);
    }
}

#[tokio::test]
async fn a_failure_status_is_an_api_error_with_the_request_in_the_trace() {
    let server =
        answering(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"overloaded"}}"#).await;
    let provider = builder(server.base_url()).build().expect("it builds");

    let (outcome, trace) = ask(&provider, true).await;
    let error = outcome.expect_err("a failure status");

    let SdkErrorKind::Api(api) = error.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(server.request_count(), 1);
    assert_eq!(parsed(trace.request().expect("a recorded request")), recorded_body(&server));
    assert_eq!(trace.response(), None);
    assert_eq!(trace.finish_reason(), None);
}

#[tokio::test]
async fn a_success_body_that_is_not_json_leaves_no_response_in_the_trace() {
    let server = answering(StatusCode::OK, "<html>upstream gateway page</html>").await;
    let provider = builder(server.base_url()).build().expect("it builds");

    let (outcome, trace) = ask(&provider, false).await;
    let message = outcome.expect("a success status").expect_err("not JSON").to_string();

    assert_eq!(message, "Anthropic did not answer: status 200 with a body that is not JSON");
    assert!(trace.request().is_some());
    assert_eq!(trace.response(), None);
}
