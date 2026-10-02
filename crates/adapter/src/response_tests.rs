//! Tests for the response, its usage and its trace.

use std::sync::Arc;

use super::*;
use crate::provider::{Provider, ProviderCall, Role};

/// Text that must never appear in a `Debug` rendering.
const DOCUMENT: &str = "the caller's private document";

fn schema() -> Schema {
    Schema::from_json(r#"{"type":"object","properties":{}}"#).expect("an object is a schema")
}

fn attempt() -> Attempt {
    Attempt::new(
        vec![Message::new(Role::System, "rules"), Message::new(Role::User, DOCUMENT)],
        schema(),
        true,
        "gpt-4o-mini".to_owned(),
        "system_one_adapter::OpenAiProvider".to_owned(),
    )
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("the value serializes")
}

fn usage() -> Usage {
    Usage {
        input_tokens: Some(120),
        output_tokens: Some(30),
        input_tokens_total: Some(240),
        output_tokens_total: None,
        n_retries: 1,
        n_retries_malformed_structure: 1,
        latency: Duration::from_millis(1500),
    }
}

#[test]
fn an_attempt_that_recorded_nothing_and_failed_has_a_null_response() {
    let mut failed = attempt();
    failed.record(AttemptTrace::default());
    failed.record_error("Request timed out (timeout=600s).", "Timeout");

    let written = json(&failed);

    assert_eq!(
        written,
        concat!(
            r#"{"messages":[{"role":"system","content":"rules"},{"role":"user","content":"the caller's private document"}],"#,
            r#""model_request_parameters":{"schema":{"type":"object","properties":{}},"structured":true},"#,
            r#""llm_response":null,"#,
            r#""debug_info":{"model_name":"gpt-4o-mini","provider":"system_one_adapter::OpenAiProvider","#,
            r#""error":"Request timed out (timeout=600s).","error_type":"Timeout"}}"#,
        )
    );
    assert_eq!(failed.response(), None);
    assert_eq!(failed.request(), None);
    assert_eq!(failed.api(), None);
    assert_eq!(failed.error(), Some("Request timed out (timeout=600s)."));
    assert_eq!(failed.error_type(), Some("Timeout"));
}

#[test]
fn an_attempt_with_a_recorded_exchange_carries_api_finish_reason_and_request() {
    let mut trace = AttemptTrace::default();
    trace.record_request(r#"{"model":"gpt-4o-mini","input":[]}"#, "responses");
    trace.record_response(r#"{"id":"resp_1","status":"completed"}"#, Some("completed"));
    let result = ProviderResult::new("{\"answers\":{}}".to_owned(), Some(10), Some(2));
    let mut done = attempt();

    done.record(trace);
    done.record_result(&result);

    let written = json(&done);
    assert!(
        written.ends_with(concat!(
            r#""llm_response":{"id":"resp_1","status":"completed"},"#,
            r#""debug_info":{"model_name":"gpt-4o-mini","provider":"system_one_adapter::OpenAiProvider","#,
            r#""api":"responses","finish_reason":"completed"},"#,
            r#""request":{"model":"gpt-4o-mini","input":[]}}"#,
        )),
        "{written}"
    );
    assert_eq!(
        done.response(),
        Some(r#"{"id":"resp_1","status":"completed"}"#),
        "the recorded body wins over the fallback"
    );
    assert_eq!(done.finish_reason(), Some("completed"));
}

#[test]
fn a_recorded_response_without_a_stop_reason_serializes_a_null_finish_reason() {
    let mut trace = AttemptTrace::default();
    trace.record_response("{}", None);
    let mut done = attempt();

    done.record(trace);

    let written = json(&done);
    assert!(
        written
            .contains(r#""provider":"system_one_adapter::OpenAiProvider","finish_reason":null}"#),
        "{written}"
    );
    assert_eq!(done.finish_reason(), None);
}

#[test]
fn a_result_without_a_recorded_response_falls_back_to_text_and_tokens() {
    let result = ProviderResult::new("{\"answers\":{\"x\":0.5}}".to_owned(), None, Some(7));
    let mut done = attempt();
    done.record(AttemptTrace::default());

    done.record_result(&result);

    assert_eq!(
        done.response(),
        Some(r#"{"text":"{\"answers\":{\"x\":0.5}}","input_tokens":null,"output_tokens":7}"#)
    );
}

#[test]
fn debug_of_an_attempt_prints_roles_lengths_and_kinds() {
    let mut trace = AttemptTrace::default();
    trace.record_request(&format!("{{\"input\":\"{DOCUMENT}\"}}"), "responses");
    trace.record_response(&format!("{{\"output\":\"{DOCUMENT}\"}}"), Some("incomplete"));
    let mut failed = attempt();
    failed.record(trace);
    failed.record_error(format!("vendor said: {DOCUMENT}"), "Api");

    let rendered = format!("{failed:?}");

    assert!(
        failed.request().is_some_and(|body| body.contains(DOCUMENT)),
        "the request body holds the document"
    );
    assert!(
        failed.response().is_some_and(|body| body.contains(DOCUMENT)),
        "the response body holds the document"
    );
    assert!(!rendered.contains(DOCUMENT), "Debug printed user data: {rendered}");
    assert!(rendered.contains("finish_reason: Some(Some(\"incomplete\"))"), "{rendered}");
    assert!(rendered.contains("messages: [System, User]"), "{rendered}");
    assert!(rendered.contains("error_type: Some(\"Api\")"), "{rendered}");
    assert!(rendered.contains("gpt-4o-mini"), "{rendered}");
}

#[test]
fn usage_serializes_upstream_members_with_latency_in_seconds() {
    let written = json(&usage());

    assert_eq!(
        written,
        concat!(
            r#"{"input_tokens":120,"output_tokens":30,"input_tokens_total":240,"output_tokens_total":null,"#,
            r#""n_retries":1,"n_retries_malformed_structure":1,"latency":1.5}"#,
        )
    );
}

#[test]
fn usage_accessors_return_what_was_counted() {
    let usage = usage();

    assert_eq!(
        (
            usage.input_tokens(),
            usage.output_tokens(),
            usage.input_tokens_total(),
            usage.output_tokens_total()
        ),
        (Some(120), Some(30), Some(240), None)
    );
    assert_eq!((usage.n_retries(), usage.n_retries_malformed_structure()), (1, 1));
    assert_eq!(usage.latency(), Duration::from_millis(1500));
}

#[test]
fn a_response_trace_serializes_probabilities_before_the_attempts() {
    let trace = Trace {
        attempts: Vec::new(),
        retry_reasons: vec![RetryReason::new(
            RetryCategory::ProviderError,
            "Request timed out (timeout=600s).",
        )],
        probabilities: Some(ProbabilityDebug {
            max_error: 0.25,
            invalid_probs: 1,
            probability_errors: vec![("genre".to_owned(), 0.25)],
            original_probabilities: vec![(
                "genre".to_owned(),
                vec![("fiction".to_owned(), 0.5), ("nonfiction".to_owned(), 0.75)],
            )],
        }),
    };

    let written = json(&trace);

    assert_eq!(
        written,
        concat!(
            r#"{"max_error":0.25,"invalid_probs":1,"probability_errors":{"genre":0.25},"#,
            r#""original_probabilities":{"genre":{"fiction":0.5,"nonfiction":0.75}},"#,
            r#""llm_attempts":[],"retry_reasons":[["provider_error","Request timed out (timeout=600s)."]]}"#,
        )
    );
    assert_eq!(trace.max_error(), 0.25);
    assert_eq!(trace.invalid_probs(), 1);
    assert_eq!(trace.probability_errors().collect::<Vec<_>>(), [("genre", 0.25)]);
    let original = trace
        .original_probabilities()
        .map(|(question, labels)| (question, labels.collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    assert_eq!(original, [("genre", vec![("fiction", 0.5), ("nonfiction", 0.75)])]);
}

#[test]
fn original_probabilities_are_left_out_when_empty() {
    let trace = Trace {
        attempts: Vec::new(),
        retry_reasons: Vec::new(),
        probabilities: Some(ProbabilityDebug::default()),
    };

    let written = json(&trace);

    assert_eq!(
        written,
        r#"{"max_error":0.0,"invalid_probs":0,"probability_errors":{},"llm_attempts":[],"retry_reasons":[]}"#
    );
}

#[test]
fn a_response_serializes_model_usage_answers_and_debug_in_order() {
    let response = Response {
        model: "claude-haiku-4-5".to_owned(),
        usage: usage(),
        answers: Answers::default(),
        n_answers: 0,
        debug: Trace {
            attempts: Vec::new(),
            retry_reasons: Vec::new(),
            probabilities: Some(ProbabilityDebug::default()),
        },
    };

    let written = json(&response);

    assert!(
        written.starts_with(r#"{"model":"claude-haiku-4-5","usage":{"input_tokens":120,"#),
        "{written}"
    );
    assert!(written.ends_with(r#""answers":{},"debug":{"max_error":0.0,"invalid_probs":0,"probability_errors":{},"llm_attempts":[],"retry_reasons":[]}}"#), "{written}");
    assert_eq!(response.model(), "claude-haiku-4-5");
    assert_eq!(response.usage(), &usage());
    assert_eq!(response.answers(), &Answers::default());
    assert_eq!(response.debug().attempts().len(), 0);
    assert_eq!(response.into_answers(), Answers::default());
}

#[test]
fn debug_of_a_response_prints_counts_and_never_the_trace_text() {
    let mut failed = attempt();
    failed.record_error(DOCUMENT, "Connection");
    let response = Response {
        model: "gemini-3-flash".to_owned(),
        usage: usage(),
        answers: Answers::default(),
        n_answers: 3,
        debug: Trace {
            attempts: vec![failed, attempt()],
            retry_reasons: vec![RetryReason::new(RetryCategory::ProviderError, DOCUMENT)],
            probabilities: Some(ProbabilityDebug::default()),
        },
    };

    let rendered = format!("{response:?}");

    assert!(!rendered.contains(DOCUMENT), "Debug printed user data: {rendered}");
    assert!(rendered.contains("answers: 3"), "{rendered}");
    assert!(rendered.contains("attempts: 2"), "{rendered}");
    assert!(rendered.contains("retry_reasons: [ProviderError]"), "{rendered}");
}

#[test]
fn a_retry_reason_is_a_category_message_pair() {
    let reason =
        RetryReason::new(RetryCategory::MalformedStructure, "answers.x: expected a number");

    let written = json(&reason);

    assert_eq!(written, r#"["malformed_structure","answers.x: expected a number"]"#);
    assert_eq!(reason.category(), RetryCategory::MalformedStructure);
    assert_ne!(reason.category(), RetryCategory::ProviderError);
    assert_eq!(reason.message(), "answers.x: expected a number");
    assert_eq!(
        format!("{reason:?}"),
        "RetryReason { category: MalformedStructure, message: <28 bytes> }"
    );
}

#[test]
fn the_response_types_cross_threads() {
    // A later `Rc` or `RefCell` field would break these silently otherwise.
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_send<T: Send>() {}

    assert_send_sync::<Trace>();
    assert_send_sync::<Attempt>();
    assert_send_sync::<RetryReason>();
    assert_send_sync::<Usage>();
    assert_send_sync::<Response<Answers>>();
    assert_send_sync::<Arc<dyn Provider>>();
    assert_send::<ProviderCall<'_>>();
}

#[test]
fn retry_categories_work_as_set_members() {
    let seen = [
        RetryCategory::ProviderError,
        RetryCategory::MalformedStructure,
        RetryCategory::ProviderError,
    ]
    .into_iter()
    .collect::<std::collections::HashSet<_>>();

    assert_eq!(seen.len(), 2);
}
