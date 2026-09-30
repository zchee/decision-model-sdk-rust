// The attribute grammar: required lists, keys given once, known keys, and
// string literals only.
use typesafe_sdk::{ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer};

const INSTRUCTIONS: &str = "Spam?";

#[derive(QuestionSet)]
struct Missing {
    #[choice(instructions = "Tone?")]
    tone: ChoiceAnswer,
    #[score]
    urgency: ScoreAnswer,
}

#[derive(QuestionSet)]
struct Repeated {
    #[choice(options("calm" = "polite", "angry", "calm"))]
    tone: ChoiceAnswer,
    #[noul(instructions = "a", instructions = "b")]
    spam: NoulAnswer,
}

#[derive(QuestionSet)]
struct Unknown {
    #[noul(options("x"))]
    spam: NoulAnswer,
    #[score(levels("low"), weight = "3")]
    urgency: ScoreAnswer,
}

#[derive(QuestionSet)]
struct NotText {
    #[noul(instructions = INSTRUCTIONS)]
    constant: NoulAnswer,
    #[choice(options(calm))]
    bare_option: ChoiceAnswer,
    #[score(levels("low", 2))]
    number_level: ScoreAnswer,
    #[noul(instructions = "x"suffix)]
    suffixed: NoulAnswer,
}

#[derive(QuestionSet)]
struct Malformed {
    #[noul = "Spam?"]
    name_value: NoulAnswer,
    #[choice(options = "calm")]
    options_value: ChoiceAnswer,
}

fn main() {}
