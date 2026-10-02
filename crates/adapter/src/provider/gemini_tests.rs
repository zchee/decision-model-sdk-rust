//! Tests for the Gemini provider: the request it writes, the reply it reads,
//! the text rule of the vendor's SDK, the builder's refusals and the key's
//! sources. Every request goes to a loopback server.

use ::http::StatusCode;
use serde_json::json;
use test_support::{Protocol, TestServer, json_response};
use typesafe_sdk::{ApiErrorKind, ErrorKind as SdkErrorKind};

use super::*;
use crate::{
    error::ErrorKind,
    provider::{AttemptTrace, Message},
};

/// The made-up key every test passes to the builder. No test reads a real
/// one.
const KEY: &str = "sk-test-0123456789abcdef";

/// The model the tests ask.
const MODEL: &str = "gemini-3.8-flash";

/// Upstream's schema of `tests/test_provider_requests.py`.
const SCHEMA: &str = r#"{"type":"object","properties":{"answers":{"type":"object"}}}"#;

/// What one call of the provider returns.
type Outcome = Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>;

/// Upstream's two messages of `tests/test_provider_requests.py`.
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

/// A builder with the made-up key, `base_url` and a five-second deadline.
fn builder(base_url: &str) -> GeminiProviderBuilder {
    GeminiProvider::builder(MODEL).api_key(KEY).base_url(base_url).timeout(Duration::from_secs(5))
}

/// A provider with the made-up key that sends to `base_url`.
fn provider(base_url: &str) -> GeminiProvider {
    builder(base_url).build().expect("a provider")
}

/// One call of `provider`, and what it recorded.
async fn ask<S>(
    provider: &GeminiProvider<S>,
    messages: &[Message],
    structured: bool,
) -> (Outcome, AttemptTrace)
where
    S: HttpService,
{
    let schema = Schema::from_json(SCHEMA).expect("a schema");
    let mut trace = AttemptTrace::default();
    let outcome =
        provider.request(ProviderCall::new(messages, &schema, structured, &mut trace)).await;
    (outcome, trace)
}

/// The body the provider sends for `messages`, as the server received it.
async fn sent_body(messages: &[Message], structured: bool) -> Value {
    let server = answering(StatusCode::OK, interaction("{}", "completed").to_string()).await;
    let (outcome, trace) = ask(&provider(server.base_url()), messages, structured).await;
    outcome.expect("the exchange succeeds").expect("an answer");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).expect("a JSON body");
    // The trace holds the body as it was sent.
    let recorded: Value =
        serde_json::from_str(trace.request().expect("a recorded request")).expect("JSON");
    assert_eq!(recorded, body);
    assert_eq!(trace.api(), Some("interactions"));
    body
}

/// Upstream's `_interaction_payload` of `tests/test_gemini_transports.py`.
fn interaction(text: &str, status: &str) -> Value {
    json!({
        "id": "interaction-test",
        "status": status,
        "model": MODEL,
        "steps": [{"type": "model_output", "content": [{"type": "text", "text": text}]}],
        "usage": {"total_input_tokens": 20, "total_output_tokens": 5, "total_tokens": 25},
    })
}

/// What the provider makes of the success response `body`, and what it
/// recorded.
async fn reply_to(body: &Value) -> (Result<ProviderResult, NonAnswer>, AttemptTrace) {
    let server = answering(StatusCode::OK, body.to_string()).await;
    let (outcome, trace) = ask(&provider(server.base_url()), &messages(), true).await;
    assert_eq!(server.request_count(), 1);
    (outcome.expect("the exchange succeeds"), trace)
}

/// A `model_output` step holding `content`.
fn model_output(content: Value) -> Value {
    json!({"type": "model_output", "content": content})
}

