//! The OpenAI provider under the client: retry budgets, non-answers and
//! token usage, what never reaches an error, a trace or an event, the
//! transport rules, and the replay of the eight recorded OpenAI cassettes.
//!
//! Every provider here is built with a made-up key and the base URL of a
//! server the test started; no request leaves the machine.

#[path = "support/cassette.rs"]
mod cassette;
#[path = "support/expected.rs"]
mod expected;
#[cfg(feature = "tracing")]
#[path = "support/recorder.rs"]
mod recorder;

use std::{
    error::Error as StdError,
    future::{Ready, ready},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderValue, Request, StatusCode, header::LOCATION};
use http_body_util::Full;
use serde_json::{Value, json};
use system_one_adapter::{
    AnswerMode, Answers, AttemptTrace, Client, ClientBuilder, Error, ErrorKind, Noul, OpenAiApi,
    OpenAiProvider, OpenAiProviderBuilder, PreparedQuestions, Provider, ProviderCall, Questions,
    Response, RetryPolicy, StructuredOutputs, Trace,
    typesafe_sdk::{self, ApiErrorKind, Body, BoxError},
};
use test_support::{
    Protocol, RecordedRequest, RefusingPort, SilentServer, TestServer, json_response,
};
use tower_service::Service;

use crate::cassette::Cassette;

/// The key every provider of this file is built with. It is made up, and no
/// test may find it in an error, a trace, a `Debug` or an event.
const KEY: &str = "sk-test-0123456789abcdef";

/// The text placed in a state that no error, `Debug` or event may hold.
const SENTINEL: &str = "SENTINEL-7f3a";

/// The state upstream's provider tests ask about.
const STATE: &str = "A delightful book.";

/// The reply text of a model that answered the one question.
const ANSWER: &str = r#"{"answers":{"positive":true}}"#;

/// The refusal text of upstream's refusal test.
const REFUSAL: &str = "Cannot evaluate this request.";

/// The one question upstream's provider tests ask.
fn positive() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .prepare()
        .expect("one noul is a valid question set")
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

/// A builder with the made-up key, pointed at the server at `base_url`.
fn builder(base_url: &str) -> OpenAiProviderBuilder {
    OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(format!("{base_url}/v1"))
        .timeout(Duration::from_secs(5))
}

fn provider(base_url: &str, api: OpenAiApi) -> Arc<OpenAiProvider> {
    Arc::new(builder(base_url).api(api).build().expect("the provider builds"))
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

/// The client of upstream's `_evaluate`: discrete answers, two corrective
/// retries, a retry budget of two without a wait.
fn evaluating(structured: bool) -> ClientBuilder {
    Client::builder(structured_outputs(structured), AnswerMode::Discrete)
        .n_retry_malformed_structure(2)
        .retry(retries(2))
}

/// Asks the one question about [`STATE`] of `provider` through `client`.
async fn evaluate(
    client: ClientBuilder,
    provider: Arc<OpenAiProvider>,
) -> Result<Response<Answers>, Error> {
    let client = client.provider_instance(provider).build().expect("the client builds");
    client.system_one(STATE, &positive()).send().await
}

fn parsed(json: Option<&str>) -> Value {
    serde_json::from_str(json.expect("it was recorded")).expect("JSON")
}

fn body_of(request: &RecordedRequest) -> Value {
    serde_json::from_slice(&request.body).expect("the request body is JSON")
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

/// How often `needle` occurs in `text`.
fn occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// The message of the adapter's error for a non-answer.
fn non_answer_message(error: &Error) -> String {
    match error.kind() {
        ErrorKind::NonAnswer(non_answer) => non_answer.to_string(),
        other => panic!("expected a non-answer, got {other:?}"),
    }
}

/// The API error inside the adapter's error for a failed exchange.
fn api_error(error: &Error) -> &typesafe_sdk::ApiError {
    match error.kind() {
        ErrorKind::Provider(error) => match error.kind() {
            typesafe_sdk::ErrorKind::Api(api) => api,
            other => panic!("expected an API error, got {other:?}"),
        },
        other => panic!("expected a provider failure, got {other:?}"),
    }
}

// ------------------------------------------------------------ retry budgets

/// A server that always answers 503 is asked `budget + 1` times, and every
/// attempt is in the error's trace.
async fn retry_budget(budget: u32) {
    let server =
        answering(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"unavailable"}}"#).await;
    let provider = provider(server.base_url(), OpenAiApi::Responses);
    let client =
        Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete).retry(retries(budget));

    let error = evaluate(client, provider).await.expect_err("every attempt fails");

    let api = api_error(&error);
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.kind(), ApiErrorKind::InternalServer);
    let expected = usize::try_from(budget).expect("a small number") + 1;
    assert_eq!(server.request_count(), expected);
    let trace = error.debug().expect("the error carries a trace");
    let attempts = trace.attempts();
    assert_eq!(attempts.len(), expected);
    let sent: Vec<Value> = server.requests().iter().map(body_of).collect();
    let recorded: Vec<Value> = attempts.iter().map(|attempt| parsed(attempt.request())).collect();
    assert_eq!(recorded, sent);
    for attempt in attempts {
        assert_eq!(attempt.response(), None);
        assert_eq!(attempt.error_type(), Some("Api"));
        let message = attempt.error().expect("a failed attempt has an error");
        assert!(message.contains("unavailable"), "{message}");
    }
    serde_json::to_string(trace).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_zero_sends_one_request() {
    retry_budget(0).await;
}

#[tokio::test]
// Upstream: tests/test_provider_retries.py::test_retry_policy_controls_http_attempts
async fn retry_budget_of_one_sends_two_requests() {
    retry_budget(1).await;
}

// ------------------------------------------- finish reasons (Chat Completions)

