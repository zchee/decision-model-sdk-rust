// A field's type is the answer type of its question, recognized by name;
// an optional answer is refused with the reason.
use typesafe_sdk::{ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer};

type Spam = NoulAnswer;

#[derive(QuestionSet)]
struct Types {
    #[noul]
    kind_mismatch: ChoiceAnswer,
    #[score(levels("low", "high"))]
    another_mismatch: typesafe_sdk::NoulAnswer,
    #[noul]
    number: f64,
    #[noul]
    alias: Spam,
    #[noul]
    optional: Option<NoulAnswer>,
    #[choice(options("calm"))]
    optional_other_kind: core::option::Option<ScoreAnswer>,
}

fn main() {}
