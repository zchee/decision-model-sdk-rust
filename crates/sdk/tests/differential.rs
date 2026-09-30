//! The SDK's codec against the other JSON engine on the same documents:
//! serde_json with the `sonic` feature, sonic-rs otherwise.
//!
//! Decoding: every fixture, then 10,000 generated documents whose numbers are
//! written as decimal text - long mantissas, large and small exponents,
//! subnormals, negative zero - and whose strings carry every escape JSON has.
//! Both codecs decode each document into the same probe type, and numbers are
//! compared by their bits. Where the two disagree, the disagreement has to be
//! one of the [`Divergence`]s listed below; anything else fails the test.
//!
//! Encoding: 10,000 generated `state` values are encoded by the SDK and parsed
//! back by the reference engine, and must come back as exactly the value that went in,
//! apart from non-finite floats, which both codecs write as `null`, and the
//! reference sonic-rs parser's loss of a literal negative zero's sign.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt::{self, Write as _},
    path::Path,
    time::Instant,
};

use proptest::{
    collection::vec,
    prelude::*,
    sample::select,
    test_runner::{Config, TestCaseError, TestRunner},
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, MapAccess, SeqAccess, Visitor},
    ser::{SerializeMap, SerializeSeq},
};
use typesafe_sdk::{
    __internals as sdk, Content, DecodeError, DecodeErrorKind,
    models::ModelMetadata,
    response::{Answer, Answers, Usage},
};

#[cfg(feature = "sonic")]
use serde_json as reference_codec;
#[cfg(not(feature = "sonic"))]
use sonic_rs as reference_codec;

#[cfg(feature = "sonic")]
const REFERENCE_CODEC: &str = "serde_json";
#[cfg(not(feature = "sonic"))]
const REFERENCE_CODEC: &str = "sonic-rs";

/// Generated cases per property. Kept at the full count in every profile; the
/// test prints how long each property took.
const CASES: u32 = 10_000;

/// The nesting depth past which the SDK's codec refuses a document.
const MAX_DEPTH: usize = sdk::MAX_JSON_DEPTH;

// ------------------------------------------------------------ the probe

