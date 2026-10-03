//! Tests for the adapter's `Error`.

use std::{mem, time::Duration};

use bytes::Bytes;
use decision_model_sdk::ApiError;
use http::{HeaderMap, StatusCode};

use super::*;
use crate::response::{RetryCategory, RetryReason};

/// A validation sentence of the kind the correction prompt carries; it must
/// stay out of `Debug`.
const VALIDATION: &str = "answers.genre: expected one of 'fiction', 'nonfiction'";

fn error_trace() -> Trace {
    Trace {
        attempts: Vec::new(),
        retry_reasons: vec![RetryReason::new(RetryCategory::MalformedStructure, VALIDATION)],
        probabilities: None,
    }
}

#[test]
fn an_error_is_one_pointer_and_crosses_threads() {
    fn assert_bounds<T: Send + Sync + 'static>() {}

    assert_bounds::<Error>();
    assert_eq!(mem::size_of::<Error>(), mem::size_of::<usize>());
    assert_eq!(mem::size_of::<Result<(), Error>>(), mem::size_of::<usize>());
}

#[test]
fn display_is_the_sentence_of_each_kind() {
    let cases = [
        (
            "provider",
            Error::provider(decision_model_sdk::Error::timeout(Duration::from_secs(600))),
            "Request timed out (timeout=600s).",
        ),
        (
            "non-answer",
            Error::non_answer(NonAnswer::new("OpenAI stopped the reply: max_output_tokens.")),
            "OpenAI stopped the reply: max_output_tokens.",
        ),
        ("malformed", Error::malformed_structure(VALIDATION), VALIDATION),
        ("invalid request", Error::invalid_request("A model is required."), "A model is required."),
        ("config", Error::config("OPENAI_API_KEY is not set."), "OPENAI_API_KEY is not set."),
    ];

    for (name, error, want) in cases {
        assert_eq!(error.to_string(), want, "{name}");
    }
}

#[test]
fn the_kind_and_its_name_match_the_constructor() {
    let cases = [
        (Error::provider(decision_model_sdk::Error::timeout(Duration::from_secs(1))), "Timeout"),
        (Error::provider(decision_model_sdk::Error::response_too_large(16)), "ResponseTooLarge"),
        (Error::provider(decision_model_sdk::Error::connection("refused", None)), "Connection"),
        (Error::non_answer(NonAnswer::new("refusal")), "NonAnswer"),
        (Error::malformed_structure("bad"), "MalformedStructure"),
        (Error::invalid_request("bad"), "InvalidRequest"),
        (Error::config("bad"), "Config"),
    ];

    for (error, want) in cases {
        let matched = match error.kind() {
            ErrorKind::Provider(_) => matches!(want, "Timeout" | "ResponseTooLarge" | "Connection"),
            ErrorKind::NonAnswer(_) => want == "NonAnswer",
            ErrorKind::MalformedStructure => want == "MalformedStructure",
            ErrorKind::InvalidRequest => want == "InvalidRequest",
            ErrorKind::Config => want == "Config",
        };

        assert!(matched, "{error:?} is not {want}");
        assert_eq!(error.kind().name(), want);
    }
}

#[test]
fn source_is_the_sdk_errors_cause_or_the_recorded_cause() {
    let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
    let provider = Error::provider(decision_model_sdk::Error::connection(
        "could not reach the vendor",
        Some(Box::new(refused)),
    ));
    let without_cause =
        Error::provider(decision_model_sdk::Error::connection("could not reach the vendor", None));
    let decode = serde_json::from_str::<u8>("\"seven\"").expect_err("a string is not a u8");
    let decode_text = decode.to_string();
    let malformed = Error::malformed_structure(VALIDATION).with_source(Box::new(decode));
    let plain = Error::invalid_request("A model is required.");

    let provider_source = provider.source().expect("the SDK error's cause is underneath");
    let malformed_source = malformed.source().expect("the decode failure was recorded");

    assert_eq!(provider_source.to_string(), "refused");
    assert!(provider_source.downcast_ref::<std::io::Error>().is_some());
    assert!(without_cause.source().is_none());
    assert!(matches!(provider.kind(), ErrorKind::Provider(_)), "the SDK error is the kind's");
    assert_eq!(malformed_source.to_string(), decode_text);
    assert!(plain.source().is_none());
}

