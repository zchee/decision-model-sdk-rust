//! A question set written by hand, without the derive: the typed tests of
//! `tests/client.rs` run in every feature set, and the derive exists only
//! under `macros`.

use std::sync::LazyLock;

use serde::{Deserializer, de::Error as _};
use system_one_adapter::{
    AnswerSet, Answers, Choice, ChoiceAnswer, Noul, NoulAnswer, PreparedQuestions, QuestionSet,
    Questions, Score, ScoreAnswer, typesafe_sdk::AnswerContext,
};

/// The answers to [`review_questions`], one field per question.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Review {
    pub(crate) positive: NoulAnswer,
    pub(crate) stars: ScoreAnswer,
    pub(crate) genre: ChoiceAnswer,
}

/// `QUESTIONS` of upstream's `tests/test_client_with_fake_model.py`.
pub(crate) fn review_questions() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .score("stars", Score::new(["Bad.", "Good."]).instructions("Rating."))
        .choice(
            "genre",
            Choice::new(["fiction", "nonfiction"])
                .option("fiction", "A story.")
                .option("nonfiction", "Facts.")
                .instructions("Genre."),
        )
        .prepare()
        .expect("the review questions are valid")
}

impl AnswerSet for Review {
    /// Reads the answers by name first and then moves each into its field;
    /// an answer that is missing or of another kind is a missing field.
    fn deserialize_answers<'de, D>(
        deserializer: D,
        context: AnswerContext,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let answers = Answers::deserialize_answers(deserializer, context)?;
        Ok(Self {
            positive: *answers
                .noul("positive")
                .ok_or_else(|| D::Error::missing_field("positive"))?,
            stars: answers
                .score("stars")
                .cloned()
                .ok_or_else(|| D::Error::missing_field("stars"))?,
            genre: answers
                .choice("genre")
                .cloned()
                .ok_or_else(|| D::Error::missing_field("genre"))?,
        })
    }
}

impl QuestionSet for Review {
    fn prepared() -> &'static PreparedQuestions {
        static PREPARED: LazyLock<PreparedQuestions> = LazyLock::new(review_questions);
        &PREPARED
    }
}