/// A JSON value as a codec reported it: which visitor method it called, and
/// with what. A float is kept as its bits, so `0.1` and the double nearest to
/// it are only equal if they are the same double.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Json {
    Null,
    Bool(bool),
    U64(u64),
    I64(i64),
    F64(u64),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Json, E> {
        Ok(Json::U64(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Json, E> {
        Ok(Json::I64(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Json, E> {
        Ok(Json::F64(value.to_bits()))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Json, E> {
        Ok(Json::Str(value.to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        let Some(key) = map.next_key::<String>()? else {
            return Ok(Json::Object(Vec::new()));
        };
        if key == "$serde_json::private::Number" {
            let text: String = map.next_value()?;
            if map.next_key::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("a number token must be the only entry"));
            }
            if let Ok(value) = text.parse::<u64>() {
                return Ok(Json::U64(value));
            }
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Json::I64(value));
            }
            let value = text
                .parse::<f64>()
                .map_err(|_| <A::Error as de::Error>::custom("number out of range"))?;
            if !value.is_finite() {
                return Err(de::Error::custom("number out of range"));
            }
            return Ok(Json::F64(value.to_bits()));
        }
        let mut members = vec![(key, map.next_value()?)];
        while let Some(entry) = map.next_entry::<String, Json>()? {
            members.push(entry);
        }
        Ok(Json::Object(members))
    }
}

// ----------------------------------------------------------- divergences

/// Every way the two codecs are allowed to disagree. Anything not listed here
/// fails the test.
///
/// A disagreement where one codec refuses a document is accepted only when
/// the refusing codec's own error names the listed cause, so that a document
/// carrying one of these features cannot hide a refusal for another reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Divergence {
    /// A `\u` escape of half a surrogate pair, when only the reference
    /// refuses and names a surrogate error. At the pinned versions both
    /// engines refuse the measured cases in either backend direction.
    LoneSurrogate,
    /// A number beyond the largest double, when only the reference refuses
    /// and names a range error. Both engines refuse 1e400 and -1e309, bare
    /// and in an array, at the pinned versions.
    BeyondF64,
    /// An integer beyond u64/i64, read as f64 by both engines. A one-sided
    /// reference refusal is accepted only when its error names the range.
    IntegerAboveU64,
    /// A document nested deeper than the SDK parses at all. Accepted when the
    /// SDK's error is [`DecodeErrorKind::TooDeep`].
    ///
    /// The SDK's error names only a class of failure, and a syntax error
    /// could come from any literal, so this is the only cause accepted for a
    /// document the SDK alone refuses.
    NestingDepth,
    /// A literal negative zero: sonic-rs reads -0, -0.0 and -0.0e5 as
    /// positive zero, whichever side it is on. With arbitrary_precision,
    /// serde_json instead reads the integer spelling -0 as I64(0).
    NegativeZeroSign,
}

/// What a generated document contains that could explain a disagreement.
#[derive(Debug, Default, Clone, Copy)]
struct Features {
    lone_surrogate: bool,
    beyond_f64: bool,
    integer_above_u64: bool,
    depth: usize,
}

impl Features {
    /// The divergence that explains the SDK alone refusing this document: the
    /// depth cap, when the document crosses it and the SDK says so.
    fn explain_sdk_refusal(self, error: &DecodeError) -> Option<Divergence> {
        (self.depth > MAX_DEPTH && error.kind() == DecodeErrorKind::TooDeep)
            .then_some(Divergence::NestingDepth)
    }

    /// The divergence the reference engine's own error names.
    fn explain_reference_refusal(self, error: &reference_codec::Error) -> Option<Divergence> {
        let message = error.to_string();
        let said = |prefix: &str| message.starts_with(prefix);
        #[cfg(feature = "sonic")]
        let surrogate =
            said("lone leading surrogate in hex escape") || said("unexpected end of hex escape");
        #[cfg(not(feature = "sonic"))]
        let surrogate = said("Invalid surrogate Unicode code point");
        if self.lone_surrogate && surrogate {
            return Some(Divergence::LoneSurrogate);
        }
        #[cfg(feature = "sonic")]
        let range = said("number out of range");
        #[cfg(not(feature = "sonic"))]
        let range = said("Number is bigger than the maximum value of its type")
            || said("Float number must be finite, not be Infinity or NaN");
        if range {
            if self.beyond_f64 {
                return Some(Divergence::BeyondF64);
            }
            if self.integer_above_u64 {
                return Some(Divergence::IntegerAboveU64);
            }
        }
        None
    }
}

/// Where two decodes of the same document part ways, if they do.
fn first_difference(
    sdk: &Json,
    reference: &Json,
    path: &mut String,
) -> Option<(String, Json, Json)> {
    match (sdk, reference) {
        (Json::Array(left), Json::Array(right)) if left.len() == right.len() => {
            left.iter().zip(right).enumerate().find_map(|(index, (left, right))| {
                let mark = path.len();
                write!(path, "[{index}]").expect("writing to a String cannot fail");
                let found = first_difference(left, right, path);
                path.truncate(mark);
                found
            })
        }
        (Json::Object(left), Json::Object(right)) if left.len() == right.len() => {
            left.iter().zip(right).find_map(|((left_key, left), (right_key, right))| {
                if left_key != right_key {
                    return Some((
                        path.clone(),
                        Json::Str(left_key.clone()),
                        Json::Str(right_key.clone()),
                    ));
                }
                let mark = path.len();
                write!(path, ".{left_key:?}").expect("writing to a String cannot fail");
                let found = first_difference(left, right, path);
                path.truncate(mark);
                found
            })
        }
        (left, right) if left == right => None,
        (left, right) => Some((path.clone(), left.clone(), right.clone())),
    }
}

/// Whether the values differ only by the selected engines' negative-zero rules.
fn is_negative_zero_read_as_positive(sdk: &Json, reference: &Json) -> bool {
    #[cfg(feature = "sonic")]
    {
        matches!(reference, Json::F64(bits) if *bits == (-0.0_f64).to_bits())
            && matches!(sdk, Json::F64(0) | Json::U64(0) | Json::I64(0))
    }
    #[cfg(not(feature = "sonic"))]
    {
        matches!(reference, Json::F64(0) | Json::U64(0) | Json::I64(0))
            && (matches!(sdk, Json::F64(bits) if *bits == (-0.0_f64).to_bits())
                || matches!(sdk, Json::I64(0) | Json::U64(0)))
    }
}

/// Replaces each reference zero affected by that rule with the SDK's reading,
/// so the rest of the document can still be compared.
fn without_negative_zero(sdk: &Json, reference: &Json) -> Json {
    match (sdk, reference) {
        (Json::Array(left), Json::Array(right)) if left.len() == right.len() => Json::Array(
            left.iter()
                .zip(right)
                .map(|(left, right)| without_negative_zero(left, right))
                .collect(),
        ),
        (Json::Object(left), Json::Object(right)) if left.len() == right.len() => Json::Object(
            left.iter()
                .zip(right)
                .map(|((_, left), (key, right))| (key.clone(), without_negative_zero(left, right)))
                .collect(),
        ),
        (left, right) if is_negative_zero_read_as_positive(left, right) => left.clone(),
        (_, right) => right.clone(),
    }
}

/// Decodes `text` with both codecs and says whether they agree, or which
/// listed divergence explains why they do not.
fn judge(text: &str, features: Features) -> Result<Option<Divergence>, String> {
    let sdk = sdk::decode::<Json>(text.as_bytes());
    let reference = reference_codec::from_str::<Json>(text);
    let explained = match (&sdk, &reference) {
        (Ok(sdk), Ok(reference)) => {
            let Some((path, left, right)) = first_difference(sdk, reference, &mut String::new())
            else {
                return Ok(None);
            };
            let normalized = without_negative_zero(sdk, reference);
            if first_difference(sdk, &normalized, &mut String::new()).is_none() {
                return Ok(Some(Divergence::NegativeZeroSign));
            }
            return Err(format!(
                "the codecs disagree at `{path}`: sdk {left:?}, {REFERENCE_CODEC} {right:?}"
            ));
        }
        (Err(_), Err(_)) => return Ok(None),
        (Err(refused), Ok(_)) => features.explain_sdk_refusal(refused),
        (Ok(_), Err(refused)) => features.explain_reference_refusal(refused),
    };
    explained.map(Some).ok_or_else(|| {
        format!(
            "only one codec accepts the document: sdk {:?}, {REFERENCE_CODEC} {:?}",
            sdk.as_ref().map(|_| "ok").map_err(ToString::to_string),
            reference.as_ref().map(|_| "ok").map_err(ToString::to_string),
        )
    })
}

// ---------------------------------------------------- generated documents

/// One character of a JSON string as it is spelled in the document.
#[derive(Debug, Clone)]
enum Piece {
    /// A character written as itself.
    Plain(char),
    /// One of the two-character escapes: `\"`, `\\`, `\/`, `\b`, `\f`, `\n`,
    /// `\r`, `\t`.
    Escape(char),
    /// A `\uXXXX` escape of a character in the basic plane.
    Unit(u16),
    /// A surrogate pair, as two `\u` escapes.
    Pair(u16, u16),
    /// Half a surrogate pair on its own.
    Lone(u16),
}

/// A JSON document as it is written, before it is text.
#[derive(Debug, Clone)]
enum Node {
    Null,
    Bool(bool),
    Number(String),
    Str(Vec<Piece>),
    Array(Vec<Node>),
    Object(Vec<(Vec<Piece>, Node)>),
}

/// Number literals worth trying on every run, on top of the random ones.
const EDGE_NUMBERS: &[&str] = &[
    "0",
    "-0",
    "0.0",
    "-0.0",
    "-0.0e5",
    "0e0",
    "1e-7",
    "0.1",
    "0.30000000000000004",
    "5e-324",
    "4.9406564584124654e-324",
    "2.2250738585072011e-308",
    "2.2250738585072014e-308",
    "1.7976931348623157e308",
    "1e308",
    "1e309",
    "-1e309",
    "1e400",
    "1e-400",
    "-1e-400",
    "18446744073709551615",
    "18446744073709551616",
    "-9223372036854775808",
    "-9223372036854775809",
    "9007199254740993",
    "123456789012345678901234567890",
    "1.234567890123456789012345678901",
    "0.000000000000000000000000000001",
];

fn number() -> impl Strategy<Value = String> {
    let decimal = proptest::string::string_regex(
        "-?(0|[1-9][0-9]{0,25})(\\.[0-9]{1,30})?([eE][+-]?[0-9]{1,3})?",
    )
    .expect("the number pattern is a valid regex");
    prop_oneof![
        8 => decimal,
        2 => select(EDGE_NUMBERS).prop_map(str::to_owned),
    ]
}

fn piece() -> impl Strategy<Value = Piece> {
    prop_oneof![
        60 => any::<char>()
            .prop_filter("a raw control character, quote or backslash is not valid inside a JSON string", |c| {
                *c >= ' ' && *c != '"' && *c != '\\'
            })
            .prop_map(Piece::Plain),
        20 => select(&['"', '\\', '/', 'b', 'f', 'n', 'r', 't'][..]).prop_map(Piece::Escape),
        20 => any::<u16>().prop_filter("a surrogate is not a character", |unit| !(0xD800..0xE000).contains(unit)).prop_map(Piece::Unit),
        10 => (0xD800_u16..0xDC00, 0xDC00_u16..0xE000).prop_map(|(high, low)| Piece::Pair(high, low)),
        1 => (0xD800_u16..0xE000).prop_map(Piece::Lone),
    ]
}

fn text() -> impl Strategy<Value = Vec<Piece>> {
    vec(piece(), 0..10)
}

fn document() -> impl Strategy<Value = Node> {
    let leaf = prop_oneof![
        1 => Just(Node::Null),
        1 => any::<bool>().prop_map(Node::Bool),
        3 => number().prop_map(Node::Number),
        2 => text().prop_map(Node::Str),
    ];
    let tree = leaf.prop_recursive(6, 48, 6, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..6).prop_map(Node::Array),
            vec((text(), inner), 0..6).prop_map(Node::Object),
        ]
    });
    // Most documents are as deep as they grew; some are wrapped in enough
    // arrays to cross the depth the SDK refuses at.
    (tree, prop_oneof![9 => Just(0_usize), 1 => 8_usize..=24]).prop_map(|(mut node, wraps)| {
        for _ in 0..wraps {
            node = Node::Array(vec![node]);
        }
        node
    })
}

