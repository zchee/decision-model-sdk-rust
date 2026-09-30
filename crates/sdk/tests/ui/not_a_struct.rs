// A question set is a struct: an enum or a union has no fields to answer
// into one by one.
use typesafe_sdk::QuestionSet;

#[derive(QuestionSet)]
enum Mood {
    Calm,
    Angry,
}

#[derive(QuestionSet)]
union Bits {
    a: u8,
}

fn main() {}
