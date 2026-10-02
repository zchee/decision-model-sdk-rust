//! Tests for the OpenAI provider: what it is built from, the two request
//! bodies as a loopback server receives them, and how the two replies are
//! read. No test sends a request anywhere but to a server it started.

use std::{
    future::{Ready, ready},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use ::http::{Method, Request, Response, StatusCode};
use http_body_util::Full;
use serde_json::{Value, json};
use test_support::{Protocol, TestServer, json_response};
use tower_service::Service;
use typesafe_sdk::{ApiErrorKind, Body, BoxError, ErrorKind as SdkErrorKind};

use super::*;
use crate::error::ErrorKind;

/// The key every test passes to its builder. It is made up.
const KEY: &str = "sk-test-0123456789abcdef";

/// The schema upstream's request tests send.
const SCHEMA: &str = r#"{"type":"object","properties":{"answers":{"type":"object"}}}"#;

/// The reply text of a model that answered.
const ANSWER: &str = r#"{"answers":{"positive":true}}"#;

fn schema() -> Schema {
    Schema::from_json(SCHEMA).expect("a JSON object")
}

fn schema_value() -> Value {
    serde_json::from_str(SCHEMA).expect("JSON")
}

/// Upstream's two messages.
fn messages() -> Vec<Message> {
    vec![Message::new(Role::System, "system prompt"), Message::new(Role::User, "the document")]
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

/// A builder with the made-up key, pointed at `server`.
fn builder(server: &TestServer) -> OpenAiProviderBuilder {
    OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(format!("{}/v1", server.base_url()))
        .timeout(Duration::from_secs(5))
}

fn provider(server: &TestServer, api: OpenAiApi) -> OpenAiProvider {
    builder(server).api(api).build().expect("the provider builds")
}

/// One call of `provider` with upstream's two messages, and what it left in
/// the attempt's trace.
async fn ask(
    provider: &dyn Provider,
    structured: bool,
) -> (Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>, AttemptTrace) {
    ask_with(provider, &messages(), structured).await
}

async fn ask_with(
    provider: &dyn Provider,
    messages: &[Message],
    structured: bool,
) -> (Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>, AttemptTrace) {
    let schema = schema();
    let mut trace = AttemptTrace::default();
    let result =
        provider.request(ProviderCall::new(messages, &schema, structured, &mut trace)).await;
    (result, trace)
}

/// The body of the one request `server` received, as JSON.
fn sent(server: &TestServer) -> Value {
    let requests = server.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    serde_json::from_slice(&requests[0].body).expect("the request body is JSON")
}

fn parsed(json: Option<&str>) -> Value {
    serde_json::from_str(json.expect("it was recorded")).expect("JSON")
}

/// A completed Responses reply holding `text` and upstream's usage.
fn responses_reply(text: &str) -> Value {
    json!({
        "status": "completed",
        "output": [
            {"id": "reasoning-test", "type": "reasoning", "summary": [{"type": "summary_text", "text": "Ignored summary."}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]},
        ],
        "usage": {"input_tokens": 12, "output_tokens": 7, "total_tokens": 19},
    })
}

/// A Chat Completions reply holding `text`, `finish_reason` and upstream's
/// usage.
fn chat_reply(text: &str, finish_reason: Option<&str>) -> Value {
    json!({
        "choices": [{"finish_reason": finish_reason, "message": {"role": "assistant", "content": text}}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19},
    })
}

/// Reads `reply` as `api`'s, as the provider does after an exchange.
fn read(api: OpenAiApi, reply: &Value) -> (Result<ProviderResult, NonAnswer>, AttemptTrace) {
    let json = reply.to_string();
    let mut trace = AttemptTrace::default();
    let result = match api {
        OpenAiApi::Responses => read_responses(&json, &mut trace),
        OpenAiApi::ChatCompletions => read_chat(&json, &mut trace),
    };
    (result, trace)
}

// ------------------------------------------------------- building a provider

#[test]
fn the_api_defaults_to_responses_only_for_openais_own_host() {
    assert_eq!(default_api("api.openai.com"), OpenAiApi::Responses);
    // A host name is the same host in any case.
    assert_eq!(default_api("API.OpenAI.com"), OpenAiApi::Responses);

    for other in [
        "127.0.0.1",
        "localhost",
        "compatible.test",
        "proxy.test",
        "openai.com",
        "eu.api.openai.com",
        "api.openai.com.proxy.test",
        "api.openai.com.",
        "",
    ] {
        assert_eq!(default_api(other), OpenAiApi::ChatCompletions, "{other}");
    }
}

#[tokio::test]
async fn a_loopback_base_url_selects_chat_completions_unless_the_api_is_named() {
    let server = answering(StatusCode::OK, "{}").await;

    let unnamed = builder(&server).build().expect("the provider builds");
    assert_eq!(unnamed.api, OpenAiApi::ChatCompletions);

    let named = builder(&server).api(OpenAiApi::Responses).build().expect("the provider builds");
    assert_eq!(named.api, OpenAiApi::Responses);

    assert_eq!(server.request_count(), 0, "building sends nothing");
}

#[tokio::test]
async fn openai_names_its_endpoint_for_the_retry_line() {
    let server = answering(StatusCode::OK, "{}").await;
    let addr = server.addr();

    for (api, path) in [
        (OpenAiApi::Responses, "/v1/responses"),
        (OpenAiApi::ChatCompletions, "/v1/chat/completions"),
    ] {
        // The caller's prefix holds a tenant and, as a path segment, the key.
        let provider = OpenAiProvider::builder("test-model")
            .api_key(KEY)
            .base_url(format!("http://{addr}/tenant-4b1e/{KEY}/v1/"))
            .api(api)
            .build()
            .expect("the provider builds");

        let uri = Provider::log_uri(&provider).expect("a built-in provider names its endpoint");

        assert_eq!(uri.query(), None);
        let authority = uri.authority().expect("an authority").as_str();
        assert!(!authority.contains('@'), "{authority}");
        assert_eq!(uri.path(), path);
        let text = uri.to_string();
        assert_eq!(text, format!("http://{addr}{path}"));
        assert!(!text.contains("tenant-4b1e"), "{text}");
        assert!(!text.contains(KEY), "{text}");
    }
}

#[tokio::test]
async fn a_provider_reports_its_model_and_its_public_type_path() {
    let server = answering(StatusCode::OK, "{}").await;
    let provider = provider(&server, OpenAiApi::Responses);

    assert_eq!(provider.model_name(), "test-model");
    assert_eq!(provider.type_name(), "system_one_adapter::OpenAiProvider");
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
        OpenAiProvider::builder("test-model").api_key(KEY).build().expect("the provider builds");

    assert_eq!(provider.limits, Limits::new(None, None).expect("the defaults are legal"));
}

#[tokio::test]
async fn neither_the_provider_nor_its_builder_prints_the_key_or_the_base_url() {
    let server = answering(StatusCode::OK, "{}").await;
    let addr = server.addr();
    let builder = OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(format!("http://{addr}/{KEY}/secret-path/v1"))
        .api(OpenAiApi::Responses)
        .max_response_bytes(1024)
        .timeout(Duration::from_secs(3));
    let provider = builder.clone().build().expect("the provider builds");

    assert_eq!(
        format!("{builder:?}"),
        "OpenAiProviderBuilder { model: \"test-model\", api_key: true, base_url: true, \
         api: Some(Responses), timeout: Some(3s), max_response_bytes: Some(1024), extra_roots: 0 }"
    );
    assert_eq!(
        format!("{provider:?}"),
        "OpenAiProvider { model: \"test-model\", host: \"127.0.0.1\", api: Responses, .. }"
    );
    for text in [
        format!("{builder:?}"),
        format!("{builder:#?}"),
        format!("{provider:?}"),
        format!("{provider:#?}"),
        // The transport inside it, which no integration test can reach.
        format!("{:?}", provider.service),
        format!("{:#?}", provider.service),
    ] {
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("secret-path"), "{text}");
    }
    assert_eq!(format!("{:?}", provider.service), "Transport { extra_roots: 0 }");
}

/// The `Config` error `builder` is refused with, after checking that no
/// rendering of it holds the key.
#[track_caller]
fn refused(builder: OpenAiProviderBuilder) -> String {
    let error = builder.build().expect_err("the builder is refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert!(!text.contains(KEY), "{text}");
    }
    error.to_string()
}

#[tokio::test]
async fn a_setting_that_cannot_be_used_is_refused_when_the_provider_is_built() {
    let server = answering(StatusCode::OK, "{}").await;

    assert_eq!(
        refused(builder(&server).timeout(Duration::ZERO)),
        "timeout must be greater than zero."
    );
    assert_eq!(
        refused(builder(&server).max_response_bytes(0)),
        "max_response_bytes must be at least 1: every response carries a body."
    );
    assert_eq!(refused(builder(&server).api_key("")), "The API key is empty.");
    assert_eq!(
        refused(builder(&server).api_key(format!("{KEY}\nx-injected: 1"))),
        "The API key holds a character that cannot be sent in an HTTP header."
    );
    assert_eq!(
        refused(builder(&server).base_url(format!("{}/v1?api-key={KEY}", server.base_url()))),
        "The base URL must not carry a query ('?...')."
    );
    assert_eq!(
        refused(builder(&server).base_url(format!("http://user:{KEY}@{}/v1", server.addr()))),
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
    assert_eq!(
        refused(builder(&server).base_url("/v1")),
        "The base URL must be absolute, with a scheme and a host, \
         such as https://api.example.com/v1."
    );
    // What the verifier says of the bytes is the platform's own text.
    let message = refused(builder(&server).add_root_certificate(b"not a certificate".to_vec()));
    assert!(message.starts_with("The TLS certificate verifier could not be built: "), "{message}");
    assert_eq!(server.request_count(), 0);
}

/// A caller's own service: it answers every request with one fixed body and
/// records the URI each request was sent to.
#[derive(Clone)]
struct Canned {
    reply: Bytes,
    uris: Arc<Mutex<Vec<String>>>,
}

impl Canned {
    fn new(reply: impl Into<Bytes>) -> Self {
        Self { reply: reply.into(), uris: Arc::default() }
    }

    /// The URIs of the requests asked so far, in order.
    fn uris(&self) -> Vec<String> {
        self.uris.lock().expect("the lock is not poisoned").clone()
    }
}

impl Service<Request<Body>> for Canned {
    type Response = Response<Full<Bytes>>;
    type Error = BoxError;
    type Future = Ready<Result<Self::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        assert_eq!(request.method(), Method::POST);
        self.uris.lock().expect("the lock is not poisoned").push(request.uri().to_string());
        ready(Ok(Response::new(Full::new(self.reply.clone()))))
    }
}

#[tokio::test]
async fn a_provider_runs_over_a_service_of_the_callers_own() {
    const REPLY: &str = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{}"}}]}"#;
    let server = answering(StatusCode::OK, "{}").await;

    let provider =
        builder(&server).build_with_service(Canned::new(REPLY)).expect("the provider builds");
    let (result, trace) = ask(&provider, true).await;

    let result = result.expect("the service answers").expect("the reply is an answer");
    assert_eq!(result.text(), "{}");
    assert_eq!(trace.response(), Some(REPLY));
    assert_eq!(server.request_count(), 0, "the request went through the service");

    // A root is a setting of the default transport, which is not there.
    let error = builder(&server)
        .add_root_certificate(vec![0x30, 0x00])
        .build_with_service(Canned::new(REPLY))
        .expect_err("a root with a service of the caller's own");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert_eq!(
        error.to_string(),
        "add_root_certificate configures the default transport and has no effect on \
         build_with_service."
    );
}

#[tokio::test]
async fn a_clone_asks_the_same_model_at_the_same_endpoint() {
    let server = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let provider = provider(&server, OpenAiApi::Responses);
    let clone = provider.clone();

    for provider in [&provider, &clone] {
        let (result, _) = ask(provider, true).await;
        assert_eq!(result.expect("it answers").expect("an answer").text(), ANSWER);
    }
    assert_eq!(server.request_count(), 2);
    assert_eq!(server.accepted_connections(), 1, "a clone shares the connection pool");
}

#[tokio::test]
async fn a_provider_left_at_the_default_base_url_sends_to_openais_paths() {
    for (api, reply, uri) in [
        (None, responses_reply(ANSWER), "https://api.openai.com/v1/responses"),
        (
            Some(OpenAiApi::ChatCompletions),
            chat_reply(ANSWER, Some("stop")),
            "https://api.openai.com/v1/chat/completions",
        ),
    ] {
        let service = Canned::new(reply.to_string());
        let provider = {
            // With the `internals` feature another test's variables could be
            // in force in this process; an empty replacement of this test's
            // own leaves the default as the only base URL. It is dropped
            // before the call: the environment is read when the provider is
            // built.
            #[cfg(feature = "internals")]
            let _environment = crate::__internals::env::replace();
            let builder = OpenAiProvider::builder("test-model").api_key(KEY);
            let builder = match api {
                Some(api) => builder.api(api),
                None => builder,
            };
            builder.build_with_service(service.clone()).expect("the provider builds")
        };

        let (result, _) = ask(&provider, true).await;

        assert_eq!(result.expect("the service answers").expect("an answer").text(), ANSWER);
        assert_eq!(service.uris(), [uri]);
    }
}

// ------------------------------------------------------------ the environment

#[cfg(feature = "internals")]
#[test]
// Upstream: tests/test_openai_transports.py::test_custom_endpoint_from_environment_defaults_to_chat
fn a_custom_endpoint_from_the_environment_defaults_to_chat() {
    let environment = crate::__internals::env::replace();
    environment.set("OPENAI_BASE_URL", "https://compatible.test/v1");

    let provider =
        OpenAiProvider::builder("test-model").api_key(KEY).build().expect("the provider builds");

    assert_eq!(provider.api, OpenAiApi::ChatCompletions);
    assert_eq!(&*provider.host, "compatible.test");
    assert_eq!(
        provider.endpoint.log_uri().to_string(),
        "https://compatible.test/v1/chat/completions"
    );
}

#[cfg(feature = "internals")]
#[test]
fn without_a_base_url_anywhere_the_provider_speaks_responses_to_openai() {
    let _environment = crate::__internals::env::replace();

    let provider =
        OpenAiProvider::builder("test-model").api_key(KEY).build().expect("the provider builds");

    assert_eq!(provider.api, OpenAiApi::Responses);
    assert_eq!(&*provider.host, "api.openai.com");
    assert_eq!(provider.endpoint.log_uri().to_string(), "https://api.openai.com/v1/responses");
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn the_builder_wins_over_the_environment() {
    let server = answering(StatusCode::OK, "{}").await;
    let environment = crate::__internals::env::replace();
    environment.set("OPENAI_BASE_URL", "https://from-the-environment.test/v1");
    environment.set("OPENAI_API_KEY", "sk-from-the-environment");

    let provider = builder(&server).build().expect("the provider builds");

    assert_eq!(&*provider.host, "127.0.0.1");
    assert_eq!(provider.endpoint.log_uri().port_u16(), Some(server.addr().port()));
    assert_eq!(provider.headers[AUTHORIZATION].as_bytes(), format!("Bearer {KEY}").as_bytes());
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn the_key_and_the_base_url_are_read_from_the_environment_once_at_build() {
    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()).await;
    let environment = crate::__internals::env::replace();
    environment.set("OPENAI_API_KEY", "sk-test-from-the-environment");
    environment.set("OPENAI_BASE_URL", format!("{}/v1", server.base_url()));

    let provider = OpenAiProvider::builder("test-model")
        .timeout(Duration::from_secs(5))
        .build()
        .expect("the provider builds");
    // What the environment holds after the build is not read again.
    environment.set("OPENAI_API_KEY", "sk-test-set-too-late");
    environment.set("OPENAI_BASE_URL", "https://set-too-late.test/v1");

    let (result, _) = ask(&provider, false).await;

    assert_eq!(result.expect("it answers").expect("an answer").text(), ANSWER);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].uri.path(), "/v1/chat/completions");
    assert_eq!(requests[0].header_values("authorization"), ["Bearer sk-test-from-the-environment"]);
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn a_key_given_nowhere_is_a_config_error_that_names_the_variable() {
    let server = answering(StatusCode::OK, "{}").await;
    let environment = crate::__internals::env::replace();

    for empty in [false, true] {
        if empty {
            environment.set("OPENAI_API_KEY", "");
        }
        let error = OpenAiProvider::builder("test-model")
            .base_url(format!("{}/v1", server.base_url()))
            .build()
            .expect_err("no key");

        assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
        assert_eq!(
            error.to_string(),
            "The OpenAI API key is missing: pass api_key or set the OPENAI_API_KEY environment \
             variable."
        );
    }
}

// --------------------------------------------------------- the request bodies

#[tokio::test]
async fn responses_in_native_mode_moves_the_system_prompt_into_instructions() {
    let server = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let provider = provider(&server, OpenAiApi::Responses);
    let conversation = [
        Message::new(Role::System, "system prompt"),
        Message::new(Role::User, "the document"),
        Message::new(Role::System, "a second system prompt"),
        Message::new(Role::Assistant, r#"{"answers":"#),
        Message::new(Role::User, "fix it"),
    ];

    let (result, trace) = ask_with(&provider, &conversation, true).await;

    let result = result.expect("it answers").expect("an answer");
    assert_eq!(result.text(), ANSWER);
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(12), Some(7)));

    let body = sent(&server);
    assert_eq!(
        body,
        json!({
            "model": "test-model",
            "input": [
                {"role": "user", "content": "the document"},
                {"role": "assistant", "content": "{\"answers\":"},
                {"role": "user", "content": "fix it"},
            ],
            "instructions": "system prompt\n\na second system prompt",
            "text": {"format": {
                "type": "json_schema", "name": "evaluation", "schema": schema_value(), "strict": true,
            }},
            "store": false,
        })
    );
    let request = &server.requests()[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri.path(), "/v1/responses");
    assert_eq!(request.uri.query(), None);
    assert_eq!(request.header_values("authorization"), [format!("Bearer {KEY}")]);
    assert_eq!(request.header_values("content-type"), ["application/json"]);

    // The trace holds what was sent and what came back, byte for byte.
    assert_eq!(trace.request().map(str::as_bytes), Some(&*request.body));
    assert_eq!(trace.api(), Some("responses"));
    assert_eq!(parsed(trace.response()), responses_reply(ANSWER));
    assert_eq!(trace.finish_reason(), Some("completed"));
}