fn render_text(pieces: &[Piece], out: &mut String) {
    out.push('"');
    for piece in pieces {
        match piece {
            Piece::Plain(character) => out.push(*character),
            Piece::Escape(character) => {
                out.push('\\');
                out.push(*character);
            }
            Piece::Unit(unit) | Piece::Lone(unit) => {
                write!(out, "\\u{unit:04x}").expect("writing to a String cannot fail");
            }
            Piece::Pair(high, low) => {
                write!(out, "\\u{high:04X}\\u{low:04x}").expect("writing to a String cannot fail");
            }
        }
    }
    out.push('"');
}

/// Writes `node` as JSON text and notes what it contains.
fn render(node: &Node, depth: usize, out: &mut String, features: &mut Features) {
    match node {
        Node::Null => out.push_str("null"),
        Node::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Node::Number(literal) => {
            out.push_str(literal);
            let integer = !literal.contains(['.', 'e', 'E']);
            if integer && literal.parse::<u64>().is_err() && literal.parse::<i64>().is_err() {
                features.integer_above_u64 = true;
            }
            if literal.parse::<f64>().is_ok_and(f64::is_infinite) {
                features.beyond_f64 = true;
            }
        }
        Node::Str(pieces) => {
            features.lone_surrogate |= pieces.iter().any(|piece| matches!(piece, Piece::Lone(_)));
            render_text(pieces, out);
        }
        Node::Array(items) => {
            features.depth = features.depth.max(depth + 1);
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                render(item, depth + 1, out, features);
            }
            out.push(']');
        }
        Node::Object(members) => {
            features.depth = features.depth.max(depth + 1);
            out.push('{');
            for (index, (key, value)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                features.lone_surrogate |= key.iter().any(|piece| matches!(piece, Piece::Lone(_)));
                render_text(key, out);
                out.push_str(": ");
                render(value, depth + 1, out, features);
            }
            out.push('}');
        }
    }
}

