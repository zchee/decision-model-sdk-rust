//! Typed answers through `Client::ask` with a derived question set, in both
//! forms of the derive the README describes.
//!
//! This crate's tests do not depend on the SDK directly, so both structs
//! reach it through the adapter's re-export: one by the path the README
//! documents, `decision_model_adapter::decision_model_sdk`, and one by a local name for
//! that re-export, as a crate with its own facade module would.

#[path = "support/scripted.rs"]
mod scripted;

use std::sync::Arc;

use decision_model_adapter::{
    AnswerMode, Answers, ChoiceAnswer, Client, NoulAnswer, QuestionSet, Response, ScoreAnswer,
    StructuredOutputs, decision_model_sdk as sdk,
};

use crate::scripted::{Scripted, reply};

const STATE: &str = "This is a delightful fiction novel.";

/// The derive pointed at the SDK through the adapter, for a caller that
/// depends on the adapter alone.
#[derive(Debug, QuestionSet)]
#[question_set(crate = decision_model_adapter::decision_model_sdk)]
struct Review {
    #[noul(instructions = "The review is positive.")]
    positive: NoulAnswer,
    #[score(instructions = "Rating.", levels("Bad.", "Good."))]
    stars: ScoreAnswer,
    #[choice(instructions = "Genre.", options("fiction" = "A story.", "nonfiction" = "Facts."))]
    genre: ChoiceAnswer,
}

/// The derive pointed at a name the caller gave the SDK.
#[derive(Debug, QuestionSet)]
#[question_set(crate = crate::sdk)]
struct Verdict {
    #[noul(instructions = "The review is positive.")]
    positive: NoulAnswer,
    #[score(instructions = "Rating.", levels("Bad.", "Good."))]
    stars: ScoreAnswer,
    #[choice(instructions = "Genre.", options("fiction" = "A story.", "nonfiction" = "Facts."))]
    genre: ChoiceAnswer,
}

/// Asks the questions of `Q` twice with the same scripted reply: typed
/// through `ask`, and untyped through `system_one` with `Q`'s prepared
/// questions, the JSON the derive wrote at compile time.
async fn round_trip<Q: QuestionSet>(
    answer_mode: AnswerMode,
    payload: &str,
) -> (Response<Q>, Response<Answers>) {
    let scripted = Arc::new(Scripted::new(vec![reply(payload)]));
    let client = Client::builder(StructuredOutputs::Native, answer_mode)
        .provider_instance(scripted.clone())
        .build()
        .expect("builds");

    let typed = client.ask::<Q>(STATE).send().await.expect("the typed call answers");
    let untyped =
        client.system_one(STATE, Q::prepared()).send().await.expect("the untyped call answers");

    let calls = scripted.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call.structured && call.messages.len() == 2));
    assert_eq!(calls[0].messages, calls[1].messages);
    assert!(scripted.log_uri.is_none());
    (typed, untyped)
}

#[tokio::test]
async fn typed_round_trip_probabilities() {
    let payload = r#"{"answers":{"positive":0.8,"stars":{"0":0.25,"1":0.75},"genre":{"fiction":0.9,"nonfiction":0.1}}}"#;

    let (typed, untyped) = round_trip::<Review>(AnswerMode::Probabilities, payload).await;

    let review = typed.answers();
    assert_eq!(review.positive.noul(), 0.8);
    assert_eq!(review.stars.score(), 0.75);
    assert_eq!(review.stars.probabilities().collect::<Vec<_>>(), [(0, 0.25), (1, 0.75)]);
    assert_eq!(review.genre.choice(), "fiction");
    assert_eq!(Some(&review.positive), untyped.answers().noul("positive"));
    assert_eq!(Some(&review.stars), untyped.answers().score("stars"));
    assert_eq!(Some(&review.genre), untyped.answers().choice("genre"));
    assert_eq!(Review::prepared().names().collect::<Vec<_>>(), ["positive", "stars", "genre"]);
}

#[tokio::test]
async fn typed_round_trip_discrete() {
    let payload = r#"{"answers":{"positive":true,"stars":1,"genre":"nonfiction"}}"#;

    let (typed, untyped) = round_trip::<Verdict>(AnswerMode::Discrete, payload).await;

    let verdict = typed.answers();
    assert_eq!(verdict.positive.noul(), 1.0);
    assert_eq!(verdict.stars.score(), 1.0);
    assert_eq!(verdict.stars.probabilities().collect::<Vec<_>>(), [(0, 0.0), (1, 1.0)]);
    assert_eq!(verdict.genre.choice(), "nonfiction");
    assert_eq!(Some(&verdict.positive), untyped.answers().noul("positive"));
    assert_eq!(Some(&verdict.stars), untyped.answers().score("stars"));
    assert_eq!(Some(&verdict.genre), untyped.answers().choice("genre"));
    assert_eq!(Verdict::prepared().as_json(), Review::prepared().as_json());
}
