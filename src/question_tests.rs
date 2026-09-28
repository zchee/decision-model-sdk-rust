//! Tests for the question model.
//!
//! Wire shapes are compared against JSON literals copied from the upstream
//! Python SDK's tests (v0.7.2), parsed with `serde_json`, so key order and
//! spacing do not matter there; the tests that pin exact bytes say so.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;
use crate::ErrorKind;

// ------------------------------------------------------------- helpers

/// The prepared JSON object, parsed as data.
fn wire(questions: Questions<'_>) -> Value {
    let prepared = questions.prepare().expect("the question set is valid");
    serde_json::from_slice(prepared.as_bytes()).unwrap_or_else(|error| {
        panic!(
            "prepared bytes are not JSON ({error}): {:?}",
            String::from_utf8_lossy(prepared.as_bytes())
        )
    })
}

/// The prepared JSON object, as text.
fn text(questions: Questions<'_>) -> String {
    let prepared = questions.prepare().expect("the question set is valid");
    String::from_utf8(prepared.as_bytes().to_vec()).expect("the codec emits UTF-8")
}

/// The failure `prepare` reports, which must be an invalid-request error.
fn rejection(questions: Questions<'_>) -> String {
    let error = questions.prepare().expect_err("the question set must be rejected");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "wrong kind: {error:?}");
    assert!(
        std::error::Error::source(&error).is_none(),
        "an invalid request carries no source: {error:?}"
    );
    error.to_string()
}

/// Raw JSON with exactly the given spelling, as a document would carry it.
fn spelled(text: &str) -> RawJson {
    codec::decode::<RawJson>(text.as_bytes()).expect("one JSON value")
}

fn content(value: &Value) -> Content<'static> {
    Content::json(value).expect("a JSON object or array is content")
}

/// Builds a raw question from a JSON object literal, field by field in the
/// literal's order.
fn raw_from(object: &Value) -> RawQuestion<'static> {
    let Value::Object(members) = object else { panic!("not an object: {object}") };
    let kind = members.get("type").expect("a raw literal names its type");
    let mut question = RawQuestion::new("placeholder").field("type", kind);
    for (name, value) in members {
        if name != "type" {
            question = question.field(name.clone(), value);
        }
    }
    question
}

// ------------------------------------------------------------- wire shape

/// Upstream `test_normalization_preserves_objects`.
#[test]
fn typed_questions_encode_to_the_upstream_wire_form() {
    let questions = Questions::new()
        .noul("noul", Noul::new().instructions("Spam?"))
        .choice("choice", Choice::new(["calm"]).instructions("Tone?"))
        .score("score", Score::new(["bad", "good"]).instructions("Quality?"));

    assert_eq!(
        wire(questions),
        json!({
            "noul": {"type": "noul", "instructions": "Spam?"},
            "choice": {"type": "choice", "instructions": "Tone?", "criteria": {"calm": null}},
            "score": {"type": "score", "instructions": "Quality?", "criteria": ["bad", "good"]},
        })
    );
}

/// The bytes themselves: member order is upstream's (`type`, `instructions`,
/// `criteria`), question order is the caller's, and nothing is padded.
#[test]
fn the_prepared_bytes_are_compact_and_in_caller_order() {
    let questions = Questions::new()
        .score("urgency", Score::new(["can wait", "today"]).instructions("How urgent?"))
        .noul("billing", Noul::new().instructions("Billing?").yes("payments").no("anything else"))
        .choice("tone", Choice::new(["calm", "angry"]).option("calm", "neutral or polite"));

    assert_eq!(
        text(questions),
        concat!(
            r#"{"urgency":{"type":"score","instructions":"How urgent?","criteria":["can wait","today"]},"#,
            r#""billing":{"type":"noul","instructions":"Billing?","criteria":{"true":"payments","false":"anything else"}},"#,
            r#""tone":{"type":"choice","criteria":{"calm":"neutral or polite","angry":null}}}"#,
        )
    );
}