/// A `text` content item.
fn text(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// The text the rule finds in a response with these `steps`.
fn text_of(steps: Value) -> String {
    output_text(&json!({"status": "completed", "steps": steps}))
}

// ------------------------------------------------------------- the request

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_request_puts_schema_in_response_format_when_structured
async fn request_puts_schema_in_response_format_when_structured() {
    let body = sent_body(&messages(), true).await;

    assert_eq!(
        body,
        json!({
            "model": MODEL,
            "input": [
                {"type": "user_input", "content": [{"type": "text", "text": "the document"}]},
            ],
            "store": false,
            "system_instruction": "system prompt",
            "response_format": {
                "type": "text",
                "mime_type": "application/json",
                "schema": {"type": "object", "properties": {"answers": {"type": "object"}}},
            },
        })
    );
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_request_omits_response_format_when_prompted
async fn request_omits_response_format_when_prompted() {
    let body = sent_body(&messages(), false).await;

    assert_eq!(
        body,
        json!({
            "model": MODEL,
            "input": [
                {"type": "user_input", "content": [{"type": "text", "text": "the document"}]},
            ],
            "store": false,
            "system_instruction": "system prompt",
        })
    );
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_request_sends_correction_turns_as_steps
async fn request_sends_correction_turns_as_steps() {
    let mut messages = messages();
    messages.push(Message::new(Role::Assistant, r#"{"answers":"#));
    messages.push(Message::new(Role::User, "fix it"));

    let body = sent_body(&messages, true).await;

    assert_eq!(
        body["input"],
        json!([
            {"type": "user_input", "content": [{"type": "text", "text": "the document"}]},
            {"type": "model_output", "content": [{"type": "text", "text": "{\"answers\":"}]},
            {"type": "user_input", "content": [{"type": "text", "text": "fix it"}]},
        ])
    );
}

#[tokio::test]
async fn system_messages_are_joined_and_left_out_when_empty() {
    let two = [
        Message::new(Role::System, "first"),
        Message::new(Role::User, "the document"),
        Message::new(Role::System, "second"),
    ];
    assert_eq!(sent_body(&two, false).await["system_instruction"], json!("first\n\nsecond"));

    for none in [
        vec![Message::new(Role::User, "the document")],
        vec![Message::new(Role::System, ""), Message::new(Role::User, "the document")],
    ] {
        let body = sent_body(&none, false).await;
        assert_eq!(
            body,
            json!({
                "model": MODEL,
                "input": [
                    {"type": "user_input", "content": [{"type": "text", "text": "the document"}]},
                ],
                "store": false,
            })
        );
    }
}

#[tokio::test]
async fn the_request_is_one_post_to_the_interactions_path_with_the_key() {
    let server = answering(StatusCode::OK, interaction("{}", "completed").to_string()).await;
    let prefixed = format!("{}/proxy/", server.base_url());

    for (base_url, path) in [
        (server.base_url(), "/v1beta/interactions"),
        (prefixed.as_str(), "/proxy/v1beta/interactions"),
    ] {
        let (outcome, _) = ask(&provider(base_url), &messages(), true).await;
        outcome.expect("the exchange succeeds").expect("an answer");

        let request = server.requests().pop().expect("a request");
        assert_eq!(request.method, ::http::Method::POST);
        assert_eq!(request.uri.path(), path);
        assert_eq!(request.uri.query(), None);
        assert_eq!(request.header_values("x-goog-api-key"), [KEY]);
        assert_eq!(request.header_values("content-type"), ["application/json"]);
        assert_eq!(
            request.header_values("user-agent"),
            [concat!("typesafe-sdk-rust-adapter/", env!("CARGO_PKG_VERSION"))]
        );
    }
    assert_eq!(server.request_count(), 2);
}

// --------------------------------------------------------------- the reply

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_result_reads_output_text_and_usage
async fn result_reads_output_text_and_usage() {
    let body = interaction(r#"{"answers": {}}"#, "completed");

    let (result, trace) = reply_to(&body).await;

    let result = result.expect("an answer");
    assert_eq!(result.text(), r#"{"answers": {}}"#);
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(20), Some(5)));
    let recorded: Value =
        serde_json::from_str(trace.response().expect("a recorded response")).expect("JSON");
    assert_eq!(recorded, body);
    assert_eq!(trace.finish_reason(), Some("completed"));
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_incomplete_status_is_not_treated_as_an_answer
async fn incomplete_status_is_not_an_answer() {
    for status in ["incomplete", "failed", "cancelled", "budget_exceeded"] {
        let body = interaction(r#"{"answers":{"positive":true}}"#, status);

        let (result, trace) = reply_to(&body).await;

        let refused = result.expect_err("not an answer");
        assert_eq!(refused.to_string(), format!("Gemini did not answer: status {status}"));
        // The refused reply is in the trace, with its status as the reason.
        let recorded: Value =
            serde_json::from_str(trace.response().expect("a recorded response")).expect("JSON");
        assert_eq!(recorded, body);
        assert_eq!(trace.finish_reason(), Some(status));
    }
}

#[tokio::test]
// Upstream: tests/test_provider_requests.py::test_gemini_omitted_usage_is_not_treated_as_an_answer
async fn omitted_usage_is_not_an_answer() {
    let without =
        |member: &str| format!("Gemini did not answer: status completed without {member}");
    let mut no_usage = interaction(r#"{"answers":{}}"#, "completed");
    no_usage.as_object_mut().expect("an object").remove("usage");
    let mut null_usage = no_usage.clone();
    null_usage["usage"] = Value::Null;
    let mut no_input = interaction(r#"{"answers":{}}"#, "completed");
    no_input["usage"] = json!({"total_output_tokens": 5});
    let mut no_output = interaction(r#"{"answers":{}}"#, "completed");
    no_output["usage"] = json!({"total_input_tokens": 20, "total_output_tokens": null});
    let mut text_count = interaction(r#"{"answers":{}}"#, "completed");
    text_count["usage"] = json!({"total_input_tokens": "20", "total_output_tokens": 5});

    for (body, member) in [
        (no_usage, "total_input_tokens"),
        (null_usage, "total_input_tokens"),
        (no_input, "total_input_tokens"),
        (no_output, "total_output_tokens"),
        (text_count, "total_input_tokens"),
    ] {
        let (result, trace) = reply_to(&body).await;

        assert_eq!(result.expect_err("not an answer").to_string(), without(member), "{body}");
        assert!(trace.response().is_some());
        assert_eq!(trace.finish_reason(), Some("completed"));
    }
}

#[tokio::test]
async fn a_body_without_a_status_is_not_the_vendor_s_reply() {
    let mut missing = interaction("{}", "completed");
    missing.as_object_mut().expect("an object").remove("status");
    let mut number = interaction("{}", "completed");
    number["status"] = json!(7);

    for body in [missing, number, json!([]), json!("completed")] {
        let (result, trace) = reply_to(&body).await;

        assert_eq!(
            result.expect_err("not an answer").to_string(),
            "Gemini did not answer: a body that is not the vendor's reply",
            "{body}"
        );
        assert!(trace.response().is_some());
        assert_eq!(trace.finish_reason(), None);
    }
}

#[tokio::test]
async fn the_body_s_error_text_stays_out_of_the_refusal() {
    let mut body = interaction("the model's own words", "failed");
    body["errors"] = json!([{"code": "internal", "message": "the vendor's own words"}]);

    let (result, trace) = reply_to(&body).await;

    let refused = result.expect_err("not an answer").to_string();
    assert_eq!(refused, "Gemini did not answer: status failed");
    // Both texts stay where the caller can read them: in the trace.
    let recorded = trace.response().expect("a recorded response");
    assert!(recorded.contains("the vendor's own words"));
    assert!(recorded.contains("the model's own words"));
}

#[tokio::test]
async fn a_status_the_vendor_chose_is_escaped_and_cut() {
    let (result, _) = reply_to(&interaction("{}", "line\nbreak")).await;
    assert_eq!(
        result.expect_err("not an answer").to_string(),
        "Gemini did not answer: status line\\nbreak"
    );

    let (result, _) = reply_to(&interaction("{}", &"s".repeat(500))).await;
    let refused = result.expect_err("not an answer").to_string();
    assert_eq!(refused.chars().count(), "Gemini did not answer: ".len() + 200 + 1);
    assert!(refused.ends_with('\u{2026}'));
}

#[tokio::test]
async fn a_success_body_that_is_not_json_is_refused_and_not_recorded() {
    let server = answering(StatusCode::OK, "<html>").await;

    let (outcome, trace) = ask(&provider(server.base_url()), &messages(), true).await;

    let refused = outcome.expect("the exchange succeeds").expect_err("not an answer");
    assert_eq!(
        refused.to_string(),
        "Gemini did not answer: status 200 with a body that is not JSON"
    );
    assert!(trace.request().is_some());
    assert_eq!(trace.response(), None);
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_failure_status_is_an_api_error_with_the_request_recorded() {
    let server = answering(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"error":{"code":503,"message":"The model is overloaded.","status":"UNAVAILABLE"}}"#,
    )
    .await;

    let (outcome, trace) = ask(&provider(server.base_url()), &messages(), false).await;

    let error = outcome.expect_err("a failure status");
    let SdkErrorKind::Api(api) = error.kind() else {
        panic!("not an API error: {error:?}");
    };
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.kind(), ApiErrorKind::InternalServer);
    assert!(trace.request().is_some());
    assert_eq!(trace.response(), None);
    assert_eq!(server.request_count(), 1);
}

// ------------------------------------------------------------ the text rule

#[test]
fn text_is_the_last_model_output_s_text() {
    assert_eq!(text_of(json!([model_output(json!([text("answer")]))])), "answer");
}

#[test]
fn text_walk_stops_at_a_user_input_step() {
    let steps = json!([
        model_output(json!([text("before the question")])),
        {"type": "user_input", "content": [text("a question")]},
        model_output(json!([text("after the question")])),
    ]);
    assert_eq!(text_of(steps), "after the question");

    // A trailing `user_input` step ends the walk before any text is found.
    let steps = json!([
        model_output(json!([text("an older answer")])),
        {"type": "user_input", "content": [text("a question")]},
    ]);
    assert_eq!(text_of(steps), "");
}

#[test]
fn text_walk_skips_other_steps_until_text_is_found() {
    let steps = json!([
        model_output(json!([text("answer")])),
        {"type": "thought", "signature": "c2lnbmF0dXJl"},
        {"type": "function_call", "name": "lookup"},
        {"no type": true},
        "not a step",
    ]);
    assert_eq!(text_of(steps), "answer");
}

#[test]
fn text_walk_stops_at_another_step_once_text_is_found() {
    let steps = json!([
        model_output(json!([text("first, ")])),
        {"type": "thought", "signature": "c2lnbmF0dXJl"},
        model_output(json!([text("second")])),
    ]);
    assert_eq!(text_of(steps), "second");
}

#[test]
fn text_walk_treats_content_that_is_not_a_list_as_another_step() {
    // Skipped while no text has been found.
    let steps = json!([
        model_output(json!([text("answer")])),
        model_output(json!("not a list")),
        {"type": "model_output"},
    ]);
    assert_eq!(text_of(steps), "answer");

    // Ends the walk once text has been found.
    let steps = json!([
        model_output(json!([text("first, ")])),
        {"type": "model_output"},
        model_output(json!([text("second")])),
    ]);
    assert_eq!(text_of(steps), "second");
}

#[test]
fn text_that_is_not_a_string_counts_as_empty() {
    let steps = json!([model_output(json!([
        text("kept"),
        {"type": "text", "text": 7},
        {"type": "text"},
        {"type": "text", "text": null},
    ]))]);
    assert_eq!(text_of(steps), "kept");

    // An empty part still counts as found text: the step before it ends
    // the walk.
    let steps = json!([
        model_output(json!([text("older")])),
        {"type": "thought"},
        model_output(json!([{"type": "text", "text": 7}])),
    ]);
    assert_eq!(text_of(steps), "");
}

#[test]
fn text_walk_stops_at_the_first_other_item_after_a_text_one() {
    // Items of another kind after the last text are passed over; the one
    // before it ends the walk, in this step and in every step before it.
    let steps = json!([
        model_output(json!([text("oldest, ")])),
        model_output(json!([
            text("older, "),
            {"type": "image", "data": "aW1hZ2U="},
            text("kept"),
            {"type": "image", "data": "aW1hZ2U="},
            {"no type": true},
        ])),
    ]);
    assert_eq!(text_of(steps), "kept");
}

#[test]
fn text_parts_are_joined_in_forward_order() {
    let steps = json!([
        model_output(json!([text("one, "), text("two, ")])),
        model_output(json!([])),
        model_output(json!([text("three, "), text("four")])),
    ]);
    assert_eq!(text_of(steps), "one, two, three, four");
}

#[test]
fn text_is_empty_without_any() {
    for response in [
        json!({"status": "completed"}),
        json!({"status": "completed", "steps": null}),
        json!({"status": "completed", "steps": {"type": "model_output"}}),
        json!({"status": "completed", "steps": []}),
        json!({"status": "completed", "steps": [{"type": "thought"}]}),
        json!({"status": "completed", "steps": [model_output(json!([{"type": "image"}]))]}),
        json!([]),
    ] {
        assert_eq!(output_text(&response), "", "{response}");
    }
}

#[tokio::test]
async fn a_completed_reply_without_text_is_an_empty_answer() {
    let mut body = interaction("", "completed");
    body["steps"] = json!([{"type": "thought", "signature": "c2lnbmF0dXJl"}]);

    let (result, _) = reply_to(&body).await;

    let result = result.expect("an answer");
    assert_eq!(result.text(), "");
    assert_eq!((result.input_tokens(), result.output_tokens()), (Some(20), Some(5)));
}

// ----------------------------------------------------------- the provider

#[test]
fn the_provider_names_its_model_and_its_public_path() {
    let provider = provider("http://127.0.0.1:1");

    assert_eq!(provider.model_name(), MODEL);
    assert_eq!(provider.type_name(), "system_one_adapter::GeminiProvider");
}

#[test]
fn a_builder_left_at_its_defaults_holds_the_shared_limits() {
    // The key is given and Gemini reads no base URL from the environment,
    // so no variable is read here.
    let provider = GeminiProvider::builder(MODEL).api_key(KEY).build().expect("a provider");

    assert_eq!(provider.limits, Limits::new(None, None).expect("the defaults are legal"));
}

#[test]
fn gemini_names_its_endpoint_for_the_retry_line() {
    let fixed = |base_url: &str| {
        let provider = provider(base_url);
        let uri = provider.log_uri().expect("a log URI").clone();
        assert_eq!(uri.query(), None);
        assert!(!uri.authority().expect("an authority").as_str().contains('@'));
        assert_eq!(uri.path(), "/v1beta/interactions");
        uri.to_string()
    };

    // The caller's prefix, a key placed in it included, is not named.
    assert_eq!(fixed("http://127.0.0.1:8080"), "http://127.0.0.1:8080/v1beta/interactions");
    assert_eq!(
        fixed(&format!("http://127.0.0.1:8080/{KEY}/v1/")),
        "http://127.0.0.1:8080/v1beta/interactions"
    );
    assert_eq!(
        fixed("https://proxy.example:443/tenant/7"),
        "https://proxy.example/v1beta/interactions"
    );

    // Without a base URL the endpoint is the vendor's own.
    let default = GeminiProvider::builder(MODEL).api_key(KEY).build().expect("a provider");
    assert_eq!(
        default.log_uri().expect("a log URI").to_string(),
        "https://generativelanguage.googleapis.com/v1beta/interactions"
    );
}

#[test]
fn debug_prints_the_model_the_host_and_the_api_only() {
    let base_url = "http://127.0.0.1:8080/caller-prefix";
    let builder = builder(base_url).add_root_certificate(b"not read by this test".to_vec());
    let built = provider(base_url);

    let provider_text = format!("{built:?}");
    assert_eq!(
        provider_text,
        r#"GeminiProvider { model: "gemini-3.8-flash", host: "127.0.0.1", api: "interactions", .. }"#
    );
    let builder_text = format!("{builder:?}");
    assert!(builder_text.contains(MODEL), "{builder_text}");
    assert!(builder_text.contains("extra_roots: 1"), "{builder_text}");
    for text in [provider_text, builder_text, format!("{built:#?}"), format!("{builder:#?}")] {
        assert!(!text.contains(KEY), "{text}");
        assert!(!text.contains("caller-prefix"), "{text}");
    }
}

#[tokio::test]
async fn a_clone_asks_the_same_model_at_the_same_endpoint() {
    let server = answering(StatusCode::OK, interaction("{}", "completed").to_string()).await;
    let original = provider(server.base_url());
    let clone = original.clone();
    drop(original);

    let (outcome, _) = ask(&clone, &messages(), false).await;

    outcome.expect("the exchange succeeds").expect("an answer");
    assert_eq!(clone.model_name(), MODEL);
    assert_eq!(server.requests()[0].header_values("x-goog-api-key"), [KEY]);
}

// ------------------------------------------------------------- the builder

/// The text of the `Config` error `builder` is refused with.
fn refusal(builder: GeminiProviderBuilder) -> String {
    let error = builder.build().expect_err("a refused builder");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    let message = error.to_string();
    assert!(!message.contains(KEY), "{message}");
    message
}

#[test]
fn the_builder_refuses_a_zero_limit() {
    let base_url = "http://127.0.0.1:1";

    assert_eq!(
        refusal(builder(base_url).timeout(Duration::ZERO)),
        "timeout must be greater than zero."
    );
    assert_eq!(
        refusal(builder(base_url).max_response_bytes(0)),
        "max_response_bytes must be at least 1: every response carries a body."
    );
    // The smallest legal values are accepted.
    builder(base_url)
        .timeout(Duration::from_nanos(1))
        .max_response_bytes(1)
        .build()
        .expect("a provider");
}

#[test]
fn the_builder_refuses_a_base_url_it_cannot_use() {
    for base_url in [
        "http://user:secret-word@127.0.0.1:1",
        "http://127.0.0.1:1/v1?secret-word=1",
        "http://127.0.0.1:1/v1#secret-word",
        "ftp://127.0.0.1/secret-word",
        "127.0.0.1/secret-word",
        "http://secret-word example",
    ] {
        let message = refusal(builder(base_url));
        assert!(message.contains("base URL"), "{message}");
        assert!(!message.contains("secret-word"), "{message}");
        assert!(!message.contains("127.0.0.1"), "{message}");
    }
}

#[test]
fn the_builder_refuses_a_key_it_cannot_send() {
    let base_url = "http://127.0.0.1:1";

    assert_eq!(
        refusal(GeminiProvider::builder(MODEL).base_url(base_url).api_key("")),
        "The API key is empty."
    );
    let message =
        refusal(GeminiProvider::builder(MODEL).base_url(base_url).api_key("sk-test-line\nfeed"));
    assert_eq!(message, "The API key holds a character that cannot be sent in an HTTP header.");
    assert!(!message.contains("sk-test-line"));
}

#[test]
fn the_builder_refuses_a_root_that_is_not_a_certificate() {
    let message = refusal(builder("http://127.0.0.1:1").add_root_certificate(b"not DER".to_vec()));
    assert!(message.starts_with("The TLS certificate verifier could not be built: "), "{message}");

    // The roots belong to the default transport: a caller's service is
    // built without reading them.
    let transport = Transport::new(Vec::new()).expect("the default transport");
    builder("http://127.0.0.1:1")
        .add_root_certificate(b"not DER".to_vec())
        .build_with_service(transport)
        .expect("a provider");
}

#[test]
fn a_provider_without_a_key_is_refused() {
    // With the `internals` feature another test's variables could be in
    // force in this process; an empty replacement of this test's own rules
    // that out. Without the feature a unit-test build finds no variable.
    #[cfg(feature = "internals")]
    let _environment = crate::__internals::env::replace();

    assert_eq!(
        refusal(GeminiProvider::builder(MODEL).base_url("http://127.0.0.1:1")),
        "No Gemini API key: pass one to api_key, or set GOOGLE_API_KEY or GEMINI_API_KEY."
    );
}

// ------------------------------------------------------ the key's sources

/// The key the provider sends, as the server received it.
#[cfg(feature = "internals")]
async fn key_sent(server: &TestServer, provider: &GeminiProvider) -> String {
    let (outcome, _) = ask(provider, &messages(), false).await;
    outcome.expect("the exchange succeeds").expect("an answer");
    let request = server.requests().pop().expect("a request");
    request.header_values("x-goog-api-key").join(",")
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn the_key_is_read_from_the_two_variables_in_the_vendor_s_order() {
    let server = answering(StatusCode::OK, interaction("{}", "completed").to_string()).await;
    let from_environment = || GeminiProvider::builder(MODEL).base_url(server.base_url()).build();

    // Each provider is built while the replacement lives and asked after it
    // is dropped: the environment is read once, when the provider is built.
    let (google_only, gemini_only, both, empty_google, explicit) = {
        let environment = crate::__internals::env::replace();
        environment.set("GOOGLE_API_KEY", "sk-test-from-google");
        let google_only = from_environment().expect("a provider");

        environment.remove("GOOGLE_API_KEY");
        environment.set("GEMINI_API_KEY", "sk-test-from-gemini");
        let gemini_only = from_environment().expect("a provider");

        environment.set("GOOGLE_API_KEY", "sk-test-from-google");
        let both = from_environment().expect("a provider");

        environment.set("GOOGLE_API_KEY", "");
        let empty_google = from_environment().expect("a provider");

        environment.set("GOOGLE_API_KEY", "sk-test-from-google");
        let explicit = provider(server.base_url());

        (google_only, gemini_only, both, empty_google, explicit)
    };

    assert_eq!(key_sent(&server, &google_only).await, "sk-test-from-google");
    assert_eq!(key_sent(&server, &gemini_only).await, "sk-test-from-gemini");
    assert_eq!(key_sent(&server, &both).await, "sk-test-from-google");
    assert_eq!(key_sent(&server, &empty_google).await, "sk-test-from-gemini");
    assert_eq!(key_sent(&server, &explicit).await, KEY);
}

#[cfg(feature = "internals")]
#[tokio::test]
async fn the_base_url_is_never_read_from_the_environment() {
    let server = answering(StatusCode::OK, interaction("{}", "completed").to_string()).await;

    let provider = {
        let environment = crate::__internals::env::replace();
        environment.set("GOOGLE_GEMINI_BASE_URL", server.base_url());
        environment.set("GEMINI_BASE_URL", server.base_url());
        environment.set("GOOGLE_API_KEY", KEY);
        GeminiProvider::builder(MODEL).build().expect("a provider")
    };

    assert_eq!(
        provider.log_uri().expect("a log URI").to_string(),
        "https://generativelanguage.googleapis.com/v1beta/interactions"
    );
    assert!(format!("{provider:?}").contains("generativelanguage.googleapis.com"));
    assert_eq!(server.request_count(), 0);
}

#[cfg(all(feature = "internals", unix))]
#[test]
fn a_key_variable_that_is_not_utf_8_is_refused_by_name() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt as _};

    let environment = crate::__internals::env::replace();
    environment.set("GOOGLE_API_KEY", OsString::from_vec(vec![b's', b'k', 0xff]));
    environment.set("GEMINI_API_KEY", KEY);

    assert_eq!(
        refusal(GeminiProvider::builder(MODEL).base_url("http://127.0.0.1:1")),
        "The GOOGLE_API_KEY environment variable is not valid UTF-8."
    );
}
