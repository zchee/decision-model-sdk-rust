//! The recorded upstream exchanges under `tests/fixtures/` replayed through
//! each built-in provider, and the responses compared with upstream's.
//!
//! The replays themselves live in `tests/providers_<vendor>.rs`. This binary
//! holds the tests of the helpers they share: the cassette reader and matcher
//! of `support/cassette.rs`, the expected-response comparison and the
//! transcribed question sets of `support/expected.rs`.

#[path = "support/cassette.rs"]
mod cassette;
#[path = "support/expected.rs"]
mod expected;

use std::collections::BTreeSet;

use cassette::{CASSETTES, Cassette, first_difference};
use http::{Method, StatusCode, Uri};
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The `.json` file stems of the fixture directory `dir`, sorted; any other
/// file fails the test.
fn stems(dir: &str) -> Vec<String> {
    let path = format!("{FIXTURES}/{dir}");
    let mut stems: Vec<String> = std::fs::read_dir(&path)
        .unwrap_or_else(|error| panic!("{path}: {error}"))
        .map(|entry| {
            let name = entry.unwrap_or_else(|error| panic!("{path}: {error}")).file_name();
            let name = name.to_str().unwrap_or_else(|| panic!("{path}: a file name is not UTF-8"));
            name.strip_suffix(".json")
                .unwrap_or_else(|| panic!("{path}: `{name}` is not a .json file"))
                .to_owned()
        })
        .collect();
    stems.sort_unstable();
    stems
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
    names.sort_unstable();
    names
}

/// The raw recorded request of the cassette `name`, read without the reader.
fn recorded_request(name: &str) -> (Method, Uri, Vec<u8>) {
    let path = format!("{FIXTURES}/cassettes/{name}.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    let file: Value = serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"));
    let member = |pointer: &str| {
        file.pointer(pointer).unwrap_or_else(|| panic!("{path}: no member `{pointer}`")).clone()
    };
    let method = member("/interactions/0/request/method");
    let method = Method::from_bytes(method.as_str().expect("a method is text").as_bytes())
        .expect("a method");
    let uri = member("/interactions/0/request/uri");
    let uri = uri.as_str().expect("a URI is text").parse::<Uri>().expect("a URI");
    let body =
        serde_json::to_vec(&member("/interactions/0/request/body")).expect("a JSON value encodes");
    (method, uri, body)
}

/// The vendor of a vendor case, `openai` for `...[discrete-native-openai]`.
fn vendor(name: &str) -> Option<&str> {
    let (_, parameters) = name.split_once('[')?;
    let (_, vendor) = parameters.strip_suffix(']')?.rsplit_once('-')?;
    Some(vendor)
}

#[test]
fn cassette_reader_reads_all() {
    assert_eq!(stems("cassettes"), sorted(&CASSETTES), "the cassette files differ from the list");

    for name in CASSETTES {
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(cassette.response.status, StatusCode::OK, "{name}");
        let response: Value =
            serde_json::from_slice(&cassette.response.body).expect("the body is JSON");
        assert!(response.is_object(), "{name}: the response body is {response}");
        assert!(cassette.request.body.is_object(), "{name}: the request body is not an object");

        let (method, uri, body) = recorded_request(name);
        if let Err(mismatch) = cassette.matches(&method, &uri, &body) {
            panic!("{name}: the recorded request does not match itself: {mismatch}");
        }
        // Scheme, host and port are not compared: a replay sends to a local
        // server.
        let local: Uri = format!("http://127.0.0.1:8080{}", uri.path()).parse().expect("a URI");
        assert!(cassette.matches(&method, &local, &body).is_ok(), "{name}: a local server's URI");
        // A request body in another member order and number spelling is the
        // same body.
        let reordered = serde_json::to_vec(&cassette.request.body).expect("a JSON value encodes");
        assert!(cassette.matches(&method, &uri, &reordered).is_ok(), "{name}: the body re-encoded");
    }

    let name = CASSETTES[0];
    let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
    let (method, uri, body) = recorded_request(name);
    let refused =
        |method: &Method, uri: &Uri, body: &[u8]| match cassette.matches(method, uri, body) {
            Ok(()) => panic!("{name}: a changed request matches"),
            Err(mismatch) => mismatch.to_string(),
        };
    let mismatch = refused(&Method::GET, &uri, &body);
    assert!(
        mismatch.contains(&format!("cassette `{name}.json`")) && mismatch.contains("method"),
        "{mismatch}"
    );
    let other_path: Uri = "https://api.openai.com/v1/chat/completions".parse().expect("a URI");
    assert!(refused(&method, &other_path, &body).contains("path `/v1/chat/completions`"));
    let with_query: Uri = format!("{uri}?stream=false").parse().expect("a URI");
    assert!(refused(&method, &with_query, &body).contains("query `stream=false`"));
    assert!(refused(&method, &uri, b"model=gpt").contains("body is not JSON"));
    let mut changed = cassette.request.body.clone();
    changed["text"]["format"]["type"] = json!("json_schema");
    let changed = serde_json::to_vec(&changed).expect("a JSON value encodes");
    let mismatch = refused(&method, &uri, &changed);
    assert!(mismatch.contains("body differs at `/text/format/type`"), "{mismatch}");

    match Cassette::read("test_live_unlisted") {
        Ok(_) => panic!("an unlisted name is read"),
        Err(error) => {
            assert_eq!(
                (error.file.as_str(), error.member.as_str()),
                ("test_live_unlisted.json", "")
            );
            assert!(error.to_string().contains("not one of the listed cassettes"), "{error}");
        }
    }
}

