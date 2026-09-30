// What `Questions::prepare` rejects at run time is rejected here at compile
// time, with the runtime's message: a score without levels (and a set
// without questions, in `struct_shape.rs`).
use typesafe_sdk::{QuestionSet, ScoreAnswer};

#[derive(QuestionSet)]
struct Urgency {
    #[score(instructions = "How urgent?", levels())]
    urgency: ScoreAnswer,
    #[question(name = "renamed \"score\"")]
    #[score(levels())]
    renamed: ScoreAnswer,
}

fn main() {}
