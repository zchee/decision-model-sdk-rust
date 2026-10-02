//! The decoder's entry point, for the decode fuzz target.

use crate::{AnswerMode, model::QuestionModel, prompt};

/// Decodes `reply`, the JSON text of a model's reply, as the answers to
/// `questions`, the question object in the form of
/// `PreparedQuestions::as_json`, asked in `mode`.
///
/// Returns how many answers the reply held, one per question.
///
/// # Errors
///
/// Returns the adapter's sentence when `questions` is not a question set the
/// adapter takes, and the validation message, the text a correction prompt
/// carries, when `reply` does not answer them. Neither holds text of `reply`.
pub fn decode(questions: &str, mode: AnswerMode, reply: &str) -> Result<usize, String> {
    let questions = QuestionModel::from_json(questions).map_err(|error| error.to_string())?;
    crate::decode::decode(&questions, mode, reply)
        .map(|decoded| decoded.answers.len())
        .map_err(|problems| prompt::validation_message(&problems))
}
