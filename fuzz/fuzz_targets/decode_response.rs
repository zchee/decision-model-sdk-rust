//! Arbitrary bytes as a response body, through every decoder a body can
//! reach.
//!
//! The property is that each of them returns - `Ok` or `Err` - and never
//! panics, aborts or hangs. libFuzzer reports a panic or an abort as a crash,
//! a stack overflow as a crash, and an input that runs past `-timeout` as a
//! hang, so the target itself asserts nothing beyond running the decoders and
//! rendering what they return.
//!
//! The depth guard runs first on its own, because every decoder below runs it
//! again before the codec sees a byte: a nesting the guard miscounted would
//! reach the codec, which has no recursion limit of its own.

#![no_main]

use std::hint::black_box;

use bytes::Bytes;
use decision_model_sdk::{
    __internals, Answer, Answers, ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer,
};
use http::{HeaderMap, StatusCode};
use libfuzzer_sys::fuzz_target;

/// The typed answer set the upstream `RESULT` fixture answers, so a body
/// close to a real one reaches the derived decoder's field-by-field path.
#[derive(QuestionSet)]
struct Ticket {
    #[noul(instructions = "Is this spam?")]
    spam: NoulAnswer,
    #[choice(instructions = "What is the tone?", options("friendly", "hostile"))]
    tone: ChoiceAnswer,
    #[score(instructions = "How good is it?", levels("bad", "ok", "great"))]
    quality: ScoreAnswer,
}

/// Reads every part of an answer a caller could read.
fn touch(answer: &Answer) {
    if let Some(noul) = answer.as_noul() {
        black_box(noul.noul());
    }
    if let Some(choice) = answer.as_choice() {
        black_box((choice.choice(), choice.confidence()));
        for (name, probability) in choice.probabilities() {
            black_box((name, probability));
        }
    }
    if let Some(score) = answer.as_score() {
        black_box((score.score(), score.confidence()));
        for (level, description) in score.legend() {
            black_box((level, description.as_text()));
        }
        for (level, probability) in score.probabilities() {
            black_box((level, probability));
        }
    }
}

/// Renders an error the ways a caller would print it.
fn render(error: &decision_model_sdk::Error) {
    black_box(error.to_string());
    black_box(format!("{error:?}"));
}

fuzz_target!(|data: &[u8]| {
    black_box(__internals::check_depth(data).is_ok());

    let body = Bytes::copy_from_slice(data);

    match __internals::decode_system_one::<Answers>(
        body.clone(),
        StatusCode::OK,
        HeaderMap::new(),
        3,
    ) {
        Ok(response) => {
            black_box(response.model());
            for (name, answer) in response.answers().iter() {
                black_box(name);
                touch(answer);
            }
        }
        Err(error) => render(&error),
    }

    match __internals::decode_system_one::<Ticket>(
        body.clone(),
        StatusCode::OK,
        HeaderMap::new(),
        3,
    ) {
        Ok(response) => {
            let answers = response.answers();
            black_box((answers.spam.noul(), answers.tone.choice(), answers.quality.score()));
        }
        Err(error) => render(&error),
    }

    match __internals::decode_list_models(body.clone(), StatusCode::OK, HeaderMap::new()) {
        Ok(list) => {
            for model in list.models() {
                black_box((model.name(), model.description(), model.release_date()));
            }
        }
        Err(error) => render(&error),
    }

    for status in [StatusCode::BAD_REQUEST, StatusCode::UNPROCESSABLE_ENTITY] {
        let error = __internals::api_error(status, body.clone(), HeaderMap::new());
        black_box((error.message(), error.error_type(), error.kind()));
        render(&error.into());
    }
});