// ------------------------------------------------------------ the tests

fn runner() -> TestRunner {
    println!("reference codec: {REFERENCE_CODEC}");
    TestRunner::new(Config { cases: CASES, failure_persistence: None, ..Config::default() })
}

#[test]
fn every_fixture_decodes_the_same_through_both_codecs() {
    println!("reference codec: {REFERENCE_CODEC}");
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut compared = 0;
    for entry in std::fs::read_dir(&directory).expect("the fixture directory is readable") {
        let path = entry.expect("a directory entry is readable").path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a fixture is UTF-8 text");
        let name = path.display();

        assert_eq!(judge(&text, Features::default()), Ok(None), "fixture {name}");

        let sdk: Json = sdk::decode(text.as_bytes()).expect("the fixture decodes");
        let Json::Object(members) = sdk else { panic!("fixture {name} is not an object") };
        if members.iter().any(|(key, _)| key == "answers") {
            let sdk = sdk::decode::<ResponseProbe>(text.as_bytes()).expect("the fixture decodes");
            let reference: ResponseProbe =
                reference_codec::from_str(&text).expect("the fixture decodes");
            assert_eq!(sdk.normalized(), reference.normalized(), "fixture {name}");
        } else {
            let sdk = sdk::decode::<ModelsProbe>(text.as_bytes()).expect("the fixture decodes");
            let reference: ModelsProbe =
                reference_codec::from_str(&text).expect("the fixture decodes");
            assert_eq!(sdk, reference, "fixture {name}");
        }
        compared += 1;
    }
    assert!(compared >= 7, "only {compared} fixtures were found in {}", directory.display());
}

