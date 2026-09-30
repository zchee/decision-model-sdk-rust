// `#[question_set(crate = ...)]` is the path every generated path starts at:
// a path that does not resolve is reported where it is written.
use typesafe_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
#[question_set(crate = not_the_sdk)]
struct WrongPath {
    #[noul]
    spam: NoulAnswer,
}

fn main() {}