#[tokio::test]
async fn responses_in_prompted_mode_keeps_the_system_prompt_in_the_input() {
    let server = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let provider = provider(&server, OpenAiApi::Responses);

    let (result, trace) = ask(&provider, false).await;

    assert_eq!(result.expect("it answers").expect("an answer").text(), ANSWER);
    let body = sent(&server);
    assert_eq!(
        body,
        json!({
            "model": "test-model",
            "input": [
                {"role": "system", "content": "system prompt"},
                {"role": "user", "content": "the document"},
            ],
            "text": {"format": {"type": "json_object"}},
            "store": false,
        })
    );
    assert!(body.get("instructions").is_none());
    assert!(body.get("previous_response_id").is_none());
    assert_eq!(parsed(trace.request()), body);
}

#[tokio::test]
async fn chat_completions_in_native_mode_sends_the_wrapped_schema() {
    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()).await;
    let provider = provider(&server, OpenAiApi::ChatCompletions);

    let (result, trace) = ask(&provider, true).await;

    let result = result.expect("it answers").expect("an answer");
    assert_eq!(result.text(), ANSWER);
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(12), Some(7)));

    let body = sent(&server);
    assert_eq!(
        body,
        json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "system prompt"},
                {"role": "user", "content": "the document"},
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "evaluation", "schema": schema_value(), "strict": true},
            },
        })
    );
    assert_eq!(server.requests()[0].uri.path(), "/v1/chat/completions");
    assert_eq!(parsed(trace.request()), body);
    assert_eq!(trace.api(), Some("chat_completions"));
    assert_eq!(parsed(trace.response()), chat_reply(ANSWER, Some("stop")));
    assert_eq!(trace.finish_reason(), Some("stop"));
}