/// The public response types, read through `Deserialize` alone, so that the
/// same readers run under both codecs.
#[derive(Debug, Deserialize)]
struct ResponseProbe {
    model: String,
    usage: Usage,
    answers: Answers,
}

#[derive(Debug, PartialEq, Deserialize)]
struct ModelsProbe {
    models: Vec<ModelMetadata>,
}

impl ResponseProbe {
    /// Every value as plain data. A legend description that is an object or an
    /// array is held as the JSON text it arrived as, and a foreign codec
    /// re-renders that text, so descriptions are compared as parsed values.
    fn normalized(&self) -> (String, Usage, Vec<(String, Json)>) {
        let answers = self
            .answers
            .iter()
            .map(|(name, answer)| {
                let value = match answer {
                    Answer::Noul(noul) => Json::Array(vec![Json::F64(noul.noul().to_bits())]),
                    Answer::Choice(choice) => Json::Array(vec![
                        Json::Str(choice.choice().to_owned()),
                        Json::F64(choice.confidence().to_bits()),
                        Json::Object(
                            choice
                                .probabilities()
                                .map(|(option, p)| (option.to_owned(), Json::F64(p.to_bits())))
                                .collect(),
                        ),
                    ]),
                    Answer::Score(score) => Json::Array(vec![
                        Json::F64(score.score().to_bits()),
                        Json::F64(score.confidence().to_bits()),
                        Json::Array(
                            score
                                .legend()
                                .map(|(level, text)| {
                                    Json::Array(vec![Json::U64(level.into()), parsed(text)])
                                })
                                .collect(),
                        ),
                        Json::Array(
                            score
                                .probabilities()
                                .map(|(level, p)| {
                                    Json::Array(vec![
                                        Json::U64(level.into()),
                                        Json::F64(p.to_bits()),
                                    ])
                                })
                                .collect(),
                        ),
                    ]),
                    other => panic!("an answer kind this test does not know: {other:?}"),
                };
                (name.to_owned(), value)
            })
            .collect();
        (self.model.clone(), self.usage, answers)
    }
}

