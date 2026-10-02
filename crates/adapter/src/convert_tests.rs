//! Tests for the conversion of decoded values into the SDK's answers.

use super::*;
use crate::ErrorKind;

const REVIEW: &str = r#"{
    "positive": {"type": "noul"},
    "rating": {"type": "score", "criteria": ["bad", "mixed", "good"]},
    "genre": {"type": "choice", "criteria": {"fiction": "A novel.", "nonfiction": null}}
}"#;

fn model(json: &str) -> QuestionModel {
    QuestionModel::from_json(json).expect("the questions are valid")
}

fn converted(json: &str, answers: Vec<DecodedAnswer>, normalize_probabilities: bool) -> Converted {
    convert(&model(json), DecodedAnswers { answers }, normalize_probabilities)
        .expect("the decoded answers match the questions")
}

fn bits(values: impl IntoIterator<Item = f64>) -> Vec<u64> {
    values.into_iter().map(f64::to_bits).collect()
}

#[test]
fn metrics_tie_choice() {
    let tie = || vec![DecodedAnswer::Distribution(vec![0.5, 0.5])];

    let fiction_first = converted(
        r#"{"genre": {"type": "choice", "criteria": {"fiction": null, "nonfiction": null}}}"#,
        tie(),
        false,
    );
    let answer = fiction_first.answers.choice("genre").expect("a choice answer");
    assert_eq!(answer.choice(), "fiction");
    assert_eq!(answer.confidence().to_bits(), 0.0_f64.to_bits());

    // The order is the question's criteria order, not an order of the names.
    let nonfiction_first = converted(
        r#"{"genre": {"type": "choice", "criteria": {"nonfiction": null, "fiction": null}}}"#,
        tie(),
        false,
    );
    let answer = nonfiction_first.answers.choice("genre").expect("a choice answer");
    assert_eq!(answer.choice(), "nonfiction");
}

#[test]
fn probabilities_become_answers_in_question_order() {
    let converted = converted(
        REVIEW,
        vec![
            DecodedAnswer::Probability(0.75),
            DecodedAnswer::Distribution(vec![0.1, 0.2, 0.7]),
            DecodedAnswer::Distribution(vec![0.25, 0.75]),
        ],
        false,
    );

    let answers = &converted.answers;
    assert_eq!(answers.names().collect::<Vec<_>>(), ["positive", "rating", "genre"]);
    assert_eq!(answers.noul("positive").map(NoulAnswer::noul), Some(0.75));

    let rating = answers.score("rating").expect("a score answer");
    assert_eq!(
        bits(rating.probabilities().map(|(_, probability)| probability)),
        bits([0.1, 0.2, 0.7])
    );
    assert_eq!(rating.probabilities().map(|(level, _)| level).collect::<Vec<_>>(), [0, 1, 2]);
    assert_eq!(rating.score().to_bits(), metrics::expected_score(&[0.1, 0.2, 0.7]).to_bits());
    assert!((rating.score() - 1.6).abs() < 1e-12, "{}", rating.score());
    assert_eq!(
        rating.confidence().to_bits(),
        metrics::score_confidence(&[0.1, 0.2, 0.7]).to_bits()
    );
    assert_eq!(
        rating.legend().map(|(level, text)| (level, text.as_text())).collect::<Vec<_>>(),
        [(0, Some("bad")), (1, Some("mixed")), (2, Some("good"))]
    );

    let genre = answers.choice("genre").expect("a choice answer");
    assert_eq!(genre.choice(), "nonfiction");
    assert_eq!(genre.confidence().to_bits(), 0.5_f64.to_bits());
    assert_eq!(
        genre.probabilities().collect::<Vec<_>>(),
        [("fiction", 0.25), ("nonfiction", 0.75)]
    );

    assert_eq!(converted.probabilities, ProbabilityDebug::default());
}

