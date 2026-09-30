// The type is recognized by its name, but it must be the SDK's: a type of
// the caller's own with the same name is refused by the compiler.
use typesafe_sdk::QuestionSet;

struct NoulAnswer;

#[derive(QuestionSet)]
struct Impostor {
    #[noul]
    spam: NoulAnswer,
}

fn main() {}
