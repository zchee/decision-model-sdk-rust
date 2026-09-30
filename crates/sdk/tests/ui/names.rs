// Answers are matched by name, so no two fields answer to one; and
// `#[question(...)]` takes `name = "..."` only.
use typesafe_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
struct Duplicate {
    #[noul]
    spam: NoulAnswer,
    #[question(name = "spam")]
    #[noul]
    other: NoulAnswer,
}

#[derive(QuestionSet)]
struct BadName {
    #[question(name)]
    #[noul]
    no_value: NoulAnswer,
    #[question(rename = "x")]
    #[noul]
    unknown_key: NoulAnswer,
}

fn main() {}
