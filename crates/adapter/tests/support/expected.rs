//! The responses upstream recorded under `tests/fixtures/expected_responses`,
//! the comparison of a response with one of them, and the two question sets
//! the recorded cases ask, transcribed from
//! `U:tests/test_client_with_live_apis.py:38-85`.
//!
//! `compare` applies upstream's field rules (`U:tests/test_client_with_live_apis.py:127-141`)
//! and the field-limited rules of the attempt-trace deviation (plan section
//! 10, row 11), so that a response this crate builds from a cassette's wire
//! body can equal the file upstream wrote from its SDK's dump.
//!
//! This file uses `first_difference` of `cassette.rs`: the including test
//! binary declares both, as `mod cassette` and `mod expected`.

use serde_json::Value;
use typesafe_sdk::question::{Choice, Noul, PreparedQuestions, Questions, Score};

use crate::cassette::{Difference, first_difference};

/// The file stems under `tests/fixtures/expected_responses`: the 12 vendor
/// cases of `test_live_responses_match_reference_shape`, which the provider
/// tests compare with, 4 per provider.
pub(crate) const EXPECTED: [&str; 13] = [
    "test_live_responses_match_reference_shape[probabilities-prompted-openai]",
    "test_live_responses_match_reference_shape[probabilities-prompted-anthropic]",
    "test_live_responses_match_reference_shape[probabilities-prompted-gemini]",
    "test_live_responses_match_reference_shape[probabilities-native-openai]",
    "test_live_responses_match_reference_shape[probabilities-native-anthropic]",
    "test_live_responses_match_reference_shape[probabilities-native-gemini]",
    "test_live_responses_match_reference_shape[discrete-prompted-openai]",
    "test_live_responses_match_reference_shape[discrete-prompted-anthropic]",
    "test_live_responses_match_reference_shape[discrete-prompted-gemini]",
    "test_live_responses_match_reference_shape[discrete-native-openai]",
    "test_live_responses_match_reference_shape[discrete-native-anthropic]",
    "test_live_responses_match_reference_shape[discrete-native-gemini]",
    // Deliberately unread: the TypeSafe API's response, not the adapter's
    // (plan section 7.2). It is listed so that every file of the directory is
    // named.
    "test_live_typesafe_response_matches_reference_shape",
];

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/expected_responses");