/// Upstream `test_direct_encoding_omits_only_default_fields`, the three cases
/// Rust can express; the other two set `criteria` to `{}` and to
/// `{"true": null}`, which the builder does not represent (an undescribed
/// outcome is left out instead).
#[test]
fn unset_members_are_left_off_the_wire() {
    let cases = [
        (Question::from(Noul::new()), json!({"type": "noul"})),
        (Question::from(Choice::new(["a"])), json!({"type": "choice", "criteria": {"a": null}})),
        (Question::from(Score::new(["good"])), json!({"type": "score", "criteria": ["good"]})),
        (Question::from(Noul::new().instructions("")), json!({"type": "noul", "instructions": ""})),
        (
            Question::from(Noul::new().instructions(content(&json!([])))),
            json!({"type": "noul", "instructions": []}),
        ),
    ];
    for (question, expected) in cases {
        let debug = format!("{question:?}");
        assert_eq!(
            wire(Questions::new().question("q", question)),
            json!({"q": expected}),
            "{debug}"
        );
    }
}

/// Upstream `test_discriminators_are_automatic`: the `type` tag comes from the
/// builder, never from the caller.
#[test]
fn each_builder_writes_its_own_type_tag() {
    let value = wire(
        Questions::new()
            .noul("n", Noul::new().instructions("Spam?"))
            .choice("c", Choice::new(["calm"]).instructions("Tone?"))
            .score("s", Score::new(["good"]).instructions("Quality?")),
    );
    assert_eq!(value["n"]["type"], "noul");
    assert_eq!(value["c"]["type"], "choice");
    assert_eq!(value["s"]["type"], "score");
}

/// Upstream `test_optional_noul_criteria`, typed half: every criteria value
/// but `{}`, which the builder does not represent.
#[test]
fn noul_criteria_carry_either_outcome_or_both() {
    let object = json!({"summary": "Unsolicited", "examples": ["Buy now"]});
    let cases = [
        (Noul::new(), None),
        (Noul::new().yes("Yes"), Some(json!({"true": "Yes"}))),
        (Noul::new().no("No"), Some(json!({"false": "No"}))),
        (Noul::new().yes("Yes").no("No"), Some(json!({"true": "Yes", "false": "No"}))),
        (Noul::new().yes(content(&object)), Some(json!({"true": object}))),
    ];
    for (noul, criteria) in cases {
        let mut expected = json!({"type": "noul", "instructions": "Spam?"});
        if let Some(criteria) = criteria {
            expected["criteria"] = criteria;
        }
        let debug = format!("{noul:?}");
        assert_eq!(
            wire(Questions::new().noul("q", noul.instructions("Spam?"))),
            json!({"q": expected}),
            "{debug}"
        );
    }
}

/// Upstream `test_optional_noul_criteria`, raw half: all six criteria values,
/// `{}` included, pass through as given.
#[test]
fn raw_noul_criteria_pass_through_as_given() {
    let criteria = [
        None,
        Some(json!({})),
        Some(json!({"true": "Yes"})),
        Some(json!({"false": "No"})),
        Some(json!({"true": "Yes", "false": "No"})),
        Some(json!({"true": {"summary": "Unsolicited", "examples": ["Buy now"]}})),
    ];
    for criteria in criteria {
        let mut expected = json!({"type": "noul", "instructions": "Spam?"});
        if let Some(criteria) = criteria {
            expected["criteria"] = criteria;
        }
        assert_eq!(wire(Questions::new().raw("q", raw_from(&expected))), json!({"q": expected}));
    }
}

/// Upstream `test_array_inputs` (`tests/test_types.py`), both halves. The
/// typed half sends `{"true": description}` where upstream sends
/// `{"true": description, "false": null}`: the builder leaves an undescribed
/// outcome out rather than writing `null`, which the API reads the same way.
#[test]
fn array_content_is_accepted_everywhere_content_is() {
    let instructions = json!(["Read the message", {"context": null}]);
    let description = json!(["Example", null]);
    let raw = json!({
        "yes": {"type": "noul", "instructions": instructions, "criteria": {"true": description, "false": null}},
        "label": {"type": "choice", "instructions": instructions, "criteria": {"a": description, "b": null}},
        "rating": {"type": "score", "instructions": instructions, "criteria": [description]},
    });

    let from_raw = Questions::new()
        .raw("yes", raw_from(&raw["yes"]))
        .raw("label", raw_from(&raw["label"]))
        .raw("rating", raw_from(&raw["rating"]));
    assert_eq!(wire(from_raw), raw);

    let typed = Questions::new()
        .noul("yes", Noul::new().instructions(content(&instructions)).yes(content(&description)))
        .choice(
            "label",
            Choice::new(["a", "b"])
                .instructions(content(&instructions))
                .option("a", content(&description)),
        )
        .score("rating", Score::new([content(&description)]).instructions(content(&instructions)));
    let mut expected = raw;
    expected["yes"]["criteria"] = json!({"true": description});
    assert_eq!(wire(typed), expected);
}

