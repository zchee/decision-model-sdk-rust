// Named fields, at least one: a field's name is its question's name.
use decision_model_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
struct Pair(NoulAnswer, NoulAnswer);

#[derive(QuestionSet)]
struct Nothing;

#[derive(QuestionSet)]
struct Empty {}

fn main() {}