#[tokio::test]
async fn chat_completions_in_prompted_mode_sends_a_null_response_format() {
    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()).await;
    let provider = provider(&server, OpenAiApi::ChatCompletions);

    let (result, _) = ask(&provider, false).await;

    assert_eq!(result.expect("it answers").expect("an answer").text(), ANSWER);
    let body = sent(&server);
    // The member is there, and it is JSON null.
    assert_eq!(body.get("response_format"), Some(&Value::Null));
    assert_eq!(
        body,
        json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "system prompt"},
                {"role": "user", "content": "the document"},
            ],
            "response_format": null,
        })
    );
}

#[test]
// Upstream: tests/test_provider_requests.py::test_openai_native_response_format_wraps_schema
fn the_native_response_format_wraps_the_schema() {
    let schema = schema();

    let format = serde_json::to_value(response_format(&schema, true)).expect("it serializes");

    assert_eq!(
        format,
        json!({
            "type": "json_schema",
            "json_schema": {"name": "evaluation", "schema": schema_value(), "strict": true},
        })
    );
}

#[test]
// Upstream: tests/test_provider_requests.py::test_openai_prompted_sends_no_response_format
fn prompted_mode_sends_no_response_format() {
    let schema = schema();

    assert!(response_format(&schema, false).is_none());
    assert_eq!(
        serde_json::to_value(response_format(&schema, false)).expect("it serializes"),
        Value::Null
    );
}

