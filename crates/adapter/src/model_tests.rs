//! Tests for the question model.

use serde_json::json;
use typesafe_sdk::{Choice, Content as SdkContent, Noul, Questions, Score, question::RawQuestion};

use super::*;
use crate::ErrorKind;

/// Prepares `questions` with the SDK, which must accept them, so that a
/// refusal below is the adapter's own.
fn prepared(questions: Questions<'_>) -> PreparedQuestions {
    questions
        .prepare()
        .expect("the SDK accepts the question set; the adapter's check is the one under test")
}

/// The adapter's verdict on a set holding one raw question named `answer`.
fn raw(question: RawQuestion<'_>) -> Result<QuestionModel, Error> {
    QuestionModel::from_prepared(&prepared(Questions::new().raw("answer", question)))
}

fn invalid_request(result: Result<QuestionModel, Error>, case: &str) -> String {
    let error = result.expect_err(case);
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{case}: {error:?}");
    error.to_string()
}

fn text(text: &str) -> Content {
    Content::Text(text.to_owned())
}

fn raw_json(text: &str) -> Content {
    Content::Json(RawValue::from_string(text.to_owned()).expect("valid JSON"))
}

#[test]
// Upstream: tests/test_schema.py::test_invalid_dictionary_questions_are_rejected
fn invalid_dictionary_questions_are_rejected() {
    let cases = [
        ("an unknown type", RawQuestion::new("unknown")),
        ("instructions that are a number", RawQuestion::new("noul").field("instructions", 42)),
        (
            "choice criteria that are a list",
            RawQuestion::new("choice").field("criteria", ["yes", "no"]),
        ),
        (
            "score criteria that are an object",
            RawQuestion::new("score").field("criteria", json!({"0": "Bad.", "1": "Good."})),
        ),
    ];

    for (case, question) in cases {
        let message = invalid_request(raw(question), case);

        assert!(message.starts_with("Question \"answer\" "), "{case}: {message}");
    }
}

#[test]
// Upstream: tests/test_schema.py::test_sdk_question_fields_are_revalidated
fn sdk_question_fields_are_revalidated() {
    let question = RawQuestion::new("noul").field("criteria", json!({"true": 42}));

    let message = invalid_request(raw(question), "a noul criterion that is a number");

    assert_eq!(
        message,
        "Question \"answer\" has a noul criterion that is not a string, an object, an array or null."
    );
}

#[test]
fn built_questions_keep_their_order_labels_and_descriptions() {
    let questions = Questions::new()
        .score("rating", Score::new(["Bad.", "Fine.", "Good."]).instructions("Rate it."))
        .noul(
            "positive",
            Noul::new().instructions("Is it positive?").yes("praise").no("complaints"),
        )
        .choice("genre", Choice::new(["fiction", "nonfiction"]).option("fiction", "made up"));

    let model = QuestionModel::from_prepared(&prepared(questions)).expect("a valid set");

    let ids = model.questions.iter().map(|question| question.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["rating", "positive", "genre"]);
    assert_eq!(model.questions[0].instructions, Some(text("Rate it.")));
    assert_eq!(
        model.questions[0].kind,
        QuestionKind::Score { criteria: vec![text("Bad."), text("Fine."), text("Good.")] }
    );
    assert_eq!(
        model.questions[1].kind,
        QuestionKind::Noul {
            criteria: Some(NoulCriteria {
                yes: Some(text("praise")),
                no: Some(text("complaints"))
            })
        }
    );
    assert_eq!(model.questions[2].instructions, None);
    assert_eq!(
        model.questions[2].kind,
        QuestionKind::Choice {
            criteria: vec![
                ("fiction".to_owned(), Some(text("made up"))),
                ("nonfiction".to_owned(), None)
            ]
        }
    );
}