#[test]
fn discrete_values_become_certain_answers() {
    let converted = converted(
        REVIEW,
        vec![DecodedAnswer::Bool(true), DecodedAnswer::Level(2), DecodedAnswer::Label(0)],
        // Normalization has nothing to change in a discrete answer.
        true,
    );

    let answers = &converted.answers;
    assert_eq!(answers.noul("positive").map(NoulAnswer::noul), Some(1.0));
    let rating = answers.score("rating").expect("a score answer");
    assert_eq!(rating.score(), 2.0);
    assert_eq!(rating.confidence(), 1.0);
    assert_eq!(rating.probabilities().collect::<Vec<_>>(), [(0, 0.0), (1, 0.0), (2, 1.0)]);
    let genre = answers.choice("genre").expect("a choice answer");
    assert_eq!(genre.choice(), "fiction");
    assert_eq!(genre.confidence(), 1.0);
    assert_eq!(genre.probabilities().collect::<Vec<_>>(), [("fiction", 1.0), ("nonfiction", 0.0)]);
    assert_eq!(converted.probabilities, ProbabilityDebug::default());

    let no = super::convert(
        &model(r#"{"positive": {"type": "noul"}}"#),
        DecodedAnswers { answers: vec![DecodedAnswer::Bool(false)] },
        false,
    )
    .expect("a noul answer");
    assert_eq!(no.answers.noul("positive").map(NoulAnswer::noul), Some(0.0));
}

#[test]
fn unnormalized_probabilities_are_reported_as_given_and_scored_as_a_distribution() {
    let answers = || {
        vec![
            DecodedAnswer::Probability(0.5),
            DecodedAnswer::Distribution(vec![0.1, 0.1, 0.2]),
            DecodedAnswer::Distribution(vec![0.2, 0.6]),
        ]
    };

    let off = converted(REVIEW, answers(), false);
    let rating = off.answers.score("rating").expect("a score answer");
    // The reported probabilities are the model's; the score is the expected
    // level of the rescaled ones, 0.25 * 1 + 0.5 * 2.
    assert_eq!(
        bits(rating.probabilities().map(|(_, probability)| probability)),
        bits([0.1, 0.1, 0.2])
    );
    assert_eq!(rating.score(), 1.25);
    let genre = off.answers.choice("genre").expect("a choice answer");
    assert_eq!(genre.probabilities().collect::<Vec<_>>(), [("fiction", 0.2), ("nonfiction", 0.6)]);
    assert_eq!(genre.choice(), "nonfiction");
    assert_eq!(off.probabilities.invalid_probs, 2);
    assert_eq!(
        off.probabilities.probability_errors.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
        ["rating", "genre"]
    );
    assert!((off.probabilities.max_error - 0.6).abs() < 1e-12);
    assert!(off.probabilities.original_probabilities.is_empty());

    let on = converted(REVIEW, answers(), true);
    let rating = on.answers.score("rating").expect("a score answer");
    assert_eq!(rating.probabilities().collect::<Vec<_>>(), [(0, 0.25), (1, 0.25), (2, 0.5)]);
    assert_eq!(rating.score(), 1.25);
    let genre = on.answers.choice("genre").expect("a choice answer");
    assert_eq!(
        bits(genre.probabilities().map(|(_, probability)| probability)),
        bits([0.2 / 0.8, 0.6 / 0.8])
    );
    assert_eq!(on.probabilities.invalid_probs, 2);
    assert!((on.probabilities.max_error - 0.6).abs() < 1e-12);
    assert_eq!(
        on.probabilities.original_probabilities,
        vec![
            (
                "rating".to_owned(),
                vec![("0".to_owned(), 0.1), ("1".to_owned(), 0.1), ("2".to_owned(), 0.2)]
            ),
            ("genre".to_owned(), vec![("fiction".to_owned(), 0.2), ("nonfiction".to_owned(), 0.6)]),
        ]
    );
}

#[test]
fn a_structured_criterion_keeps_its_members_in_order_in_the_legend() {
    let converted = converted(
        r#"{"rating": {"type": "score", "criteria": [{"b": 1, "a": [2, 3]}, "plain"]}}"#,
        vec![DecodedAnswer::Level(0)],
        false,
    );

    let rating = converted.answers.score("rating").expect("a score answer");
    assert_eq!(
        rating.description(0).and_then(|content| content.as_json()).map(|raw| raw.as_str()),
        Some(r#"{"b":1,"a":[2,3]}"#)
    );
    assert_eq!(rating.description(1).and_then(|content| content.as_text()), Some("plain"));
    assert_eq!(
        serde_json::to_string(rating).expect("a score answer serializes"),
        r#"{"type":"score","score":0.0,"confidence":1.0,"legend":{"0":{"b":1,"a":[2,3]},"1":"plain"},"probabilities":{"0":1.0,"1":0.0}}"#
    );
}

#[test]
fn values_that_do_not_match_the_questions_are_refused() {
    let cases: [(&str, Vec<DecodedAnswer>); 9] = [
        ("too few values", vec![DecodedAnswer::Bool(true), DecodedAnswer::Level(0)]),
        (
            "too many values",
            vec![
                DecodedAnswer::Bool(true),
                DecodedAnswer::Level(0),
                DecodedAnswer::Label(0),
                DecodedAnswer::Label(0),
            ],
        ),
        (
            "a noul with a level",
            vec![DecodedAnswer::Level(0), DecodedAnswer::Level(0), DecodedAnswer::Label(0)],
        ),
        (
            "a score with a label",
            vec![DecodedAnswer::Bool(true), DecodedAnswer::Label(0), DecodedAnswer::Label(0)],
        ),
        (
            "a choice with a level",
            vec![DecodedAnswer::Bool(true), DecodedAnswer::Level(0), DecodedAnswer::Level(0)],
        ),
        (
            "a level past the last",
            vec![DecodedAnswer::Bool(true), DecodedAnswer::Level(3), DecodedAnswer::Label(0)],
        ),
        (
            "a label past the last",
            vec![DecodedAnswer::Bool(true), DecodedAnswer::Level(0), DecodedAnswer::Label(2)],
        ),
        (
            "a score distribution of the wrong length",
            vec![
                DecodedAnswer::Bool(true),
                DecodedAnswer::Distribution(vec![0.5, 0.5]),
                DecodedAnswer::Label(0),
            ],
        ),
        (
            "a choice distribution of the wrong length",
            vec![
                DecodedAnswer::Bool(true),
                DecodedAnswer::Level(0),
                DecodedAnswer::Distribution(vec![0.2, 0.3, 0.5]),
            ],
        ),
    ];
    let model = model(REVIEW);
    for (case, answers) in cases {
        let error = convert(&model, DecodedAnswers { answers }, false).expect_err(case);
        assert!(matches!(error.kind(), ErrorKind::MalformedStructure), "{case}: {error:?}");
    }
}