/// A legend description as data.
fn parsed(content: &Content<'static>) -> Json {
    match (content.as_text(), content.as_json()) {
        (Some(text), _) => Json::Str(text.to_owned()),
        (None, Some(raw)) => {
            reference_codec::from_str(raw.as_str()).expect("held JSON text parses")
        }
        (None, None) => panic!("content that is neither text nor JSON: {content:?}"),
    }
}

#[test]
fn generated_documents_decode_the_same_or_diverge_only_as_listed() {
    let started = Instant::now();
    let agreed = Cell::new(0_u32);
    let diverged = RefCell::new(BTreeMap::<Divergence, u32>::new());

    let outcome = runner().run(&document(), |node| {
        let mut text = String::new();
        let mut features = Features::default();
        render(&node, 0, &mut text, &mut features);
        match judge(&text, features) {
            Ok(None) => agreed.set(agreed.get() + 1),
            Ok(Some(divergence)) => *diverged.borrow_mut().entry(divergence).or_default() += 1,
            Err(reason) => return Err(TestCaseError::fail(format!("{reason}\ndocument: {text}"))),
        }
        Ok(())
    });

    println!(
        "{CASES} documents in {:.2?}: {} agreed, divergences {:?}",
        started.elapsed(),
        agreed.get(),
        diverged.borrow()
    );
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
    assert!(
        agreed.get() >= CASES * 3 / 4,
        "only {} of {CASES} documents were compared without an accepted divergence",
        agreed.get()
    );
}

/// Which of the listed divergences the pinned codec versions actually have.
/// Two of them occur. The other three are listed because they are where JSON
/// parsers commonly part ways, and a codec upgrade may make them occur; at the
/// pinned versions both codecs refuse the document or read the same double, so
/// the judge sees agreement.
#[test]
fn only_negative_zero_and_nesting_depth_divergences_occur() {
    let too_deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    let cases: [(&str, Features, Option<Divergence>); 10] = [
        ("[-0.0, 1]", Features::default(), Some(Divergence::NegativeZeroSign)),
        ("[-0]", Features::default(), Some(Divergence::NegativeZeroSign)),
        ("{\"a\": -0.0e5}", Features::default(), Some(Divergence::NegativeZeroSign)),
        (
            &too_deep,
            Features { depth: MAX_DEPTH + 1, ..Features::default() },
            Some(Divergence::NestingDepth),
        ),
        ("[\"\\ud800\"]", Features { lone_surrogate: true, ..Features::default() }, None),
        ("[\"\\udc00\"]", Features { lone_surrogate: true, ..Features::default() }, None),
        ("[1e400]", Features { beyond_f64: true, ..Features::default() }, None),
        ("[-1e309]", Features { beyond_f64: true, ..Features::default() }, None),
        (
            "[18446744073709551616]",
            Features { integer_above_u64: true, ..Features::default() },
            None,
        ),
        (
            "[-9223372036854775809]",
            Features { integer_above_u64: true, ..Features::default() },
            None,
        ),
    ];

    for (text, features, expected) in cases {
        assert_eq!(judge(text, features), Ok(expected), "document {text}");
    }

    let depth_16 = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
    assert_eq!(judge(&depth_16, Features::default()), Ok(None), "16 levels decode in both codecs");
    assert_eq!(
        judge(&too_deep, Features::default()),
        Err(format!(
            "only one codec accepts the document: sdk Err(\"JSON input is nested deeper than the maximum of 16\"), \
             {REFERENCE_CODEC} Ok(\"ok\")"
        )),
        "a disagreement the document's features do not explain fails the test"
    );
}

