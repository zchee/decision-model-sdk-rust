// Every field asks exactly one question and has at most one name.
use typesafe_sdk::{ChoiceAnswer, NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
struct Unasked {
    #[noul]
    spam: NoulAnswer,
    plain: NoulAnswer,
    #[question(name = "renamed")]
    renamed_only: NoulAnswer,
}

#[derive(QuestionSet)]
struct Twice {
    #[noul]
    #[choice(options("calm"))]
    both: ChoiceAnswer,
    #[question(name = "a")]
    #[question(name = "b")]
    #[noul]
    two_names: NoulAnswer,
}

fn main() {}
