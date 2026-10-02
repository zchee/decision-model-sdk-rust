//! Unit tests of `run.rs`: the parts of an evaluation that have no provider
//! in them. The loop itself is tested through the client, in
//! `tests/client.rs`.

use std::{error::Error as _, time::Instant};

use typesafe_sdk::RetryPolicy;

use super::{Evaluation, InvalidReply, NON_ANSWER, add, evaluate};
use crate::{
    error::{Error, ErrorKind},
    model::QuestionModel,
    options::{AnswerMode, StructuredOutputs},
    prompt::{self, Location, Problem},
    provider::{BoxFuture, NonAnswer, Provider, ProviderCall, ProviderResult},
};

#[test]
fn a_total_is_unknown_once_any_count_is() {
    let cases = [
        (Some(10), Some(4), Some(14)),
        (Some(0), Some(0), Some(0)),
        (Some(10), None, None),
        (None, Some(4), None),
        (None, None, None),
        (Some(u64::MAX), Some(1), None),
    ];

    for (total, count, expected) in cases {
        assert_eq!(add(total, count), expected, "add({total:?}, {count:?})");
    }
}

#[test]
fn the_error_type_of_a_refused_reply_is_the_kind_name() {
    let error = Error::non_answer(NonAnswer::new("refused"));

    assert_eq!(error.kind().name(), NON_ANSWER);
    assert!(matches!(error.kind(), ErrorKind::NonAnswer(_)));
}

#[test]
fn the_decode_failure_displays_the_validation_message_and_debugs_a_count() {
    let problems = vec![
        Problem::NotJson,
        Problem::Missing { at: Location::Answers, member: "secret question".to_owned() },
    ];
    let expected = prompt::validation_message(&problems);

    let failure = InvalidReply(problems);

    assert_eq!(failure.to_string(), expected);
    assert_eq!(format!("{failure:?}"), "InvalidReply { problems: 2 }");
    assert!(failure.source().is_none());
}

/// A provider that is never asked: the test below only builds the future.
#[derive(Debug)]
struct Silent;

impl Provider for Silent {
    fn model_name(&self) -> &str {
        "silent"
    }

    fn request<'a>(
        &'a self,
        _: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async { Err(typesafe_sdk::Error::connection("never asked", None)) })
    }
}

/// The evaluation's future is `Send`, so a caller can spawn a call: the
/// attempt list is behind a `Mutex`, never a `RefCell`.
#[test]
fn the_evaluation_future_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let questions = QuestionModel::from_json(r#"{"answer":{"type":"noul"}}"#)
        .expect("one noul is a valid question set");
    let retry = RetryPolicy::none();

    let future = evaluate(Evaluation {
        provider: &Silent,
        questions: &questions,
        structured_outputs: StructuredOutputs::Native,
        answer_mode: AnswerMode::Probabilities,
        normalize_probabilities: false,
        n_retry_malformed_structure: 0,
        retry: &retry,
        user_message: String::new(),
        started: Instant::now(),
    });

    assert_send(&future);
}