#[test]
fn the_schema_is_sent_as_the_text_it_was_given_as() {
    // Member order and number text are the caller's; a re-serialized schema
    // would lose both.
    let text = r#"{"z":1.50,"a":{"type":"object"},"m":1e2}"#;
    let schema = Schema::from_json(text).expect("a JSON object");
    let provider_messages = messages();

    for body in [
        serde_json::to_string(&ResponsesBody::new("m", &provider_messages, &schema, true)),
        serde_json::to_string(&response_format(&schema, true)),
    ] {
        let body = body.expect("it serializes");
        assert!(body.contains(text), "{body}");
    }
}

// --------------------------------------------------------------- the replies

#[test]
// Upstream: tests/test_provider_requests.py::test_openai_result_reads_content_and_usage
fn a_chat_reply_gives_its_content_and_its_usage() {
    let reply = json!({
        "choices": [{"message": {"content": "{\"answers\": {}}"}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 12, "completion_tokens": 3},
    });

    let (result, trace) = read(OpenAiApi::ChatCompletions, &reply);

    let result = result.expect("an answer");
    assert_eq!(result.text(), r#"{"answers": {}}"#);
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(12), Some(3)));
    assert_eq!(trace.finish_reason(), Some("stop"));
}

#[test]
// Upstream: tests/test_openai_transports.py::test_unfinished_responses_are_not_treated_as_answers
fn an_unfinished_response_is_not_an_answer() {
    for (status, expected) in [
        ("failed", "OpenAI did not answer: response status failed"),
        (
            "incomplete",
            "OpenAI did not answer: response status incomplete, reason max_output_tokens",
        ),
    ] {
        let reply = json!({
            "status": status,
            "error": (status == "failed").then(|| json!({"code": "server_error", "message": "generation failed"})),
            "incomplete_details": (status == "incomplete").then(|| json!({"reason": "max_output_tokens"})),
            "output": [
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]},
            ],
            "usage": {"input_tokens": 12, "output_tokens": 7, "total_tokens": 19},
        });

        let (result, trace) = read(OpenAiApi::Responses, &reply);

        let message = result.expect_err("the reply is unfinished").to_string();
        assert_eq!(message, expected);
        // The vendor's own error message stays in the trace.
        assert!(!message.contains("generation failed"), "{message}");
        assert_eq!(trace.finish_reason(), Some(status));
        assert_eq!(parsed(trace.response()), reply);
    }
}

