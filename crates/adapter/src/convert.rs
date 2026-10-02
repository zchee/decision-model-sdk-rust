//! The conversion of decoded reply values into the SDK's answer types.
//!
//! Ported from `_client.py` of system-one-adapter-python.

use serde::{
    Deserialize,
    de::value::{Error as ValueError, StrDeserializer},
};
use typesafe_sdk::{Answer, Answers, ChoiceAnswer, Content as SdkContent, NoulAnswer, ScoreAnswer};

use crate::{
    error::Error,
    metrics::{self, Normalization},
    model::{Content, DecodedAnswer, DecodedAnswers, Question, QuestionKind, QuestionModel},
    response::ProbabilityDebug,
};

/// The answers of one reply and what probability normalization found in it.
#[derive(Debug, Clone)]
pub(crate) struct Converted {
    /// One answer per question, in question order.
    pub(crate) answers: Answers,
    /// The probability part of the response's trace.
    pub(crate) probabilities: ProbabilityDebug,
}

/// Turns the decoded values of one reply into the SDK's answers (upstream's
/// `_convert_llm_value_to_typesafe_answer`, applied to every question, and
/// its `probability_debug_data`).
///
/// `normalize_probabilities` rescales a choice's or a score's probabilities
/// that miss summing to 1 by more than the tolerance; a noul's probability and
/// a discrete answer are never changed.
///
/// # Errors
///
/// Returns a malformed-structure error when `decoded` does not hold exactly
/// one value of the right shape per question. The decoder never produces such
/// values; the check keeps a mismatch from becoming a wrong answer or a panic.
pub(crate) fn convert(
    model: &QuestionModel,
    decoded: DecodedAnswers,
    normalize_probabilities: bool,
) -> Result<Converted, Error> {
    if decoded.answers.len() != model.questions.len() {
        return Err(mismatch());
    }
    let converted = model
        .questions
        .iter()
        .zip(decoded.answers)
        .map(|(question, value)| convert_answer(question, value, normalize_probabilities))
        .collect::<Result<Vec<_>, _>>()?;
    let probabilities = metrics::probability_debug(
        model
            .questions
            .iter()
            .zip(&converted)
            .map(|(question, (_, normalization))| (question, normalization.as_ref())),
    );
    let answers = model
        .questions
        .iter()
        .zip(converted)
        .map(|(question, (answer, _))| (question.id.as_str(), answer))
        .collect();
    Ok(Converted { answers, probabilities })
}

/// One question's answer, and its distribution when it has one.
fn convert_answer(
    question: &Question,
    value: DecodedAnswer,
    normalize_probabilities: bool,
) -> Result<(Answer, Option<Normalization>), Error> {
    match (&question.kind, value) {
        (QuestionKind::Noul { .. }, DecodedAnswer::Probability(probability)) => {
            Ok((NoulAnswer::new(probability).into(), None))
        }
        (QuestionKind::Noul { .. }, DecodedAnswer::Bool(yes)) => {
            Ok((NoulAnswer::new(if yes { 1.0 } else { 0.0 }).into(), None))
        }
        (QuestionKind::Score { criteria }, value) => {
            let normalization = match value {
                DecodedAnswer::Level(level) if level < criteria.len() => {
                    metrics::one_hot(criteria.len(), level)
                }
                DecodedAnswer::Distribution(probabilities)
                    if probabilities.len() == criteria.len() =>
                {
                    metrics::normalize(probabilities, normalize_probabilities)
                }
                _ => return Err(mismatch()),
            };
            let probabilities = &normalization.probabilities;
            let legend = (0_u32..)
                .zip(criteria)
                .map(|(level, description)| Ok((level, sdk_content(description)?)))
                .collect::<Result<Vec<_>, Error>>()?;
            let answer = ScoreAnswer::new(
                metrics::expected_score(probabilities),
                metrics::score_confidence(probabilities),
                legend,
                (0_u32..).zip(probabilities.iter().copied()),
            );
            Ok((answer.into(), Some(normalization)))
        }
        (QuestionKind::Choice { criteria }, value) => {
            let normalization = match value {
                DecodedAnswer::Label(label) if label < criteria.len() => {
                    metrics::one_hot(criteria.len(), label)
                }
                DecodedAnswer::Distribution(probabilities)
                    if probabilities.len() == criteria.len() =>
                {
                    metrics::normalize(probabilities, normalize_probabilities)
                }
                _ => return Err(mismatch()),
            };
            let probabilities = &normalization.probabilities;
            // The first label in criteria order among the most likely ones,
            // as upstream's `max(answers, key=...)` chooses.
            let (chosen, _) =
                metrics::first_max(probabilities.iter().copied()).ok_or_else(mismatch)?;
            let answer = ChoiceAnswer::new(
                criteria[chosen].0.as_str(),
                metrics::choice_confidence(probabilities),
                criteria.iter().map(|(label, _)| label.as_str()).zip(probabilities.iter().copied()),
            );
            Ok((answer.into(), Some(normalization)))
        }
        (QuestionKind::Noul { .. }, _) => Err(mismatch()),
    }
}

/// A criterion as the SDK's answer holds it in a score's legend: the text, or
/// the JSON object or array, byte for byte as the question model keeps it.
///
/// The JSON text is handed to the SDK as a string through serde's own string
/// deserializer, which the SDK's `Content` checks to be one JSON value and
/// then keeps unparsed. That holds under either JSON backend of the SDK;
/// `Content::json` of a `serde_json` raw value does not, because the SDK's
/// `sonic` backend writes such a value as an object wrapping the text.
fn sdk_content(content: &Content) -> Result<SdkContent<'static>, Error> {
    match content {
        Content::Text(text) => Ok(SdkContent::text(text.clone())),
        Content::Json(json) => {
            SdkContent::deserialize(StrDeserializer::<ValueError>::new(json.get())).map_err(|_| {
                Error::invalid_request("A score criterion is not JSON the SDK accepts.")
            })
        }
    }
}

fn mismatch() -> Error {
    Error::malformed_structure("The decoded answers do not match the questions.")
}

#[cfg(test)]
#[path = "convert_tests.rs"]
mod tests;
