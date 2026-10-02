//! Tests for the adapter's `Error`.

use std::{mem, time::Duration};

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
            Error::provider(typesafe_sdk::Error::timeout(Duration::from_secs(600))),
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
        (Error::provider(typesafe_sdk::Error::timeout(Duration::from_secs(1))), "Timeout"),
        (Error::provider(typesafe_sdk::Error::response_too_large(16)), "ResponseTooLarge"),
        (Error::provider(typesafe_sdk::Error::connection("refused", None)), "Connection"),
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
fn source_is_the_sdk_error_or_the_recorded_cause() {
    let provider =
        Error::provider(typesafe_sdk::Error::connection("could not reach the vendor", None));
    let decode = serde_json::from_str::<u8>("\"seven\"").expect_err("a string is not a u8");
    let decode_text = decode.to_string();
    let malformed = Error::malformed_structure(VALIDATION).with_source(Box::new(decode));
    let plain = Error::invalid_request("A model is required.");

    let provider_source = provider.source().expect("a provider error has its SDK error underneath");
    let malformed_source = malformed.source().expect("the decode failure was recorded");

    assert_eq!(provider_source.to_string(), "could not reach the vendor");
    assert!(provider_source.downcast_ref::<typesafe_sdk::Error>().is_some());
    assert_eq!(malformed_source.to_string(), decode_text);
    assert!(plain.source().is_none());
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
