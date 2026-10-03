//! Tests for the selected engine: a caller's raw JSON goes into a request
//! body byte for byte with either engine.
//!
//! The value is encoded through `codec::encode_into`, the one entry every
//! caller value takes (`Content::json`, a request's state and extra members,
//! a question field), and compared with what serde_json writes for it. With
//! the default engine that comparison is the engine against itself; with
//! `sonic` it is the parity the bridge exists for. The last tests compare
//! the bridge with the engine it wraps on values without a raw value: the
//! bridge passes every other call on unchanged.

use std::collections::BTreeMap;

use proptest::prelude::*;
use serde::Serialize;
use serde_json::value::RawValue;

use crate::codec;

fn sdk_encoded<T: Serialize + ?Sized>(value: &T) -> String {
    let mut buffer = Vec::new();
    codec::encode_into(&mut buffer, value).expect("the SDK encodes the value");
    String::from_utf8(buffer).expect("the codec emits UTF-8")
}

fn serde_json_encoded<T: Serialize + ?Sized>(value: &T) -> String {
    serde_json::to_string(value).expect("serde_json encodes the value")
}

fn raw(text: &str) -> Box<RawValue> {
    RawValue::from_string(text.to_owned()).expect("the test text is one JSON value")
}

/// One JSON value of every kind, spelled with spacing and number forms that
/// a re-rendering engine would change.
fn every_kind() -> Vec<String> {
    // Built at run time: a JSON escape written into a source file through a
    // tool can arrive as the character it stands for.
    let escaped = format!(r#""caf{b}u00e9 {b}"quoted{b}" {b}{b}""#, b = '\\');
    vec![
        r#"{ "b" : [ 1 , 2 ], "a" : { } }"#.to_owned(),
        r#"[ 1, "x" ,null , [ ] ]"#.to_owned(),
        escaped,
        r#""plain""#.to_owned(),
        "1.50".to_owned(),
        "1E2".to_owned(),
        "-0".to_owned(),
        "123456789012345678901234567890".to_owned(),
        "true".to_owned(),
        "false".to_owned(),
        "null".to_owned(),
    ]
}

#[derive(Serialize)]
struct Holder<'a> {
    label: &'static str,
    value: &'a RawValue,
    boxed: Box<RawValue>,
    maybe: Option<&'a RawValue>,
    list: Vec<&'a RawValue>,
    map: BTreeMap<&'static str, &'a RawValue>,
}

