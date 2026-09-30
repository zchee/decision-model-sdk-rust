//! Tests for the codec.
#![expect(
    dead_code,
    reason = "the fixture types exist to be decoded into; their fields are never read back"
)]

use std::{
    collections::BTreeMap,
    panic::{self, AssertUnwindSafe},
    thread,
};

use proptest::prelude::*;
use serde::{Deserialize, de::IgnoredAny};
#[cfg(feature = "sonic")]
use serde_json as other_codec;
#[cfg(not(feature = "sonic"))]
use sonic_rs as other_codec;

use super::*;
use crate::rendering_tests::assert_printable;

#[test]
fn the_backend_is_the_one_the_sonic_feature_names() {
    #[cfg(feature = "sonic")]
    assert_eq!(backend::SPLICE_TOKEN, "$sonic_rs::LazyValue");
    #[cfg(not(feature = "sonic"))]
    assert_eq!(backend::SPLICE_TOKEN, "$serde_json::private::RawValue");
}

#[test]
fn map_keys_the_doc_lists_encode() {
    struct FloatKey(f64);
    impl Serialize for FloatKey {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(&self.0, &1)?;
            map.end()
        }
    }
    assert_eq!(sdk_encoded(&BTreeMap::from([(true, 1)])), r#"{"true":1}"#);
    assert_eq!(sdk_encoded(&FloatKey(0.5)), r#"{"0.5":1}"#);
    let error = RawJson::from_value(&FloatKey(f64::NAN)).expect_err("a NaN key is not JSON");
    #[cfg(feature = "sonic")]
    assert!(error.message().contains("NaN or Infinite f64"), "{error}");
    #[cfg(not(feature = "sonic"))]
    assert_eq!(error.message(), "float key must be finite (got NaN or +/-inf)");
}

#[test]
fn raw_json_reads_back_through_from_reader_from_value_and_a_token_keyed_map() {
    use serde::de::value::{Error as ValueError, MapDeserializer};

    let document = r#"{"label":"ok","list":[1,2]}"#;
    let expected: RawJson = serde_json::from_str(document).expect("a caller reads the JSON");
    let value: serde_json::Value = serde_json::from_str(document).expect("valid JSON");
    let from_reader: RawJson =
        serde_json::from_reader(document.as_bytes()).expect("owned raw text");
    let from_value: RawJson = serde_json::from_value(value.clone()).expect("owned value");
    let borrowed_value = RawJson::deserialize(&value).expect("borrowed value");
    for (case, raw) in [("reader", from_reader), ("value", from_value), ("&Value", borrowed_value)]
    {
        assert_eq!(raw, expected, "{case}");
    }

    #[cfg(not(feature = "sonic"))]
    {
        use serde::de::value::StringDeserializer;

        let case = "owned raw text keeps its buffer";
        let text = r#"{"a":1}"#;
        let owned = String::from(text);
        let pointer = owned.as_ptr();
        let length = owned.len();
        let raw = RawJson::deserialize(StringDeserializer::<ValueError>::new(owned))
            .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(raw.as_str(), text, "{case}");
        assert_eq!(raw.as_str().len(), length, "{case}");
        assert_eq!(raw.as_str().as_ptr(), pointer, "{case}");
    }
    #[cfg(not(feature = "sonic"))]
    {
        let case = "owned token value keeps its buffer inside the SDK";
        let text = r#"{"a":1}"#;
        let owned = String::from(text);
        let pointer = owned.as_ptr();
        let length = owned.len();
        // Only the SDK decode path keeps token text without rendering it again.
        let _inside = DecoderMark::enter();
        let map = MapDeserializer::<_, ValueError>::new([(SPLICE_TOKEN, owned)].into_iter());
        let raw = RawJson::deserialize(map).unwrap_or_else(|error| panic!("{case}: {error}"));
        assert_eq!(raw.as_str(), text, "{case}");
        assert_eq!(raw.as_str().len(), length, "{case}");
        assert_eq!(raw.as_str().as_ptr(), pointer, "{case}");
    }

    let injected = r#"{"a":1},"x":2"#;
    let map =
        MapDeserializer::<_, ValueError>::new([(backend::SPLICE_TOKEN, injected)].into_iter());
    let result = RawJson::deserialize(map);
    #[cfg(feature = "sonic")]
    {
        let rendered = result.expect("an unknown raw token is an ordinary map");
        let expected = serde_json::to_string(&BTreeMap::from([(backend::SPLICE_TOKEN, injected)]))
            .expect("the reference writes the map as data");
        assert_eq!(rendered.as_str(), expected);
    }
    #[cfg(not(feature = "sonic"))]
    {
        let error = result.expect_err("a raw token's text is validated").to_string();
        assert!(error.contains("invalid JSON syntax"), "{error}");
        assert!(!error.contains("\"x\""), "the input is not copied into {error}");
    }
    let empty = MapDeserializer::<_, ValueError>::new(std::iter::empty::<(&str, &str)>());
    assert_eq!(RawJson::deserialize(empty).expect("an empty map renders").as_str(), "{}");
}

#[test]
fn a_padded_document_is_verbatim_inside_the_sdk_and_re_rendered_through_another_codec() {
    let arbitrary = std::env::var("TYPESAFE_SDK_TEST_ARBITRARY_PRECISION").as_deref() == Ok("1");
    let probe = serde_json::to_string(
        &serde_json::from_str::<serde_json::Value>("1E2").expect("the feature probe parses"),
    )
    .expect("the feature probe serializes");
    assert_eq!(
        probe,
        if arbitrary { "1e+2" } else { "100.0" },
        "the feature probe must match the test environment"
    );
    let cases = [
        ("{ \"a\" : 1 }", r#"{"a":1}"#, r#"{"a":1}"#),
        (r#"{"n":1E2,"h":0.50}"#, r#"{"n":100.0,"h":0.5}"#, r#"{"n":1e+2,"h":0.50}"#),
        (r#"{"z":-0}"#, r#"{"z":-0.0}"#, r#"{"z":0}"#),
    ];
    for (document, ordinary, precise) in cases {
        assert_eq!(decode::<RawJson>(document.as_bytes()).expect("SDK decode").as_str(), document);
        assert_eq!(
            decode_seed(document.as_bytes(), PhantomData::<RawJson>)
                .expect("seeded SDK decode")
                .as_str(),
            document
        );
        let reloaded: RawJson = serde_json::from_str(document).expect("caller reload");
        assert_eq!(reloaded.as_str(), if arbitrary { precise } else { ordinary }, "{document}");
    }
}

#[test]
fn a_number_token_map_is_read_as_its_number() {
    use serde::de::value::{Error as ValueError, MapDeserializer};

    for text in ["0.5", "12345678901234567890123"] {
        let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, text)].into_iter());
        assert_eq!(RawJson::deserialize(map).expect("a number token renders").as_str(), text);
    }
    for text in ["1,\"x\":2", "1 "] {
        let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, text)].into_iter());
        let error = RawJson::deserialize(map).expect_err("not one number").to_string();
        assert!(error.contains("invalid JSON"), "{text:?}: {error}");
    }
    let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, "1"), ("x", "2")].into_iter());
    assert_eq!(
        RawJson::deserialize(map).expect_err("an extra entry is refused").to_string(),
        "a JSON number token must be the only entry"
    );
    let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, 1_u64)].into_iter());
    let error =
        RawJson::deserialize(map).expect_err("the token value must be a string").to_string();
    assert!(error.contains("expected a string"), "{error}");
    let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, "0.5")].into_iter());
    let error = crate::response::Answer::deserialize(map).expect_err("a number is not an answer");
    assert_eq!(error.to_string(), "missing field `type`");

    #[cfg(not(feature = "sonic"))]
    for (text, expected) in [
        ("0.5", Some("0.5")),
        ("12345678901234567890123", Some("1.2345678901234568e+22")),
        ("1,\"x\":2", None),
        ("1e+400", None),
    ] {
        let map = MapDeserializer::<_, ValueError>::new([(NUMBER_TOKEN, text)].into_iter());
        let mut output = Vec::new();
        let result = Transcoder::new(map).serialize(&mut other_codec::Serializer::new(&mut output));
        match expected {
            Some(expected) => {
                result.unwrap_or_else(|error| panic!("{text}: {error}"));
                assert_eq!(output, expected.as_bytes(), "{text}");
            }
            None => {
                let error = result.expect_err("the number cannot be transcoded").to_string();
                assert!(
                    error.contains("the stored JSON text could not be read back"),
                    "{text}: {error}"
                );
            }
        }
    }
}

