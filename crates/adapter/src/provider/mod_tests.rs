//! Tests for the provider seam.

use std::sync::Arc;

use super::*;
use crate::ErrorKind;

/// Text that must never appear in a `Debug` rendering.
const DOCUMENT: &str = "the caller's private document";

/// A provider of the kind a caller writes: it records what it sends and
/// answers from the messages it was given.
#[derive(Debug)]
struct EchoProvider;

impl Provider for EchoProvider {
    fn model_name(&self) -> &str {
        "echo-1"
    }

    fn request<'a>(
        &'a self,
        mut call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async move {
            let last = call
                .messages()
                .last()
                .map(|message| message.content().to_owned())
                .unwrap_or_default();
            let structured = call.structured();
            let schema_len = call.schema().as_str().len();
            call.trace().record_request(r#"{"model":"echo-1"}"#, "echo");
            if last.is_empty() {
                return Ok(Err(NonAnswer::new("echo refused an empty document")));
            }
            call.trace().record_response(r#"{"output":"ok"}"#, Some("stop"));
            Ok(Ok(ProviderResult::new(format!("{structured}:{schema_len}:{last}"), Some(3), None)))
        })
    }
}

fn schema() -> Schema {
    Schema::from_json(r#"{"type":"object"}"#).expect("an object is a schema")
}

#[tokio::test]
async fn a_caller_provider_runs_behind_a_trait_object() {
    let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
    let messages = [Message::new(Role::System, "rules"), Message::new(Role::User, "hello")];
    let schema = schema();
    let mut trace = AttemptTrace::default();

    let reply = provider.request(ProviderCall::new(&messages, &schema, true, &mut trace)).await;

    let result = reply.expect("no transport failure").expect("an answer");
    assert_eq!(provider.model_name(), "echo-1");
    assert_eq!(result.text(), "true:17:hello");
    assert_eq!(result.input_tokens(), Some(3));
    assert_eq!(result.output_tokens(), None);
    assert_eq!(trace.api, Some("echo"));
    assert_eq!(trace.request.as_deref().map(RawValue::get), Some(r#"{"model":"echo-1"}"#));
    assert_eq!(trace.response.as_deref().map(RawValue::get), Some(r#"{"output":"ok"}"#));
    assert_eq!(trace.finish_reason, Some(Some("stop".to_owned())));
}

#[tokio::test]
async fn a_non_answer_is_the_inner_error_and_leaves_the_response_unrecorded() {
    let provider: Arc<dyn Provider> = Arc::new(EchoProvider);
    let messages = [Message::new(Role::User, "")];
    let schema = schema();
    let mut trace = AttemptTrace::default();

    let reply = provider.request(ProviderCall::new(&messages, &schema, false, &mut trace)).await;

    let non_answer = reply.expect("no transport failure").expect_err("a non-answer");
    assert_eq!(non_answer.to_string(), "echo refused an empty document");
    assert!(trace.request.is_some(), "the request was recorded before the refusal");
    assert!(trace.response.is_none());
    assert_eq!(trace.finish_reason, None, "no response recorded means no finish_reason member");
}

#[test]
fn the_future_of_a_provider_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let provider = EchoProvider;
    let messages = [Message::new(Role::User, "x")];
    let schema = schema();
    let mut trace = AttemptTrace::default();

    let future = provider.request(ProviderCall::new(&messages, &schema, true, &mut trace));

    assert_send(&future);
}

#[test]
fn a_message_serializes_as_role_and_content_and_reads_back() {
    let messages = [
        (Message::new(Role::System, "rules"), r#"{"role":"system","content":"rules"}"#),
        (
            Message::new(Role::User, "<document>\n1\n</document>"),
            r#"{"role":"user","content":"<document>\n1\n</document>"}"#,
        ),
        (Message::new(Role::Assistant, "{}"), r#"{"role":"assistant","content":"{}"}"#),
    ];

    for (message, want) in messages {
        let written = serde_json::to_string(&message).expect("a message serializes");
        let read: Message =
            serde_json::from_str(&written).expect("a serialized message reads back");

        assert_eq!(written, want);
        assert_eq!(read, message, "round trip of {want}");
    }
}

#[test]
fn a_role_outside_the_three_is_refused() {
    let read = serde_json::from_str::<Message>(r#"{"role":"tool","content":"x"}"#);

    assert!(read.is_err(), "`tool` is not a role: {read:?}");
}

#[test]
fn debug_of_a_message_and_a_result_prints_lengths_not_text() {
    let message = Message::new(Role::User, DOCUMENT);
    let result = ProviderResult::new(DOCUMENT.to_owned(), Some(1), Some(2));

    let rendered = format!("{message:?} {result:?}");

    assert!(!rendered.contains(DOCUMENT), "Debug printed the text: {rendered}");
    assert!(rendered.contains(&format!("<{} bytes>", DOCUMENT.len())), "{rendered}");
    assert!(rendered.contains("User"), "{rendered}");
}

#[test]
fn a_schema_keeps_the_text_of_an_object_and_drops_surrounding_space() {
    let text = " \n{\"z\":1, \"a\":{\"b\":[true]}}\t";

    let schema = Schema::from_json(text).expect("an object is a schema");

    assert_eq!(schema.as_str(), "{\"z\":1, \"a\":{\"b\":[true]}}");
    assert_eq!(
        serde_json::to_string(&schema).expect("a schema serializes"),
        "{\"z\":1, \"a\":{\"b\":[true]}}",
        "serializing embeds the text unchanged, member order included"
    );
}

#[test]
fn a_schema_that_is_not_one_json_object_is_an_invalid_request() {
    let cases = [
        ("an array", "[1,2]"),
        ("a string", "\"object\""),
        ("a number", "1"),
        ("null", "null"),
        ("not JSON", "{\"type\":"),
        ("two values", "{} {}"),
        ("empty", ""),
    ];

    for (name, text) in cases {
        let error = Schema::from_json(text).expect_err(name);

        assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{name}: {error:?}");
        assert!(error.to_string().starts_with("The schema "), "{name}: {error}");
    }
}

#[test]
fn debug_of_a_schema_prints_its_length() {
    let text = format!(r#"{{"description":"{DOCUMENT}"}}"#);
    let schema = Schema::from_json(&text).expect("an object");

    let rendered = format!("{schema:?}");

    assert_eq!(rendered, format!("Schema {{ json: <{} bytes> }}", text.len()));
}

#[test]
fn an_attempt_trace_keeps_text_that_is_not_json_as_a_json_string() {
    let mut trace = AttemptTrace::default();

    trace.record_request("not json at all", "chat.completions");
    trace.record_response("{\"partial\":", None);

    assert_eq!(trace.request.as_deref().map(RawValue::get), Some("\"not json at all\""));
    assert_eq!(trace.response.as_deref().map(RawValue::get), Some("\"{\\\"partial\\\":\""));
    assert_eq!(
        trace.finish_reason,
        Some(None),
        "a response without a stop reason is recorded as null"
    );
}

#[test]
fn debug_of_a_call_and_its_trace_prints_counts_and_kinds() {
    let messages = [Message::new(Role::System, "rules"), Message::new(Role::User, DOCUMENT)];
    let schema = schema();
    let mut trace = AttemptTrace::default();
    trace.record_request(&format!("{{\"input\":\"{DOCUMENT}\"}}"), "responses");
    trace.record_response(&format!("{{\"output\":\"{DOCUMENT}\"}}"), Some("completed"));

    let rendered = format!("{:?}", ProviderCall::new(&messages, &schema, true, &mut trace));

    assert!(!rendered.contains(DOCUMENT), "Debug printed a body: {rendered}");
    assert!(rendered.contains("messages: 2"), "{rendered}");
    assert!(rendered.contains("api: Some(\"responses\")"), "{rendered}");
    assert!(rendered.contains("completed"), "{rendered}");
}

#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[test]
fn provider_names_read_and_print_as_upstream_spells_them() {
    for provider in ProviderName::ALL {
        let name = provider.to_string();

        let read = name.parse::<ProviderName>().expect("a compiled-in name reads back");

        assert_eq!(read, *provider);
        assert!(["openai", "anthropic", "gemini"].contains(&name.as_str()), "{name}");
    }
}

#[test]
fn an_unknown_provider_name_is_refused_without_repeating_it() {
    let names = ["mistral", "OpenAI", " openai", "openai ", "", "x-unlisted-provider"];

    for name in names {
        let error = name.parse::<ProviderName>().expect_err(name);
        let message = error.to_string();

        assert_eq!(error, ParseProviderNameError);
        assert!(message.starts_with("Unknown provider. "), "{message}");
        assert!(message.contains("feature is off"), "{message}");
        for unlisted in ["mistral", "OpenAI", "x-unlisted-provider"] {
            assert!(!message.contains(unlisted), "the message repeats {unlisted:?}: {message}");
        }
    }
}

#[cfg(all(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[test]
fn the_unknown_provider_message_lists_every_compiled_in_provider() {
    let message = ParseProviderNameError.to_string();

    assert_eq!(
        message,
        "Unknown provider. Use 'openai', 'anthropic', or 'gemini'; a provider whose feature is off is not among them."
    );
}

#[cfg(not(any(feature = "openai", feature = "anthropic", feature = "gemini")))]
#[test]
fn without_a_provider_feature_no_name_reads() {
    let error = "openai".parse::<ProviderName>().expect_err("no provider is compiled in");

    assert_eq!(
        error.to_string(),
        "Unknown provider. No provider is compiled in; a provider whose feature is off is not available."
    );
}
