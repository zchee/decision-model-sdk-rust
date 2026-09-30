// The questions are serialized once for the whole program, so they cannot
// depend on a type, lifetime or const parameter.
use typesafe_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
struct Typed<T> {
    #[noul]
    spam: NoulAnswer,
    #[noul]
    other: T,
}

#[derive(QuestionSet)]
struct Borrowed<'a> {
    #[noul]
    spam: &'a NoulAnswer,
}

#[derive(QuestionSet)]
struct Counted<const N: usize> {
    #[noul]
    spam: NoulAnswer,
}

#[derive(QuestionSet)]
struct Bounded
where
    u8: Copy,
{
    #[noul]
    spam: NoulAnswer,
}

fn main() {}
