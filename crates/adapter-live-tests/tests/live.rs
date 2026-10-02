//! The Python adapter's `tests/test_client_with_live_apis.py`, against the
//! vendors' live APIs: two tests, each for three providers, two structured
//! modes and two answer modes, so 24 cases of one request each.
//!
//! Run them with `TYPESAFE_ADAPTER_LIVE_TESTS=1` and the three vendors' keys,
//! `cargo nextest run -p typesafe-sdk-rust-adapter-live-tests`; nothing else
//! in the repository runs them.
//!
//! Upstream compares each reference-shape response with a recorded file; that
//! file is written by recording, so a live run asserts what holds of any good
//! answer instead: the thresholds, the latency range and the typed views.

use std::{collections::BTreeSet, ptr};

use adapter_live_tests::{
    Live,
    Mode::{Discrete, Probabilities},
    Structured::{Native, Prompted},
    anthropic, gemini, openai,
};
use system_one_adapter::{
    Answer, Attempt, Choice, Noul, NoulAnswer, PreparedQuestions, Questions, Response, Score,
};

/// The review the reference-shape test asks about
/// (`U:tests/test_client_with_live_apis.py:38-42`).
const STATE: &str = concat!(
    "The reviewer calls this entirely invented novel about dragons and wizards a ",
    "flawless masterpiece and the best book they have ever read. They say it has no ",
    "weaknesses, offer only unreserved praise, and urge everyone to read it.",
);

/// The levels of the `rating` score, lowest first (`:45-54`).
const RATING: [&str; 5] = [
    "The reviewer condemns the book and urges readers to avoid it.",
    "The reviewer is mostly critical and does not recommend the book.",
    "The reviewer expresses mixed or neutral feelings about the book.",
    "The reviewer praises the book overall while noting meaningful flaws.",
    "The reviewer offers unreserved praise and an emphatic recommendation.",
];

/// The instructions of the `genre` choice (`:56`).
const GENRE_INSTRUCTIONS: &str = "Which genre this review is about.";

/// The options of the `genre` choice with their criteria (`:57-60`).
const GENRE: [(&str, &str); 2] = [
    ("fiction", "A novel or short story."),
    ("nonfiction", "A book based on facts, real events, or ideas."),
];

/// The questions asked of [`STATE`] (`:43-62`).
fn questions() -> PreparedQuestions {
    let [(fiction, fiction_criterion), (nonfiction, nonfiction_criterion)] = GENRE;
    Questions::new()
        .noul("positive", Noul::new().instructions("The book review is positive."))
        .score(
            "rating",
            Score::new(RATING).instructions("How favorable the reviewer's overall assessment is."),
        )
        .choice(
            "genre",
            Choice::new([fiction, nonfiction])
                .option(fiction, fiction_criterion)
                .option(nonfiction, nonfiction_criterion)
                .instructions(GENRE_INSTRUCTIONS),
        )
        .prepare()
        .expect("invariant: upstream's question set is valid")
}

/// The facts the instruction test asks about (`:63-69`).
const CONTEXT_PROBE_STATE: &str = "Catalog facts:
- marker_fen has state DORMANT.
- marker_tor has state ACTIVE.

Shipping facts:
- The parcel's handling class is CLASS_CRYSTAL.
";

/// The questions asked of [`CONTEXT_PROBE_STATE`] (`:70-85`).
fn context_probe_questions() -> PreparedQuestions {
    Questions::new()
        .choice(
            "instruction_probe",
            Choice::new(["marker_fen", "marker_tor"])
                .option("marker_fen", "The marker_fen catalog entry.")
                .option("marker_tor", "The marker_tor catalog entry.")
                .instructions("Return the only marker whose state is ACTIVE."),
        )
        .choice(
            "criteria_probe",
            Choice::new(["route_7q", "route_2m"])
                .option("route_7q", "Use when the handling class is CLASS_CRYSTAL.")
                .option("route_2m", "Use when the handling class is CLASS_STEEL.")
                .instructions("Return the correct opaque handling route for the parcel."),
        )
        .prepare()
        .expect("invariant: upstream's question set is valid")
}

/// The case as upstream's test id names it: `probabilities-prompted-openai`.
fn case(live: &Live) -> String {
    format!("{}-{}-{}", live.mode().as_str(), live.structured().as_str(), live.provider())
}

/// The response to the case's one request.
///
/// # Panics
///
/// When the request fails, with the adapter's error.
async fn answered(live: &Live, state: &str, questions: &PreparedQuestions) -> Response {
    match live.system_one(state, questions).await {
        Ok(response) => response,
        Err(error) => panic!("[{}] the live request failed: {error}", case(live)),
    }
}

