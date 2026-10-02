//! The schema writer's entry point, for the schema parity test and the
//! schema fuzz target.

use crate::{AnswerMode, Error, Schema, model::QuestionModel};

/// The schema the adapter sends for the question set `questions` in `mode`.
///
/// `questions` is the JSON object the SDK prepares a question set as
/// (`PreparedQuestions::as_json`): one member per question, in question
/// order. It goes through the same validation and the same writer as the
/// questions of a real call, so the result is, byte for byte, the schema a
/// provider receives and a prompted request carries in its system message.
///
/// # Errors
///
/// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
/// error when `questions` is not a JSON object of valid questions: no
/// question, a question that is not a noul, a choice or a score of the
/// expected shape, or a choice or a score with fewer than two criteria.
pub fn schema(questions: &str, mode: AnswerMode) -> Result<Schema, Error> {
    QuestionModel::from_json(questions).map(|questions| crate::schema::write(&questions, mode))
}