/// One reply with `finish_reason`: an answer for `stop` or none, otherwise a
/// non-answer that spends no retry and keeps the reply in the trace.
async fn chat_finish_reason(finish_reason: Option<&str>, structured: bool) {
    let reply = chat_reply(ANSWER, finish_reason);
    let server = answering(StatusCode::OK, reply.to_string()).await;
    let provider = provider(server.base_url(), OpenAiApi::ChatCompletions);
    let succeeds = matches!(finish_reason, Some("stop") | None);

    let trace: Trace = match evaluate(evaluating(structured), provider).await {
        Ok(response) if succeeds => {
            assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
            response.debug().clone()
        }
        Err(error) if !succeeds => {
            let reason = finish_reason.expect("a reason that is not an answer");
            assert_eq!(
                non_answer_message(&error),
                format!("OpenAI did not answer: finish reason {reason}")
            );
            let trace = error.debug().expect("the error carries a trace").clone();
            assert!(trace.retry_reasons().is_empty());
            trace
        }
        other => panic!("finish reason {finish_reason:?}: {other:?}"),
    };

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(trace.attempts().len(), 1);
    let attempt = &trace.attempts()[0];
    assert_eq!(parsed(attempt.request()), body_of(&requests[0]));
    let recorded = parsed(attempt.response());
    assert_eq!(recorded["choices"][0]["message"]["content"], ANSWER);
    assert_eq!(recorded["choices"][0]["finish_reason"], json!(finish_reason));
    assert_eq!(attempt.finish_reason(), finish_reason);
    assert_eq!(attempt.error().is_some(), !succeeds);
    serde_json::to_string(&trace).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_stop_prompted() {
    chat_finish_reason(Some("stop"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_stop_native() {
    chat_finish_reason(Some("stop"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_none_prompted() {
    chat_finish_reason(None, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_none_native() {
    chat_finish_reason(None, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_length_prompted() {
    chat_finish_reason(Some("length"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_length_native() {
    chat_finish_reason(Some("length"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_content_filter_prompted() {
    chat_finish_reason(Some("content_filter"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_content_filter_native() {
    chat_finish_reason(Some("content_filter"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_tool_calls_prompted() {
    chat_finish_reason(Some("tool_calls"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_tool_calls_native() {
    chat_finish_reason(Some("tool_calls"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_function_call_prompted() {
    chat_finish_reason(Some("function_call"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_function_call_native() {
    chat_finish_reason(Some("function_call"), true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_unknown_prompted() {
    chat_finish_reason(Some("unknown"), false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_chat_completion_finish_reason
async fn nonanswer_chat_finish_reason_unknown_native() {
    chat_finish_reason(Some("unknown"), true).await;
}

// ------------------------------------------------------ refusals (Responses)

/// A completed Responses reply that holds a refusal, after a valid answer or
/// alone, is a non-answer that spends no retry; the refusal's text is in the
/// trace and not in the error.
async fn responses_refusal(with_valid_text: bool, structured: bool) {
    let mut output = vec![json!({"type": "reasoning", "id": "reasoning-test", "summary": []})];
    if with_valid_text {
        output.push(json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]}));
    }
    output.push(json!({"type": "message", "role": "assistant", "content": [{"type": "refusal", "refusal": REFUSAL}]}));
    // Usage is left out, as upstream leaves it out: a refusal fails all the same.
    let reply = json!({"status": "completed", "output": output});
    let server = answering(StatusCode::OK, reply.to_string()).await;
    let provider = provider(server.base_url(), OpenAiApi::Responses);

    let error = evaluate(evaluating(structured), provider).await.expect_err("a refusal");

    assert_eq!(non_answer_message(&error), "OpenAI did not answer: refusal");
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert_eq!(occurrences(&text, REFUSAL), 0, "{text}");
    }
    let trace = error.debug().expect("the error carries a trace");
    assert!(trace.retry_reasons().is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(trace.attempts().len(), 1);
    let attempt = &trace.attempts()[0];
    assert_eq!(parsed(attempt.request()), body_of(&requests[0]));
    let recorded = parsed(attempt.response());
    let last = recorded["output"].as_array().expect("an array").last().expect("an item");
    assert_eq!(last["content"][0]["refusal"], REFUSAL);
    assert_eq!(attempt.finish_reason(), Some("completed"));
    let message = attempt.error().expect("a refused attempt has an error");
    assert!(message.contains("refusal"), "{message}");
    serde_json::to_string(trace).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_responses_refusal
async fn nonanswer_responses_refusal_alone_prompted() {
    responses_refusal(false, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_responses_refusal
async fn nonanswer_responses_refusal_alone_native() {
    responses_refusal(false, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_responses_refusal
async fn nonanswer_responses_refusal_after_valid_text_prompted() {
    responses_refusal(true, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_responses_refusal
async fn nonanswer_responses_refusal_after_valid_text_native() {
    responses_refusal(true, true).await;
}

// ------------------------------------------------------------------- usage

/// How upstream's `usage_case` shapes the reply's `usage` member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageCase {
    Omitted,
    Null,
    MissingInput,
    NullInput,
    MissingOutput,
    NullOutput,
    Zero,
    Present,
}

/// An answer whose usage is shaped by `case`: a count the vendor left out or
/// sent as null is `None`, a zero stays a zero, and nothing is retried.
async fn usage(case: UsageCase, api: OpenAiApi, structured: bool) {
    let (mut reply, input_field, output_field) = if api == OpenAiApi::Responses {
        (
            json!({
                "status": "completed",
                "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]}],
            }),
            "input_tokens",
            "output_tokens",
        )
    } else {
        (
            json!({"choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": ANSWER}}]}),
            "prompt_tokens",
            "completion_tokens",
        )
    };
    let count = if case == UsageCase::Zero { 0 } else { 1 };
    let mut counts =
        json!({input_field: 12 * count, output_field: 7 * count, "total_tokens": 19 * count});
    match case {
        UsageCase::MissingInput => {
            drop(counts.as_object_mut().expect("an object").remove(input_field))
        }
        UsageCase::NullInput => counts[input_field] = Value::Null,
        UsageCase::MissingOutput => {
            drop(counts.as_object_mut().expect("an object").remove(output_field))
        }
        UsageCase::NullOutput => counts[output_field] = Value::Null,
        UsageCase::Omitted | UsageCase::Null | UsageCase::Zero | UsageCase::Present => {}
    }
    match case {
        UsageCase::Omitted => {}
        UsageCase::Null => reply["usage"] = Value::Null,
        _ => reply["usage"] = counts.clone(),
    }
    let server = answering(StatusCode::OK, reply.to_string()).await;
    let provider = provider(server.base_url(), api);

    let response = evaluate(evaluating(structured), provider).await.expect("an answer");

    assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
    let no_usage = matches!(case, UsageCase::Omitted | UsageCase::Null);
    let expected_input =
        if no_usage || matches!(case, UsageCase::MissingInput | UsageCase::NullInput) {
            None
        } else {
            Some(12 * count)
        };
    let expected_output =
        if no_usage || matches!(case, UsageCase::MissingOutput | UsageCase::NullOutput) {
            None
        } else {
            Some(7 * count)
        };
    let usage = response.usage();
    assert_eq!(usage.input_tokens(), expected_input, "{case:?}");
    assert_eq!(usage.input_tokens_total(), expected_input, "{case:?}");
    assert_eq!(usage.output_tokens(), expected_output, "{case:?}");
    assert_eq!(usage.output_tokens_total(), expected_output, "{case:?}");
    assert_eq!(usage.n_retries(), 0);
    assert_eq!(usage.n_retries_malformed_structure(), 0);
    let dumped = serde_json::to_value(&response).expect("the response serializes");
    assert_eq!(dumped["usage"]["input_tokens"], json!(expected_input));
    assert_eq!(dumped["usage"]["output_tokens"], json!(expected_output));

    let trace = response.debug();
    assert!(trace.retry_reasons().is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(trace.attempts().len(), 1);
    let attempt = &trace.attempts()[0];
    assert_eq!(parsed(attempt.request()), body_of(&requests[0]));
    let recorded = parsed(attempt.response());
    if no_usage {
        assert!(recorded.get("usage").is_none_or(Value::is_null), "{recorded}");
    } else {
        assert_eq!(recorded["usage"], counts);
    }
    let finished = if api == OpenAiApi::Responses { "completed" } else { "stop" };
    assert_eq!(attempt.finish_reason(), Some(finished));
    assert_eq!(attempt.error(), None);
    serde_json::to_string(trace).expect("the trace serializes");
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_omitted_chat_prompted() {
    usage(UsageCase::Omitted, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_omitted_chat_native() {
    usage(UsageCase::Omitted, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_omitted_responses_prompted() {
    usage(UsageCase::Omitted, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_omitted_responses_native() {
    usage(UsageCase::Omitted, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_chat_prompted() {
    usage(UsageCase::Null, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_chat_native() {
    usage(UsageCase::Null, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_responses_prompted() {
    usage(UsageCase::Null, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_responses_native() {
    usage(UsageCase::Null, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_input_chat_prompted() {
    usage(UsageCase::MissingInput, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_input_chat_native() {
    usage(UsageCase::MissingInput, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_input_responses_prompted() {
    usage(UsageCase::MissingInput, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_input_responses_native() {
    usage(UsageCase::MissingInput, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_input_chat_prompted() {
    usage(UsageCase::NullInput, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_input_chat_native() {
    usage(UsageCase::NullInput, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_input_responses_prompted() {
    usage(UsageCase::NullInput, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_input_responses_native() {
    usage(UsageCase::NullInput, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_output_chat_prompted() {
    usage(UsageCase::MissingOutput, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_output_chat_native() {
    usage(UsageCase::MissingOutput, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_output_responses_prompted() {
    usage(UsageCase::MissingOutput, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_missing_output_responses_native() {
    usage(UsageCase::MissingOutput, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_output_chat_prompted() {
    usage(UsageCase::NullOutput, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_output_chat_native() {
    usage(UsageCase::NullOutput, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_output_responses_prompted() {
    usage(UsageCase::NullOutput, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_null_output_responses_native() {
    usage(UsageCase::NullOutput, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_zero_chat_prompted() {
    usage(UsageCase::Zero, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_zero_chat_native() {
    usage(UsageCase::Zero, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_zero_responses_prompted() {
    usage(UsageCase::Zero, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_zero_responses_native() {
    usage(UsageCase::Zero, OpenAiApi::Responses, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_present_chat_prompted() {
    usage(UsageCase::Present, OpenAiApi::ChatCompletions, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_present_chat_native() {
    usage(UsageCase::Present, OpenAiApi::ChatCompletions, true).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_present_responses_prompted() {
    usage(UsageCase::Present, OpenAiApi::Responses, false).await;
}

#[tokio::test]
// Upstream: tests/test_provider_nonanswers.py::test_openai_missing_usage
async fn usage_case_present_responses_native() {
    usage(UsageCase::Present, OpenAiApi::Responses, true).await;
}

// ------------------------------------------- corrections, and two calls at once

#[tokio::test]
// Upstream: tests/test_openai_transports.py::test_openai_transport_preserves_corrections_and_usage
async fn openai_transport_preserves_corrections_and_usage() {
    const MALFORMED: &str = r#"{"answers":"#;
    // A loopback base URL is never `api.openai.com`, so upstream's row
    // without a base URL and without an api is the row that names Responses.
    let rows = [
        (Some(OpenAiApi::Responses), "/v1/responses", "responses"),
        (None, "/v1/chat/completions", "chat_completions"),
        (Some(OpenAiApi::ChatCompletions), "/v1/chat/completions", "chat_completions"),
    ];
    for (api, endpoint, api_name) in rows {
        for structured in [false, true] {
            let responses = endpoint == "/v1/responses";
            let server = TestServer::start_nth(Protocol::Http1, move |nth, request| {
                assert_eq!(request.uri.path(), endpoint);
                let text = if nth == 1 { MALFORMED } else { ANSWER };
                let reply = if responses {
                    let mut reply = responses_reply(text);
                    reply["text"] = body_of(request)["text"].clone();
                    reply
                } else {
                    // Upstream's reply carries no finish reason here.
                    json!({
                        "choices": [{"message": {"role": "assistant", "content": text}}],
                        "usage": {"prompt_tokens": 12, "completion_tokens": 7, "total_tokens": 19},
                    })
                };
                json_response(StatusCode::OK, reply.to_string())
            })
            .await
            .expect("a loopback server");
            let builder = builder(server.base_url());
            let provider = Arc::new(
                api.map_or(builder.clone(), |api| builder.api(api))
                    .build()
                    .expect("the provider builds"),
            );
            let client = Client::builder(structured_outputs(structured), AnswerMode::Discrete)
                .n_retry_malformed_structure(1);

            let response = evaluate(client, provider).await.expect("the second reply answers");

            assert_eq!(response.answers().noul("positive").expect("a noul").noul(), 1.0);
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
            let recorded: Vec<Value> =
                attempts.iter().map(|attempt| parsed(attempt.request())).collect();
            assert_eq!(recorded, requests);
            let lengths: Vec<usize> =
                attempts.iter().map(|attempt| attempt.messages().len()).collect();
            assert_eq!(lengths, [2, 4]);
            for (index, attempt) in attempts.iter().enumerate() {
                let raw = parsed(attempt.response());
                let text = if responses {
                    assert_eq!(raw["text"]["format"], recorded[index]["text"]["format"]);
                    &raw["output"][1]["content"][0]["text"]
                } else {
                    &raw["choices"][0]["message"]["content"]
                };
                assert_eq!(text, if index == 0 { MALFORMED } else { ANSWER });
                assert_eq!(attempt.api(), Some(api_name));
            }
            serde_json::to_string(response.debug()).expect("the trace serializes");

            for body in &requests {
                if responses {
                    assert_eq!(body["store"], false);
                    assert!(body.get("previous_response_id").is_none());
                    let instructions = body
                        .get("instructions")
                        .unwrap_or(&body["input"][0]["content"])
                        .as_str()
                        .expect("text");
                    assert!(instructions.starts_with("Evaluate every question"), "{instructions}");
                    assert!(structured || body["input"].to_string().contains("JSON"));
                    let format = &body["text"]["format"];
                    assert_eq!(
                        format["type"],
                        if structured { "json_schema" } else { "json_object" }
                    );
                    if structured {
                        assert_eq!(format["strict"], true);
                        let properties =
                            &format["schema"]["$defs"]["TypeSafeAnswers"]["properties"];
                        assert!(properties.get("positive").is_some(), "{format}");
                    }
                    assert_eq!(
                        body["input"][0]["role"],
                        if structured { "user" } else { "system" }
                    );
                } else {
                    assert_eq!(body["messages"][0]["role"], "system");
                    if structured {
                        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
                    } else {
                        assert_eq!(body.get("response_format"), Some(&Value::Null));
                    }
                }
            }

            let last = requests.last().expect("two requests");
            let messages = last[if responses { "input" } else { "messages" }]
                .as_array()
                .expect("an array of messages");
            assert_eq!(
                messages[messages.len() - 2],
                json!({"role": "assistant", "content": MALFORMED})
            );
            let correction = &messages[messages.len() - 1];
            assert_eq!(correction["role"], "user");
            let content = correction["content"].as_str().expect("text");
            assert!(content.contains("previous response did not match"), "{content}");
        }
    }
}

/// Two calls at once through one provider, the first of which the vendor
/// ends with `first_status`: each trace holds its own call, and a later
/// direct call of the provider changes neither.
async fn two_calls_at_once(first_status: &'static str) {
    // Each request is answered only once both have arrived.
    let (arrived, _) = tokio::sync::watch::channel(0_usize);
    let arrived = Arc::new(arrived);
    let server = TestServer::start(Protocol::Http1, {
        let arrived = Arc::clone(&arrived);
        move |request| {
            let arrived = Arc::clone(&arrived);
            async move {
                arrived.send_modify(|count| *count += 1);
                let mut both = arrived.subscribe();
                tokio::time::timeout(Duration::from_secs(5), both.wait_for(|count| *count >= 2))
                    .await
                    .expect("the second request arrives")
                    .expect("the counter lives");
                let body = body_of(&request);
                let document = body["input"][0]["content"].as_str().expect("text");
                let status =
                    if document.contains("first document") { first_status } else { "completed" };
                let reply = json!({
                    "status": status,
                    "error": (status == "failed").then(|| json!({"code": "server_error", "message": "generation failed"})),
                    "incomplete_details": (status == "incomplete").then(|| json!({"reason": "max_output_tokens"})),
                    "output": [{"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": ANSWER}]}],
                    "usage": {"input_tokens": 12, "output_tokens": 7, "total_tokens": 19},
                });
                json_response(StatusCode::OK, reply.to_string())
            }
        }
    })
    .await
    .expect("a loopback server");
    let provider = provider(server.base_url(), OpenAiApi::Responses);
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(provider.clone())
        .build()
        .expect("the client builds");
    let questions = positive();
    let call = async |document: &'static str| -> Trace {
        match client.system_one(document, &questions).send().await {
            Ok(response) => response.debug().clone(),
            Err(error) => error.debug().expect("the error carries a trace").clone(),
        }
    };

    let (first, second) = tokio::join!(call("first document"), call("second document"));

    let requests: Vec<Value> = server.requests().iter().map(body_of).collect();
    assert_eq!(requests.len(), 2);
    for (trace, document, status) in
        [(&first, "first document", first_status), (&second, "second document", "completed")]
    {
        assert_eq!(trace.attempts().len(), 1);
        let attempt = &trace.attempts()[0];
        assert!(attempt.messages()[1].content().contains(document));
        let request = parsed(attempt.request());
        assert!(request["input"][0]["content"].as_str().expect("text").contains(document));
        assert!(requests.contains(&request));
        assert_eq!(parsed(attempt.response())["status"], status);
        assert_eq!(attempt.finish_reason(), Some(status));
        assert_eq!(attempt.error().is_some(), status != "completed");
    }

    // A later direct provider call must not change either finished trace.
    let before = serde_json::to_string(&[&first, &second]).expect("the traces serialize");
    let attempt = &second.attempts()[0];
    let mut direct = AttemptTrace::default();
    provider
        .request(ProviderCall::new(
            attempt.messages(),
            attempt.schema(),
            attempt.structured(),
            &mut direct,
        ))
        .await
        .expect("the exchange succeeds")
        .expect("the reply is an answer");
    assert!(direct.response().is_some());
    assert_eq!(serde_json::to_string(&[&first, &second]).expect("the traces serialize"), before);
    assert_eq!(server.request_count(), 3);
}

#[tokio::test]
// Upstream: tests/test_openai_transports.py::test_concurrent_attempts_are_isolated_and_preserve_failed_responses
async fn concurrent_attempts_are_isolated_when_the_first_completes() {
    two_calls_at_once("completed").await;
}

#[tokio::test]
// Upstream: tests/test_openai_transports.py::test_concurrent_attempts_are_isolated_and_preserve_failed_responses
async fn concurrent_attempts_are_isolated_when_the_first_is_incomplete() {
    two_calls_at_once("incomplete").await;
}

#[tokio::test]
// Upstream: tests/test_openai_transports.py::test_concurrent_attempts_are_isolated_and_preserve_failed_responses
async fn concurrent_attempts_are_isolated_when_the_first_failed() {
    two_calls_at_once("failed").await;
}

// --------------------------------------------- the Chat Completions request

/// The Chat Completions body the client sends through the provider, against
/// upstream's assertions on it.
async fn chat_request(structured: bool) {
    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()).await;
    let provider = provider(server.base_url(), OpenAiApi::ChatCompletions);

    let response = evaluate(evaluating(structured), provider).await.expect("an answer");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].uri.path(), "/v1/chat/completions");
    let body = body_of(&requests[0]);
    let members: Vec<&str> =
        body.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(members, ["messages", "model", "response_format"]);
    assert_eq!(body["model"], "test-model");
    let attempt = &response.debug().attempts()[0];
    assert_eq!(
        body["messages"],
        serde_json::to_value(attempt.messages()).expect("messages serialize")
    );
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
    if structured {
        assert_eq!(
            body["response_format"],
            json!({
                "type": "json_schema",
                "json_schema": {"name": "evaluation", "schema": parsed(Some(attempt.schema().as_str())), "strict": true},
            })
        );
    } else {
        assert_eq!(body.get("response_format"), Some(&Value::Null));
    }
}

#[tokio::test]
async fn chat_request_shape_prompted() {
    chat_request(false).await;
}

#[tokio::test]
async fn chat_request_shape_native() {
    chat_request(true).await;
}

// -------------------------------------------------- the key is never printed

/// Every way a call or a build of the provider can fail, by name, each
/// checked to be the failure it is named for.
async fn every_error_path() -> Vec<(&'static str, Error)> {
    let question = || Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete);
    let mut errors = Vec::new();

    let server =
        answering(StatusCode::UNAUTHORIZED, r#"{"error":{"message":"Incorrect API key."}}"#).await;
    let error = evaluate(question(), provider(server.base_url(), OpenAiApi::Responses))
        .await
        .expect_err("a failure status");
    assert_eq!(api_error(&error).kind(), ApiErrorKind::Authentication);
    errors.push(("status", error));

    let port = RefusingPort::new().await.expect("a refusing port");
    let error = evaluate(question(), provider(port.base_url(), OpenAiApi::Responses))
        .await
        .expect_err("the connect is refused");
    assert!(
        matches!(error.kind(), ErrorKind::Provider(error) if matches!(error.kind(), typesafe_sdk::ErrorKind::Connection)),
        "{error:?}"
    );
    errors.push(("connect", error));

    let silent = SilentServer::start().await.expect("a silent server");
    let slow = builder(&format!("http://{}", silent.addr()))
        .timeout(Duration::from_millis(100))
        .build()
        .expect("the provider builds");
    let error = evaluate(question(), Arc::new(slow)).await.expect_err("the server never answers");
    assert!(
        matches!(error.kind(), ErrorKind::Provider(error) if matches!(error.kind(), typesafe_sdk::ErrorKind::Timeout { .. })),
        "{error:?}"
    );
    errors.push(("timeout", error));

    let server = answering(StatusCode::OK, chat_reply(ANSWER, Some("length")).to_string()).await;
    let error = evaluate(question(), provider(server.base_url(), OpenAiApi::ChatCompletions))
        .await
        .expect_err("an unfinished reply");
    assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)), "{error:?}");
    errors.push(("non-answer", error));

    let server = answering(StatusCode::OK, responses_reply(r#"{"answers":"#).to_string()).await;
    let error = evaluate(question(), provider(server.base_url(), OpenAiApi::Responses))
        .await
        .expect_err("a reply that is not the answers");
    assert!(matches!(error.kind(), ErrorKind::MalformedStructure), "{error:?}");
    errors.push(("malformed structure", error));

    let error = OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(format!("http://[{KEY}/v1"))
        .build()
        .expect_err("a base URL that does not parse");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    errors.push(("unparsable base URL", error));

    let error = builder(server.base_url())
        .api_key(format!("{KEY}\n"))
        .build()
        .expect_err("a key with a line feed");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    errors.push(("illegal key byte", error));

    errors
}

/// Every link of `error`'s chain, the error itself first.
fn chain(error: &Error) -> Vec<&(dyn StdError + 'static)> {
    let mut links = Vec::new();
    let mut link: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(current) = link {
        links.push(current);
        link = current.source();
    }
    links
}

#[tokio::test]
async fn key_never_printed_in_an_error_display() {
    let errors = every_error_path().await;
    assert_eq!(errors.len(), 7);

    for (path, error) in &errors {
        assert_eq!(occurrences(&format!("{error}"), KEY), 0, "{path}: {error}");
        for link in chain(error) {
            let text = link.to_string();
            assert!(!text.is_empty(), "{path}");
            assert_eq!(occurrences(&text, KEY), 0, "{path}: {text}");
        }
    }
}

#[tokio::test]
async fn key_never_printed_in_an_error_debug_or_any_other_debug() {
    for (path, error) in &every_error_path().await {
        assert_eq!(occurrences(&format!("{error:?}"), KEY), 0, "{path}: {error:?}");
        for link in chain(error) {
            for text in [format!("{link:?}"), format!("{link:#?}")] {
                assert_eq!(occurrences(&text, KEY), 0, "{path}: {text}");
            }
        }
    }

    // What holds the provider, and the provider itself. The key is also a
    // path segment of the base URL here.
    let server = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let builder = OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(format!("{}/{KEY}/v1", server.base_url()))
        .api(OpenAiApi::Responses)
        .timeout(Duration::from_secs(5));
    let provider = Arc::new(builder.clone().build().expect("the provider builds"));
    let client_builder = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(provider.clone());
    let client = client_builder.clone().build().expect("the client builds");
    let questions = positive();
    let request = client.system_one(STATE, &questions).provider_instance(provider.clone());
    let mut texts = vec![
        format!("{builder:?}"),
        format!("{builder:#?}"),
        format!("{provider:?}"),
        format!("{provider:#?}"),
        format!("{client_builder:?}"),
        format!("{client_builder:#?}"),
        format!("{client:?}"),
        format!("{client:#?}"),
        format!("{request:?}"),
        format!("{request:#?}"),
    ];

    let response = request.send().await.expect("an answer");
    let attempt = &response.debug().attempts()[0];
    let mut trace = AttemptTrace::default();
    provider
        .request(ProviderCall::new(
            attempt.messages(),
            attempt.schema(),
            attempt.structured(),
            &mut trace,
        ))
        .await
        .expect("the exchange succeeds")
        .expect("an answer");
    assert!(trace.request().is_some() && trace.response().is_some());
    texts.extend([format!("{trace:?}"), format!("{trace:#?}"), format!("{response:?}")]);

    for text in &texts {
        assert_eq!(occurrences(text, KEY), 0, "{text}");
    }
    // The requests did carry the key, in the header and in the path.
    for request in server.requests() {
        assert_eq!(request.header_values("authorization"), [format!("Bearer {KEY}")]);
        assert_eq!(request.uri.path(), format!("/{KEY}/v1/responses"));
    }
}

#[tokio::test]
async fn key_never_printed_in_a_serialized_trace() {
    let mut traces = 0;
    for (path, error) in &every_error_path().await {
        let Some(trace) = error.debug() else {
            assert!(matches!(error.kind(), ErrorKind::Config), "{path}: only a build has no trace");
            continue;
        };
        assert_eq!(trace.attempts().len(), 1, "{path}");
        assert!(trace.attempts()[0].request().is_some(), "{path}");
        let json = serde_json::to_string(trace).expect("the trace serializes");
        assert_eq!(occurrences(&json, KEY), 0, "{path}: {json}");
        traces += 1;
    }
    assert_eq!(traces, 5);

    let server = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let response = evaluate(evaluating(true), provider(server.base_url(), OpenAiApi::Responses))
        .await
        .expect("an answer");
    let json = serde_json::to_string(&response).expect("the response serializes");
    assert!(json.contains("llm_attempts"), "{json}");
    assert_eq!(occurrences(&json, KEY), 0, "{json}");
}

/// The lines of `target` among `lines`, without the target. The shared
/// recorder starts every line with the target of its event or span.
#[cfg(feature = "tracing")]
fn lines_of(lines: &[String], target: &str) -> Vec<String> {
    lines.iter().filter_map(|line| line.strip_prefix(target)).map(str::to_owned).collect()
}

/// The target of the SDK's events, the retry line among them.
#[cfg(feature = "tracing")]
const SDK_TARGET: &str = "typesafe_sdk";

/// The target of the adapter's own events.
#[cfg(feature = "tracing")]
const ADAPTER_TARGET: &str = "system_one_adapter";

/// A retried failure, a non-answer, a corrected reply and an answer, a
/// refused connect and a deadline: the calls whose events are read.
#[cfg(feature = "tracing")]
async fn calls_that_log() -> String {
    let server = TestServer::start_nth(Protocol::Http1, |nth, _| match nth {
        1 => json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"busy"}}"#),
        2 => json_response(StatusCode::OK, chat_reply(ANSWER, Some("length")).to_string()),
        3 => json_response(StatusCode::OK, chat_reply(r#"{"answers":"#, Some("stop")).to_string()),
        _ => json_response(StatusCode::OK, chat_reply(ANSWER, Some("stop")).to_string()),
    })
    .await
    .expect("a loopback server");
    // The key is a path segment of the base URL as well as the header.
    let provider = Arc::new(
        OpenAiProvider::builder("test-model")
            .api_key(KEY)
            .base_url(format!("{}/{KEY}/v1", server.base_url()))
            .api(OpenAiApi::ChatCompletions)
            .timeout(Duration::from_secs(5))
            .build()
            .expect("the provider builds"),
    );
    let client = Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete)
        .retry(retries(1))
        .n_retry_malformed_structure(1);
    let unfinished = evaluate(client.clone(), provider.clone()).await.expect_err("unfinished");
    assert!(matches!(unfinished.kind(), ErrorKind::NonAnswer(_)), "{unfinished:?}");
    evaluate(client.clone(), provider).await.expect("the corrected reply answers");
    assert_eq!(server.request_count(), 4);

    let port = RefusingPort::new().await.expect("a refusing port");
    evaluate(client.clone(), self::provider(port.base_url(), OpenAiApi::Responses))
        .await
        .expect_err("the connect is refused");
    let silent = SilentServer::start().await.expect("a silent server");
    let slow = builder(&format!("http://{}", silent.addr()))
        .timeout(Duration::from_millis(100))
        .build()
        .expect("the provider builds");
    evaluate(client, Arc::new(slow)).await.expect_err("the server never answers");

    format!("http://{}/v1/chat/completions", server.addr())
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_in_the_events_of_any_target() {
    let events = recorder::Recorder::default();
    let log_uri = {
        let _installed = recorder::install(&events);
        calls_that_log().await
    };

    // The adapter's own events: one per exchange, named by the fixed path.
    let own = events.at(tracing::Level::DEBUG);
    let exchanges: Vec<&String> = own.iter().filter(|line| line.contains(" uri=")).collect();
    assert_eq!(exchanges.len(), 8, "{own:?}");
    let named = exchanges.iter().filter(|line| line.contains(&format!(" uri={log_uri} "))).count();
    assert_eq!(named, 4, "{own:?}");

    // Every target at every level, span lines included.
    let lines = events.all();
    assert!(!lines_of(&lines, ADAPTER_TARGET).is_empty(), "{lines:?}");
    assert!(lines.iter().any(|line| line.starts_with("hyper_util")), "{lines:?}");
    let retries: Vec<String> =
        lines_of(&lines, SDK_TARGET).into_iter().filter(|line| line.contains(" retry ")).collect();
    // One retry each: the 503, the refused connect and the deadline.
    assert_eq!(retries.len(), 3, "{lines:?}");
    for line in &lines {
        assert_eq!(occurrences(line, KEY), 0, "{line}");
        assert_eq!(occurrences(line, "Bearer"), 0, "{line}");
    }
}

#[cfg(feature = "tracing")]
#[tokio::test]
async fn key_never_printed_in_the_retry_line_for_a_key_in_the_base_url() {
    let server = TestServer::start_nth(Protocol::Http1, |nth, _| match nth {
        1 => json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"busy"}}"#),
        _ => json_response(StatusCode::OK, responses_reply(ANSWER).to_string()),
    })
    .await
    .expect("a loopback server");
    let addr = server.addr();
    let provider = Arc::new(
        OpenAiProvider::builder("test-model")
            .api_key(KEY)
            .base_url(format!("http://{addr}/{KEY}/v1"))
            .api(OpenAiApi::Responses)
            .timeout(Duration::from_secs(5))
            .build()
            .expect("the provider builds"),
    );
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .retry(RetryPolicy::new().max_retries(1).backoff_initial(Duration::ZERO));
    let events = recorder::Recorder::default();

    {
        let _installed = recorder::install(&events);
        evaluate(client, provider).await.expect("the second attempt answers");
    }
    let lines = events.all();

    // The requests went to the caller's prefix, the key in their path.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.uri.path(), format!("/{KEY}/v1/responses"));
    }
    let retries: Vec<String> =
        lines_of(&lines, SDK_TARGET).into_iter().filter(|line| line.contains(" retry ")).collect();
    assert_eq!(retries, [format!(" message=POST http://{addr}/v1/responses retry 1")]);
    assert!(!lines_of(&lines, ADAPTER_TARGET).is_empty(), "{lines:?}");
    for line in &lines {
        assert_eq!(occurrences(line, KEY), 0, "{line}");
    }
}

/// A call that fails with `status` and `body`, through a provider that reads
/// at most `limit` bytes of a response.
async fn failing(status: StatusCode, body: String, limit: usize) -> Error {
    let server = answering(status, body).await;
    let provider = builder(server.base_url())
        .api(OpenAiApi::Responses)
        .max_response_bytes(limit)
        .build()
        .expect("the provider builds");
    let client = Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete);
    let error = evaluate(client, Arc::new(provider)).await.expect_err("a failure status");
    assert_eq!(server.request_count(), 1);
    error
}

/// `error` is the API error `status` with the fixed text `message`, and
/// neither its renderings nor the serialized trace of the failed call hold
/// the key.
#[track_caller]
fn assert_fixed_text(error: &Error, status: StatusCode, message: &str) {
    let api = api_error(error);
    assert_eq!(api.status(), status);
    assert_eq!(api.message(), message);
    assert!(error.to_string().contains(message), "{error}");
    for link in chain(error) {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
        }
    }
    let trace = error.debug().expect("the error carries a trace");
    assert_eq!(trace.attempts().len(), 1);
    let attempt = &trace.attempts()[0];
    assert!(attempt.request().is_some());
    assert_eq!(attempt.response(), None);
    let recorded = attempt.error().expect("a failed attempt has an error");
    assert!(recorded.contains(message), "{recorded}");
    let json = serde_json::to_string(trace).expect("the trace serializes");
    assert_eq!(occurrences(&json, KEY), 0, "{json}");
}

#[tokio::test]
async fn a_failure_body_over_the_size_limit_is_reported_by_a_fixed_text() {
    let body = json!({"error": {"message": "x".repeat(4096)}}).to_string();

    let error = failing(StatusCode::BAD_GATEWAY, body, 64).await;

    assert_fixed_text(
        &error,
        StatusCode::BAD_GATEWAY,
        "The response body was larger than the limit and is not shown.",
    );
    assert_eq!(api_error(&error).kind(), ApiErrorKind::InternalServer);
}

#[tokio::test]
async fn a_failure_body_that_repeats_the_key_is_not_shown() {
    // A gateway that quotes the header it refused.
    let body =
        json!({"error": {"message": format!("Incorrect API key provided: {KEY}.")}}).to_string();

    let error = failing(StatusCode::UNAUTHORIZED, body, 1 << 20).await;

    assert_fixed_text(
        &error,
        StatusCode::UNAUTHORIZED,
        "The response's body and headers are not shown, because showing them could reveal \
         the API key.",
    );
    assert_eq!(api_error(&error).kind(), ApiErrorKind::Authentication);
    assert!(api_error(&error).headers().is_empty(), "the headers are dropped with the body");

    // The same body over the limit: a body over the limit is not kept, so
    // the error holds nothing of it, whichever fixed text it carries.
    let long = json!({"error": {"message": format!("{} {KEY}", "x".repeat(4096))}}).to_string();
    let error = failing(StatusCode::UNAUTHORIZED, long, 64).await;
    for link in chain(&error) {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
        }
    }
    let json = serde_json::to_string(error.debug().expect("a trace")).expect("it serializes");
    assert_eq!(occurrences(&json, KEY), 0, "{json}");
}

// ------------------------------------------------- user data is not printed

/// A state that holds the sentinel.
fn sentinel_state() -> Value {
    json!({"review": format!("A delightful book. {SENTINEL}"), "reader": {"note": SENTINEL}})
}

/// The calls whose output must not hold the sentinel: an answer, a failure
/// status whose body does not repeat the request, a non-answer, and a reply
/// that is not the answers.
async fn calls_with_the_sentinel() -> (Response<Answers>, Vec<Error>) {
    let state = sentinel_state();
    let questions = positive();
    let ask = async |status: StatusCode, reply: String| {
        let server = answering(status, reply).await;
        let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
            .provider_instance(provider(server.base_url(), OpenAiApi::Responses))
            .build()
            .expect("the client builds");
        let result = client.system_one(&state, &questions).send().await;
        // The state was sent, so the checks that follow are not vacuous.
        let sent = String::from_utf8(server.requests()[0].body.to_vec()).expect("UTF-8");
        assert!(sent.contains(SENTINEL), "{sent}");
        result
    };

    let response = ask(StatusCode::OK, responses_reply(ANSWER).to_string()).await.expect("answers");
    let errors = vec![
        ask(StatusCode::SERVICE_UNAVAILABLE, r#"{"error":{"message":"unavailable"}}"#.to_owned())
            .await
            .expect_err("a failure status"),
        ask(StatusCode::OK, json!({"status": "incomplete", "output": []}).to_string())
            .await
            .expect_err("a non-answer"),
        ask(StatusCode::OK, responses_reply(r#"{"answers":{}}"#).to_string())
            .await
            .expect_err("a reply that is not the answers"),
    ];
    (response, errors)
}

#[tokio::test]
async fn user_data_not_printed_by_an_error_a_response_or_an_event() {
    #[cfg(feature = "tracing")]
    let events = recorder::Recorder::default();
    let (response, errors) = {
        #[cfg(feature = "tracing")]
        let _installed = recorder::install(&events);
        calls_with_the_sentinel().await
    };

    let mut texts = vec![format!("{response:?}"), format!("{response:#?}")];
    let mut traces = vec![response.debug()];
    assert_eq!(errors.len(), 3);
    for error in &errors {
        texts.extend([format!("{error}"), format!("{error:?}"), format!("{error:#?}")]);
        for link in chain(error) {
            texts.extend([link.to_string(), format!("{link:?}")]);
        }
        traces.push(error.debug().expect("the error carries a trace"));
    }
    for trace in traces {
        texts.extend([format!("{trace:?}"), format!("{trace:#?}")]);
        assert_eq!(trace.attempts().len(), 1);
        for attempt in trace.attempts() {
            texts.extend([format!("{attempt:?}"), format!("{attempt:#?}")]);
        }
        // The serialized trace is where the state is kept, on purpose.
        let json = serde_json::to_string(trace).expect("the trace serializes");
        assert!(json.contains(SENTINEL), "{json}");
    }
    #[cfg(feature = "tracing")]
    {
        let lines = events.all();
        assert!(!lines_of(&lines, ADAPTER_TARGET).is_empty(), "{lines:?}");
        assert!(!events.at(tracing::Level::DEBUG).is_empty(), "{lines:?}");
        texts.extend(lines);
    }

    for text in &texts {
        assert_eq!(occurrences(text, SENTINEL), 0, "{text}");
    }
}

// ------------------------------------------------------- a caller's service

/// A service of the caller's own that fails every call with an error whose
/// message and whose `Debug` both hold the request's headers.
#[derive(Clone)]
struct Leaking {
    saw_the_key: Arc<AtomicBool>,
}

/// The error of [`Leaking`].
#[derive(Debug)]
struct Leak {
    headers: String,
}

impl std::fmt::Display for Leak {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the proxy refused the request with headers {}", self.headers)
    }
}

impl StdError for Leak {}

impl Service<Request<Body>> for Leaking {
    type Response = http::Response<Full<Bytes>>;
    type Error = BoxError;
    type Future = Ready<Result<Self::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        // A sensitive value prints `Sensitive`; a service that wants the
        // header's text reads its bytes, as this one does.
        let headers = request
            .headers()
            .iter()
            .map(|(name, value)| format!("{name}: {}", String::from_utf8_lossy(value.as_bytes())))
            .collect::<Vec<_>>()
            .join(", ");
        self.saw_the_key.store(headers.contains(KEY), Ordering::SeqCst);
        ready(Err(Box::new(Leak { headers })))
    }
}

#[tokio::test]
async fn foreign_service_error_holding_the_headers_shows_no_key() {
    let server = answering(StatusCode::OK, "{}").await;
    let saw_the_key = Arc::new(AtomicBool::new(false));
    let provider = builder(server.base_url())
        .api(OpenAiApi::Responses)
        .build_with_service(Leaking { saw_the_key: Arc::clone(&saw_the_key) })
        .expect("the provider builds");
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(Arc::new(provider))
        .build()
        .expect("the client builds");

    let error = client.system_one(STATE, &positive()).send().await.expect_err("the service fails");

    assert!(saw_the_key.load(Ordering::SeqCst), "the service's error did hold the key");
    assert!(
        matches!(error.kind(), ErrorKind::Provider(error) if matches!(error.kind(), typesafe_sdk::ErrorKind::Connection)),
        "{error:?}"
    );
    let links = chain(&error);
    assert!(links.len() >= 2, "the adapter's error and the SDK's");
    for link in links {
        for text in [link.to_string(), format!("{link:?}"), format!("{link:#?}")] {
            assert_eq!(occurrences(&text, KEY), 0, "{text}");
            assert_eq!(occurrences(&text, "the proxy refused"), 0, "{text}");
        }
    }
    let trace = serde_json::to_string(error.debug().expect("a trace")).expect("it serializes");
    assert_eq!(occurrences(&trace, KEY), 0, "{trace}");
    assert_eq!(server.request_count(), 0, "the request went through the service");
}

// ------------------------------------------------------ the transport rules

#[tokio::test]
async fn redirect_not_followed_and_reported_as_an_api_error() {
    let target = answering(StatusCode::OK, responses_reply(ANSWER).to_string()).await;
    let location = HeaderValue::from_str(&format!("{}/v1/responses", target.base_url()))
        .expect("a header value");
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

    let error = evaluate(evaluating(true), provider(server.base_url(), OpenAiApi::Responses))
        .await
        .expect_err("a redirect is a failure status");

    assert_eq!(api_error(&error).status(), StatusCode::FOUND);
    assert_eq!(server.request_count(), 1);
    assert_eq!(target.request_count(), 0, "the redirect was followed to a second host");
    assert_eq!(error.debug().expect("a trace").attempts().len(), 1);
}

/// The `Config` error a provider with `base_url` is refused with, after
/// checking that no rendering of it holds the URL or one of its `parts`.
#[track_caller]
fn refused(base_url: &str, parts: &[&str]) -> String {
    let error = OpenAiProvider::builder("test-model")
        .api_key(KEY)
        .base_url(base_url)
        .build()
        .expect_err("the base URL is refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert_eq!(occurrences(&text, base_url), 0, "{text}");
        for part in parts {
            assert_eq!(occurrences(&text, part), 0, "{text}");
        }
    }
    error.to_string()
}

#[tokio::test]
async fn base_url_refused_with_userinfo() {
    let server = answering(StatusCode::OK, "{}").await;
    let addr = server.addr().to_string();

    assert_eq!(
        refused(&format!("http://alice:hunter2@{addr}/v1"), &["alice", "hunter2", &addr]),
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn base_url_refused_with_a_fragment() {
    let server = answering(StatusCode::OK, "{}").await;
    let addr = server.addr().to_string();

    assert_eq!(
        refused(&format!("http://{addr}/v1#token-9f2"), &["token-9f2", &addr]),
        "The base URL must not carry a fragment ('#...')."
    );
    assert_eq!(server.request_count(), 0);
}

#[tokio::test]
async fn base_url_refused_with_a_query() {
    let server = answering(StatusCode::OK, "{}").await;
    let addr = server.addr().to_string();

    assert_eq!(
        refused(&format!("http://{addr}/v1?api_key=k-77"), &["api_key", "k-77", &addr]),
        "The base URL must not carry a query ('?...')."
    );
    assert_eq!(server.request_count(), 0);
}

// ------------------------------------------------------- the cassette replays

/// The upstream test a cassette was recorded by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recorded {
    /// `test_live_responses_match_reference_shape`: an expected response
    /// exists for each of its cassettes.
    ReferenceShape,
    /// `test_live_models_follow_question_instructions_and_criteria`.
    ContextProbe,
}

/// What one replay leaves behind.
struct Replay {
    cassette: Cassette,
    /// The one request the server received.
    request: RecordedRequest,
    response: Response<Answers>,
}

/// Replays the OpenAI cassette of `recorded` for `mode` and `outputs`: a
/// server answers with the recorded response, and the client asks the test's
/// questions of a provider pointed at that server.
async fn replay(recorded: Recorded, mode: AnswerMode, outputs: StructuredOutputs) -> Replay {
    let name = cassette_name(recorded, mode, outputs);
    let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
    let server = TestServer::start(Protocol::Http1, {
        let (status, body) = (cassette.response.status, cassette.response.body.clone());
        move |_| {
            let body = body.clone();
            async move { json_response(status, body) }
        }
    })
    .await
    .expect("a loopback server");
    let model = cassette.request.body["model"].as_str().expect("the recorded model");
    let provider = OpenAiProvider::builder(model)
        .api_key(KEY)
        .base_url(format!("{}/v1", server.base_url()))
        .api(OpenAiApi::Responses)
        .timeout(Duration::from_secs(5))
        .build()
        .expect("the provider builds");
    let client = Client::builder(outputs, mode)
        .provider_instance(Arc::new(provider))
        .build()
        .expect("the client builds");
    let (state, questions) = match recorded {
        Recorded::ReferenceShape => (expected::STATE, expected::questions()),
        Recorded::ContextProbe => {
            (expected::CONTEXT_PROBE_STATE, expected::context_probe_questions())
        }
    };

    let response = client.system_one(state, &questions).send().await.expect("the replay answers");

    let mut requests = server.requests();
    assert_eq!(requests.len(), 1, "exactly one request");
    Replay { cassette, request: requests.remove(0), response }
}

/// The cassette's name: upstream's test function and its parameter ids.
fn cassette_name(recorded: Recorded, mode: AnswerMode, outputs: StructuredOutputs) -> &'static str {
    let function = match recorded {
        Recorded::ReferenceShape => "test_live_responses_match_reference_shape",
        Recorded::ContextProbe => "test_live_models_follow_question_instructions_and_criteria",
    };
    let mode = if mode == AnswerMode::Discrete { "discrete" } else { "probabilities" };
    let outputs = if outputs == StructuredOutputs::Native { "native" } else { "prompted" };
    let name = format!("{function}[{mode}-{outputs}-openai]");
    cassette::CASSETTES
        .into_iter()
        .find(|listed| *listed == name)
        .unwrap_or_else(|| panic!("`{name}` is not a listed cassette"))
}

/// A replay whose request is the recorded one (method, path, query, and the
/// body as parsed JSON), with what the upstream test of `recorded` asserts
/// of the response.
async fn replayed(recorded: Recorded, mode: AnswerMode, outputs: StructuredOutputs) {
    let Replay { cassette, request, response } = replay(recorded, mode, outputs).await;

    if let Err(mismatch) = cassette.matches(&request.method, &request.uri, &request.body) {
        panic!("{mismatch}");
    }
    assert_eq!(request.uri.path(), "/v1/responses");
    assert_eq!(request.header_values("authorization"), [format!("Bearer {KEY}")]);

    let found = serde_json::to_value(&response).expect("the response serializes");
    match recorded {
        Recorded::ReferenceShape => {
            let name = cassette_name(recorded, mode, outputs);
            assert!(expected::EXPECTED.contains(&name), "{name}");
            if let Err(difference) = expected::compare(&found, &expected::read(name)) {
                panic!("{name}: {difference}");
            }
        }
        Recorded::ContextProbe => {
            assert_eq!(found["answers"]["instruction_probe"]["choice"], "marker_tor");
            assert_eq!(found["answers"]["criteria_probe"]["choice"], "route_7q");
            assert_eq!(response.debug().attempts().len(), 1);
            assert_eq!(response.usage().n_retries(), 0);
        }
    }
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_prompted() {
    replayed(Recorded::ReferenceShape, AnswerMode::Probabilities, StructuredOutputs::Prompted)
        .await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_probabilities_native() {
    replayed(Recorded::ReferenceShape, AnswerMode::Probabilities, StructuredOutputs::Native).await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_prompted() {
    replayed(Recorded::ReferenceShape, AnswerMode::Discrete, StructuredOutputs::Prompted).await;
}

#[tokio::test]
async fn cassette_replay_reference_shape_discrete_native() {
    replayed(Recorded::ReferenceShape, AnswerMode::Discrete, StructuredOutputs::Native).await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_prompted() {
    replayed(Recorded::ContextProbe, AnswerMode::Probabilities, StructuredOutputs::Prompted).await;
}

#[tokio::test]
async fn cassette_replay_context_probe_probabilities_native() {
    replayed(Recorded::ContextProbe, AnswerMode::Probabilities, StructuredOutputs::Native).await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_prompted() {
    replayed(Recorded::ContextProbe, AnswerMode::Discrete, StructuredOutputs::Prompted).await;
}

#[tokio::test]
async fn cassette_replay_context_probe_discrete_native() {
    replayed(Recorded::ContextProbe, AnswerMode::Discrete, StructuredOutputs::Native).await;
}

/// In prompted mode the system string and the user string the client sent
/// are the recorded ones, byte for byte.
async fn prompts_equal_the_recorded_ones(recorded: Recorded, mode: AnswerMode) {
    let Replay { cassette, request, .. } =
        replay(recorded, mode, StructuredOutputs::Prompted).await;

    let sent = body_of(&request);
    let recorded_input = cassette.request.body["input"].as_array().expect("the recorded input");
    let sent_input = sent["input"].as_array().expect("the sent input");
    assert_eq!(recorded_input.len(), 2);
    assert_eq!(sent_input.len(), 2);
    for (index, role) in ["system", "user"].into_iter().enumerate() {
        assert_eq!(recorded_input[index]["role"], role);
        assert_eq!(sent_input[index]["role"], role);
        let recorded_text = recorded_input[index]["content"].as_str().expect("text");
        let sent_text = sent_input[index]["content"].as_str().expect("text");
        assert!(!recorded_text.is_empty());
        assert_eq!(sent_text.as_bytes(), recorded_text.as_bytes(), "the {role} string");
    }
}

#[tokio::test]
async fn prompt_bytes_reference_shape_probabilities() {
    prompts_equal_the_recorded_ones(Recorded::ReferenceShape, AnswerMode::Probabilities).await;
}

#[tokio::test]
async fn prompt_bytes_reference_shape_discrete() {
    prompts_equal_the_recorded_ones(Recorded::ReferenceShape, AnswerMode::Discrete).await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_probabilities() {
    prompts_equal_the_recorded_ones(Recorded::ContextProbe, AnswerMode::Probabilities).await;
}

#[tokio::test]
async fn prompt_bytes_context_probe_discrete() {
    prompts_equal_the_recorded_ones(Recorded::ContextProbe, AnswerMode::Discrete).await;
}