/// `test_live_responses_match_reference_shape` for one case
/// (`:111-131,150-207`).
async fn reference_shape(live: Live) {
    let case = case(&live);
    let response = answered(&live, STATE, &questions()).await;
    let answers = response.answers();

    let expected_answer_probabilities = [
        ("positive", answers.noul("positive").map(NoulAnswer::noul)),
        ("rating", answers.score("rating").and_then(|rating| rating.probability(4))),
        ("genre", answers.choice("genre").and_then(|genre| genre.probability("fiction"))),
    ];
    for (question, probability) in expected_answer_probabilities {
        assert!(
            probability.is_some_and(|probability| probability > 0.9),
            "[{case}] low confidence for the expected {question} answer: {probability:?}"
        );
    }

    let latency = response.usage().latency().as_secs_f64();
    assert!(0.0 < latency && latency < 120.0, "[{case}] latency {latency} s is not in (0, 120)");

    // The typed views and the integer score levels (`:180-186`).
    let rating = answers.score("rating").expect("rating is answered as a score");
    let legend: Vec<(u32, Option<&str>)> =
        rating.legend().map(|(level, description)| (level, description.as_text())).collect();
    let criteria: Vec<(u32, Option<&str>)> = (0..).zip(RATING.map(Some)).collect();
    assert_eq!(legend, criteria, "[{case}] the rating's legend is not its criteria by level");
    let levels: BTreeSet<u32> = rating.probabilities().map(|(level, _)| level).collect();
    assert_eq!(levels, (0..5).collect(), "[{case}] the rating's probabilities cover other levels");
    let positive_view = answers.nouls().find(|(name, _)| *name == "positive").map(|(_, a)| a);
    let positive = answers.get("positive").and_then(Answer::as_noul);
    assert!(
        positive_view.zip(positive).is_some_and(|(view, answer)| ptr::eq(view, answer)),
        "[{case}] the nouls view does not hold the positive answer"
    );
    let genre_view = answers.choices().find(|(name, _)| *name == "genre").map(|(_, a)| a);
    let genre = answers.get("genre").and_then(Answer::as_choice);
    assert!(
        genre_view.zip(genre).is_some_and(|(view, answer)| ptr::eq(view, answer)),
        "[{case}] the choices view does not hold the genre answer"
    );

    // A probability-mode choice is a schema reached through `$ref`; the
    // request the vendor received carries it with the question's
    // instructions and criteria (`:188-207`). Its JSON escapes none of them.
    if live.structured() == Native && live.mode() == Probabilities {
        let request = response
            .debug()
            .attempts()
            .first()
            .and_then(Attempt::request)
            .unwrap_or_else(|| panic!("[{case}] the trace holds no request"));
        let [(_, fiction), (_, nonfiction)] = GENRE;
        for text in ["$defs", "$ref", GENRE_INSTRUCTIONS, fiction, nonfiction] {
            assert!(request.contains(text), "[{case}] the request does not carry {text:?}");
        }
    }
}

/// `test_live_models_follow_question_instructions_and_criteria` for one case
/// (`:214-236`).
async fn follows_instructions_and_criteria(live: Live) {
    let case = case(&live);
    let response = answered(&live, CONTEXT_PROBE_STATE, &context_probe_questions()).await;
    let expected_choices = [("instruction_probe", "marker_tor"), ("criteria_probe", "route_7q")];
    for (question, expected) in expected_choices {
        let answer = response
            .answers()
            .choice(question)
            .unwrap_or_else(|| panic!("[{case}] {question} is not answered as a choice"));
        assert_eq!(answer.choice(), expected, "[{case}] {question}");
        let probability = answer.probability(expected);
        assert!(
            probability.is_some_and(|probability| probability > 0.9),
            "[{case}] {question}: the probability of {expected} is {probability:?}"
        );
    }
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_prompted_openai() {
    let live = openai(Prompted, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_prompted_anthropic() {
    let live = anthropic(Prompted, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_prompted_gemini() {
    let live = gemini(Prompted, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_native_openai() {
    let live = openai(Native, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_native_anthropic() {
    let live = anthropic(Native, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_probabilities_native_gemini() {
    let live = gemini(Native, Probabilities);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_prompted_openai() {
    let live = openai(Prompted, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_prompted_anthropic() {
    let live = anthropic(Prompted, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_prompted_gemini() {
    let live = gemini(Prompted, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_native_openai() {
    let live = openai(Native, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_native_anthropic() {
    let live = anthropic(Native, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_responses_match_reference_shape
async fn reference_shape_discrete_native_gemini() {
    let live = gemini(Native, Discrete);
    reference_shape(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_prompted_openai() {
    let live = openai(Prompted, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_prompted_anthropic() {
    let live = anthropic(Prompted, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_prompted_gemini() {
    let live = gemini(Prompted, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_native_openai() {
    let live = openai(Native, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_native_anthropic() {
    let live = anthropic(Native, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_probabilities_native_gemini() {
    let live = gemini(Native, Probabilities);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_prompted_openai() {
    let live = openai(Prompted, Discrete);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_prompted_anthropic() {
    let live = anthropic(Prompted, Discrete);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_prompted_gemini() {
    let live = gemini(Prompted, Discrete);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_native_openai() {
    let live = openai(Native, Discrete);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_native_anthropic() {
    let live = anthropic(Native, Discrete);
    follows_instructions_and_criteria(live).await;
}

#[tokio::test]
// Upstream: tests/test_client_with_live_apis.py::test_live_models_follow_question_instructions_and_criteria
async fn follows_instructions_and_criteria_discrete_native_gemini() {
    let live = gemini(Native, Discrete);
    follows_instructions_and_criteria(live).await;
}