#[test]
fn outcomes_are_labels_or_level_numbers_in_criteria_order() {
    let questions = Questions::new()
        .noul("positive", Noul::new())
        .choice("genre", Choice::new(["zeta", "alpha"]).option("alpha", "first letter"))
        .score("rating", Score::new(["low", "high"]));

    let model = QuestionModel::from_prepared(&prepared(questions)).expect("a valid set");

    assert_eq!(model.questions[0].outcomes(), []);
    assert_eq!(
        model.questions[1].outcomes(),
        [(Cow::from("zeta"), None), (Cow::from("alpha"), Some(&text("first letter")))]
    );
    assert_eq!(
        model.questions[2].outcomes(),
        [(Cow::from("0"), Some(&text("low"))), (Cow::from("1"), Some(&text("high")))]
    );
}

#[test]
fn non_string_content_keeps_member_order_as_compact_text() {
    let instructions = serde_json::from_str::<SdkContent<'_>>(
        "{ \"z\": 1,\n \"a\": [ \"keep  this space\", {\"y\" : null} ] }",
    )
    .expect("an object is content")
    .into_owned();
    let level = SdkContent::json(&json!(["low", 0])).expect("an array is content");
    let questions = Questions::new()
        .score("rating", Score::new([level, SdkContent::text("high")]).instructions(instructions));

    let model = QuestionModel::from_prepared(&prepared(questions)).expect("a valid set");

    let question = &model.questions[0];
    assert_eq!(
        question.instructions,
        Some(raw_json(r#"{"z":1,"a":["keep  this space",{"y":null}]}"#))
    );
    assert_eq!(
        Content::prompt_text(question.instructions.as_ref()),
        r#"{"z":1,"a":["keep  this space",{"y":null}]}"#
    );
    assert_eq!(
        question.kind,
        QuestionKind::Score { criteria: vec![raw_json(r#"["low",0]"#), text("high")] }
    );
}

#[test]
fn absent_content_is_written_as_no_additional_instructions() {
    assert_eq!(Content::prompt_text(None), "No additional instructions.");
    assert_eq!(Content::prompt_text(Some(&text(""))), "");
}

#[test]
fn null_members_mean_absent_where_upstream_allows_none() {
    let cases = [
        (
            "null instructions and criteria",
            RawQuestion::new("noul")
                .field("instructions", json!(null))
                .field("criteria", json!(null)),
            QuestionKind::Noul { criteria: None },
        ),
        (
            "empty noul criteria",
            RawQuestion::new("noul").field("criteria", json!({})),
            QuestionKind::Noul { criteria: Some(NoulCriteria::default()) },
        ),
        (
            "a null outcome",
            RawQuestion::new("noul").field("criteria", json!({"false": null, "true": "yes"})),
            QuestionKind::Noul {
                criteria: Some(NoulCriteria { yes: Some(text("yes")), no: None }),
            },
        ),
        (
            "an undescribed label",
            RawQuestion::new("choice").field("criteria", json!({"a": null, "b": ["x"]})),
            QuestionKind::Choice {
                criteria: vec![
                    ("a".to_owned(), None),
                    ("b".to_owned(), Some(raw_json(r#"["x"]"#))),
                ],
            },
        ),
    ];

    for (case, question, want) in cases {
        let model = raw(question).expect(case);

        assert_eq!(model.questions[0].instructions, None, "{case}");
        assert_eq!(model.questions[0].kind, want, "{case}");
    }
}

#[test]
fn shapes_the_closed_models_refuse_are_invalid_requests() {
    let cases = [
        (
            "a member the type does not take",
            RawQuestion::new("noul").field("weight", 3),
            "has a member \"weight\" that a noul question does not take",
        ),
        (
            "a noul outcome other than true and false",
            RawQuestion::new("noul").field("criteria", json!({"maybe": "x"})),
            "has a noul criterion \"maybe\"",
        ),
        (
            "noul criteria that are a string",
            RawQuestion::new("noul").field("criteria", "yes"),
            "has noul criteria that are not an object or null",
        ),
        (
            "a choice criterion that is a boolean",
            RawQuestion::new("choice").field("criteria", json!({"a": true, "b": null})),
            "has a choice criterion that is not",
        ),
        (
            "a score level that is null",
            RawQuestion::new("score").field("criteria", json!(["low", null])),
            "has a score criterion that is not",
        ),
        (
            "a score level that is a number",
            RawQuestion::new("score").field("criteria", json!(["low", 1])),
            "has a score criterion that is not",
        ),
        (
            "instructions that are a boolean",
            RawQuestion::new("score").field("criteria", ["a", "b"]).field("instructions", false),
            "has instructions that are not",
        ),
        ("a type spelled in upper case", RawQuestion::new("Noul"), "must have a \"type\" of"),
    ];

    for (case, question, want) in cases {
        let message = invalid_request(raw(question), case);

        assert!(message.contains(want), "{case}: {message}");
    }
}

#[test]
fn shapes_the_sdk_already_refuses_are_refused_here_too() {
    // `prepare` refuses these first; a set compiled into the program reaches
    // the adapter without it, so the adapter checks them again.
    let cases = [
        (
            "choice criteria missing",
            r#"{"answer":{"type":"choice","instructions":"Pick."}}"#,
            "requires \"criteria\"",
        ),
        ("score criteria missing", r#"{"answer":{"type":"score"}}"#, "requires \"criteria\""),
        ("a type that is not a string", r#"{"answer":{"type":1}}"#, "must have a \"type\" of"),
        ("no type", r#"{"answer":{"instructions":"x"}}"#, "must have a \"type\" of"),
        ("a question that is a string", r#"{"answer":"noul"}"#, "must be a JSON object"),
    ];

    for (case, json, want) in cases {
        let message = invalid_request(QuestionModel::from_json(json), case);

        assert!(message.contains(want), "{case}: {message}");
    }
}

#[test]
fn a_choice_or_score_with_one_criterion_gets_upstream_sentence() {
    let sets = [
        ("one label", Questions::new().choice("genre", Choice::new(["fiction"]))),
        ("one level", Questions::new().score("rating", Score::new(["only"]))),
        (
            "an empty raw choice",
            Questions::new().raw("genre", RawQuestion::new("choice").field("criteria", json!({}))),
        ),
    ];

    for (case, questions) in sets {
        let message = invalid_request(QuestionModel::from_prepared(&prepared(questions)), case);

        assert_eq!(message, "Score and choice questions require at least two criteria.", "{case}");
    }
}

#[test]
fn a_malformed_question_is_reported_before_too_few_criteria() {
    let questions = Questions::new()
        .choice("first", Choice::new(["only"]))
        .raw("second", RawQuestion::new("noul").field("instructions", 7));

    let message = invalid_request(QuestionModel::from_prepared(&prepared(questions)), "two faults");

    assert_eq!(
        message,
        "Question \"second\" has instructions that are not a string, an object, an array or null."
    );
}

#[test]
fn a_repeated_name_keeps_its_first_place_and_its_last_value() {
    let json = r#"{"a":{"type":"noul","instructions":"old"},"b":{"type":"noul"},"a":{"type":"noul","instructions":"new"}}"#;

    let model = QuestionModel::from_json(json).expect("a valid set");

    let seen = model
        .questions
        .iter()
        .map(|question| (question.id.as_str(), question.instructions.clone()))
        .collect::<Vec<_>>();
    assert_eq!(seen, [("a", Some(text("new"))), ("b", None)]);
}

#[test]
fn text_that_is_not_a_question_object_is_refused() {
    let cases = [
        ("an empty object", "{}", "At least one question is required."),
        ("an array", "[]", "The questions must be one JSON object."),
        ("not JSON", "{", "The questions must be one JSON object."),
    ];

    for (case, json, want) in cases {
        let message = invalid_request(QuestionModel::from_json(json), case);

        assert_eq!(message, want, "{case}");
    }
}

#[test]
fn debug_of_content_prints_its_shape_and_length() {
    let rendered =
        format!("{:?} {:?}", text("the caller's private document"), raw_json(r#"{"secret":1}"#));

    assert_eq!(rendered, "Text(<29 bytes>) Json(<12 bytes>)");
}
