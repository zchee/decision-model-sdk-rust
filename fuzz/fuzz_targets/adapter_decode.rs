//! Arbitrary text as a question set and a model's reply to it, through the
//! adapter's reply decoder in both answer modes.
//!
//! The property is that the decoder returns - `Ok` with a count of answers
//! or `Err` with a message - and never panics, aborts or hangs. libFuzzer
//! reports a panic or an abort as a crash, a stack overflow as a crash, and
//! an input that runs past `-timeout` as a hang, so the target asserts
//! nothing beyond running the decoder and reading what it returns.
//!
//! The input is split at its first line feed:
//!
//! - the text before it is the question set, the JSON object
//!   `PreparedQuestions::as_json` writes (one line, since that writer emits
//!   no line feed);
//! - the text after it is the reply, line feeds included: a model's reply
//!   may hold them, inside a string or around a Markdown fence;
//! - an input without a line feed is a question set with an empty reply.
//!
//! The same pair is decoded as probabilities and as discrete answers. The
//! decoder takes text, so an input that is not UTF-8 is skipped: a provider
//! refuses such a response body before the decoder is reached.
//!
//! An input longer than [`MAX_INPUT`] is skipped as well. The decoder finds
//! a member of the reply by a linear scan of the question names, so its cost
//! is the product of the two counts; the limit keeps that product small
//! enough that an input measures the decoder rather than that scan.

#![no_main]

use std::hint::black_box;

use decision_model_adapter::{__internals::decode::decode, AnswerMode};
use libfuzzer_sys::fuzz_target;

/// The longest input decoded, in bytes: four times libFuzzer's own default
/// of 4,096, which applies while no seed is longer than that.
const MAX_INPUT: usize = 16 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT {
        return;
    }
    let Ok(text) = str::from_utf8(data) else { return };
    let (questions, reply) = text.split_once('\n').unwrap_or((text, ""));

    for mode in [AnswerMode::Probabilities, AnswerMode::Discrete] {
        match decode(questions, mode, reply) {
            Ok(answers) => {
                black_box(answers);
            }
            Err(message) => {
                black_box(message);
            }
        }
    }
});