/// A one-sided refusal is accepted only for the cause the refusing codec's
/// own error names. A document that carries a listed feature cannot excuse a
/// refusal for another reason: before this check, the first feature the
/// document carried was taken as the cause without looking at the error.
#[test]
fn a_one_sided_refusal_is_accepted_only_for_the_cause_its_error_names() {
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
    let too_deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
    let every_feature =
        Features { lone_surrogate: true, beyond_f64: true, integer_above_u64: true, depth: 30 };
    let refused = |features: Features| {
        format!(
            "only one codec accepts the document: sdk Err(\"JSON input is nested deeper than the \
             maximum of 16\"), {REFERENCE_CODEC} Ok(\"ok\") ({features:?})"
        )
    };

    // The SDK refuses on depth; only the depth feature explains that.
    assert_eq!(judge(&too_deep, every_feature), Ok(Some(Divergence::NestingDepth)));
    for features in [
        Features { lone_surrogate: true, ..Features::default() },
        Features { beyond_f64: true, ..Features::default() },
        Features { integer_above_u64: true, ..Features::default() },
    ] {
        assert_eq!(
            judge(&too_deep, features).map_err(|reason| format!("{reason} ({features:?})")),
            Err(refused(features)),
        );
    }

    // What each codec's error names, taken on its own.
    let sdk_error = |text: &str| sdk::decode::<Json>(text.as_bytes()).expect_err("the SDK refuses");
    let reference_error = |text: &str| {
        reference_codec::from_str::<Json>(text).expect_err("the reference codec refuses")
    };
    let depth_only = Features { depth: MAX_DEPTH + 1, ..Features::default() };

    assert_eq!(
        every_feature.explain_sdk_refusal(&sdk_error(&too_deep)),
        Some(Divergence::NestingDepth)
    );
    assert_eq!(Features::default().explain_sdk_refusal(&sdk_error(&too_deep)), None);
    for syntax in ["[1e400]", "[\"\\ud800\"]", "[1,]"] {
        let error = sdk_error(syntax);
        // With arbitrary_precision the parser yields a number token; this
        // probe's visitor rejects infinity as Data instead of parser Syntax.
        let kind = if arbitrary && syntax == "[1e400]" {
            DecodeErrorKind::Data
        } else {
            DecodeErrorKind::Syntax
        };
        assert_eq!(error.kind(), kind, "document {syntax}");
        assert_eq!(every_feature.explain_sdk_refusal(&error), None, "document {syntax}");
    }

    let rows: [(&str, Features, Option<Divergence>); 9] = [
        ("[\"\\ud800\"]", every_feature, Some(Divergence::LoneSurrogate)),
        ("[\"\\udc00\"]", every_feature, Some(Divergence::LoneSurrogate)),
        ("[\"\\ud800\\n\"]", every_feature, Some(Divergence::LoneSurrogate)),
        ("[\"\\ud800\"]", Features { beyond_f64: true, ..depth_only }, None),
        ("[1e400]", every_feature, Some(Divergence::BeyondF64)),
        (
            "[1e400]",
            Features { integer_above_u64: true, ..Features::default() },
            Some(Divergence::IntegerAboveU64),
        ),
        ("[1e400]", Features { lone_surrogate: true, ..depth_only }, None),
        ("[1,]", every_feature, None),
        ("[\"a\u{1}b\"]", every_feature, None),
    ];
    for (text, features, expected) in rows {
        assert_eq!(
            features.explain_reference_refusal(&reference_error(text)),
            expected,
            "document {text:?}, {features:?}, {REFERENCE_CODEC} said {}",
            reference_error(text)
        );
    }
}