#[derive(Serialize)]
enum Tagged<'a> {
    Newtype(&'a RawValue),
    Tuple(u8, &'a RawValue),
    Fields { inner: &'a RawValue },
}

#[test]
fn a_raw_value_of_every_kind_is_written_as_serde_json_writes_it() {
    for text in every_kind() {
        let value = raw(&text);
        // RawValue keeps the text it was built from, inner spacing included;
        // the engines write that text unchanged.
        assert_eq!(value.get(), text, "construction kept the text");
        assert_eq!(sdk_encoded(&*value), text, "top level, borrowed");
        assert_eq!(sdk_encoded(&value), text, "top level, boxed");
        assert_eq!(serde_json_encoded(&*value), text, "serde_json's own spelling");
    }
}

#[test]
fn a_raw_value_nested_anywhere_is_written_as_serde_json_writes_it() {
    for text in every_kind() {
        let value = raw(&text);
        let holder = Holder {
            label: "kept",
            value: &value,
            boxed: value.clone(),
            maybe: Some(&value),
            list: vec![&value, &value],
            map: BTreeMap::from([("k", &*value)]),
        };
        let expected = format!(
            r#"{{"label":"kept","value":{t},"boxed":{t},"maybe":{t},"list":[{t},{t}],"map":{{"k":{t}}}}}"#,
            t = text
        );
        assert_eq!(serde_json_encoded(&holder), expected, "serde_json's spelling");
        assert_eq!(sdk_encoded(&holder), expected, "the SDK's spelling");

        for (tagged, expected) in [
            (Tagged::Newtype(&value), format!(r#"{{"Newtype":{text}}}"#)),
            (Tagged::Tuple(7, &value), format!(r#"{{"Tuple":[7,{text}]}}"#)),
            (Tagged::Fields { inner: &value }, format!(r#"{{"Fields":{{"inner":{text}}}}}"#)),
        ] {
            assert_eq!(serde_json_encoded(&tagged), expected, "serde_json's spelling");
            assert_eq!(sdk_encoded(&tagged), expected, "the SDK's spelling");
        }

        let deep = vec![BTreeMap::from([(
            "outer",
            Some(Holder {
                label: "deep",
                value: &value,
                boxed: value.clone(),
                maybe: None,
                list: Vec::new(),
                map: BTreeMap::new(),
            }),
        )])];
        assert_eq!(sdk_encoded(&deep), serde_json_encoded(&deep), "three levels down");
    }
}

#[test]
fn spacing_outside_the_value_is_dropped_when_the_raw_value_is_built() {
    // Both engines write `RawValue::get` unchanged, so what a body carries
    // is what construction kept: the spacing inside the value, not around it.
    let value = raw(" \n[ 1 ,\t2 ]\r\n ");
    assert_eq!(value.get(), "[ 1 ,\t2 ]");
    assert_eq!(sdk_encoded(&value), "[ 1 ,\t2 ]");
    assert_eq!(serde_json_encoded(&value), "[ 1 ,\t2 ]");
}

#[test]
fn a_raw_value_number_keeps_its_spelling_with_either_number_mode() {
    // The arbitrary-precision run sets the variable; the probe proves the
    // feature is on in that run, so the spelling below is checked in both.
    let arbitrary =
        std::env::var("DECISION_MODEL_SDK_TEST_ARBITRARY_PRECISION").as_deref() == Ok("1");
    let probe = serde_json::to_string(
        &serde_json::from_str::<serde_json::Value>("1E2").expect("the feature probe parses"),
    )
    .expect("the feature probe serializes");
    assert_eq!(probe, if arbitrary { "1e+2" } else { "100.0" }, "probe of the number mode");

    for text in ["1E2", "0.50", "-0", "1e-7", "18446744073709551616", "-9.999999999999999e400"] {
        let value = raw(text);
        assert_eq!(sdk_encoded(&value), text);
        assert_eq!(sdk_encoded(&[&*value]), format!("[{text}]"));
        assert_eq!(serde_json_encoded(&[&*value]), format!("[{text}]"));
    }
}

#[test]
fn a_raw_value_map_key_is_refused_by_both_engines() {
    // Neither engine writes a struct as an object key; the bridge leaves the
    // key path alone, so the refusal stays.
    struct RawKey(Box<RawValue>);
    impl Serialize for RawKey {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut map = serializer.serialize_map(Some(1))?;
            map.serialize_entry(&*self.0, &1)?;
            map.end()
        }
    }
    let map = RawKey(raw(r#""k""#));
    let mut buffer = Vec::new();
    assert!(codec::encode_into(&mut buffer, &map).is_err(), "the SDK refuses it");
    assert!(serde_json::to_string(&map).is_err(), "serde_json refuses it");
}

#[test]
fn a_struct_that_only_resembles_the_raw_value_protocol_is_an_ordinary_object() {
    // A field named like serde_json's token in a struct of another name is
    // data, and stays data with either engine.
    struct Lookalike;
    impl Serialize for Lookalike {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeStruct;
            let mut fields = serializer.serialize_struct("Lookalike", 1)?;
            fields.serialize_field("$serde_json::private::RawValue", "[1]")?;
            fields.end()
        }
    }
    let expected = r#"{"$serde_json::private::RawValue":"[1]"}"#;
    assert_eq!(sdk_encoded(&Lookalike), expected);
    assert_eq!(serde_json_encoded(&Lookalike), expected);
}

/// sonic-rs's own raw type is spliced only by sonic-rs. The default engine
/// writes it as the one-field object its protocol looks like to serde_json;
/// this pins that the splice changes nothing under the default engine.
#[test]
fn a_sonic_lazy_value_is_raw_with_sonic_and_an_object_by_default() {
    let lazy: sonic_rs::LazyValue<'_> =
        sonic_rs::from_str(r#"[ 1, "x" ]"#).expect("sonic-rs reads the text");
    #[cfg(feature = "sonic")]
    assert_eq!(sdk_encoded(&lazy), r#"[ 1, "x" ]"#);
    #[cfg(not(feature = "sonic"))]
    assert_eq!(sdk_encoded(&lazy), r#"{"$sonic_rs::LazyValue":"[ 1, \"x\" ]"}"#);
}

/// A JSON document of bounded size, with every kind of value in it.
fn json_document() -> impl Strategy<Value = serde_json::Value> {
    let leaf = prop_oneof![
        Just(serde_json::Value::Null),
        any::<bool>().prop_map(serde_json::Value::Bool),
        any::<i64>().prop_map(serde_json::Value::from),
        (-1.0e9..1.0e9_f64).prop_map(serde_json::Value::from),
        "[a-z\"\\\\ \u{e9}\u{2028}\n]{0,8}".prop_map(serde_json::Value::String),
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(serde_json::Value::Array),
            prop::collection::btree_map("[a-z]{0,3}", inner, 0..4)
                .prop_map(|map| serde_json::Value::Object(map.into_iter().collect())),
        ]
    })
}

proptest! {
    #[test]
    fn any_document_spliced_in_raw_is_written_as_serde_json_writes_it(
        document in json_document(),
        pretty in any::<bool>(),
    ) {
        let text = if pretty {
            serde_json::to_string_pretty(&document)
        } else {
            serde_json::to_string(&document)
        }
        .expect("serde_json renders the document");
        let value = raw(&text);
        let nested = BTreeMap::from([("state", vec![Some(&*value)])]);
        prop_assert_eq!(sdk_encoded(&value), text.clone());
        prop_assert_eq!(sdk_encoded(&nested), serde_json_encoded(&nested));
    }
}

#[derive(Serialize)]
struct Newtype<'a>(&'a RawValue);

#[derive(Serialize)]
struct Pair<'a>(u8, &'a RawValue);

/// A map written one key and one value at a time, as a hand-written
/// `Serialize` may write it; the standard maps write whole entries.
struct SplitEntry<'a>(&'a RawValue);

impl Serialize for SplitEntry<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_key("k")?;
        map.serialize_value(self.0)?;
        map.end()
    }
}

#[test]
fn a_raw_value_in_a_newtype_a_tuple_struct_or_a_split_map_entry_is_written_as_serde_json_writes_it()
{
    for text in every_kind() {
        let value = raw(&text);
        assert_eq!(serde_json_encoded(&Newtype(&value)), text, "serde_json, newtype struct");
        assert_eq!(sdk_encoded(&Newtype(&value)), text, "the SDK, newtype struct");

        let pair = format!("[7,{text}]");
        assert_eq!(serde_json_encoded(&Pair(7, &value)), pair, "serde_json, tuple struct");
        assert_eq!(sdk_encoded(&Pair(7, &value)), pair, "the SDK, tuple struct");

        let split = format!(r#"{{"k":{text}}}"#);
        assert_eq!(serde_json_encoded(&SplitEntry(&value)), split, "serde_json, split entry");
        assert_eq!(sdk_encoded(&SplitEntry(&value)), split, "the SDK, split entry");
    }
}

/// `value` written through `to_writer`, the bridge included under `sonic`.
fn bridged<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    let mut buffer = Vec::new();
    super::to_writer(&mut buffer, value).map_err(|error| error.to_string())?;
    Ok(String::from_utf8(buffer).expect("the codec emits UTF-8"))
}

/// `value` written by the engine's own serializer, with no bridge.
fn unbridged<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    let mut buffer = Vec::new();
    let mut serializer = super::Serializer::new(&mut buffer);
    value.serialize(&mut serializer).map_err(|error| error.to_string())?;
    Ok(String::from_utf8(buffer).expect("the engine emits UTF-8"))
}

/// A value written through `Serializer::collect_str`.
struct Collected<D>(D);

impl<D: std::fmt::Display> Serialize for Collected<D> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// A `Display` that writes its text in pieces that need escaping.
struct Pieces;

impl std::fmt::Display for Pieces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for piece in ["say ", "\"", "caf\u{e9}", "\"", "\n", "\u{1}"] {
            formatter.write_str(piece)?;
        }
        Ok(())
    }
}

/// A `Display` that writes part of its text and then fails, which breaks the
/// contract of `Display`: the engines panic on it.
struct Failing;

impl std::fmt::Display for Failing {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("partial")?;
        Err(std::fmt::Error)
    }
}

