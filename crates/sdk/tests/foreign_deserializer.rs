//! A derived question set decodes its answers from a deserializer the caller
//! built, not the SDK's.
//!
//! An adapter that asks another service for answers holds that service's JSON
//! and needs the struct `#[derive(QuestionSet)]` produced, so it calls
//! `AnswerSet::deserialize_answers` with a `serde_json::Deserializer` of its
//! own. This holds under either JSON backend of the SDK: `serde_json` (the
//! default) and sonic-rs (the `sonic` feature), where the deserializer the
//! caller passes is not the codec the SDK parses responses with. A structured
//! legend description is the case that differs between the two, since the
//! SDK keeps it as the JSON text it arrived as.

use decision_model_sdk::{
    AnswerContext, AnswerSet, ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer,
};

/// One question of each kind; the score's levels give the legend.
#[derive(Debug, QuestionSet)]
struct Triage {
    #[noul(instructions = "Is this spam?", yes = "unsolicited advertising")]
    spam: NoulAnswer,
    #[choice(instructions = "What is the tone?", options("friendly", "hostile" = "insulting"))]
    tone: ChoiceAnswer,
    #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
    urgency: ScoreAnswer,
}

/// The `answers` object of a response to `Triage`, as another service writes
/// it: level 1's description is an object rather than text, and an answer the
/// struct has no field for sits between two that it has.
const ANSWERS: &str = r#"{
    "spam": {"type": "noul", "noul": 0.125},
    "extra": {"type": "future", "nested": [{"deep": [1, 2, 3]}]},
    "tone": {
        "type": "choice",
        "choice": "hostile",
        "confidence": 0.75,
        "probabilities": {"friendly": 0.25, "hostile": 0.75}
    },
    "urgency": {
        "type": "score",
        "score": 1.5,
        "confidence": 0.5,
        "legend": {"0": "can wait", "1": {"label": "this week", "days": [1, 7]}, "2": "today"},
        "probabilities": {"0": 0.125, "1": 0.25, "2": 0.625}
    }
}"#;

#[test]
fn a_derived_set_decodes_from_a_callers_serde_json_deserializer() {
    let mut deserializer = serde_json::Deserializer::from_str(ANSWERS);

    let triage = Triage::deserialize_answers(&mut deserializer, AnswerContext::default())
        .expect("the answers decode into the derived set");
    deserializer.end().expect("nothing follows the answers object");

    assert_eq!(triage.spam.noul(), 0.125);

    assert_eq!(triage.tone.choice(), "hostile");
    assert_eq!(triage.tone.confidence(), 0.75);
    assert_eq!(
        triage.tone.probabilities().collect::<Vec<_>>(),
        [("friendly", 0.25), ("hostile", 0.75)]
    );

    assert_eq!(triage.urgency.score(), 1.5);
    assert_eq!(triage.urgency.confidence(), 0.5);
    let legend: Vec<(u32, Option<&str>, Option<&str>)> = triage
        .urgency
        .legend()
        .map(|(level, description)| {
            (level, description.as_text(), description.as_json().map(|raw| raw.as_str()))
        })
        .collect();
    assert_eq!(
        legend,
        [
            (0, Some("can wait"), None),
            (1, None, Some(r#"{"label":"this week","days":[1,7]}"#)),
            (2, Some("today"), None),
        ],
        "a text description stays text and an object stays the JSON it arrived as"
    );
    assert_eq!(
        triage.urgency.probabilities().collect::<Vec<_>>(),
        [(0, 0.125), (1, 0.25), (2, 0.625)]
    );
}
