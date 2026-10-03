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
use std::path::{Path, PathBuf};

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

/// The cassette `name` as plain JSON, read without the reader.
fn raw(name: &str) -> Value {
    let path = format!("{FIXTURES}/cassettes/{name}.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// The recorded request of the cassette `name`, read without the reader.
fn recorded_request(name: &str) -> (Method, Uri, Vec<u8>) {
    let path = format!("{FIXTURES}/cassettes/{name}.json");
    let file = raw(name);
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

/// The public path the built-in provider of `vendor` returns from
/// `Provider::type_name()`.
fn public_path(vendor: &str) -> &'static str {
    match vendor {
        "openai" => "decision_model_adapter::OpenAiProvider",
        "anthropic" => "decision_model_adapter::AnthropicProvider",
        "gemini" => "decision_model_adapter::GeminiProvider",
        other => panic!("no built-in provider of the vendor `{other}`"),
    }
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
        // `llm_response`, a Rust type path as the provider (first the form of
        // a provider that does not override `type_name()`), and a latency.
        let cassette = Cassette::read(name).unwrap_or_else(|error| panic!("{error}"));
        let mut found = response.clone();
        found["usage"]["latency"] = json!(0.25);
        let attempt = found.pointer_mut("/debug/llm_attempts/0").expect("one attempt");
        attempt["llm_response"] =
            serde_json::from_slice(&cassette.response.body).expect("a JSON body");
        attempt["debug_info"]["provider"] =
            json!(format!("decision_model_adapter::provider::{vendor}::Provider"));
        assert_eq!(expected::compare(&found, &response), Ok(()), "{name}: the wire form");

        // A built-in provider names its public path instead: the same
        // vendor's is equal, another vendor's is not.
        let other = if vendor == "openai" { "gemini" } else { "openai" };
        found["debug"]["llm_attempts"][0]["debug_info"]["provider"] = json!(public_path(vendor));
        assert_eq!(expected::compare(&found, &response), Ok(()), "{name}: the public path");
        found["debug"]["llm_attempts"][0]["debug_info"]["provider"] = json!(public_path(other));
        assert_eq!(
            expected::compare(&found, &response).map_err(|difference| difference.pointer),
            Err("/debug/llm_attempts/0/debug_info/provider".to_owned()),
            "{name}: the public path of `{other}`",
        );
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
                json!("decision_model_adapter::provider::openai::OpenAi");
        }),
        Err("/debug/llm_attempts/0/debug_info/provider".to_owned()),
    );
    // The scope of row 11's exclusions. Of a Gemini `llm_response`, only the
    // five listed members are left out: its `status` is compared.
    assert_eq!(
        difference(&|found| {
            found["debug"]["llm_attempts"][0]["llm_response"]["status"] = json!("incomplete");
        }),
        Err("/debug/llm_attempts/0/llm_response/status".to_owned()),
    );
    // The Gemini exclusion is Gemini's: an OpenAI `llm_response.id` is compared.
    let openai =
        expected::read("test_live_responses_match_reference_shape[probabilities-native-openai]");
    let mut found = openai.clone();
    found["debug"]["llm_attempts"][0]["llm_response"]["id"] = json!("resp_other");
    assert_eq!(
        expected::compare(&found, &openai).map_err(|difference| difference.pointer),
        Err("/debug/llm_attempts/0/llm_response/id".to_owned()),
    );
    // Only `debug_info.provider` is reduced to its vendor: a class path
    // against a Rust path of the same vendor in `model_name` is a difference.
    let mut expected_form = response.clone();
    expected_form["debug"]["llm_attempts"][0]["debug_info"]["model_name"] =
        json!("system_one_adapter.providers.gemini.AsyncGeminiProvider");
    let mut found = response.clone();
    found["debug"]["llm_attempts"][0]["debug_info"]["model_name"] =
        json!("decision_model_adapter::provider::gemini::Provider");
    assert_eq!(
        expected::compare(&found, &expected_form).map_err(|difference| difference.pointer),
        Err("/debug/llm_attempts/0/debug_info/model_name".to_owned()),
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

/// The label the refusal tests give a changed copy of a cassette.
const COPY: &str = "changed-copy.json";

/// The reader's error for `changed`, a changed copy of a cassette, which must
/// name the copy and the member `member`; returns the error's text.
fn refusal(changed: &Value, member: &str) -> String {
    let text = serde_json::to_string(changed).expect("a JSON value encodes");
    match Cassette::parse(COPY, &text) {
        Ok(_) => panic!("the reader takes a copy changed at `{member}`"),
        Err(error) => {
            assert_eq!((error.file.as_str(), error.member.as_str()), (COPY, member), "{error}");
            let message = error.to_string();
            assert!(
                message.contains(&format!("`{COPY}`")) && message.contains(&format!("`{member}`")),
                "{message}"
            );
            message
        }
    }
}

#[test]
fn cassette_reader_refuses_wrong_version() {
    for version in [json!(2), json!(0), json!("1"), Value::Null] {
        let mut changed = raw(CASSETTES[0]);
        changed["version"] = version.clone();
        let message = refusal(&changed, "/version");
        assert!(message.contains("the reader knows 1"), "{version}: {message}");
    }
}

#[test]
fn cassette_reader_refuses_two_interactions() {
    let mut changed = raw(CASSETTES[0]);
    let interaction = changed["interactions"][0].clone();
    changed["interactions"].as_array_mut().expect("an array").push(interaction);
    let message = refusal(&changed, "/interactions");
    assert!(message.contains("2 interactions, a cassette holds exactly one"), "{message}");

    changed["interactions"] = json!([]);
    assert!(refusal(&changed, "/interactions").contains("0 interactions"));
}

#[test]
fn cassette_reader_refuses_string_body() {
    // VCR's own serializer stores a body as the text that crossed the wire;
    // upstream's stores the parsed object (`U:tests/conftest.py:45-75`).
    for member in ["/interactions/0/request/body", "/interactions/0/response/body/string"] {
        let mut changed = raw(CASSETTES[0]);
        let body = changed.pointer_mut(member).expect("a recorded body");
        *body = Value::String(body.to_string());
        let message = refusal(&changed, member);
        assert!(message.contains("a string is not a JSON object"), "{message}");
    }
}

#[test]
fn cassette_reader_refuses_unknown_member() {
    let mut changed = raw(CASSETTES[0]);
    changed["recorded_with"] = json!("vcrpy");
    assert!(refusal(&changed, "/recorded_with").contains("an unknown member"));
}

#[test]
fn cassette_reader_refuses_filtered_request_header() {
    // Upper case: the names are compared without regard to case.
    let names = [
        "AUTHORIZATION",
        "X-API-KEY",
        "X-GOOG-API-KEY",
        "API-KEY",
        "OPENAI-ORGANIZATION",
        "OPENAI-PROJECT",
        "COOKIE",
        "SET-COOKIE",
    ];
    for name in names {
        let mut changed = raw(CASSETTES[0]);
        changed["interactions"][0]["request"]["headers"][name] = json!(["redacted"]);
        let message = refusal(&changed, &format!("/interactions/0/request/headers/{name}"));
        assert!(message.contains("a request header upstream's recorder strips"), "{message}");
    }
}

#[test]
fn cassette_reader_refuses_filtered_query_parameter() {
    let member = "/interactions/0/request/uri";
    let with_query = |query: &str| {
        let mut changed = raw(CASSETTES[0]);
        let uri = changed.pointer_mut(member).expect("a recorded URI");
        *uri = json!(format!("{}?{query}", uri.as_str().expect("a URI is text")));
        changed
    };
    for name in ["api_key", "key"] {
        let message = refusal(&with_query(&format!("alt=json&{name}=redacted")), member);
        assert!(message.contains(&format!("the query parameter `{name}`")), "{message}");
    }
    // Only the exact names are stripped.
    let text =
        serde_json::to_string(&with_query("monkey=1&api_keys=2")).expect("a JSON value encodes");
    if let Err(error) = Cassette::parse(COPY, &text) {
        panic!("{error}");
    }
}

#[test]
fn cassette_reader_refuses_response_header_outside_allowlist() {
    for name in ["x-request-id", "openai-organization", "set-cookie"] {
        let mut changed = raw(CASSETTES[0]);
        changed["interactions"][0]["response"]["headers"][name] = json!(["recorded"]);
        let message = refusal(&changed, &format!("/interactions/0/response/headers/{name}"));
        assert!(message.contains("outside upstream's allowlist `content-type`"), "{message}");
    }
}

/// What `name_set` finds in a directory against a name list.
#[derive(Debug, PartialEq)]
struct NameSet {
    /// The file names that are not `<name>.json` of a listed name, sorted.
    unlisted: Vec<String>,
    /// The listed names without a `<name>.json` file, sorted.
    missing: Vec<String>,
}

/// Compares the entries of `dir` with `names`: every entry must be the file
/// `<name>.json` of one listed name, and every listed name must have one.
/// Any other entry (another extension, a subdirectory) is unlisted.
fn name_set(dir: &Path, names: &[&str]) -> NameSet {
    let listed: BTreeSet<String> = names.iter().map(|name| format!("{name}.json")).collect();
    let present: BTreeSet<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
        .map(|entry| {
            let entry = entry.unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
            entry.file_name().to_string_lossy().into_owned()
        })
        .collect();
    NameSet {
        unlisted: present.difference(&listed).cloned().collect(),
        missing: listed
            .difference(&present)
            .map(|file| file.strip_suffix(".json").expect("a listed file name").to_owned())
            .collect(),
    }
}

/// A directory under the system's temporary directory, named after the test
/// and the process, removed with everything in it when dropped (also when an
/// assertion fails).
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new(test: &str) -> Self {
        let path = std::env::temp_dir()
            .join(format!("decision-model-adapter-{test}-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        Self(path)
    }

    /// Creates the empty file `name` in the directory.
    fn touch(&self, name: &str) {
        let path = self.0.join(name);
        std::fs::write(&path, b"").unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("{}: not removed: {error}", self.0.display());
        }
    }
}

/// `name_set` on a scratch directory holding a file for each of `names` and
/// one unlisted file names that file; with one listed file removed, it names
/// that name as missing.
fn name_set_names_what_differs(test: &str, names: &[&str]) {
    let dir = ScratchDir::new(test);
    for name in names {
        dir.touch(&format!("{name}.json"));
    }
    dir.touch("test_live_unlisted.json");
    assert_eq!(
        name_set(&dir.0, names),
        NameSet { unlisted: vec!["test_live_unlisted.json".to_owned()], missing: Vec::new() },
    );

    let removed = names.last().expect("a name list is not empty");
    let path = dir.0.join(format!("{removed}.json"));
    std::fs::remove_file(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(
        name_set(&dir.0, names),
        NameSet {
            unlisted: vec!["test_live_unlisted.json".to_owned()],
            missing: vec![(*removed).to_owned()],
        },
    );
}

#[test]
fn cassette_name_set_equals_the_fixture_directory() {
    let set = name_set(Path::new(cassette::DIR), &CASSETTES);
    assert_eq!(set, NameSet { unlisted: Vec::new(), missing: Vec::new() }, "{}", cassette::DIR);

    // 24 vendor cases, each replayed by `tests/providers_<vendor>.rs`, and the
    // TypeSafe one, named in the list as deliberately unreplayed.
    let unreplayed: Vec<&str> =
        CASSETTES.into_iter().filter(|name| vendor(name).is_none()).collect();
    assert_eq!(CASSETTES.len() - unreplayed.len(), 24);
    assert_eq!(unreplayed, ["test_live_typesafe_response_matches_reference_shape"]);
}

#[test]
fn cassette_name_set_check_names_an_unlisted_file() {
    name_set_names_what_differs("cassette_name_set", &CASSETTES);
}

#[test]
fn expected_name_set_equals_the_fixture_directory() {
    let set = name_set(Path::new(expected::DIR), &expected::EXPECTED);
    assert_eq!(set, NameSet { unlisted: Vec::new(), missing: Vec::new() }, "{}", expected::DIR);

    // 12 vendor cases, each compared by `tests/providers_<vendor>.rs`, and the
    // TypeSafe one, named in the list as deliberately unread.
    let unread: Vec<&str> =
        expected::EXPECTED.into_iter().filter(|name| vendor(name).is_none()).collect();
    assert_eq!(expected::EXPECTED.len() - unread.len(), 12);
    assert_eq!(unread, ["test_live_typesafe_response_matches_reference_shape"]);
}

#[test]
fn expected_name_set_check_names_an_unlisted_file() {
    name_set_names_what_differs("expected_name_set", &expected::EXPECTED);
}