#[test]
fn a_value_without_a_raw_value_is_written_as_the_engine_itself_writes_it() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let address = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
    let socket = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 8080);
    let cases = [
        ("address", bridged(&address), unbridged(&address)),
        ("socket", bridged(&socket), unbridged(&socket)),
        ("i128 min", bridged(&i128::MIN), unbridged(&i128::MIN)),
        ("i128 max", bridged(&i128::MAX), unbridged(&i128::MAX)),
        ("u128 max", bridged(&u128::MAX), unbridged(&u128::MAX)),
        ("collected", bridged(&Collected(Pieces)), unbridged(&Collected(Pieces))),
        ("in a list", bridged(&[address]), unbridged(&[address])),
    ];
    for (label, bridged, unbridged) in cases {
        assert_eq!(bridged, unbridged, "{label}");
    }

    // Both engines are human-readable, so an address is its text; a
    // serializer that says otherwise is given the octets instead.
    assert_eq!(bridged(&address).as_deref(), Ok(r#""192.0.2.1""#));
    assert_eq!(bridged(&socket).as_deref(), Ok(r#""[::1]:8080""#));
    // Both engines write integers wider than 64 bits as numbers; serde's
    // fallback for a serializer that has no such method refuses them.
    assert_eq!(bridged(&i128::MIN), Ok(i128::MIN.to_string()));
    assert_eq!(bridged(&u128::MAX), Ok(u128::MAX.to_string()));
    // Built at run time, as in `every_kind`; the non-ASCII letter is written
    // as it is and the control character as a six-character escape.
    let escaped = format!(r#""say {b}"caf{e}{b}"{b}n{b}u0001""#, b = '\\', e = '\u{e9}');
    assert_eq!(bridged(&Collected(Pieces)), Ok(escaped));
}

#[test]
fn a_display_that_fails_panics_as_it_does_in_the_engine_itself() {
    fn panic_text(encode: impl FnOnce() -> Result<String, String>) -> Option<String> {
        let payload = std::panic::catch_unwind(std::panic::AssertUnwindSafe(encode)).err()?;
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
    }

    // The engine's own text, not the standard library's for a failing
    // `to_string`: the bridge hands the `Display` to the engine's writer.
    let unbridged = panic_text(|| unbridged(&Collected(Failing)));
    assert!(unbridged.is_some(), "the engine panics on a failing Display");
    assert_eq!(panic_text(|| bridged(&Collected(Failing))), unbridged);
}