#[test]
fn encode_writes_the_bytes_the_other_engine_writes() {
    #[derive(Serialize)]
    struct Value {
        text: &'static str,
        tenth: f64,
        exponent: f64,
        negative_zero: f64,
        largest: u64,
        nested: BTreeMap<&'static str, [u8; 2]>,
    }
    let value = Value {
        text: "\u{1b}\u{7f}\u{2028}",
        tenth: 0.1,
        exponent: 1e22,
        negative_zero: -0.0,
        largest: u64::MAX,
        nested: BTreeMap::from([("items", [1, 2])]),
    };
    let mut output = Vec::new();
    encode_into(&mut output, &value).expect("the SDK writes JSON");
    assert_eq!(output, other_codec::to_vec(&value).expect("the reference writes JSON"));
    let text = std::str::from_utf8(&output).expect("JSON is UTF-8");
    assert!(text.contains(r#""exponent":1e+22"#), "{text}");
}

// ------------------------------------------------------------- fixtures

/// A response-shaped target, cut down to what the field-path tests need.
#[derive(Debug, Deserialize)]
struct Envelope {
    answers: Answers,
}

#[derive(Debug, Deserialize)]
struct Answers {
    spam: Noul,
    tone: Tone,
}

#[derive(Debug, Deserialize)]
struct Noul {
    noul: f64,
}

#[derive(Debug, Deserialize)]
struct Tone {
    confidence: f64,
}

#[derive(Debug, Deserialize)]
struct ModelList {
    models: Vec<Model>,
}

#[derive(Debug, Deserialize)]
struct Model {
    name: String,
}

/// A target that ignores everything but one field, so that the value of the
/// other field goes through the codec's skip path.
#[derive(Debug, Deserialize)]
struct Known {
    known: u32,
}

/// `[[[ ... ]]]`, `depth` levels deep.
fn nested_array(depth: usize) -> String {
    let mut text = String::with_capacity(depth * 2);
    text.push_str(&"[".repeat(depth));
    text.push_str(&"]".repeat(depth));
    text
}

/// Runs `body` on a thread with the 2 MiB stack a Tokio worker gets.
///
/// The depth tests have to hold on the stack the SDK actually runs on, and in
/// an unoptimized build the codec's skip path costs tens of kilobytes per
/// nesting level. A test that only passed on the main thread, which has 8 MiB,
/// would not prove anything about the worker threads.
fn on_worker_stack<F, T>(body: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(body)
        .expect("invariant: the test thread can be spawned")
        .join()
        .expect("the decode must finish on a 2 MiB stack instead of overflowing it")
}

// ----------------------------------------------------------- depth guard

#[test]
fn depth_at_the_limit_is_accepted_and_one_more_is_not() {
    let at_limit = nested_array(MAX_JSON_DEPTH);
    let over_limit = nested_array(MAX_JSON_DEPTH + 1);

    check_depth(at_limit.as_bytes()).expect("a document at the limit is accepted");
    decode::<IgnoredAny>(at_limit.as_bytes()).expect("a document at the limit decodes");

    let error = check_depth(over_limit.as_bytes()).expect_err("one level deeper is rejected");
    assert_eq!(error.kind(), DecodeErrorKind::TooDeep, "kind of {error}");
    assert_eq!(
        decode::<IgnoredAny>(over_limit.as_bytes()).expect_err("decoding rejects it too").kind(),
        DecodeErrorKind::TooDeep
    );
}

#[test]
fn a_hundred_thousand_levels_are_rejected_before_the_parser_sees_them() {
    // The parser has no recursion limit and aborts the process on input like
    // this, so reaching the assertion at all is the result being tested.
    let bare = nested_array(100_000);
    let in_a_field = format!(r#"{{"legend":{bare}}}"#);

    let kinds = on_worker_stack(move || {
        [
            decode::<IgnoredAny>(bare.as_bytes())
                .expect_err("a 100,000 level array is rejected")
                .kind(),
            decode::<IgnoredAny>(in_a_field.as_bytes())
                .expect_err("the same nested in an object field is rejected")
                .kind(),
        ]
    });

    assert_eq!(kinds, [DecodeErrorKind::TooDeep, DecodeErrorKind::TooDeep]);
}

#[test]
fn the_skip_path_holds_at_the_depth_limit_on_a_worker_stack() {
    // Depth 16 counting the enclosing object, so the ignored value is 15 deep.
    let document = format!(r#"{{"unknown":{},"known":7}}"#, nested_array(MAX_JSON_DEPTH - 1));

    let known = on_worker_stack(move || {
        decode::<Known>(document.as_bytes())
            .expect("an unknown field at the depth limit is skipped")
            .known
    });

    assert_eq!(known, 7);
}

#[test]
fn brackets_and_escapes_inside_strings_do_not_count_as_nesting() {
    let brackets = "[".repeat(MAX_JSON_DEPTH * 4) + &"{".repeat(MAX_JSON_DEPTH * 4);
    // `\"` ends no string and `\\` escapes only itself, so the `[` after
    // either of them is still inside the string value.
    let document = format!(r#"{{"a":"{brackets}","b":"\"[[[[","c":"\\","d":"\\\"[[[["}}"#);

    check_depth(document.as_bytes()).expect("only the enclosing object nests");

    let decoded = decode::<serde_json::Value>(document.as_bytes())
        .expect("the document is valid JSON at depth 2");
    assert_eq!(decoded["b"], "\"[[[[", "the escaped quote is part of the value: {decoded}");
    assert_eq!(decoded["c"], "\\", "the escaped backslash decodes to one");
    assert_eq!(decoded["d"], "\\\"[[[[");
}

#[test]
fn an_unterminated_string_cannot_hide_a_bracket_run() {
    // Everything after the opening quote is string content, so the run of
    // brackets never counts and the pre-scan hands the input to the parser,
    // which rejects it as truncated.
    let document = format!(r#"{{"a":"{}"#, "[".repeat(MAX_JSON_DEPTH * 10));

    check_depth(document.as_bytes()).expect("nothing inside the string nests");
    let error = decode::<IgnoredAny>(document.as_bytes()).expect_err("the input is truncated");
    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "kind of {error}");
}

// ------------------------------------------------------------ field paths

#[test]
fn a_decode_error_never_carries_the_input() {
    // A value the caller would not want in a log line. The `state` of a real
    // call is application data, and both the codec's own error text and
    // serde's type-mismatch messages quote what they were given.
    const SECRET: &str = "pentachlorophenol-42";

    let wrong_type = format!(r#"{{"answers":{{"spam":{{"noul":"{SECRET}"}}}}}}"#);
    let truncated = format!(r#"{{"answers":{{"spam":{{"noul":"{SECRET}"#);
    let deep = format!("{}{SECRET}", "[".repeat(MAX_JSON_DEPTH + 1));

    for document in [wrong_type, truncated, deep] {
        let error = decode::<Envelope>(document.as_bytes())
            .expect_err("every one of these documents fails");
        let rendered = error.to_string();
        let debugged = format!("{error:?}");

        assert!(!rendered.contains(SECRET), "Display leaked the input: {rendered}");
        assert!(!debugged.contains(SECRET), "Debug leaked the input: {debugged}");
    }
}

// ------------------------------------------------- keys the input chose

/// Answers keyed by a name the document chose, as a response carries them.
#[derive(Debug, Deserialize)]
struct Keyed {
    answers: BTreeMap<String, Noul>,
}

/// Three levels of names the document chose.
#[derive(Debug, Deserialize)]
struct DeeplyKeyed {
    answers: BTreeMap<String, BTreeMap<String, BTreeMap<String, Noul>>>,
}

/// `{"answers":{<key>:{}}}` with `key` written as a JSON string, escapes and
/// all, so that no raw control character has to appear in this file.
fn keyed_document(key: &str) -> String {
    let key = other_codec::to_string(key).expect("a string encodes");
    format!(r#"{{"answers":{{{key}:{{}}}}}}"#)
}

/// The path `decode` reports for a document keyed by `key`.
fn keyed_path(key: &str) -> String {
    let document = keyed_document(key);
    let error = decode::<Keyed>(document.as_bytes()).expect_err("the answer has no `noul`");
    assert_eq!(error.kind(), DecodeErrorKind::Data, "kind of {error}");
    error.path().to_owned()
}

#[test]
fn a_key_the_document_chose_is_kept_in_the_path() {
    // Which answer failed is what a caller needs from the path, so a plain
    // key is echoed exactly as the Python SDK echoes it.
    assert_eq!(keyed_path("SECRETH"), "answers.SECRETH.noul");
    // A printable non-ASCII name is kept as it is: "quality" in Japanese.
    assert_eq!(keyed_path("\u{54c1}\u{8cea}"), "answers.\u{54c1}\u{8cea}.noul");
    assert_eq!(keyed_path("caf\u{e9} \u{20bb7}"), "answers.caf\u{e9} \u{20bb7}.noul");
    // A name of 100 characters is below the cap and survives intact.
    let long_name = "q".repeat(100);
    assert_eq!(keyed_path(&long_name), format!("answers.{long_name}.noul"));
    // An empty key still gets its dot, as `serde_path_to_error` prints it.
    assert_eq!(keyed_path(""), "answers..noul");
}

#[test]
fn control_and_format_characters_in_a_key_are_escaped() {
    let rows = [
        ("a\nb\u{1b}[31mc", r"answers.a\nb\u{1b}[31mc.noul"),
        ("tab\there\rcr", r"answers.tab\there\rcr.noul"),
        ("nul\u{0}del\u{7f}nel\u{85}", r"answers.nul\u{0}del\u{7f}nel\u{85}.noul"),
        ("safe\u{202e}lmth.exe", r"answers.safe\u{202e}lmth.exe.noul"),
        ("\u{2066}iso\u{2069}", r"answers.\u{2066}iso\u{2069}.noul"),
        ("zero\u{200b}width\u{200f}", r"answers.zero\u{200b}width\u{200f}.noul"),
        ("\u{feff}bom", r"answers.\u{feff}bom.noul"),
        ("line\u{2028}para\u{2029}", r"answers.line\u{2028}para\u{2029}.noul"),
        ("tag\u{e0041}", r"answers.tag\u{e0041}.noul"),
    ];

    for (key, expected) in rows {
        let document = keyed_document(key);
        let error = decode::<Keyed>(document.as_bytes()).expect_err("the answer has no `noul`");

        assert_eq!(error.path(), expected, "key {key:?}");
        assert_eq!(
            error.to_string(),
            format!("unexpected JSON value at `{expected}`, line 1 column {}", error.column()),
            "key {key:?}"
        );
        assert_printable(&error.to_string());
        assert_printable(&format!("{error:?}"));
    }
}

#[test]
fn a_key_spelling_an_escape_renders_apart_from_a_key_holding_the_character() {
    // Six characters of text - backslash, `u`, `{`, `1`, `b`, `}` - against
    // one real ESC. The backslash is escaped, so the two render apart and a
    // reader can tell which key the server sent.
    let spelled = keyed_path(r"\u{1b}");
    let real = keyed_path("\u{1b}");

    assert_eq!(spelled, r"answers.\\u{1b}.noul");
    assert_eq!(real, r"answers.\u{1b}.noul");
    assert_ne!(spelled, real);

    // Every backslash is doubled, a trailing one included, and it counts as
    // the two characters it renders as.
    assert_eq!(keyed_path(r"a\b\"), r"answers.a\\b\\.noul");
    let before = "k".repeat(MAX_PATH_SEGMENT_CHARS - 1);
    assert_eq!(keyed_path(&format!(r"{before}\")), format!("answers.{before}\u{2026}.noul"));
    let room = "k".repeat(MAX_PATH_SEGMENT_CHARS - 2);
    assert_eq!(keyed_path(&format!(r"{room}\")), format!(r"answers.{room}\\.noul"));
}

#[test]
fn a_long_key_is_cut_at_the_segment_cap_without_splitting_a_character() {
    let cut = |kept: &str| format!("answers.{kept}\u{2026}.noul");

    // 100 KB of key gives a path of 142 characters.
    let huge = "k".repeat(100_000);
    let path = keyed_path(&huge);
    assert_eq!(path, cut(&"k".repeat(MAX_PATH_SEGMENT_CHARS)));
    assert_eq!(path.chars().count(), "answers.".len() + MAX_PATH_SEGMENT_CHARS + 1 + ".noul".len());
    let error = decode::<Keyed>(keyed_document(&huge).as_bytes()).expect_err("no `noul`");
    assert!(error.to_string().len() < 200, "{} bytes: {error}", error.to_string().len());

    // Exactly at the cap is not cut; one more character is.
    let at_cap = "k".repeat(MAX_PATH_SEGMENT_CHARS);
    assert_eq!(keyed_path(&at_cap), format!("answers.{at_cap}.noul"));
    assert_eq!(keyed_path(&format!("{at_cap}k")), cut(&at_cap));

    // The cap counts characters, not bytes: three bytes each here.
    let wide = "\u{65e5}".repeat(MAX_PATH_SEGMENT_CHARS + 50);
    assert_eq!(keyed_path(&wide), cut(&"\u{65e5}".repeat(MAX_PATH_SEGMENT_CHARS)));

    // It counts the escaped text, and an escape that would cross it is left
    // out whole rather than cut in half.
    let before = "k".repeat(MAX_PATH_SEGMENT_CHARS - 3);
    assert_eq!(keyed_path(&format!("{before}\u{1b}tail")), cut(&before));
    let room = "k".repeat(MAX_PATH_SEGMENT_CHARS - 6);
    assert_eq!(keyed_path(&format!("{room}\u{1b}")), format!(r"answers.{room}\u{{1b}}.noul"));
}

#[test]
fn a_path_of_many_long_keys_is_cut_at_the_path_cap() {
    let key = |letter: &str| letter.repeat(200);
    let document =
        format!(r#"{{"answers":{{"{}":{{"{}":{{"{}":{{}}}}}}}}}}"#, key("a"), key("b"), key("c"));

    let error = decode::<DeeplyKeyed>(document.as_bytes()).expect_err("no `noul`");

    // 8 + 129 + 1 + 129 + 1 characters come before the third key, which
    // leaves it 52 of the 320 before the path is cut.
    let expected = format!(
        "answers.{}\u{2026}.{}\u{2026}.{}\u{2026}",
        "a".repeat(MAX_PATH_SEGMENT_CHARS),
        "b".repeat(MAX_PATH_SEGMENT_CHARS),
        "c".repeat(52)
    );
    assert_eq!(error.path(), expected);
    assert_eq!(error.path().chars().count(), MAX_PATH_CHARS + 1);
}

#[test]
fn an_ordinary_path_renders_exactly_as_serde_path_to_error_prints_it() {
    let rows: [(&[u8], &str); 4] = [
        (br#"{"answers":{"spam":{},"tone":{"confidence":0.9}}}"#, "answers.spam.noul"),
        (
            br#"{"answers":{"spam":{"noul":0.98},"tone":{"confidence":"high"}}}"#,
            "answers.tone.confidence",
        ),
        (br#"[]"#, "."),
        (br#"{"answers":[]}"#, "answers"),
    ];
    for (document, expected) in rows {
        let error = decode::<Envelope>(document).expect_err("the document does not fit");
        assert_eq!(error.kind(), DecodeErrorKind::Data, "kind of {error}");
        assert_eq!(error.path(), expected, "{}", String::from_utf8_lossy(document));
        assert!(error.line() > 0, "the position is kept: {error:?}");
    }

    let models = decode::<ModelList>(br#"{"models":[{"name":"jev-latest"},{"name":123}]}"#)
        .expect_err("the second name is a number");
    assert_eq!(models.kind(), DecodeErrorKind::Data, "kind of {models}");
    assert_eq!(models.path(), "models[1].name");
    let top_level = decode::<Vec<Model>>(br#"[{"name":"a"},{}]"#).expect_err("no name");
    assert_eq!(top_level.path(), "[1].name");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// For any printable key without a backslash, the rendering is byte for
    /// byte the text `serde_path_to_error` itself prints for the same failure.
    /// A backslash is the one printable character the rendering doubles.
    #[test]
    fn a_printable_path_is_unchanged_by_the_rendering(
        outer in r"[[\p{L}\p{N}\p{P}\p{S} ]&&[^\\]]{0,40}",
        inner in r"[[\p{L}\p{N}\p{P}\p{S} ]&&[^\\]]{0,40}",
        index in 0_usize..3,
    ) {
        let elements = std::iter::repeat_n("{}".to_owned(), index)
            .chain([format!(r#"{{{}:{{}}}}"#, other_codec::to_string(&inner).expect("encodes"))])
            .collect::<Vec<_>>()
            .join(",");
        let document =
            format!(r#"{{{}:[{elements}]}}"#, other_codec::to_string(&outer).expect("encodes"));
        type Target = BTreeMap<String, Vec<BTreeMap<String, Noul>>>;

        let error = decode::<Target>(document.as_bytes()).expect_err("no `noul`");
        let mut deserializer = backend::Deserializer::from_slice(document.as_bytes());
        let tracked = serde_path_to_error::deserialize::<_, Target>(&mut deserializer)
            .expect_err("no `noul`");

        prop_assert_eq!(error.path(), format!("{}.noul", tracked.path()), "document {}", document);
    }
}

/// Every class of document `decode` treats differently, each through `decode`
/// and through `decode_seed` with serde's own stateless seed.
#[test]
fn a_seeded_decode_accepts_and_refuses_exactly_what_decode_does() {
    let mut too_deep = "[".repeat(MAX_JSON_DEPTH + 1);
    too_deep.push_str(&"]".repeat(MAX_JSON_DEPTH + 1));
    let documents: [(&str, &[u8]); 9] = [
        ("fits", br#"{"answers":{"spam":{"noul":0.98},"tone":{"confidence":0.9}}}"#),
        ("missing field", br#"{"answers":{"spam":{},"tone":{"confidence":0.9}}}"#),
        ("wrong type", br#"{"answers":{"spam":{"noul":0.98},"tone":{"confidence":"x"}}}"#),
        ("syntax", br#"{"answers":}"#),
        ("trailing value", br#"{"answers":{"spam":{"noul":1},"tone":{"confidence":1}}} 7"#),
        ("trailing space", b"{\"answers\":{\"spam\":{\"noul\":1},\"tone\":{\"confidence\":1}}} \n"),
        // A byte that is not UTF-8 inside a string no reader looks at: only
        // the codec's own final check sees it.
        (
            "bad utf-8 in a skipped string",
            b"{\"answers\":{\"spam\":{\"noul\":1,\"x\":\"\xff\"},\"tone\":{\"confidence\":1}}}",
        ),
        ("bad utf-8 outside a string", b"{\"answers\":\xff}"),
        ("too deep", too_deep.as_bytes()),
    ];

    for (label, document) in documents {
        let plain = decode::<Envelope>(document).map(|envelope| format!("{envelope:?}"));
        let seeded =
            decode_seed(document, PhantomData::<Envelope>).map(|envelope| format!("{envelope:?}"));
        assert_eq!(seeded, plain, "{label}");
    }
    for (label, document) in documents {
        let outcome = decode_seed(document, PhantomData::<Envelope>).map_err(|error| error.kind());
        let expected = match label {
            "fits" | "trailing space" => Ok(()),
            "missing field" | "wrong type" => Err(DecodeErrorKind::Data),
            "too deep" => Err(DecodeErrorKind::TooDeep),
            _ => Err(DecodeErrorKind::Syntax),
        };
        assert_eq!(outcome.map(drop), expected, "{label}");
    }
}

/// A seed that requires a member named by its own state.
#[derive(Clone, Copy)]
struct Requires(&'static str);

impl<'de> DeserializeSeed<'de> for Requires {
    type Value = u64;

    fn deserialize<D>(self, deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let members = BTreeMap::<String, u64>::deserialize(deserializer)?;
        members.get(self.0).copied().ok_or_else(|| de::Error::missing_field(self.0))
    }
}

#[test]
fn a_seed_carries_its_state_into_the_decode_and_into_the_failure_pass() {
    assert_eq!(decode_seed(br#"{"a":1,"b":2}"#, Requires("b")), Ok(2));

    // The path names the member the seed's state asked for, so the second,
    // path-tracking pass was driven by the same state as the first. The error
    // is raised after the parser has finished, so it has no position.
    let error = decode_seed(br#"{"a":1}"#, Requires("wanted")).expect_err("no `wanted`");
    assert_eq!(error.kind(), DecodeErrorKind::Data);
    assert_eq!(error.path(), "wanted");
    assert_eq!(error.to_string(), "unexpected JSON value at `wanted`, line 0 column 0");
}

#[test]
fn a_syntax_error_keeps_its_position() {
    let error = decode::<Envelope>(br#"{"answers":}"#).expect_err("the value is missing");

    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "kind of {error}");
    assert_eq!(error.line(), 1);
    assert!(error.column() > 0, "the column is kept: {error:?}");
    assert_eq!(error.path(), "", "a syntax error has no field path");
}

// --------------------------------------------------------------- encoding

#[test]
fn encode_into_appends_instead_of_clearing() {
    let mut buffer = Vec::from(&br#"{"state":"#[..]);

    encode_into(&mut buffer, "a \"quoted\" state").expect("a string always encodes");
    buffer.extend_from_slice(br#","model":"#);
    write_json_string(&mut buffer, "jev-latest");
    buffer.push(b'}');

    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        r#"{"state":"a \"quoted\" state","model":"jev-latest"}"#
    );
}

#[test]
fn encode_body_returns_exactly_the_bytes_the_closure_wrote() {
    reset_scratch();

    let body = encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"state":"#);
        encode_into(buffer, "hello")?;
        buffer.extend_from_slice(br#","model":"jev-latest"}"#);
        Ok(())
    })
    .expect("the closure succeeds");

    assert_eq!(&body[..], br#"{"state":"hello","model":"jev-latest"}"#);
}

#[test]
fn encode_body_survives_a_nested_call() {
    reset_scratch();

    let outer = encode_body(|buffer| {
        let inner = encode_body(|nested| {
            nested.extend_from_slice(br#"{"inner":true}"#);
            Ok(())
        })?;
        buffer.extend_from_slice(br#"{"outer":"#);
        buffer.extend_from_slice(&inner);
        buffer.push(b'}');
        Ok(())
    })
    .expect("the nested call gets a buffer of its own");

    assert_eq!(&outer[..], br#"{"outer":{"inner":true}}"#);
}

#[test]
fn a_panicking_closure_does_not_poison_later_calls() {
    reset_scratch();

    let previous_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        encode_body(|buffer| {
            buffer.extend_from_slice(b"partial");
            panic!("the caller's Serialize implementation gave up");
        })
    }));
    panic::set_hook(previous_hook);

    assert!(outcome.is_err(), "the panic propagates to the caller");

    let body = encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"after":"the panic"}"#);
        Ok(())
    })
    .expect("the next call gets a fresh buffer");
    assert_eq!(&body[..], br#"{"after":"the panic"}"#);
}

#[test]
fn a_repeated_body_reuses_the_retained_scratch() {
    reset_scratch();

    let state = "s".repeat(1024);
    for _ in 0..4 {
        let body = encode_body(|buffer| encode_into(buffer, state.as_str())).expect("encodes");
        assert_eq!(body.len(), state.len() + 2);
    }

    let capacity = scratch_capacity();
    let hint = scratch_hint();
    assert!(capacity >= state.len(), "the buffer is kept: {capacity}");
    assert!(
        capacity <= hint.saturating_mul(8),
        "retained {capacity} bytes against a hint of {hint}"
    );
}

#[test]
fn an_outlier_body_stops_pinning_the_scratch() {
    reset_scratch();

    let large = "L".repeat(1024 * 1024);
    let small = "s".repeat(1024);
    encode_body(|buffer| encode_into(buffer, large.as_str())).expect("encodes");
    let calls = if cfg!(feature = "sonic") { 16 } else { 32 };
    for _ in 0..calls {
        encode_body(|buffer| encode_into(buffer, small.as_str())).expect("encodes");
    }

    let capacity = scratch_capacity();
    let hint = scratch_hint();
    assert!(
        capacity <= hint.saturating_mul(8),
        "after the outlier the scratch still holds {capacity} bytes against a hint of {hint}"
    );
    assert!(capacity < large.len(), "the megabyte buffer is gone: {capacity}");
}

#[test]
fn a_scratch_past_the_ceiling_is_not_kept() {
    reset_scratch();

    // sonic-rs reserves six-fold; serde_json needs the body itself to cross
    // the ceiling. Both cases exercise dropping, not the decay-driven shrink.
    #[cfg(feature = "sonic")]
    let state = "L".repeat(2 * 1024 * 1024);
    #[cfg(not(feature = "sonic"))]
    let state = "L".repeat(MAX_RETAINED_SCRATCH + 1);
    for call in 1..=3 {
        let body = encode_body(|buffer| encode_into(buffer, state.as_str())).expect("encodes");
        assert_eq!(body.len(), state.len() + 2, "call {call}");
        assert_eq!(
            scratch_capacity(),
            0,
            "call {call}: a {} B state's scratch is kept, over the {MAX_RETAINED_SCRATCH} B ceiling",
            state.len()
        );
    }

    let small = "s".repeat(1024);
    let body = encode_body(|buffer| encode_into(buffer, small.as_str())).expect("encodes");
    assert_eq!(&body[..3], b"\"ss", "a later call encodes into a fresh buffer");
    assert!(scratch_capacity() >= small.len(), "and keeps that one: {}", scratch_capacity());
}

#[test]
fn a_scratch_under_the_ceiling_is_kept() {
    reset_scratch();

    let state = "L".repeat(1024 * 1024);
    for call in 1..=3 {
        encode_body(|buffer| encode_into(buffer, state.as_str())).expect("encodes");
        let capacity = scratch_capacity();
        #[cfg(feature = "sonic")]
        let minimum = 6 * state.len() + 1;
        #[cfg(not(feature = "sonic"))]
        let minimum = state.len() + 2;
        assert!(
            capacity >= minimum && capacity <= MAX_RETAINED_SCRATCH,
            "call {call}: a {} B state keeps {capacity} B, outside {minimum}..={MAX_RETAINED_SCRATCH}",
            state.len()
        );
    }
}

// ------------------------------------------------------------- raw JSON

#[derive(Debug, Deserialize)]
struct Holder {
    raw: RawJson,
}

#[test]
fn raw_json_keeps_the_text_it_was_given() {
    // A `\u00e9` escape, an escaped quote, an escaped backslash, a line
    // separator and a negative zero: the codec may rewrite none of them.
    let document = r#"{"raw":{"text":"caf\u00e9 \"q\" \\ \u2028","list":[1,2,3],"n":-0.0}}"#;

    let holder: Holder = decode(document.as_bytes()).expect("the document decodes");
    assert_eq!(
        holder.raw.as_str(),
        r#"{"text":"caf\u00e9 \"q\" \\ \u2028","list":[1,2,3],"n":-0.0}"#
    );

    let mut buffer = Vec::new();
    encode_into(&mut buffer, &holder.raw).expect("raw text re-encodes");
    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        holder.raw.as_str(),
        "re-encoding must splice the text in rather than re-render it"
    );
}

#[test]
fn raw_json_round_trips_through_a_value() {
    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Point {
        x: i32,
        label: String,
    }
    let point = Point { x: -3, label: "caf\u{e9}\u{2028}\u{1f600}".to_owned() };

    let raw = RawJson::from_value(&point).expect("the value encodes");
    assert_eq!(raw.decode::<Point>().expect("the text decodes"), point, "raw text {raw}");
    assert_eq!(raw, RawJson::from_value(&point).expect("encodes again"));
}

#[test]
fn raw_json_applies_the_depth_limit_when_it_is_read_back() {
    let mut value = serde_json::Value::from(1);
    for _ in 0..=MAX_JSON_DEPTH {
        value = serde_json::Value::Array(vec![value]);
    }

    let raw = RawJson::from_value(&value).expect("encoding is not depth limited");
    let error = raw.decode::<IgnoredAny>().expect_err("reading it back is");
    assert_eq!(error.kind(), DecodeErrorKind::TooDeep, "kind of {error}");
}

// ------------------------------------------------- differential behaviour

#[cfg(feature = "sonic")]
#[test]
fn a_literal_negative_zero_loses_its_sign_with_sonic() {
    // An accepted divergence of this codec, asserted so that it is a recorded
    // property rather than a surprise: a literal negative zero decodes as a
    // positive one. A zero reached by underflow keeps its sign.
    let ours: f64 = decode(b"-0.0").expect("it parses");
    let reference: f64 = other_codec::from_slice(b"-0.0").expect("it parses there too");

    assert_eq!(ours.to_bits(), 0.0_f64.to_bits());
    assert_eq!(reference.to_bits(), (-0.0_f64).to_bits());

    let underflow: f64 = decode(b"-1e-400").expect("it parses");
    assert_eq!(underflow.to_bits(), (-0.0_f64).to_bits());
}

#[cfg(not(feature = "sonic"))]
#[test]
fn a_literal_negative_zero_keeps_its_sign() {
    let ours: f64 = decode(b"-0.0").expect("it parses");
    let reference: f64 = other_codec::from_slice(b"-0.0").expect("the reference parses it");
    assert_eq!(ours.to_bits(), 0x8000_0000_0000_0000);
    assert_eq!(reference.to_bits(), 0);
    let underflow: f64 = decode(b"-1e-400").expect("it parses");
    assert_eq!(underflow.to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn awkward_characters_encode_to_text_another_codec_reads_back() {
    let samples = [
        "\u{0}\u{1}\u{1f}",
        "\u{7f}",
        "\u{a0}",
        "\u{2028}\u{2029}",
        "\u{1f600}",
        "\"\\/",
        "caf\u{e9} \u{20ac} \u{2014}",
    ];

    for sample in samples {
        let mut buffer = Vec::new();
        encode_into(&mut buffer, sample).expect("a string always encodes");
        let reparsed: String = other_codec::from_slice(&buffer)
            .unwrap_or_else(|error| panic!("{error} for {buffer:?}"));
        assert_eq!(reparsed, sample, "encoded as {:?}", String::from_utf8_lossy(&buffer));
    }
}

proptest! {
    /// The encode side of the codec differential: whatever this codec writes
    /// for a string, an independent parser must read back unchanged.
    #[test]
    fn encoded_strings_reparse_identically(
        text in proptest::collection::vec(any::<char>(), 0..48).prop_map(String::from_iter)
    ) {
        let mut buffer = Vec::new();
        encode_into(&mut buffer, text.as_str()).expect("a string always encodes");
        let reparsed: String = other_codec::from_slice(&buffer)
            .unwrap_or_else(|error| panic!("{error} for {buffer:?}"));
        prop_assert_eq!(reparsed, text);
    }
}

// ------------------------------------------------- raw JSON, other codecs

/// A response-shaped value with a raw field, which is the shape a user gets
/// when they serialize a response with a codec of their own.
#[derive(Debug, Deserialize, Serialize)]
struct Envelope2 {
    name: String,
    raw: RawJson,
}

/// The four numbers the transcode has to carry without changing their value:
/// an exponent far from 1, a decimal fraction no binary float holds exactly,
/// and both integer extremes, which do not fit in an `f64`.
#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Numbers {
    tiny: f64,
    tenth: f64,
    largest: u64,
    smallest: i64,
}

const NUMBERS: &str =
    r#"{"tiny":1e-7,"tenth":0.1,"largest":18446744073709551615,"smallest":-9223372036854775808}"#;

/// Encodes with this crate's codec, which is the serializer the SDK uses.
fn sdk_encoded<T: Serialize + ?Sized>(value: &T) -> String {
    let mut buffer = Vec::new();
    encode_into(&mut buffer, value).expect("the value encodes");
    String::from_utf8(buffer).expect("the codec emits UTF-8")
}

#[test]
fn another_codec_gets_json_data_and_never_the_splice_token() {
    let document = br#"{"name":"legend","raw":{"b":[1,{"c":"caf\u00e9"}],"a":null}}"#;
    let envelope: Envelope2 = decode(document).expect("the document decodes");

    let foreign = other_codec::to_string(&envelope).expect("another codec writes it as data");

    assert!(
        !foreign.contains(backend::SPLICE_TOKEN),
        "a private protocol of this codec reached another one: {foreign}"
    );
    assert_eq!(
        other_codec::from_str::<serde_json::Value>(&foreign).expect("the output is JSON"),
        serde_json::json!({"name": "legend", "raw": {"b": [1, {"c": "caf\u{e9}"}], "a": null}}),
        "transcoded as {foreign}"
    );
}

#[test]
fn the_same_value_splices_verbatim_through_this_codec() {
    // The exact bytes, spacing and key order of the raw field survive, which
    // is what the other codec is allowed to re-render and this one is not.
    let document = br#"{"name":"legend","raw":{ "b":[1, 2],"a":-0.0 }}"#;
    let envelope: Envelope2 = decode(document).expect("the document decodes");

    assert_eq!(sdk_encoded(&envelope), r#"{"name":"legend","raw":{ "b":[1, 2],"a":-0.0 }}"#);
}

#[test]
fn a_raw_value_round_trips_through_another_codec_with_the_same_data() {
    for text in [
        r#"{"label":"ok","weight":2,"tags":["a","b"],"none":null,"flag":true}"#,
        r#"[1,[2,[3,[]]],{"k":"v"}]"#,
        r#""a \"quoted\" caf\u00e9""#,
        "17",
    ] {
        let original = RawJson::from_text(text.to_owned());

        let written = other_codec::to_string(&original).expect("it writes as data");
        let read_back: RawJson = other_codec::from_str(&written).expect("it reads back");

        assert!(!written.contains(backend::SPLICE_TOKEN), "token leaked for {text}: {written}");
        assert_eq!(
            other_codec::from_str::<serde_json::Value>(read_back.as_str())
                .expect("the text is JSON"),
            other_codec::from_str::<serde_json::Value>(text).expect("so is the original"),
            "{text} came back as {read_back}"
        );
    }
}

#[test]
fn debug_escapes_text_hiding_characters_that_display_keeps() {
    // (the character, its escape) - each allowed raw inside a JSON string.
    let rows = [
        ('\u{202e}', r"\u{202e}"),
        ('\u{2028}', r"\u{2028}"),
        ('\u{2029}', r"\u{2029}"),
        ('\u{0085}', r"\u{85}"),
    ];
    for (character, escape) in rows {
        let text = format!(r#"{{"x":"a{character}b\n\\c"}}"#);
        let raw = RawJson::from_text(text.clone());

        let debugged = format!("{raw:?}");

        assert_eq!(
            debugged,
            format!(r#"{{"x":"a{escape}b\n\\c"}}"#),
            "U+{:04X}: the character is escaped, the JSON's own escapes are kept",
            u32::from(character)
        );
        assert!(!debugged.contains(character), "U+{:04X} raw in {debugged}", u32::from(character));
        assert_eq!(raw.to_string(), text, "Display is byte-exact");
        assert_eq!(raw.as_str(), text, "as_str is byte-exact");
        assert_eq!(sdk_encoded(&raw), text, "serialization is byte-exact");
    }
}

#[test]
fn numbers_keep_their_value_across_the_transcode() {
    let raw = RawJson::from_text(NUMBERS.to_owned());
    let expected = Numbers { tiny: 1e-7, tenth: 0.1, largest: u64::MAX, smallest: i64::MIN };

    let written = other_codec::to_string(&raw).expect("it writes as data");
    let decoded: Numbers = other_codec::from_str(&written).expect("the numbers read back");

    assert_eq!(decoded.tiny.to_bits(), expected.tiny.to_bits(), "written as {written}");
    assert_eq!(decoded.tenth.to_bits(), expected.tenth.to_bits(), "written as {written}");
    assert_eq!(decoded.largest, u64::MAX, "written as {written}");
    assert_eq!(decoded.smallest, i64::MIN, "written as {written}");

    let wide = RawJson::from_text("12345678901234567890123".to_owned());
    assert_eq!(
        other_codec::to_string(&wide).expect("a wide integer transcodes"),
        "1.2345678901234568e+22",
        "integers beyond u64/i64 take the same f64 path as number tokens"
    );

    // The same value through this codec is not re-rendered at all.
    assert_eq!(sdk_encoded(&raw), NUMBERS);
}

#[test]
fn a_negative_zero_on_the_transcode_path_is_written_as_the_backend_reads_it() {
    // The transcode reads with the selected parser; the splice never reads it.
    let raw = RawJson::from_text("-0.0".to_owned());
    #[cfg(feature = "sonic")]
    let expected = "0.0";
    #[cfg(not(feature = "sonic"))]
    let expected = "-0.0";

    assert_eq!(other_codec::to_string(&raw).expect("it writes as data"), expected);
    assert_eq!(sdk_encoded(&raw), "-0.0");
}

#[test]
fn a_value_too_deep_to_transcode_is_refused_rather_than_aborting() {
    let at_limit = RawJson::from_text(nested_array(MAX_JSON_DEPTH));
    let over_limit = RawJson::from_text(nested_array(MAX_JSON_DEPTH + 1));
    // Far past any stack: the splice path must not read this, and the
    // transcode path must reject it on the byte scan rather than recurse.
    let absurd = RawJson::from_text(nested_array(100_000));

    let outcomes = on_worker_stack(move || {
        (
            other_codec::to_string(&at_limit).expect("the limit itself transcodes"),
            other_codec::to_string(&over_limit).map(|_| ()).map_err(|error| error.to_string()),
            other_codec::to_string(&absurd).map(|_| ()).map_err(|error| error.to_string()),
            sdk_encoded(&absurd),
        )
    });

    assert_eq!(outcomes.0, nested_array(MAX_JSON_DEPTH));
    assert!(outcomes.1.is_err(), "one level past the cap must be refused: {outcomes:?}");
    assert!(outcomes.2.is_err(), "100,000 levels must be refused: {outcomes:?}");
    assert_eq!(outcomes.3, nested_array(100_000), "the splice never reads the text");
}

#[test]
fn a_document_too_deep_to_render_is_refused_by_the_other_codec_path() {
    // Under the other codec's own recursion limit, so the cap that bites here
    // is this crate's.
    let document = nested_array(MAX_JSON_DEPTH + 1);

    let error = other_codec::from_str::<RawJson>(&document).expect_err("the document is too deep");

    assert!(
        error.to_string().contains("nested deeper"),
        "the depth cap has to be the reason: {error}"
    );
    assert_eq!(
        other_codec::from_str::<RawJson>(&nested_array(MAX_JSON_DEPTH))
            .expect("the limit itself is fine")
            .as_str(),
        nested_array(MAX_JSON_DEPTH)
    );
}

/// A writer that gives up part way, so that an error raised by the other
/// codec has to travel back out through the parser driving the transcode.
struct ShortWriter {
    remaining: usize,
}

impl std::io::Write for ShortWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Err(std::io::Error::other("the sink is full"));
        }
        let taken = buf.len().min(self.remaining);
        self.remaining -= taken;
        Ok(taken)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_failure_in_the_other_codec_is_reported_and_carries_no_input() {
    const SECRET: &str = "pentachlorophenol-42";
    let raw = RawJson::from_text(format!(r#"{{"nested":[{{"state":"{SECRET}"}}]}}"#));

    let writer = ShortWriter { remaining: 4 };
    #[cfg(not(feature = "sonic"))]
    let writer = other_codec::writer::BufferedWriter::new(writer);
    let error = other_codec::to_writer(writer, &raw).expect_err("the sink refuses the value");

    let rendered = error.to_string();
    #[cfg(feature = "sonic")]
    let reason = "the sink is full";
    #[cfg(not(feature = "sonic"))]
    let reason = "io error while serializing or deserializing";
    assert!(rendered.contains(reason), "the writer's reason is kept: {rendered}");
    assert!(!rendered.contains(SECRET), "the failure leaked the text: {rendered}");
}

#[test]
fn a_wrapper_around_this_codec_forwards_the_splice() {
    // `serde_path_to_error::Serializer` is not this crate's serializer, but it
    // passes every call through to the one it wraps. Whether a splice happens
    // is decided by the encoder that is running, not by the type in hand, so
    // the raw text reaches this codec and is written out unchanged.
    let raw = RawJson::from_text(r#"{ "b":[1, 2],"a":-0.0 }"#.to_owned());

    let mut track = serde_path_to_error::Track::new();
    let mut buffer = Vec::new();
    {
        let _inside = EncoderMark::enter();
        let mut codec = backend::Serializer::new(&mut buffer);
        raw.serialize(serde_path_to_error::Serializer::new(&mut codec, &mut track))
            .expect("the wrapper forwards the splice");
    }
    assert_eq!(
        String::from_utf8(buffer).expect("the codec emits UTF-8"),
        r#"{ "b":[1, 2],"a":-0.0 }"#
    );

    // The same wrapper outside the SDK's encoder gets data instead, which is
    // what a caller assembling a serializer of their own sees.
    let mut track = serde_path_to_error::Track::new();
    let mut buffer = Vec::new();
    let mut codec = backend::Serializer::new(&mut buffer);
    raw.serialize(serde_path_to_error::Serializer::new(&mut codec, &mut track))
        .expect("outside the encoder it transcodes");
    #[cfg(feature = "sonic")]
    let expected = r#"{"b":[1,2],"a":0.0}"#;
    #[cfg(not(feature = "sonic"))]
    let expected = r#"{"b":[1,2],"a":-0.0}"#;
    assert_eq!(String::from_utf8(buffer).expect("the codec emits UTF-8"), expected);
}

#[test]
fn a_deserializer_that_ignores_the_request_still_yields_the_value() {
    // These forward every request to `deserialize_any`, so the value arrives
    // at the visitor directly instead of through the newtype struct. It is the
    // route a non-self-describing format takes.
    use serde::de::{
        IntoDeserializer,
        value::{Error as ValueError, MapDeserializer, SeqDeserializer, U64Deserializer},
    };

    // Spelled through the trait: the inherent `RawJson::deserialize` would
    // shadow it.
    let number = <RawJson as Deserialize>::deserialize(U64Deserializer::<ValueError>::new(
        9_007_199_254_740_993,
    ))
    .expect("a bare number arrives as data");
    assert_eq!(number.as_str(), "9007199254740993");

    let sequence = <RawJson as Deserialize>::deserialize(SeqDeserializer::<_, ValueError>::new(
        [1_u64, 2, 3].into_iter(),
    ))
    .expect("a bare sequence arrives as data");
    assert_eq!(sequence.as_str(), "[1,2,3]");

    let map = <RawJson as Deserialize>::deserialize(MapDeserializer::<_, ValueError>::new(
        [("we\"ird", 1_u64), ("b", 2)].into_iter().map(|(k, v)| (k, v.into_deserializer())),
    ))
    .expect("a bare map arrives as data");
    assert_eq!(map.as_str(), r#"{"we\"ird":1,"b":2}"#);
}

// --------------------------------------------- the one-JSON-value invariant

/// Reads one raw capture, which is what a response type does for a field it
/// keeps as text.
#[derive(Debug, Deserialize)]
struct RawRow<'a> {
    #[serde(borrow, deserialize_with = "deserialize_raw")]
    value: Cow<'a, str>,
}

#[test]
fn text_that_is_not_one_json_value_never_becomes_raw_json() {
    use serde::de::value::{BorrowedStrDeserializer, Error as ValueError, StrDeserializer};

    // The first of these is the dangerous one: spliced into a request body it
    // would add a key of the caller's choosing to the enclosing object. The
    // others would make the body unparseable. The refusal that reaches the
    // caller is a `serde` error carrying the rendered `DecodeError`, so the
    // kind is asserted through what it renders as.
    #[cfg(feature = "sonic")]
    let empty_error = "invalid JSON syntax at line 1 column 1";
    #[cfg(not(feature = "sonic"))]
    let empty_error = "invalid JSON syntax at line 1 column 0";
    for (text, expected) in [
        (r#"{"a":1}, "model": "evil""#, "invalid JSON syntax at line 1 column 8"),
        ("{not json", "invalid JSON syntax at line 1 column 2"),
        ("hello", "invalid JSON syntax at line 1 column 1"),
        ("", empty_error),
        ("  ", "invalid JSON syntax at line 1 column 2"),
    ] {
        let borrowed =
            <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(text));
        let owned = <RawJson as Deserialize>::deserialize(StrDeserializer::<ValueError>::new(text));

        let error = borrowed.expect_err(text).to_string();
        assert!(error.contains(expected), "{text:?} was refused as {error}");
        assert!(
            owned.expect_err(text).to_string().contains(expected),
            "the owned route refused {text:?} differently"
        );
        assert!(!error.contains("evil"), "the refusal quoted the input: {error}");
        assert!(!error.contains("not json"), "the refusal quoted the input: {error}");
    }

    // A string that does hold exactly one value is taken at face value, which
    // is the raw-text protocol this codec relies on.
    let accepted = <RawJson as Deserialize>::deserialize(
        BorrowedStrDeserializer::<ValueError>::new(r#"{"a":1}"#),
    )
    .expect("one complete value is raw JSON text");
    assert_eq!(accepted.as_str(), r#"{"a":1}"#);
}

#[test]
fn a_raw_capture_past_the_depth_cap_is_refused() {
    use serde::de::value::{BorrowedStrDeserializer, Error as ValueError};

    let deep = nested_array(MAX_JSON_DEPTH + 1);
    let at_limit = nested_array(MAX_JSON_DEPTH);

    let error =
        <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(&deep))
            .expect_err("one level past the cap is refused");
    assert!(error.to_string().contains("nested deeper"), "{error}");

    assert_eq!(
        <RawJson as Deserialize>::deserialize(BorrowedStrDeserializer::<ValueError>::new(
            &at_limit
        ))
        .expect("the limit itself is fine")
        .as_str(),
        at_limit
    );
}

#[test]
fn the_check_leaves_the_borrow_on_the_sdk_decode_path() {
    let document = br#"{"value":{"label":"ok","weight":2}}"#;

    let row: RawRow<'_> = decode(document).expect("the document decodes");

    assert!(
        matches!(row.value, Cow::Borrowed(_)),
        "the raw capture stopped borrowing the response buffer: {:?}",
        row.value
    );
    let captured = row.value.as_ptr().addr();
    let start = document.as_ptr().addr();
    assert!(
        (start..start + document.len()).contains(&captured),
        "the text must point into the response buffer, not a copy"
    );
    assert_eq!(&*row.value, r#"{"label":"ok","weight":2}"#);
}

// ------------------------------------------------------ non-finite floats

#[test]
fn a_non_finite_float_encodes_as_null_exactly_as_the_reference_codec_does() {
    // Pinned rather than asserted as desirable: this codec and `serde_json`
    // agree on `null`, the upstream Python SDK writes the bare words `NaN` and
    // `Infinity`, which are not JSON at all, and the plan says both should
    // refuse. The behaviour is recorded here so that a change to it is a test
    // failure rather than a surprise on the wire.
    #[derive(Debug, Serialize)]
    struct Scores {
        score: f64,
        ratio: f32,
    }

    for (value, expected) in [(f64::NAN, "null"), (f64::INFINITY, "null"), (-f64::INFINITY, "null")]
    {
        let ours = RawJson::from_value(&value).expect("a non-finite float encodes");
        assert_eq!(ours.as_str(), expected);
        assert_eq!(other_codec::to_string(&value).expect("so it does there"), expected);
    }

    let scores = Scores { score: f64::NAN, ratio: f32::NEG_INFINITY };
    assert_eq!(
        RawJson::from_value(&scores).expect("a struct of them encodes").as_str(),
        r#"{"score":null,"ratio":null}"#
    );
    assert_eq!(
        other_codec::to_string(&scores).expect("and there too"),
        r#"{"score":null,"ratio":null}"#
    );

    // The render path a foreign deserializer takes reaches the same writer.
    use serde::de::value::{Error as ValueError, F64Deserializer};
    assert_eq!(
        <RawJson as Deserialize>::deserialize(F64Deserializer::<ValueError>::new(f64::NAN))
            .expect("it renders")
            .as_str(),
        "null"
    );
}

#[test]
fn an_encode_error_is_a_key_the_writer_cannot_spell_or_a_value_that_refuses() {
    // The two failures the rustdoc of `EncodeError` names. A non-finite float
    // is not one of them, which is why it is not mentioned there.
    let tuple_keys = std::collections::BTreeMap::from([((1_u8, 2_u8), 3_u8)]);
    let error = RawJson::from_value(&tuple_keys).expect_err("a tuple is not a JSON key");
    assert!(error.message().contains("key"), "{error}");
    assert!(other_codec::to_string(&tuple_keys).is_err(), "the reference codec refuses it too");

    struct Refuses;
    impl Serialize for Refuses {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("this value declines to be encoded"))
        }
    }
    let error = RawJson::from_value(&Refuses).expect_err("the value refuses");
    assert_eq!(error.message(), "this value declines to be encoded");
}

#[test]
fn bytes_after_the_value_are_a_syntax_error_at_the_byte_that_follows_it() {
    // The failure path deserializes a second time to build the field path, and
    // that pass stops at the end of the value rather than at the end of the
    // input. A document with anything behind its value parses on that pass and
    // failed on the first, so the position has to come from finishing the
    // deserializer, not from the pass itself.
    let trailing_number = decode::<u64>(b"1 2").expect_err("a second value is not one value");
    assert_eq!(trailing_number.kind(), DecodeErrorKind::Syntax, "{trailing_number}");
    assert_eq!(trailing_number.line(), 1);
    assert_eq!(trailing_number.column(), 3, "the column points at the second value");
    assert_eq!(trailing_number.path(), "");

    let injected = decode::<IgnoredAny>(br#"{"a":1}, "model": "evil""#)
        .expect_err("a key behind the value is not one value");
    assert_eq!(injected.kind(), DecodeErrorKind::Syntax, "{injected}");
    assert_eq!(injected.line(), 1);
    assert_eq!(injected.column(), 8, "the column points at the comma");
    assert!(!injected.to_string().contains("evil"), "the error quoted the input: {injected}");

    let after_a_struct =
        decode::<Envelope>(br#"{"answers":{"spam":{"noul":0.9},"tone":{"confidence":0.4}}}x"#)
            .expect_err("a stray byte is not whitespace");
    assert_eq!(after_a_struct.kind(), DecodeErrorKind::Syntax, "{after_a_struct}");
    assert_eq!(after_a_struct.line(), 1);
    assert_eq!(after_a_struct.column(), 60, "the column points at the stray byte");
}

#[test]
fn a_data_error_without_a_path_says_so_rather_than_printing_an_empty_one() {
    // Reached only when the two decode passes disagree about whether the
    // document parses at all, which no input produces today; the rendering is
    // asserted here because it is what a caller would see if one did.
    let opaque = DecodeError { detail: Detail::Opaque };

    assert_eq!(opaque.kind(), DecodeErrorKind::Data);
    assert_eq!(opaque.path(), "");
    assert_eq!(opaque.line(), 0);
    assert_eq!(opaque.column(), 0);
    assert_eq!(opaque.to_string(), "the JSON document does not have the expected shape");
    assert!(!opaque.to_string().contains("``"), "an empty path must not be printed");
}

// ------------------------------------------------- bytes that are not UTF-8
//
// The codec takes a string's bytes as text without checking them, and reports
// a byte that is not UTF-8 only after the whole document has been read. A
// string kept as raw text - a member of an error body, an object in a score
// legend - is therefore checked for UTF-8 in `as_text` before the codec reads
// it: taken unchecked, such a byte panics inside the codec in a debug build
// and hands on an invalid `&str` in a release build. Each case below is
// refused before the codec sees it.

/// Documents holding a byte sequence that is not UTF-8, with the one-based
/// line and byte column of its first byte.
const NOT_UTF8: &[(&str, &[u8], usize, usize)] = &[
    ("in a string value", b"{\"a\":\"\xC9\"}", 1, 7),
    ("in an object key", b"{\"\xC9\":1}", 1, 3),
    ("right after a backslash", b"{\"a\":\"\\\xC9\"}", 1, 8),
    ("a sequence cut short at the very end", b"{\"a\":\"\xC3", 1, 7),
    ("an overlong encoding of '/'", b"[\"\xC0\xAF\"]", 1, 3),
    ("a surrogate half written as raw bytes", b"[\"\xED\xA0\x80\"]", 1, 3),
    ("after a valid two-byte character", b"[\"\xC3\xA9\xC9\"]", 1, 5),
    ("on the third line", b"{\n  \"a\": \"ok\",\n  \"b\": \"\xFF\"\n}", 3, 9),
];

/// Fails unless `result` is the syntax error at `line` and `column` that a
/// byte which is not UTF-8 gives.
fn assert_not_utf8<T: fmt::Debug>(
    case: &str,
    entry: &str,
    result: Result<T, DecodeError>,
    line: usize,
    column: usize,
) {
    let error = match result {
        Ok(value) => panic!("{case}, {entry}: decoded to {value:?} instead of failing"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "{case}, {entry}: {error:?}");
    assert_eq!((error.line(), error.column()), (line, column), "{case}, {entry}: {error:?}");
    assert_eq!(error.path(), "", "{case}, {entry}");
    assert_eq!(
        error.to_string(),
        format!("invalid JSON syntax at line {line} column {column}"),
        "{case}, {entry}"
    );
}

#[test]
fn bytes_that_are_not_utf8_are_a_syntax_error_at_the_first_of_them() {
    for &(case, bytes, line, column) in NOT_UTF8 {
        assert_not_utf8(case, "RawJson", decode::<RawJson>(bytes), line, column);
        assert_not_utf8(case, "a skipped value", decode::<IgnoredAny>(bytes), line, column);
        assert_not_utf8(
            case,
            "a string map",
            decode::<BTreeMap<String, RawJson>>(bytes),
            line,
            column,
        );
        assert_not_utf8(
            case,
            "a seeded RawJson",
            decode_seed(bytes, PhantomData::<RawJson>),
            line,
            column,
        );
    }
}

#[test]
fn the_utf8_check_comes_before_the_depth_check() {
    let mut deep = nested_array(MAX_JSON_DEPTH + 1).into_bytes();
    deep.insert(1, 0xC9);
    let error = decode::<IgnoredAny>(&deep).expect_err("neither UTF-8 nor shallow");
    assert_eq!(error.kind(), DecodeErrorKind::Syntax, "{error:?}");
    assert_eq!((error.line(), error.column()), (1, 2));
}

#[test]
fn a_document_that_is_utf8_decodes() {
    let raw =
        decode::<RawJson>("{\"caf\u{e9}\":\"\u{1F600}\"}".as_bytes()).expect("valid UTF-8 JSON");
    assert_eq!(raw.as_str(), "{\"caf\u{e9}\":\"\u{1F600}\"}");
}

/// The input the fuzzer found: a validation error list whose `msg` holds 87
/// bytes of 0xC9, each the start of a two-byte character that never comes.
fn fuzzer_find() -> Vec<u8> {
    let mut body =
        b"{\"detail\":[{\"loc\":[\"body\",\"questions\",\"spam\",0],\"msg\":\"fi".to_vec();
    body.extend(std::iter::repeat_n(0xC9, 87));
    body.extend_from_slice(b"eld required\",\"type\":\"missing\"},{\"loc\":\"x\",\"msg\":7},3]}");
    body
}

#[test]
fn an_error_body_that_is_not_utf8_becomes_its_own_lossy_message() {
    use crate::error::{ApiError, ApiErrorKind};

    let found = fuzzer_find();
    assert_eq!(found.len(), 199, "the fuzzer's input is 199 bytes");
    let bodies: [(&str, &[u8]); 3] = [
        ("a string member", b"{\"error\":\"\xC9\"}"),
        ("a message inside a detail list", &found),
        ("a key", b"{\"\xC9\":\"x\"}"),
    ];
    for (case, body) in bodies {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, http::HeaderValue::from_static("3"));
        let error = ApiError::new(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            Bytes::copy_from_slice(body),
            headers,
            None,
        );
        assert_eq!(error.kind(), ApiErrorKind::UnprocessableEntity, "{case}");
        assert_eq!(error.body(), body, "{case}: the body is kept byte for byte");
        assert_eq!(error.error_type(), None, "{case}");
        // Not JSON, so the body is the message as text, with each byte that
        // is not UTF-8 replaced by U+FFFD; every such body here is under the
        // 200-character cut.
        assert_eq!(error.message(), String::from_utf8_lossy(body), "{case}");
        assert_eq!(
            crate::error::parse_retry_after(error.headers(), std::time::SystemTime::UNIX_EPOCH),
            Some(std::time::Duration::from_secs(3)),
            "{case}: the headers are still read"
        );
    }
    let small = ApiError::new(
        http::StatusCode::UNPROCESSABLE_ENTITY,
        Bytes::from_static(b"{\"error\":\"\xC9\"}"),
        http::HeaderMap::new(),
        None,
    );
    assert_eq!(small.message(), "{\"error\":\"\u{FFFD}\"}");
    assert_eq!(small.to_string(), "422 {\"error\":\"\u{FFFD}\"}");
}

#[test]
fn a_success_body_with_a_legend_object_that_is_not_utf8_is_a_validation_error() {
    let body: &[u8] =
        b"{\"model\":\"m\",\"usage\":{},\"answers\":{\"s\":{\"type\":\"score\",\"score\":0,\
        \"confidence\":0,\"legend\":{\"0\":{\"a\":\"\xC9\"}},\"probabilities\":{\"0\":1}}}}";
    let column = 1 + body.iter().position(|&byte| byte == 0xC9).expect("the bad byte is there");
    let failure = crate::de::decode_system_one_with::<crate::response::Answers>(
        Bytes::copy_from_slice(body),
        http::StatusCode::OK,
        http::HeaderMap::new(),
        crate::de::AnswerContext::new(1),
        None,
    )
    .expect_err("a body that is not UTF-8 does not decode");
    let crate::ErrorKind::ResponseValidation(error) = failure.kind() else {
        panic!("expected a response-validation error, got {failure:?}");
    };
    assert_eq!(error.field_path(), "");
    assert_eq!(error.decode_error().kind(), DecodeErrorKind::Syntax);
    assert_eq!((error.decode_error().line(), error.decode_error().column()), (1, column));
    assert_eq!(error.body(), body, "the body is kept byte for byte");
    assert_eq!(error.message(), "Invalid response data at ''.");
}