#[test]
fn a_responses_status_is_named_even_when_it_is_unknown_or_absent() {
    for (status, expected) in [
        (json!("cancelled"), "OpenAI did not answer: response status cancelled"),
        (json!("in_progress"), "OpenAI did not answer: response status in_progress"),
        (json!("queued"), "OpenAI did not answer: response status queued"),
        (json!("line\nbreak"), "OpenAI did not answer: response status line\\nbreak"),
        (Value::Null, "OpenAI did not answer: a response without a status"),
    ] {
        let reply = json!({"status": status, "output": []});

        let (result, trace) = read(OpenAiApi::Responses, &reply);

        assert_eq!(result.expect_err("not completed").to_string(), expected);
        assert_eq!(trace.finish_reason(), status.as_str());
        assert!(trace.response().is_some());
    }

    let (result, _) = read(OpenAiApi::Responses, &json!({"output": []}));
    assert_eq!(
        result.expect_err("no status").to_string(),
        "OpenAI did not answer: a response without a status"
    );
}

#[test]
fn a_refusal_part_is_not_an_answer_wherever_it_stands() {
    let refusal = "Cannot evaluate this request.";
    let refused = json!({"type": "message", "role": "assistant", "content": [{"type": "refusal", "refusal": refusal}]});
    let answered = json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]});
    let reasoning = json!({"type": "reasoning", "id": "reasoning-test", "summary": []});

    for output in [
        json!([reasoning, refused]),
        json!([reasoning, answered, refused]),
        json!([refused, answered]),
    ] {
        let reply = json!({"status": "completed", "output": output});

        let (result, trace) = read(OpenAiApi::Responses, &reply);

        let message = result.expect_err("a refusal").to_string();
        assert_eq!(message, "OpenAI did not answer: refusal");
        assert!(!message.contains(refusal));
        // The refusal's text is in the trace, recorded before it was read.
        assert_eq!(parsed(trace.response()), reply);
        assert_eq!(trace.finish_reason(), Some("completed"));
    }
}