// ------------------------------------------------------ encoding a state

/// A `state` value a caller could hand the SDK.
#[derive(Debug, Clone)]
enum State {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    Text(String),
    List(Vec<State>),
    Map(Vec<(String, State)>),
}

impl Serialize for State {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_unit(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::I64(value) => serializer.serialize_i64(*value),
            Self::U64(value) => serializer.serialize_u64(*value),
            Self::F64(value) => serializer.serialize_f64(*value),
            Self::Text(value) => serializer.serialize_str(value),
            Self::List(items) => {
                let mut out = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    out.serialize_element(item)?;
                }
                out.end()
            }
            Self::Map(members) => {
                let mut out = serializer.serialize_map(Some(members.len()))?;
                for (key, value) in members {
                    out.serialize_entry(key, value)?;
                }
                out.end()
            }
        }
    }
}

impl State {
    /// What a reader must see after this value is written as JSON.
    fn expected(&self) -> Json {
        match self {
            Self::Null => Json::Null,
            Self::Bool(value) => Json::Bool(*value),
            Self::I64(value) => u64::try_from(*value).map_or(Json::I64(*value), Json::U64),
            Self::U64(value) => Json::U64(*value),
            Self::F64(value) if value.is_finite() => {
                let bits = value.to_bits();
                #[cfg(not(feature = "sonic"))]
                let bits = if *value == 0.0 { 0 } else { bits };
                Json::F64(bits)
            }
            Self::F64(_) => Json::Null,
            Self::Text(value) => Json::Str(value.clone()),
            Self::List(items) => Json::Array(items.iter().map(Self::expected).collect()),
            Self::Map(members) => Json::Object(
                members.iter().map(|(key, value)| (key.clone(), value.expected())).collect(),
            ),
        }
    }
}

fn state() -> impl Strategy<Value = State> {
    let leaf = prop_oneof![
        1 => Just(State::Null),
        1 => any::<bool>().prop_map(State::Bool),
        1 => any::<i64>().prop_map(State::I64),
        1 => any::<u64>().prop_map(State::U64),
        3 => any::<f64>().prop_map(State::F64),
        1 => select(&[0.0, -0.0, f64::MIN_POSITIVE, 5e-324, f64::MAX, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.1, 1e21, 1e-7][..])
            .prop_map(State::F64),
        3 => any::<String>().prop_map(State::Text),
    ];
    leaf.prop_recursive(6, 48, 6, |inner| {
        prop_oneof![
            vec(inner.clone(), 0..6).prop_map(State::List),
            vec((any::<String>(), inner), 0..6).prop_map(State::Map),
        ]
    })
}

#[test]
fn generated_states_encode_to_json_that_reads_back_as_the_same_value() {
    let started = Instant::now();
    let outcome = runner().run(&state(), |state| {
        let expected = state.expected();

        let mut encoded = Vec::new();
        sdk::encode_into(&mut encoded, &state)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let read_back: Json = reference_codec::from_slice(&encoded).map_err(|error| {
            TestCaseError::fail(format!(
                "{REFERENCE_CODEC} cannot read the SDK's output: {error}\n{}",
                String::from_utf8_lossy(&encoded)
            ))
        })?;
        prop_assert_eq!(
            &read_back,
            &expected,
            "the SDK wrote {}",
            String::from_utf8_lossy(&encoded)
        );

        let reference = reference_codec::to_vec(&state)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        let reference_read_back: Json = reference_codec::from_slice(&reference)
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert_eq!(
            &reference_read_back,
            &expected,
            "{} wrote {}",
            REFERENCE_CODEC,
            String::from_utf8_lossy(&reference)
        );
        Ok(())
    });
    println!("{CASES} states in {:.2?}", started.elapsed());
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
}
