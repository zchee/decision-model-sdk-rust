// A struct cannot be named like an item the generated code declares beside
// the struct's own name: inside the expansion the name would mean that item.
// The one refusal is at the struct's name; rustc's own errors about the clash
// never appear.
use typesafe_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
struct __QuestionSetField {
    #[noul]
    spam: NoulAnswer,
}

#[derive(QuestionSet)]
struct __D {
    #[noul]
    spam: NoulAnswer,
}

fn main() {}