#[test]
fn a_responses_text_is_every_output_text_part_of_every_message_in_order() {
    let reply = json!({
        "status": "completed",
        "output": [
            {"type": "reasoning", "summary": [{"type": "summary_text", "text": "ignored"}]},
            {"type": "message", "content": [
                {"type": "output_text", "text": "{\"answers\":"},
                {"type": "annotation", "text": "ignored"},
                {"type": "output_text", "text": "{\"positive\":"},
            ]},
            {"type": "function_call", "content": [{"type": "output_text", "text": "ignored"}]},
            {"type": "message", "content": null},
            {"type": "message", "content": [{"type": "output_text"}, {"type": "output_text", "text": "true}}"}]},
        ],
    });

    let (result, _) = read(OpenAiApi::Responses, &reply);

    let result = result.expect("an answer");
    assert_eq!(result.text(), ANSWER);
    assert_eq!((result.input_tokens(), result.output_tokens()), (None, None));

    // No message at all is the empty text, which the client then refuses.
    for output in [json!([]), Value::Null] {
        let (result, _) =
            read(OpenAiApi::Responses, &json!({"status": "completed", "output": output}));
        assert_eq!(result.expect("an answer").text(), "");
    }
}

#[test]
fn a_chat_finish_reason_other_than_stop_or_none_is_not_an_answer() {
    for reason in [Some("stop"), None] {
        let (result, trace) = read(OpenAiApi::ChatCompletions, &chat_reply(ANSWER, reason));
        assert_eq!(result.expect("an answer").text(), ANSWER);
        assert_eq!(trace.finish_reason(), reason);
    }
    // A reply without the member at all is read as one whose member is null.
    let reply = json!({"choices": [{"message": {"content": ANSWER}}]});
    let (result, trace) = read(OpenAiApi::ChatCompletions, &reply);
    assert_eq!(result.expect("an answer").text(), ANSWER);
    assert_eq!(trace.finish_reason(), None);

    for reason in ["length", "content_filter", "tool_calls", "function_call", "unknown"] {
        let reply = chat_reply(ANSWER, Some(reason));

        let (result, trace) = read(OpenAiApi::ChatCompletions, &reply);

        assert_eq!(
            result.expect_err("not finished").to_string(),
            format!("OpenAI did not answer: finish reason {reason}")
        );
        assert_eq!(trace.finish_reason(), Some(reason));
        assert_eq!(parsed(trace.response()), reply);
    }
}