/// Reads the expected response `name`, one of `EXPECTED`.
///
/// # Panics
///
/// Panics, naming the file, if `name` is not listed or the file is not JSON.
pub(crate) fn read(name: &str) -> Value {
    assert!(EXPECTED.contains(&name), "`{name}` is not one of the listed expected responses");
    let path = format!("{DIR}/{name}.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Compares `found`, a response serialized to JSON, with `expected`, one of
/// the files, and names the first differing JSON pointer.
///
/// Upstream's rules, in its order: the latency is wall-clock, so it is taken
/// out of `found` after checking that it is above 0 and below 120 seconds;
/// null members are dropped from every `llm_response` on both sides.
///
/// Then row 11 of the deviations: numbers compare by value (the OpenAI dump
/// holds `created_at` and `completed_at` as floats where the wire body holds
/// integers); `debug_info.provider` compares by vendor only, because upstream
/// names a Python class (`system_one_adapter.providers.<vendor>.<Class>`,
/// the synchronous one in prompted mode) where this crate names a Rust type
/// path (`..::provider::<vendor>::..`); for a Gemini attempt, the members on
/// one side only are left out of the comparison on both sides: `id` and
/// `output_text` of `llm_response` (in the dump only), and
/// `model_invocation_token_counts`, `non_grounding_model_invocation_token_counts`
/// and `raw_prompt_token` of `llm_response.usage` (on the wire only). No file holds an
/// `error_type`, so it has no rule.
pub(crate) fn compare(found: &Value, expected: &Value) -> Result<(), Difference> {
    let mut found = found.clone();
    let mut expected = expected.clone();
    if let Some(latency) = found
        .get_mut("usage")
        .and_then(Value::as_object_mut)
        .and_then(|usage| usage.remove("latency"))
        && !latency.as_f64().is_some_and(|seconds| seconds > 0.0 && seconds < 120.0)
    {
        return Err(Difference {
            pointer: "/usage/latency".to_owned(),
            detail: format!("{latency} is not a latency above 0 and below 120 seconds"),
        });
    }
    for response in [&mut found, &mut expected] {
        for attempt in attempts(response) {
            if let Some(llm_response) = attempt.get_mut("llm_response") {
                drop_nulls(llm_response);
            }
            if let Some(provider) = attempt.pointer_mut("/debug_info/provider")
                && let Some(name) = provider.as_str().and_then(vendor)
            {
                *provider = Value::String(name.to_owned());
            }
            if attempt.pointer("/debug_info/provider").and_then(Value::as_str) == Some("gemini") {
                remove(attempt, "/llm_response", &["id", "output_text"]);
                remove(attempt, "/llm_response/usage", &GEMINI_WIRE_ONLY);
            }
        }
    }
    first_difference(&expected, &found).map_or(Ok(()), Err)
}

/// The members of a Gemini `llm_response.usage` that the wire body holds and
/// upstream's dump does not.
const GEMINI_WIRE_ONLY: [&str; 3] = [
    "model_invocation_token_counts",
    "non_grounding_model_invocation_token_counts",
    "raw_prompt_token",
];

fn attempts(response: &mut Value) -> &mut [Value] {
    match response.pointer_mut("/debug/llm_attempts") {
        Some(Value::Array(attempts)) => attempts,
        _ => &mut [],
    }
}

/// Upstream's `_without_null_fields`: null object members go, recursively;
/// null array items stay.
fn drop_nulls(value: &mut Value) {
    match value {
        Value::Object(members) => {
            members.retain(|_, member| !member.is_null());
            members.values_mut().for_each(drop_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(drop_nulls),
        _ => {}
    }
}

/// The vendor a provider name holds, from either language's form.
fn vendor(provider: &str) -> Option<&str> {
    let (_, rest) = provider
        .split_once("system_one_adapter.providers.")
        .or_else(|| provider.split_once("::provider::"))?;
    let (vendor, _) = rest.split_once(['.', ':'])?;
    (!vendor.is_empty()).then_some(vendor)
}

fn remove(attempt: &mut Value, pointer: &str, members: &[&str]) {
    if let Some(Value::Object(object)) = attempt.pointer_mut(pointer) {
        for member in members {
            object.remove(*member);
        }
    }
}

/// The document of `test_live_responses_match_reference_shape`
/// (`U:tests/test_client_with_live_apis.py:38-42`).
pub(crate) const STATE: &str = concat!(
    "The reviewer calls this entirely invented novel about dragons and wizards a ",
    "flawless masterpiece and the best book they have ever read. They say it has no ",
    "weaknesses, offer only unreserved praise, and urge everyone to read it.",
);

/// The questions asked of `STATE` (`U:tests/test_client_with_live_apis.py:43-62`).
pub(crate) fn questions() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The book review is positive."))
        .score(
            "rating",
            Score::new([
                "The reviewer condemns the book and urges readers to avoid it.",
                "The reviewer is mostly critical and does not recommend the book.",
                "The reviewer expresses mixed or neutral feelings about the book.",
                "The reviewer praises the book overall while noting meaningful flaws.",
                "The reviewer offers unreserved praise and an emphatic recommendation.",
            ])
            .instructions("How favorable the reviewer's overall assessment is."),
        )
        .choice(
            "genre",
            Choice::new(["fiction", "nonfiction"])
                .option("fiction", "A novel or short story.")
                .option("nonfiction", "A book based on facts, real events, or ideas.")
                .instructions("Which genre this review is about."),
        )
        .prepare()
        .expect("invariant: upstream's question set is valid")
}

/// The document of `test_live_models_follow_question_instructions_and_criteria`
/// (`U:tests/test_client_with_live_apis.py:63-69`).
pub(crate) const CONTEXT_PROBE_STATE: &str = "Catalog facts:
- marker_fen has state DORMANT.
- marker_tor has state ACTIVE.

Shipping facts:
- The parcel's handling class is CLASS_CRYSTAL.
";

/// The questions asked of `CONTEXT_PROBE_STATE`
/// (`U:tests/test_client_with_live_apis.py:70-85`).
pub(crate) fn context_probe_questions() -> PreparedQuestions {
    Questions::new()
        .choice(
            "instruction_probe",
            Choice::new(["marker_fen", "marker_tor"])
                .option("marker_fen", "The marker_fen catalog entry.")
                .option("marker_tor", "The marker_tor catalog entry.")
                .instructions("Return the only marker whose state is ACTIVE."),
        )
        .choice(
            "criteria_probe",
            Choice::new(["route_7q", "route_2m"])
                .option("route_7q", "Use when the handling class is CLASS_CRYSTAL.")
                .option("route_2m", "Use when the handling class is CLASS_STEEL.")
                .instructions("Return the correct opaque handling route for the parcel."),
        )
        .prepare()
        .expect("invariant: upstream's question set is valid")
}
