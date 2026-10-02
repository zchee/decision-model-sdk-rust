//! Tests for the selected engine: a caller's raw JSON goes into a request
//! body byte for byte with either engine.
//!
//! The value is encoded through `codec::encode_into`, the one entry every
//! caller value takes (`Content::json`, a request's state and extra members,
//! a question field), and compared with what serde_json writes for it. With
//! the default engine that comparison is the engine against itself; with
//! `sonic` it is the parity the bridge exists for.

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
    let arbitrary = std::env::var("TYPESAFE_SDK_TEST_ARBITRARY_PRECISION").as_deref() == Ok("1");
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
/// this pins that, since 0.2.1 changes nothing under the default engine.
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