#[test]
fn a_chat_content_of_null_is_the_empty_text() {
    for message in [json!({"role": "assistant", "content": null}), json!({"role": "assistant"})] {
        let reply = json!({"choices": [{"finish_reason": "stop", "message": message}]});

        let (result, _) = read(OpenAiApi::ChatCompletions, &reply);

        assert_eq!(result.expect("an answer").text(), "");
    }
}

#[test]
fn only_the_first_chat_choice_is_read() {
    let reply = json!({"choices": [
        {"finish_reason": "stop", "message": {"content": "first"}},
        {"finish_reason": "length", "message": {"content": "second"}},
    ]});

    let (result, trace) = read(OpenAiApi::ChatCompletions, &reply);

    assert_eq!(result.expect("an answer").text(), "first");
    assert_eq!(trace.finish_reason(), Some("stop"));
}

#[test]
fn a_token_count_the_vendor_left_out_is_none_and_a_zero_stays_a_zero() {
    let cases = [
        (None, (None, None)),
        (Some(Value::Null), (None, None)),
        (Some(json!({})), (None, None)),
        (Some(json!({"in": null, "out": 7})), (None, Some(7))),
        (Some(json!({"out": 7})), (None, Some(7))),
        (Some(json!({"in": 12, "out": null})), (Some(12), None)),
        (Some(json!({"in": 12})), (Some(12), None)),
        (Some(json!({"in": 0, "out": 0})), (Some(0), Some(0))),
        (Some(json!({"in": 12, "out": 7, "total_tokens": 19})), (Some(12), Some(7))),
    ];
    for (api, input, output) in [
        (OpenAiApi::Responses, "input_tokens", "output_tokens"),
        (OpenAiApi::ChatCompletions, "prompt_tokens", "completion_tokens"),
    ] {
        for (usage, expected) in &cases {
            let mut reply = match api {
                OpenAiApi::Responses => responses_reply(ANSWER),
                OpenAiApi::ChatCompletions => chat_reply(ANSWER, Some("stop")),
            };
            let members = reply.as_object_mut().expect("an object");
            members.remove("usage");
            if let Some(usage) = usage {
                // The case's `in` and `out` under the api's own names.
                let usage = match usage.as_object() {
                    Some(counts) => Value::Object(
                        counts
                            .iter()
                            .map(|(name, count)| {
                                let name = match name.as_str() {
                                    "in" => input,
                                    "out" => output,
                                    other => other,
                                };
                                (name.to_owned(), count.clone())
                            })
                            .collect(),
                    ),
                    None => usage.clone(),
                };
                members.insert("usage".to_owned(), usage);
            }

            let (result, _) = read(api, &reply);

            let result = result.expect("an answer");
            assert_eq!(
                (result.input_tokens(), result.output_tokens()),
                *expected,
                "{api:?} {reply}"
            );
        }
    }
}