#[test]
fn expected_response_equals_itself_and_its_wire_form() {
    assert_eq!(
        stems("expected_responses"),
        sorted(&expected::EXPECTED),
        "the files differ from the list"
    );

    let mut wire_forms = 0;
    for name in expected::EXPECTED {
        let response = expected::read(name);
        assert_eq!(expected::compare(&response, &response), Ok(()), "{name}");
        let Some(vendor) = vendor(name) else { continue };

        // What this crate builds from the cassette: the wire body as
        // `llm_response`, a Rust type path as the provider, and a latency.
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        let mut found = response.clone();
        found["usage"]["latency"] = json!(0.25);
        let attempt = found.pointer_mut("/debug/llm_attempts/0").expect("one attempt");
        attempt["llm_response"] =
            serde_json::from_slice(&cassette.response.body).expect("a JSON body");
        attempt["debug_info"]["provider"] =
            json!(format!("system_one_adapter::provider::{vendor}::Provider"));
        assert_eq!(expected::compare(&found, &response), Ok(()), "{name}: the wire form");
        wire_forms += 1;
    }
    assert_eq!(wire_forms, 12);
}

#[test]
fn expected_response_change_names_first_pointer() {
    let response =
        expected::read("test_live_responses_match_reference_shape[probabilities-native-gemini]");
    let difference = |change: &dyn Fn(&mut Value)| {
        let mut found = response.clone();
        change(&mut found);
        expected::compare(&found, &response).map_err(|difference| difference.pointer)
    };

    assert_eq!(
        difference(&|found| found["answers"]["rating"]["probabilities"]["4"] = json!(0.5)),
        Err("/answers/rating/probabilities/4".to_owned()),
    );
    // Two changes: the first in member-name order is named.
    assert_eq!(
        difference(&|found| {
            found["model"] = json!("gemini-other");
            found["answers"]["genre"]["choice"] = json!("nonfiction");
        }),
        Err("/answers/genre/choice".to_owned()),
    );
    assert_eq!(
        difference(&|found| {
            found["usage"].as_object_mut().expect("an object").remove("n_retries");
        }),
        Err("/usage/n_retries".to_owned()),
    );
    assert_eq!(
        difference(
            &|found| found["debug"]["llm_attempts"][0]["llm_response"]["usage"]["total_tokens"] =
                json!(283)
        ),
        Err("/debug/llm_attempts/0/llm_response/usage/total_tokens".to_owned()),
    );
    assert_eq!(
        difference(&|found| {
            let attempt = found["debug"]["llm_attempts"][0].clone();
            found["debug"]["llm_attempts"].as_array_mut().expect("an array").push(attempt);
        }),
        Err("/debug/llm_attempts/1".to_owned()),
    );
    // Another vendor's provider is a difference; a Rust type path of the same
    // vendor is not.
    assert_eq!(
        difference(&|found| {
            found["debug"]["llm_attempts"][0]["debug_info"]["provider"] =
                json!("system_one_adapter::provider::openai::OpenAi");
        }),
        Err("/debug/llm_attempts/0/debug_info/provider".to_owned()),
    );
    // Numbers compare by value; a null member of `llm_response` is no member.
    assert_eq!(difference(&|found| found["answers"]["positive"]["noul"] = json!(1)), Ok(()));
    assert_eq!(
        difference(
            &|found| found["debug"]["llm_attempts"][0]["llm_response"]["error"] = Value::Null
        ),
        Ok(())
    );
    // The latency is range-checked, then left out.
    assert_eq!(difference(&|found| found["usage"]["latency"] = json!(119.5)), Ok(()));
    for latency in [json!(0), json!(120), json!("0.5")] {
        assert_eq!(
            difference(&|found| found["usage"]["latency"] = latency.clone()),
            Err("/usage/latency".to_owned()),
            "{latency}",
        );
    }

    // The message names the pointer; pointer tokens are escaped (RFC 6901).
    let difference =
        first_difference(&json!({"a/b": {"c~d": [1, 2]}}), &json!({"a/b": {"c~d": [1, 3]}}))
            .expect("the values differ");
    assert_eq!(difference.pointer, "/a~1b/c~0d/1");
    assert_eq!(difference.to_string(), "at `/a~1b/c~0d/1`: expected 2, found 3");
}