/// Upstream `test_raw_optional_fields_preserve_explicit_null`.
#[test]
fn a_raw_question_keeps_an_explicit_null() {
    let expected = json!({
        "yes": {"type": "noul", "instructions": null, "criteria": null},
        "label": {"type": "choice", "instructions": null, "criteria": {"a": null}},
        "rating": {"type": "score", "instructions": null, "criteria": ["good"]},
    });
    let questions = Questions::new()
        .raw(
            "yes",
            RawQuestion::new("noul").field("instructions", ()).field("criteria", None::<u8>),
        )
        .raw(
            "label",
            RawQuestion::new("choice")
                .field("instructions", ())
                .field("criteria", &expected["label"]["criteria"]),
        )
        .raw(
            "rating",
            RawQuestion::new("score").field("instructions", ()).field("criteria", ["good"]),
        );
    assert_eq!(wire(questions), expected);
}

/// Upstream `test_explicitly_nullable_json_values`: a `null` nested inside
/// content survives.
#[test]
fn a_null_inside_content_survives() {
    let instructions = json!({"text": "Classify", "extra": null});
    let extra = json!({"extra": null});
    let questions = Questions::new()
        .noul("yes", Noul::new().instructions(content(&instructions)).yes(content(&extra)))
        .choice(
            "label",
            Choice::new(["a"]).option("b", content(&extra)).instructions(content(&instructions)),
        )
        .score("rating", Score::new([content(&extra)]).instructions(content(&instructions)));
    assert_eq!(
        wire(questions),
        json!({
            "yes": {"type": "noul", "instructions": instructions, "criteria": {"true": {"extra": null}}},
            "label": {"type": "choice", "instructions": instructions, "criteria": {"a": null, "b": {"extra": null}}},
            "rating": {"type": "score", "instructions": instructions, "criteria": [{"extra": null}]},
        })
    );
}

/// Upstream `test_abstract_input_containers_encode`: any iterable of names or
/// levels, and any serializable array for raw criteria.
#[test]
fn builders_take_any_iterator() {
    let names = [String::from("a"), String::from("b")];
    let levels = ("low", "high");
    let questions = Questions::new()
        .choice(
            "label",
            Choice::new(names.iter().map(String::as_str))
                .option("b", "x")
                .instructions(content(&json!(["read", {"ctx": null}]))),
        )
        .score("rating", Score::new([levels.0, levels.1]))
        .raw("raw", RawQuestion::new("score").field("criteria", ("bad", "good")));
    assert_eq!(
        wire(questions),
        json!({
            "label": {"type": "choice", "instructions": ["read", {"ctx": null}], "criteria": {"a": null, "b": "x"}},
            "rating": {"type": "score", "criteria": ["low", "high"]},
            "raw": {"type": "score", "criteria": ["bad", "good"]},
        })
    );
}

/// Upstream `test_round_trip` (`tests/test_clients.py`), the `questions` part
/// of all three forms: typed, raw and mixed produce the same object.
#[test]
fn typed_raw_and_mixed_forms_agree() {
    let expected = json!({
        "spam": {"type": "noul", "instructions": "Spam?"},
        "tone": {"type": "choice", "instructions": "Tone?", "criteria": {"friendly": null, "hostile": null}},
        "quality": {"type": "score", "instructions": "Quality?", "criteria": ["bad", "ok", "great"]},
    });
    let typed_tone = || Choice::new(["friendly", "hostile"]).instructions("Tone?");
    let typed_quality = || Score::new(["bad", "ok", "great"]).instructions("Quality?");

    let dataclass = Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", typed_tone())
        .score("quality", typed_quality());
    let raw = Questions::new()
        .raw("spam", raw_from(&expected["spam"]))
        .raw("tone", raw_from(&expected["tone"]))
        .raw("quality", raw_from(&expected["quality"]));
    let mixed = Questions::new()
        .raw("spam", raw_from(&expected["spam"]))
        .choice("tone", typed_tone())
        .score("quality", typed_quality());

    for (form, questions) in [("dataclass", dataclass), ("raw", raw), ("mixed", mixed)] {
        assert_eq!(wire(questions), expected, "form {form}");
    }
}