#[test]
fn a_json_body_that_is_not_the_vendors_reply_is_not_an_answer() {
    const EXPECTED: &str = "OpenAI did not answer: a body that is not the vendor's reply";
    let cases = [
        (OpenAiApi::Responses, json!([])),
        (OpenAiApi::Responses, json!("SENTINEL text")),
        (OpenAiApi::Responses, json!({"status": 7})),
        (OpenAiApi::Responses, json!({"status": "completed", "output": "SENTINEL"})),
        (OpenAiApi::Responses, json!({"status": "completed", "output": [{"content": []}]})),
        (
            OpenAiApi::Responses,
            json!({"status": "completed", "usage": {"input_tokens": "SENTINEL"}}),
        ),
        (OpenAiApi::ChatCompletions, json!([])),
        (
            OpenAiApi::ChatCompletions,
            json!([[{"finish_reason": "stop", "message": {"content": "SENTINEL"}}]]),
        ),
        (OpenAiApi::ChatCompletions, json!({})),
        (OpenAiApi::ChatCompletions, json!({"choices": null})),
        (OpenAiApi::ChatCompletions, json!({"choices": []})),
        (OpenAiApi::ChatCompletions, json!({"choices": [{"finish_reason": "stop"}]})),
        (OpenAiApi::ChatCompletions, json!({"choices": [{"message": {"content": ["SENTINEL"]}}]})),
        (OpenAiApi::ChatCompletions, json!({"error": {"message": "SENTINEL"}})),
    ];
    for (api, reply) in cases {
        let (result, trace) = read(api, &reply);

        let message = result.expect_err("not the vendor's reply").to_string();
        assert_eq!(message, EXPECTED, "{api:?} {reply}");
        assert!(!message.contains("SENTINEL"));
        // The body is still in the trace, with no stop reason.
        assert_eq!(parsed(trace.response()), reply);
        assert_eq!(trace.finish_reason(), None);
    }
}

// ------------------------------------------------------------ the exchange

#[tokio::test]
async fn a_refused_reply_is_in_the_trace_of_a_real_exchange() {
    let reply = json!({
        "status": "completed",
        "output": [{"type": "message", "content": [{"type": "refusal", "refusal": "No."}]}],
    });
    let server = answering(StatusCode::OK, reply.to_string()).await;
    let provider = provider(&server, OpenAiApi::Responses);

    let (result, trace) = ask(&provider, true).await;

    let non_answer = result.expect("a success status is not an exchange failure");
    assert_eq!(non_answer.expect_err("a refusal").to_string(), "OpenAI did not answer: refusal");
    assert_eq!(parsed(trace.request()), sent(&server));
    assert_eq!(parsed(trace.response()), reply);
    assert_eq!(trace.finish_reason(), Some("completed"));
}

#[tokio::test]
async fn a_failure_status_is_an_api_error_with_the_request_in_the_trace() {
    for api in [OpenAiApi::Responses, OpenAiApi::ChatCompletions] {
        let server =
            answering(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"overloaded"}}"#)
                .await;
        let provider = provider(&server, api);

        let (result, trace) = ask(&provider, false).await;

        let error = result.expect_err("a failure status");
        let SdkErrorKind::Api(api_error) = error.kind() else {
            panic!("expected an API error, got {error:?}");
        };
        assert_eq!(api_error.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(api_error.kind(), ApiErrorKind::InternalServer);
        assert_eq!(parsed(trace.request()), sent(&server));
        assert_eq!(trace.api(), Some(api.name()));
        assert_eq!(trace.response(), None);
        assert_eq!(trace.finish_reason(), None);
    }
}

#[tokio::test]
async fn a_success_body_that_is_not_json_is_not_an_answer() {
    let server = answering(StatusCode::OK, "<html>a gateway's page</html>").await;
    let provider = provider(&server, OpenAiApi::ChatCompletions);

    let (result, trace) = ask(&provider, false).await;

    let non_answer = result.expect("a success status is not an exchange failure");
    assert_eq!(
        non_answer.expect_err("the body is not JSON").to_string(),
        "OpenAI did not answer: status 200 with a body that is not JSON"
    );
    assert!(trace.request().is_some());
    assert_eq!(trace.response(), None);
}

#[tokio::test]
async fn a_body_over_the_limit_is_refused_by_the_provider() {
    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()).await;
    let provider = builder(&server).max_response_bytes(16).build().expect("the provider builds");

    let (result, _) = ask(&provider, false).await;

    let error = result.expect_err("the body is over the limit");
    assert!(matches!(error.kind(), SdkErrorKind::ResponseTooLarge { limit: 16 }), "{error:?}");
}
