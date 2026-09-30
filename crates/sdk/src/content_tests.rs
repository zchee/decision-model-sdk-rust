//! Tests for `Content`.

use std::mem;

use serde::Deserialize;
#[cfg(feature = "sonic")]
use serde_json as other_codec;
#[cfg(not(feature = "sonic"))]
use sonic_rs as other_codec;

use super::*;
use crate::DecodeErrorKind;

/// One field of a response, which is where content arrives from the wire.
#[derive(Debug, Deserialize)]
struct Row<'a> {
    #[serde(borrow)]
    value: Content<'a>,
}

fn row(document: &[u8]) -> Result<Row<'_>, DecodeError> {
    codec::decode::<Row<'_>>(document)
}

fn encoded(content: &Content<'_>) -> String {
    let mut buffer = Vec::new();
    codec::encode_into(&mut buffer, content).expect("content always encodes");
    String::from_utf8(buffer).expect("the codec emits UTF-8")
}

#[test]
fn content_stays_small() {
    // One is held per legend entry and per question field, so the size is part
    // of the decode budget. Both shapes fit in the space of a `Cow<str>`: the
    // raw-JSON pointer is narrower, and the discriminant rides in the spare
    // values of the `Cow` tag rather than adding a word.
    assert_eq!(
        mem::size_of::<Content<'_>>(),
        mem::size_of::<Cow<'_, str>>(),
        "Content is {} bytes against {} for a Cow",
        mem::size_of::<Content<'_>>(),
        mem::size_of::<Cow<'_, str>>()
    );
    assert_eq!(mem::size_of::<Content<'_>>(), 24);
}

#[test]
fn a_string_decodes_as_text_and_borrows_the_input() {
    let document = br#"{"value":"neutral or polite"}"#;

    let decoded = row(document).expect("a string is valid content");

    assert_eq!(decoded.value.as_text(), Some("neutral or polite"));
    assert_eq!(decoded.value.as_json(), None);

    let borrowed = decoded.value.as_text().expect("it is text").as_ptr().addr();
    let start = document.as_ptr().addr();
    assert!(
        (start..start + document.len()).contains(&borrowed),
        "text without escapes must point into the response buffer, not a copy"
    );
}

#[test]
fn an_escaped_string_decodes_to_its_value() {
    let document = br#"{"value":"caf\u00e9 \"quoted\" \\ \u2028"}"#;

    let decoded = row(document).expect("escapes are valid content");

    assert_eq!(decoded.value.as_text(), Some("caf\u{e9} \"quoted\" \\ \u{2028}"));
}

#[test]
fn an_object_and_an_array_keep_their_text() {
    let object = br#"{"value":{"label":"ok","weight":2}}"#;
    let array = br#"{"value":["great","best"]}"#;

    let decoded_object = row(object).expect("an object is valid content");
    let decoded_array = row(array).expect("an array is valid content");

    assert_eq!(decoded_object.value.as_text(), None);
    assert_eq!(
        decoded_object.value.as_json().map(RawJson::as_str),
        Some(r#"{"label":"ok","weight":2}"#)
    );
    assert_eq!(decoded_array.value.as_json().map(RawJson::as_str), Some(r#"["great","best"]"#));
}

#[test]
fn every_shape_re_encodes_to_what_it_came_from() {
    for (document, field) in [
        (&br#"{"value":"neutral or polite"}"#[..], r#""neutral or polite""#),
        (&br#"{"value":"caf\u00e9 \"quoted\" \\"}"#[..], "\"caf\u{e9} \\\"quoted\\\" \\\\\""),
        (&br#"{"value":{"label":"ok","weight":2}}"#[..], r#"{"label":"ok","weight":2}"#),
        (&br#"{"value":["great","best"]}"#[..], r#"["great","best"]"#),
    ] {
        let decoded = row(document).expect("valid content");
        assert_eq!(
            encoded(&decoded.value),
            field,
            "round trip of {}",
            String::from_utf8_lossy(document)
        );
    }
}

#[test]
fn a_raw_value_is_spliced_in_rather_than_re_rendered() {
    let document = br#"{"value":{"b":1,"a":[ ],"n":-0.0}}"#;

    let decoded = row(document).expect("an object is valid content");

    assert_eq!(
        encoded(&decoded.value),
        r#"{"b":1,"a":[ ],"n":-0.0}"#,
        "key order, the space and the sign of the zero all survive"
    );
}

#[test]
fn numbers_booleans_and_null_are_refused() {
    for document in [
        &br#"{"value":42}"#[..],
        &br#"{"value":-0.5}"#[..],
        &br#"{"value":true}"#[..],
        &br#"{"value":false}"#[..],
        &br#"{"value":null}"#[..],
    ] {
        let error = row(document).expect_err("only string, object and array are content");
        assert_eq!(
            error.kind(),
            DecodeErrorKind::Data,
            "{} gave {error}",
            String::from_utf8_lossy(document)
        );
        assert_eq!(error.path(), "value");
    }

    assert_eq!(Content::json(&42).expect_err("a number is not content"), ContentError::Shape);
    assert_eq!(Content::json(&true).expect_err("a boolean is not content"), ContentError::Shape);
    assert_eq!(
        Content::json(&Option::<u8>::None).expect_err("null is not content"),
        ContentError::Shape
    );
}

#[test]
fn a_value_that_encodes_to_a_string_is_text() {
    let from_value = Content::json(&"payments or invoices").expect("a string is content");

    assert_eq!(from_value, Content::text("payments or invoices"));
    assert_eq!(from_value.as_text(), Some("payments or invoices"));
}

#[test]
fn a_structured_value_becomes_raw_json() {
    #[derive(Debug, serde::Serialize)]
    struct Criterion {
        label: &'static str,
        weight: u8,
    }

    let content =
        Content::json(&Criterion { label: "ok", weight: 2 }).expect("a struct is content");

    assert_eq!(content.as_json().map(RawJson::as_str), Some(r#"{"label":"ok","weight":2}"#));
    assert_eq!(encoded(&content), r#"{"label":"ok","weight":2}"#);
}

#[test]
fn borrowed_text_can_be_detached_from_its_input() {
    let owned = {
        let document = String::from(r#"{"value":"neutral or polite"}"#);
        let decoded = row(document.as_bytes()).expect("a string is valid content");
        decoded.value.into_owned()
    };

    assert_eq!(owned.as_text(), Some("neutral or polite"));
}

#[test]
fn text_is_escaped_on_the_way_out() {
    let content = Content::text("a \"quoted\" \\ caf\u{e9}");

    assert_eq!(encoded(&content), "\"a \\\"quoted\\\" \\\\ caf\u{e9}\"");
}

// ------------------------------------------------------ through other codecs

/// The shape a question has on the wire: several content fields side by side,
/// which is what a caller serializes when they dump a request or a response
/// with a codec of their own.
#[derive(Debug, serde::Serialize, Deserialize)]
struct Fields<'a> {
    #[serde(borrow)]
    instructions: Content<'a>,
    #[serde(borrow)]
    criteria: Content<'a>,
    #[serde(borrow)]
    label: Content<'a>,
}

#[test]
fn every_shape_round_trips_through_another_codec() {
    for (document, data) in [
        (&br#"{"value":"neutral or polite"}"#[..], serde_json::json!("neutral or polite")),
        (
            &br#"{"value":{"label":"ok","weight":2,"sub":{"deep":[1,2]}}}"#[..],
            serde_json::json!({"label": "ok", "weight": 2, "sub": {"deep": [1, 2]}}),
        ),
        (
            &br#"{"value":["great","best",{"n":3}]}"#[..],
            serde_json::json!(["great", "best", {"n": 3}]),
        ),
        (
            &br#"{"value":"caf\u00e9 \"quoted\" \\"}"#[..],
            serde_json::json!("caf\u{e9} \"quoted\" \\"),
        ),
    ] {
        let decoded = row(document).expect("valid content");

        let written = other_codec::to_string(&decoded.value).expect("another codec writes it");
        assert!(!written.contains(codec::backend::SPLICE_TOKEN), "token leaked: {written}");
        assert_eq!(
            other_codec::from_str::<serde_json::Value>(&written).expect("the output is JSON"),
            data,
            "from {}",
            String::from_utf8_lossy(document)
        );

        let read_back: Content<'_> =
            other_codec::from_str(&written).expect("another codec reads it back");
        assert_eq!(
            serde_json::to_value(&read_back).expect("it is JSON"),
            data,
            "round trip of {}",
            String::from_utf8_lossy(document)
        );
    }
}

#[test]
fn a_struct_of_content_fields_carries_the_same_data_through_both_codecs() {
    let document = br#"{
        "instructions": "rank the replies",
        "criteria": {"tone": ["warm", "brief"], "length": {"max": 40}},
        "label": ["a", "b"]
    }"#;
    let fields: Fields<'_> = codec::decode(document).expect("the question decodes");

    let mut buffer = Vec::new();
    codec::encode_into(&mut buffer, &fields).expect("this codec writes it");
    let ours = String::from_utf8(buffer).expect("the codec emits UTF-8");
    let theirs = other_codec::to_string(&fields).expect("another codec writes it");

    assert!(!theirs.contains(codec::backend::SPLICE_TOKEN), "token leaked: {theirs}");
    assert_eq!(
        other_codec::from_str::<serde_json::Value>(&ours).expect("our output is JSON"),
        other_codec::from_str::<serde_json::Value>(&theirs).expect("theirs is too"),
        "this codec wrote {ours}, the other wrote {theirs}"
    );
    // The difference between them is spelling, not data: this codec splices
    // the wire text back in, spaces and all.
    assert!(ours.contains(r#"{"tone": ["warm", "brief"], "length": {"max": 40}}"#), "{ours}");
    assert!(theirs.contains(r#"{"tone":["warm","brief"],"length":{"max":40}}"#), "{theirs}");
}

#[test]
fn text_that_is_not_one_json_value_never_becomes_content() {
    use serde::de::value::{BorrowedStrDeserializer, Error as ValueError};

    // A deserializer that answers the raw-text request with a string of the
    // caller's own: spliced into a request body, the first of these would put
    // a key of their choosing next to the field that holds it.
    #[cfg(feature = "sonic")]
    let empty_error = "invalid JSON syntax at line 1 column 1";
    #[cfg(not(feature = "sonic"))]
    let empty_error = "invalid JSON syntax at line 1 column 0";
    for (text, expected) in [
        (r#"{"a":1}, "model": "evil""#, "invalid JSON syntax at line 1 column 8"),
        ("{not json", "invalid JSON syntax at line 1 column 2"),
        ("hello", "invalid JSON syntax at line 1 column 1"),
        ("", empty_error),
    ] {
        let refused = <Content<'_> as Deserialize>::deserialize(BorrowedStrDeserializer::<
            ValueError,
        >::new(text));

        let error = refused.expect_err(text).to_string();
        assert!(error.contains(expected), "{text:?} was refused as {error}");
        assert!(!error.contains("evil"), "the refusal quoted the input: {error}");
        assert!(!error.contains("not json"), "the refusal quoted the input: {error}");
    }

    let accepted = <Content<'_> as Deserialize>::deserialize(
        BorrowedStrDeserializer::<ValueError>::new(r#"{"a":1}"#),
    )
    .expect("one complete value is content");
    assert_eq!(accepted.as_json().map(RawJson::as_str), Some(r#"{"a":1}"#));
}

/// Each conversion builds the text shape, equal to `Content::text` of the
/// same text, and a borrowed input stays borrowed.
#[test]
fn text_converts_into_the_text_shape_and_keeps_its_borrow() {
    let source = String::from("payments or invoices");

    let from_str = Content::from(source.as_str());
    let from_string = Content::from(source.clone());
    let from_borrowed = Content::from(Cow::Borrowed(source.as_str()));
    let from_owned = Content::from(Cow::<str>::Owned(source.clone()));

    for (case, content) in [
        ("&str", &from_str),
        ("String", &from_string),
        ("Cow::Borrowed", &from_borrowed),
        ("Cow::Owned", &from_owned),
    ] {
        assert_eq!(*content, Content::text(source.as_str()), "{case}");
        assert_eq!(content.as_text(), Some("payments or invoices"), "{case}");
        assert_eq!(content.as_json(), None, "{case}");
    }

    for (case, content) in [("&str", &from_str), ("Cow::Borrowed", &from_borrowed)] {
        assert!(
            matches!(content.repr, Repr::Text(Cow::Borrowed(_))),
            "{case} stays borrowed: {content:?}"
        );
        let text = content.as_text().expect("text");
        assert!(std::ptr::eq(text, source.as_str()), "{case} points into the source");
    }
    for (case, content) in [("String", &from_string), ("Cow::Owned", &from_owned)] {
        assert!(matches!(content.repr, Repr::Text(Cow::Owned(_))), "{case} is owned: {content:?}");
    }
}

/// A converted `String` that needs escaping comes back as the same string
/// through this codec and through `serde_json`.
#[test]
fn a_converted_string_round_trips_through_both_codecs() {
    let text = String::from("quote \" backslash \\ newline \n tab \t nul \u{0000} earth \u{1F30D}");
    let content = Content::from(text.clone());

    let ours = encoded(&content);
    assert_eq!(
        ours,
        r#""quote \" backslash \\ newline \n tab \t nul \u0000 earth "#.to_owned() + "\u{1F30D}\""
    );
    assert_eq!(codec::decode::<String>(ours.as_bytes()).expect("decodes"), text);

    let theirs = other_codec::to_string(&content).expect("serde_json encodes text");
    assert_eq!(other_codec::from_str::<String>(&theirs).expect("serde_json decodes"), text);
    assert_eq!(
        other_codec::from_str::<Content<'_>>(&theirs).expect("reads back as content"),
        content
    );
}