/// Upstream `test_rich_descriptions`, the `questions` part: object content in
/// instructions, noul criteria, choice options and score levels.
#[test]
fn object_content_is_spliced_in_unchanged() {
    let criteria = json!({"summary": "duplicated", "examples": ["charged twice"]});
    let questions = Questions::new()
        .noul(
            "duplicate",
            Noul::new()
                .instructions(content(&json!({"question": "Duplicate?"})))
                .yes(content(&criteria)),
        )
        .choice(
            "team",
            Choice::new(["billing", "other"])
                .instructions("Team?")
                .option("billing", content(&criteria)),
        )
        .score("risk", Score::new([content(&criteria)]).instructions("Risk?"));
    assert_eq!(
        wire(questions),
        json!({
            "duplicate": {"type": "noul", "instructions": {"question": "Duplicate?"}, "criteria": {"true": criteria}},
            "team": {"type": "choice", "instructions": "Team?", "criteria": {"billing": criteria, "other": null}},
            "risk": {"type": "score", "instructions": "Risk?", "criteria": [criteria]},
        })
    );
}

/// Raw JSON content keeps its own spelling: it is copied, not re-encoded.
#[test]
fn raw_json_content_keeps_its_spelling() {
    let spaced = spelled(r#"{ "k" : 1.50 }"#);
    assert_eq!(spaced.as_str(), r#"{ "k" : 1.50 }"#);
    let content = Content::json(&spaced).expect("an object is content");
    let prepared = text(Questions::new().noul("q", Noul::new().instructions(content)));
    assert_eq!(prepared, format!(r#"{{"q":{{"type":"noul","instructions":{spaced}}}}}"#));
}

/// Names, option names and text are escaped as JSON strings; names come back
/// from `names()` unescaped.
#[test]
fn names_and_text_are_escaped_on_the_wire_and_not_in_names() {
    let name = "say \"hi\"\\\n\u{1F30D}";
    let prepared = Questions::new()
        .choice(name, Choice::new(["tab\there"]).instructions("line\nbreak \u{0000} end"))
        .prepare()
        .expect("valid");
    assert_eq!(
        std::str::from_utf8(prepared.as_bytes()).expect("UTF-8"),
        concat!(
            r#"{"say \"hi\"\\\n"#,
            "\u{1F30D}",
            r#"":{"type":"choice","instructions":"line\nbreak \u0000 end","criteria":{"tab\there":null}}}"#,
        )
    );
    assert_eq!(prepared.names().collect::<Vec<_>>(), [name]);
}

// ------------------------------------------------------------- raw questions

/// Upstream `test_normalization_preserves_raw_questions` and
/// `test_raw_question_passthrough`: unknown types and unknown fields are sent
/// as given, beside typed questions.
#[test]
fn raw_questions_pass_through_beside_typed_ones() {
    let raws = [
        json!({"type": "noul", "instructions": "Spam?", "weight": 3, "criteria": {"future": "kept"}}),
        json!({"type": "future", "nested": {"k": null}}),
        json!({"type": "score", "criteria": ["good"], "weight": 3}),
        json!({"type": "noul", "instructions": "Spam?", "weight": 3, "nested": {"k": null}}),
        json!({"type": "choice", "criteria": {"a": null}, "weight": 2}),
    ];
    for raw in raws {
        let questions = Questions::new()
            .raw("raw", raw_from(&raw))
            .noul("typed", Noul::new().instructions("Spam?"));
        assert_eq!(
            wire(questions),
            json!({"raw": raw, "typed": {"type": "noul", "instructions": "Spam?"}})
        );
    }
}

/// Upstream `test_question_schema_validation_is_left_to_api`: a raw question
/// of the wrong shape for its type is the server's to reject.
#[test]
fn raw_question_schema_is_left_to_the_api() {
    for raw in [
        json!({"type": "noul", "instructions": 1}),
        json!({"type": "choice", "criteria": ["invalid", "shape"]}),
    ] {
        assert_eq!(wire(Questions::new().raw("q", raw_from(&raw))), json!({"q": raw}));
    }
}

/// Upstream `test_raw_questions_require_structural_keys`, every case Rust can
/// express. `{}`, `{"instructions": ...}` without a type, a bare string and
/// `None` are not: `RawQuestion::new` always sets a string `type`, and a
/// question is always an object.
#[test]
fn a_raw_question_needs_a_nonempty_string_type_and_criteria_where_upstream_does() {
    let no_type = "Question \"invalid\" must be a question object or a dictionary with a nonempty string \"type\".";
    let cases: [(&str, RawQuestion<'_>, &str); 7] = [
        (
            "choice without criteria",
            RawQuestion::new("choice"),
            "Question \"invalid\" requires \"criteria\".",
        ),
        (
            "score without criteria",
            RawQuestion::new("score"),
            "Question \"invalid\" requires \"criteria\".",
        ),
        ("empty type", RawQuestion::new(""), no_type),
        ("null type", RawQuestion::new("noul").field("type", ()), no_type),
        ("number type", RawQuestion::new("noul").field("type", 1), no_type),
        ("array type", RawQuestion::new("noul").field("type", ["future"]), no_type),
        ("empty type set later", RawQuestion::new("noul").field("type", ""), no_type),
    ];
    for (case, question, message) in cases {
        assert_eq!(rejection(Questions::new().raw("invalid", question)), message, "case {case}");
    }
}

/// Upstream `test_empty_score_criteria_is_rejected`, both halves, and the
/// score half of `test_validation_before_network`.
#[test]
fn a_score_without_levels_is_rejected_typed_or_raw() {
    let message = "Score question \"rating\" has no criteria; at least one score is required.";
    let typed = Score::new(Vec::<&str>::new()).instructions("Quality?");
    assert_eq!(rejection(Questions::new().score("rating", typed)), message);
    let raw = RawQuestion::new("score")
        .field("instructions", "Quality?")
        .field("criteria", Vec::<u8>::new());
    assert_eq!(rejection(Questions::new().raw("rating", raw)), message);
}

/// Upstream tests `not criteria`, so every value Python treats as false is
/// "no criteria", whitespace inside a spliced value included; anything else
/// passes to the server.
#[test]
fn raw_score_criteria_are_empty_exactly_when_python_would_say_so() {
    let message = "Score question \"rating\" has no criteria; at least one score is required.";
    let falsy = [
        RawJson::from_value(&()).expect("encodes"),
        RawJson::from_value(&false).expect("encodes"),
        RawJson::from_value(&0).expect("encodes"),
        RawJson::from_value(&-0.0).expect("encodes"),
        RawJson::from_value(&"").expect("encodes"),
        RawJson::from_value(&BTreeMap::<String, u8>::new()).expect("encodes"),
        RawJson::from_value(&Vec::<u8>::new()).expect("encodes"),
    ];
    for value in falsy {
        let shown = value.to_string();
        let raw = RawQuestion::new("score").field("criteria", &value);
        assert_eq!(rejection(Questions::new().raw("rating", raw)), message, "criteria {shown}");
    }
    let truthy = [
        json!(["good"]),
        json!({"0": "good"}),
        json!("good"),
        json!(1),
        json!(0.5),
        json!(true),
        json!(-1e-9),
    ];
    for value in truthy {
        let raw = RawQuestion::new("score").field("criteria", &value);
        assert_eq!(
            wire(Questions::new().raw("rating", raw)),
            json!({"rating": {"type": "score", "criteria": value}})
        );
    }
    // Raw JSON is spliced with its own spacing; `[ ]` is still empty.
    let spaced = spelled("[ \n ]");
    assert_eq!(spaced.as_str(), "[ \n ]");
    assert_eq!(
        rejection(
            Questions::new().raw("rating", RawQuestion::new("score").field("criteria", &spaced))
        ),
        message
    );
}

/// The falsy scan on its own, including spacing a `RawJson` captured from a
/// document can carry and numbers whose zero-ness hides in an exponent.
#[test]
fn the_falsy_scan_reads_only_the_shape() {
    for fragment in [
        "null", " false ", "\"\"", "[]", "[ \n]", "{}", "{\t}", "0", "-0", "0.000", "0e5",
        "-0.0E-3",
    ] {
        assert!(is_falsy(fragment), "{fragment:?} is falsy");
    }
    for fragment in
        ["true", "\" \"", "[0]", "{\"a\":1}", "1", "0.01", "-1e-9", "10e0", "\"0\"", "[[]]", ""]
    {
        assert!(!is_falsy(fragment), "{fragment:?} is truthy");
    }
}

/// A `type` written with an escape is still recognized, as the server would
/// read it once parsed; a `type` that is not a string never is.
#[test]
fn an_escaped_type_is_read_as_the_string_it_spells() {
    assert_eq!(string_value(r#""score""#).as_deref(), Some("score"));
    assert_eq!(string_value(r#" "choice" "#).as_deref(), Some("choice"));
    assert_eq!(string_value(r#""""#).as_deref(), Some(""));
    assert_eq!(string_value("1"), None);
    assert_eq!(string_value("[\"score\"]"), None);

    let escaped = spelled(r#""sc\u006fre""#);
    assert_eq!(escaped.as_str(), r#""sc\u006fre""#);
    let raw = RawQuestion::new("noul").field("type", &escaped);
    assert_eq!(
        rejection(Questions::new().raw("rating", raw)),
        "Question \"rating\" requires \"criteria\".",
        "a type set to {escaped} is a score"
    );
}

/// A field that cannot be encoded is reported by `prepare`, with the question
/// and field it belongs to, before any shape check; the codec's message never
/// quotes the value.
#[test]
fn a_raw_field_that_cannot_be_encoded_is_reported_by_prepare() {
    let unencodable = BTreeMap::from([((1_u8, 2_u8), "secret-value")]);
    let raw = RawQuestion::new("choice").field("bad", &unencodable).field("worse", &unencodable);
    let message = rejection(Questions::new().raw("raw", raw));
    #[cfg(feature = "sonic")]
    let expected = "Question \"raw\" field \"bad\": the value could not be encoded as JSON: \
                    Expected the key to be string/bool/number when serializing map, now is tuple";
    #[cfg(not(feature = "sonic"))]
    let expected = "Question \"raw\" field \"bad\": the value could not be encoded as JSON: key must be a string";
    assert_eq!(message, expected);
    assert!(!message.contains("secret-value"));
}

// ------------------------------------------------------------- the set

/// Upstream `test_validation_before_network`, the empty half.
#[test]
fn an_empty_set_is_rejected() {
    assert_eq!(rejection(Questions::new()), "At least one question is required.");
}

/// The first failing question, in caller order, is the one reported.
#[test]
fn the_first_failing_question_is_reported() {
    let questions = Questions::new()
        .noul("fine", Noul::new())
        .raw("second", RawQuestion::new(""))
        .score("third", Score::new(Vec::<&str>::new()));
    assert_eq!(
        rejection(questions),
        "Question \"second\" must be a question object or a dictionary with a nonempty string \"type\"."
    );
}

/// A name given twice replaces the question and keeps its first position, as
/// a Python `dict` does; the same holds for choice options and raw fields.
#[test]
fn a_repeated_name_replaces_in_place() {
    let questions = Questions::new()
        .noul("a", Noul::new().instructions("first"))
        .score("b", Score::new(["x"]))
        .choice(
            "a",
            Choice::new(["calm", "angry", "calm"])
                .option("angry", "hostile")
                .option("calm", "polite"),
        );
    let prepared = questions.prepare().expect("valid");
    assert_eq!(prepared.len(), 2);
    assert_eq!(prepared.names().collect::<Vec<_>>(), ["a", "b"]);
    assert_eq!(
        std::str::from_utf8(prepared.as_bytes()).expect("UTF-8"),
        r#"{"a":{"type":"choice","criteria":{"calm":"polite","angry":"hostile"}},"b":{"type":"score","criteria":["x"]}}"#
    );

    let raw =
        RawQuestion::new("noul").field("weight", 1).field("type", "future").field("weight", 2);
    assert_eq!(text(Questions::new().raw("r", raw)), r#"{"r":{"type":"future","weight":2}}"#);
}

/// Setting a member twice keeps the last value.
#[test]
fn setting_a_member_again_replaces_it() {
    let noul =
        Noul::new().instructions("old").instructions("new").yes("a").yes("b").no("c").no("d");
    assert_eq!(
        text(Questions::new().noul("q", noul)),
        r#"{"q":{"type":"noul","instructions":"new","criteria":{"true":"b","false":"d"}}}"#
    );
    let score = Score::new(["x"]).instructions("old").instructions("new");
    assert_eq!(
        text(Questions::new().score("q", score)),
        r#"{"q":{"type":"score","instructions":"new","criteria":["x"]}}"#
    );
}

/// Every kind reaches the set through `question` as well as through its own
/// method, with the same result.
#[test]
fn question_and_the_kind_methods_are_the_same() {
    let by_kind = Questions::new()
        .noul("n", Noul::new())
        .choice("c", Choice::new(["x"]))
        .score("s", Score::new(["x"]))
        .raw("r", RawQuestion::new("future"));
    let by_question = Questions::new()
        .question("n", Question::Noul(Noul::new()))
        .question("c", Choice::new(["x"]))
        .question("s", Score::new(["x"]))
        .question("r", Question::Raw(RawQuestion::new("future")));
    assert_eq!(by_kind, by_question);
    assert_eq!(by_kind.prepare().expect("valid"), by_question.prepare().expect("valid"));
}

// ------------------------------------------------------------- prepared set

/// Names come back in wire order, and the count matches.
#[test]
fn a_prepared_set_knows_its_names_in_order() {
    let prepared = Questions::new()
        .score("urgency", Score::new(["low"]))
        .noul("", Noul::new())
        .noul("billing", Noul::new())
        .prepare()
        .expect("valid");
    assert_eq!(prepared.len(), 3);
    assert!(!prepared.is_empty());
    assert_eq!(prepared.names().len(), 3);
    assert_eq!(prepared.names().collect::<Vec<_>>(), ["urgency", "", "billing"]);
    assert_eq!(prepared.names().rev().collect::<Vec<_>>(), ["billing", "", "urgency"]);
}

/// A clone shares the bytes rather than copying them, and the set crosses
/// threads.
#[test]
fn a_prepared_set_is_shared_not_copied() {
    const fn assert_send_sync<T: Send + Sync + 'static>() {}
    assert_send_sync::<PreparedQuestions>();

    let prepared =
        Questions::new().noul("q", Noul::new().instructions("Spam?")).prepare().expect("valid");
    let clone = prepared.clone();
    assert_eq!(clone, prepared);
    assert!(
        std::ptr::eq(clone.as_bytes(), prepared.as_bytes()),
        "the clone points at the same bytes"
    );

    let from_thread =
        std::thread::spawn(move || clone.names().map(str::to_owned).collect::<Vec<_>>())
            .join()
            .expect("the thread does not panic");
    assert_eq!(from_thread, ["q"]);
}

/// A prepared set prints its JSON; questions are not secrets.
#[test]
fn debug_shows_the_content() {
    let prepared =
        Questions::new().noul("q", Noul::new().instructions("Spam?")).prepare().expect("valid");
    assert_eq!(
        format!("{prepared:?}"),
        r#"PreparedQuestions { json: "{\"q\":{\"type\":\"noul\",\"instructions\":\"Spam?\"}}" }"#
    );
    let raw = RawQuestion::new("future").field("weight", 3);
    assert_eq!(
        format!("{raw:?}"),
        r#"RawQuestion { fields: [("type", "future"), ("weight", 3)], failure: None }"#
    );
    assert_eq!(
        format!("{:?}", Noul::new().yes("y")),
        r#"Noul { instructions: None, yes: Some(Content { repr: Text("y") }), no: None }"#
    );
}

/// The worst-case bound really is an upper bound: the buffer never grows
/// while it is written, for text made entirely of characters the codec
/// escapes to six bytes.
#[test]
fn the_size_bound_covers_the_worst_case_escape() {
    let worst = "\u{0001}".repeat(512);
    let question = Question::from(
        Choice::new([worst.as_str()])
            .option(worst.as_str(), worst.as_str())
            .instructions(worst.as_str()),
    );
    let mut buf = Vec::with_capacity(bound_of(&question));
    let capacity = buf.capacity();
    write_question(&mut buf, &question);
    assert_eq!(buf.capacity(), capacity, "the buffer grew from {capacity} to {}", buf.capacity());
    assert!(buf.len() > 6 * 512 * 2, "the text was escaped: {} bytes", buf.len());
}

/// Text may be borrowed, owned or either: a `String` built per question and a
/// `Cow` handed through from elsewhere encode exactly like a literal.
#[test]
fn owned_and_borrowed_text_encode_alike() {
    let topic = "billing";
    let owned =
        Noul::new().instructions(format!("Is this about {topic}?")).yes(Cow::Borrowed("payments"));
    let borrowed = Noul::new()
        .instructions("Is this about billing?")
        .yes(Cow::<str>::Owned(String::from("payments")));
    assert_eq!(owned, borrowed);
    assert_eq!(
        text(Questions::new().noul("q", owned)),
        r#"{"q":{"type":"noul","instructions":"Is this about billing?","criteria":{"true":"payments"}}}"#
    );
}

// ------------------------------------------------------------- compiled sets

/// A set laid out as the derive lays one out, built in a `static`: that it
/// compiles is the proof that `from_static` is a `const fn`.
static COMPILED: PreparedQuestions = PreparedQuestions::from_static(
    concat!(r#"{"billing":{"type":"noul"},"":{"type":"noul","instructions":"Spam?"}}"#, "billing"),
    69,
    &[76, 76],
);

/// A compiled set is the set `prepare` builds from the same questions: equal,
/// with the same names, length and printed JSON, whichever way each was made.
#[test]
fn a_compiled_set_equals_the_prepared_one() {
    let prepared = Questions::new()
        .noul("billing", Noul::new())
        .noul("", Noul::new().instructions("Spam?"))
        .prepare()
        .expect("valid");
    assert_eq!(COMPILED, prepared);
    assert_eq!(prepared, COMPILED);
    assert_eq!(COMPILED.len(), 2);
    assert!(!COMPILED.is_empty());
    assert_eq!(COMPILED.names().collect::<Vec<_>>(), ["billing", ""]);
    assert_eq!(COMPILED.names().rev().collect::<Vec<_>>(), ["", "billing"]);
    assert_eq!(
        COMPILED.as_bytes(),
        br#"{"billing":{"type":"noul"},"":{"type":"noul","instructions":"Spam?"}}"#
    );
    assert_eq!(format!("{COMPILED:?}"), format!("{prepared:?}"));
    // A clone of a compiled set still points at the program's bytes.
    let clone = COMPILED.clone();
    assert!(std::ptr::eq(clone.as_bytes(), COMPILED.as_bytes()));
}

/// Two sets with the same bytes but different name boundaries differ.
#[test]
fn compiled_sets_compare_their_name_boundaries() {
    let one_name = PreparedQuestions::from_static(r#"{"ab":{"type":"noul"}}ab"#, 22, &[24]);
    let two_names = PreparedQuestions::from_static(r#"{"ab":{"type":"noul"}}ab"#, 22, &[23, 24]);
    assert_ne!(one_name, two_names);
    assert_eq!(two_names.names().collect::<Vec<_>>(), ["a", "b"]);
}

/// Every layout `from_static` refuses, with its message. In the `static` the
/// derive generates, the same panic stops the build.
#[test]
fn from_static_refuses_a_layout_that_does_not_hold() {
    let cases: [(&'static str, usize, &'static [usize], &str); 7] = [
        ("{}", 2, &[], "a question set has at least one question"),
        ("{}", 3, &[3], "the JSON must end within the buffer"),
        ("{}\u{E9}", 3, &[4], "the JSON must end on a character boundary"),
        ("{}ab", 2, &[4, 3], "a name must not end before it starts"),
        ("{}ab", 2, &[9], "a name must end within the buffer"),
        ("{}\u{E9}", 2, &[3], "a name must end on a character boundary"),
        ("{}ab", 2, &[3], "the last name must end where the buffer does"),
    ];
    for (buf, json_len, name_ends, message) in cases {
        let panic = std::panic::catch_unwind(|| {
            drop(PreparedQuestions::from_static(buf, json_len, name_ends));
        })
        .expect_err("the layout is refused");
        assert_eq!(
            panic.downcast_ref::<&str>().copied(),
            Some(message),
            "{buf:?} {json_len} {name_ends:?}"
        );
    }
}

// ------------------------------------------------------------- asking a set

/// The derive used from inside the crate, where the SDK is `crate` rather
/// than `::typesafe_sdk`: the crate-path override at work.
#[cfg(feature = "macros")]
#[expect(dead_code, reason = "no response is decoded into it here")]
#[derive(typesafe_sdk_rust_macros::QuestionSet)]
#[question_set(crate = crate)]
struct Spam {
    #[noul(instructions = "Spam?")]
    spam: crate::response::NoulAnswer,
}

/// `ask` is `system_one` with the set's questions, typed as the set: the same
/// builder, and the same settings.
#[cfg(all(feature = "macros", feature = "hyper"))]
#[test]
fn ask_is_system_one_with_the_sets_questions() {
    let client = Client::builder().api_key("test-key").build().expect("the client builds");
    let asked = client.ask::<Spam>("state").model("jev-2").header("x-team", "billing");
    let built = client
        .system_one("state", Spam::prepared())
        .typed::<Spam>()
        .model("jev-2")
        .header("x-team", "billing");
    assert_eq!(format!("{asked:?}"), format!("{built:?}"));
    assert_eq!(
        format!("{asked:?}"),
        r#"SystemOne { questions: 1, model: Some("jev-2"), deadline: Client, headers: ["x-team"], extra_body: [], .. }"#
    );
    assert_eq!(
        format!("{:?}", Spam::prepared()),
        r#"PreparedQuestions { json: "{\"spam\":{\"type\":\"noul\",\"instructions\":\"Spam?\"}}" }"#
    );
}
