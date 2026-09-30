//! The compile-time JSON: the escaper against both SDK backends, and whole
//! question sets against the bytes the runtime builder writes.
//!
//! The expected question-set strings are the runtime builder's output for the
//! same questions, copied from `crates/sdk/src/question_tests.rs` where a test
//! there pins them; the main crate's `crates/sdk/tests/derive.rs` compares the
//! two directly.

use proc_macro2::Span;
use proptest::prelude::*;
use syn::Ident;

use super::*;

/// What sonic-rs writes for `text`.
fn sonic(text: &str) -> String {
    sonic_rs::to_string(text).expect("a string always encodes")
}

fn escaped(text: &str) -> String {
    let mut out = String::new();
    push_string(&mut out, text);
    out
}

fn field(name: &str, question: Question) -> QuestionField {
    QuestionField {
        member: Ident::new("field", Span::call_site()),
        name: name.to_owned(),
        question,
        ty_span: Span::call_site(),
    }
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

/// Characters that exercise every branch of the escaper, and the multi-byte
/// encodings on each side of a UTF-8 length boundary.
fn tricky_char() -> impl Strategy<Value = char> {
    prop_oneof![
        Just('"'),
        Just('\\'),
        Just('/'),
        (0_u32..0x20).prop_map(|code| char::from_u32(code).expect("below U+0020 is a char")),
        Just('\u{7F}'),
        Just('\u{80}'),
        Just('\u{7FF}'),
        Just('\u{800}'),
        Just('\u{2028}'),
        Just('\u{2029}'),
        Just('\u{FEFF}'),
        Just('\u{FFFF}'),
        Just('\u{10000}'),
        Just('\u{1F30D}'),
        Just('\u{10FFFF}'),
        any::<char>(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    /// Any string Rust can hold (a `String` has no lone surrogates) escapes
    /// to exactly what the codec writes.
    #[test]
    fn the_escaper_matches_the_codec_on_arbitrary_strings(text in any::<String>()) {
        let encoded = escaped(&text);
        prop_assert_eq!(&encoded, &sonic(&text), "sonic-rs input {:?}", text);
        prop_assert_eq!(&encoded, &serde_json::to_string(&text).expect("a string encodes"), "serde_json input {:?}", text);
    }

    /// Strings made mostly of quotes, backslashes, control characters and
    /// non-BMP characters, where an escaper would go wrong.
    #[test]
    fn the_escaper_matches_the_codec_on_escape_heavy_strings(
        text in proptest::collection::vec(tricky_char(), 0..64)
            .prop_map(|chars| chars.into_iter().collect::<String>())
    ) {
        let encoded = escaped(&text);
        prop_assert_eq!(&encoded, &sonic(&text), "sonic-rs input {:?}", text);
        prop_assert_eq!(&encoded, &serde_json::to_string(&text).expect("a string encodes"), "serde_json input {:?}", text);
    }
}

/// Every ASCII character on its own, so no single escape is left to chance.
#[test]
fn every_ascii_character_escapes_as_the_codec_does() {
    for code in 0_u8..=0x7F {
        let text = char::from(code).to_string();
        assert_eq!(escaped(&text), sonic(&text), "U+{code:04X}");
        assert_eq!(
            escaped(&text),
            serde_json::to_string(&text).expect("a string encodes"),
            "U+{code:04X}"
        );
    }
}

#[test]
fn the_escapes_are_the_short_ones_where_json_has_them() {
    assert_eq!(
        escaped("\"\\/\u{08}\u{0C}\n\r\t\u{00}\u{1F}\u{7F}\u{1F30D}"),
        "\"\\\"\\\\/\\b\\f\\n\\r\\t\\u0000\\u001f\u{7F}\u{1F30D}\""
    );
    assert_eq!(escaped(""), "\"\"");
    for text in ["\u{7F}", "\u{2028}"] {
        assert_eq!(escaped(text), sonic(text), "sonic-rs {text:?}");
        assert_eq!(
            escaped(text),
            serde_json::to_string(text).expect("a string encodes"),
            "serde_json {text:?}"
        );
    }
}

/// The three questions of the SDK's example set, with every member used.
#[test]
fn a_set_is_the_runtime_bytes_then_the_names() {
    let fields = [
        field(
            "billing",
            Question::Noul {
                instructions: text("Is this about billing?"),
                yes: text("payments or invoices"),
                no: None,
            },
        ),
        field(
            "tone",
            Question::Choice {
                instructions: text("What is the tone?"),
                options: vec![
                    ("calm".to_owned(), text("neutral or polite")),
                    ("angry".to_owned(), None),
                ],
            },
        ),
        field(
            "urgency",
            Question::Score {
                instructions: text("How urgent?"),
                levels: vec!["can wait".to_owned(), "this week".to_owned(), "today".to_owned()],
            },
        ),
    ];
    let json = concat!(
        r#"{"billing":{"type":"noul","instructions":"Is this about billing?","criteria":{"true":"payments or invoices"}},"#,
        r#""tone":{"type":"choice","instructions":"What is the tone?","criteria":{"calm":"neutral or polite","angry":null}},"#,
        r#""urgency":{"type":"score","instructions":"How urgent?","criteria":["can wait","this week","today"]}}"#,
    );
    let prepared = prepare(&fields);
    assert_eq!(
        prepared,
        Prepared {
            buf: format!("{json}billingtoneurgency"),
            json_len: json.len(),
            name_ends: vec![json.len() + 7, json.len() + 11, json.len() + 18],
        }
    );
}

/// A noul's criteria: absent when neither outcome is described, `true`
/// before `false` otherwise. Pinned by the runtime's
/// `the_prepared_bytes_are_compact_and_in_caller_order` and
/// `noul_criteria_carry_either_outcome_or_both`.
#[test]
fn noul_criteria_are_written_as_the_runtime_writes_them() {
    let cases = [
        (None, None, r#"{"q":{"type":"noul"}}"#),
        (text("Y"), None, r#"{"q":{"type":"noul","criteria":{"true":"Y"}}}"#),
        (None, text("N"), r#"{"q":{"type":"noul","criteria":{"false":"N"}}}"#),
        (text("Y"), text("N"), r#"{"q":{"type":"noul","criteria":{"true":"Y","false":"N"}}}"#),
    ];
    for (yes, no, expected) in cases {
        let prepared = prepare(&[field("q", Question::Noul { instructions: None, yes, no })]);
        assert_eq!(&prepared.buf[..prepared.json_len], expected);
    }
}

/// A choice without options is sent as `{}` criteria, as `Choice::new([])`
/// is: the runtime does not reject it, so neither does the derive.
#[test]
fn a_choice_without_options_has_empty_criteria() {
    let prepared = prepare(&[field("q", Question::Choice { instructions: None, options: vec![] })]);
    assert_eq!(prepared.buf, r#"{"q":{"type":"choice","criteria":{}}}q"#);
}

/// Names and text are escaped on the wire and copied raw after the JSON,
/// with the empty name included. Pinned by the runtime's
/// `names_and_text_are_escaped_on_the_wire_and_not_in_names`.
#[test]
fn names_are_escaped_in_the_json_and_raw_after_it() {
    let name = "say \"hi\"\\\n\u{1F30D}";
    let prepared = prepare(&[
        field(
            name,
            Question::Choice {
                instructions: text("line\nbreak \u{0000} end"),
                options: vec![("tab\there".to_owned(), None)],
            },
        ),
        field("", Question::Noul { instructions: None, yes: None, no: None }),
    ]);
    let json = concat!(
        r#"{"say \"hi\"\\\n"#,
        "\u{1F30D}",
        r#"":{"type":"choice","instructions":"line\nbreak \u0000 end","criteria":{"tab\there":null}},"":{"type":"noul"}}"#,
    );
    assert_eq!(&prepared.buf[..prepared.json_len], json);
    assert_eq!(&prepared.buf[prepared.json_len..], name);
    assert_eq!(prepared.name_ends, [json.len() + name.len(), json.len() + name.len()]);
}