/// Every object member name and every string of `value`.
fn texts<'a>(value: &'a Value, texts: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => texts.push(text),
        Value::Array(items) => items.iter().for_each(|item| self::texts(item, texts)),
        Value::Object(members) => members.iter().for_each(|(name, member)| {
            texts.push(name);
            self::texts(member, texts);
        }),
        _ => {}
    }
}

#[test]
fn expected_question_sets_match_the_recordings() {
    let questions = expected::questions();
    let probes = expected::context_probe_questions();
    // Upstream's order, which the prompts and schemas keep.
    assert_eq!(questions.names().collect::<Vec<_>>(), ["positive", "rating", "genre"]);
    assert_eq!(probes.names().collect::<Vec<_>>(), ["instruction_probe", "criteria_probe"]);
    let order = |json: &str, first: &str, second: &str| {
        let at = |label: &str| {
            json.find(&format!("\"{label}\"")).unwrap_or_else(|| panic!("no `{label}`"))
        };
        assert!(at(first) < at(second), "`{first}` after `{second}` in {json}");
    };
    order(questions.as_json(), "fiction", "nonfiction");
    order(probes.as_json(), "marker_fen", "marker_tor");
    order(probes.as_json(), "route_7q", "route_2m");

    // The ids and labels equal the answer keys of the 12 vendor files.
    let questions_value: Value =
        serde_json::from_str(questions.as_json()).expect("the questions are JSON");
    let questions_json = questions_value.as_object().expect("an object");
    let mut compared = 0;
    for name in expected::EXPECTED.into_iter().filter(|name| vendor(name).is_some()) {
        let response = expected::read(name);
        let answers = response["answers"].as_object().expect("the answers are an object");
        let keys = |object: &serde_json::Map<String, Value>| {
            object.keys().cloned().collect::<BTreeSet<_>>()
        };
        assert_eq!(keys(answers), keys(questions_json), "{name}: the question ids");
        for (id, question) in questions_json {
            let answer = &answers[id];
            assert_eq!(answer["type"], question["type"], "{name}: `{id}`");
            let labels =
                |answer: &Value| keys(answer["probabilities"].as_object().expect("probabilities"));
            match question["type"].as_str() {
                Some("noul") => assert!(answer["noul"].is_number(), "{name}: `{id}`"),
                Some("score") => {
                    let criteria = question["criteria"].as_array().expect("a score's criteria");
                    let legend: serde_json::Map<String, Value> = criteria
                        .iter()
                        .enumerate()
                        .map(|(level, text)| (level.to_string(), text.clone()))
                        .collect();
                    assert_eq!(answer["legend"], Value::Object(legend.clone()), "{name}: `{id}`");
                    assert_eq!(labels(answer), keys(&legend), "{name}: `{id}`");
                }
                Some("choice") => {
                    let criteria = question["criteria"].as_object().expect("a choice's criteria");
                    assert_eq!(labels(answer), keys(criteria), "{name}: `{id}`");
                }
                other => panic!("{name}: `{id}` is of type {other:?}"),
            }
        }
        compared += 1;
    }
    assert_eq!(compared, 12);

    // Each vendor cassette's request carries its set's document, ids, labels,
    // instructions and criteria: the transcription is upstream's text.
    let probes_json: Value =
        serde_json::from_str(probes.as_json()).expect("the questions are JSON");
    let mut checked = 0;
    for name in CASSETTES.into_iter().filter(|name| vendor(name).is_some()) {
        let (state, set) = if name.starts_with("test_live_responses_match_reference_shape[") {
            (expected::STATE, &questions_value)
        } else {
            (expected::CONTEXT_PROBE_STATE, &probes_json)
        };
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        let mut sent = Vec::new();
        texts(&cassette.request.body, &mut sent);
        let carried = |text: &str| sent.iter().any(|sent| sent.contains(text));
        let document = serde_json::to_string(state).expect("a string encodes");
        assert!(carried(&document), "{name}: no request text holds the document {document}");
        let mut wanted = Vec::new();
        texts(set, &mut wanted);
        for text in wanted.into_iter().filter(|text| {
            !matches!(*text, "type" | "instructions" | "criteria" | "noul" | "score" | "choice")
        }) {
            assert!(carried(text), "{name}: no request text holds `{text}`");
        }
        checked += 1;
    }
    assert_eq!(checked, 24);
}