/// A reporter that prints each link of the `source()` chain, as `anyhow`'s
/// `{:#}` does, prints the SDK's message once.
#[test]
fn a_chain_printer_prints_the_sdk_message_once() {
    fn chain(error: &(dyn StdError + 'static)) -> String {
        std::iter::successors(Some(error), |&link| link.source())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ")
    }
    let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
    let api = ApiError::from_response(
        StatusCode::TOO_MANY_REQUESTS,
        Bytes::from_static(br#"{"error":{"message":"Rate limit reached"}}"#),
        HeaderMap::new(),
    );
    let sdk_api = decision_model_sdk::Error::from(api);
    let api_text = sdk_api.to_string();

    let connection = Error::provider(decision_model_sdk::Error::connection(
        "could not reach the vendor",
        Some(Box::new(refused)),
    ));
    assert_eq!(chain(&connection), "could not reach the vendor: refused");
    assert_eq!(chain(&Error::provider(sdk_api)), api_text);
}

#[test]
fn the_trace_is_absent_until_attached() {
    let before = Error::non_answer(NonAnswer::new("refusal"));

    let after = Error::non_answer(NonAnswer::new("refusal")).with_trace(error_trace());

    assert!(before.debug().is_none(), "an error before the first attempt has no trace");
    let trace = after.debug().expect("the attached trace");
    assert_eq!(trace.retry_reasons().len(), 1);
    assert_eq!(trace.max_error(), 0.0, "an error's trace has no probability part");
    assert_eq!(trace.invalid_probs(), 0);
    assert_eq!(trace.probability_errors().len(), 0);
    assert_eq!(trace.original_probabilities().len(), 0);
}

#[test]
fn debug_prints_the_kind_and_counts_never_the_validation_text() {
    let error = Error::malformed_structure(VALIDATION).with_trace(error_trace());

    let rendered = format!("{error:?}");

    assert!(!rendered.contains("fiction"), "Debug printed the validation text: {rendered}");
    assert!(rendered.contains("MalformedStructure"), "{rendered}");
    assert!(rendered.contains("retry_reasons: [MalformedStructure]"), "{rendered}");
}

#[test]
fn an_error_trace_serializes_upstream_two_members() {
    let error = Error::malformed_structure(VALIDATION).with_trace(error_trace());

    let written =
        serde_json::to_string(error.debug().expect("a trace")).expect("a trace serializes");

    assert_eq!(
        written,
        format!(
            r#"{{"llm_attempts":[],"retry_reasons":[["malformed_structure","{VALIDATION}"]]}}"#
        )
    );
}

#[test]
fn a_vendor_failure_renders_by_the_sdk_rules_message_present_body_absent() {
    // The exception to "counts and kinds only": the vendor's message is
    // printed, escaped and cut by the SDK; the rest of the body is not.
    let body = Bytes::from_static(
        br#"{"error":{"message":"Invalid schema for response_format","type":"invalid_request_error","param":"the-callers-private-parameter"}}"#,
    );
    let error = Error::provider(
        ApiError::from_response(StatusCode::BAD_REQUEST, body.clone(), HeaderMap::new()).into(),
    );

    let debug = format!("{error:?}");
    let display = error.to_string();

    assert_eq!(display, "400 Invalid schema for response_format");
    assert!(debug.contains("status: 400"), "{debug}");
    assert!(debug.contains("message: \"Invalid schema for response_format\""), "{debug}");
    assert!(debug.contains(&format!("body: <{} bytes>", body.len())), "{debug}");
    assert!(debug.contains("debug: None"), "{debug}");
    for rendered in [&debug, &display] {
        assert!(
            !rendered.contains("the-callers-private-parameter"),
            "a body member outside the message was printed: {rendered}"
        );
    }
    assert_eq!(error.kind().name(), "Api");
}

#[test]
fn a_long_vendor_message_is_cut_at_200_characters() {
    let message = "x".repeat(5_000);
    let body = Bytes::from(format!(r#"{{"error":{{"message":"{message}"}}}}"#));
    let error = Error::provider(
        ApiError::from_response(StatusCode::BAD_REQUEST, body, HeaderMap::new()).into(),
    );

    let display = error.to_string();

    assert!(
        !display.contains(&"x".repeat(201)),
        "the vendor message was not cut: {} characters",
        display.chars().count()
    );
    assert!(display.contains(&"x".repeat(200)), "{display}");
}

#[test]
fn the_kind_name_of_a_provider_failure_needs_only_a_reference() {
    let cases = [
        (decision_model_sdk::Error::timeout(Duration::from_secs(1)), "Timeout"),
        (decision_model_sdk::Error::response_too_large(16), "ResponseTooLarge"),
        (decision_model_sdk::Error::connection("refused", None), "Connection"),
        (
            ApiError::from_response(StatusCode::TOO_MANY_REQUESTS, Bytes::new(), HeaderMap::new())
                .into(),
            "Api",
        ),
    ];

    for (error, want) in cases {
        assert_eq!(provider_kind_name(&error), want);
        // The error is still owned here, as the retry policy needs it.
        assert_eq!(Error::provider(error).kind().name(), want);
    }
}
