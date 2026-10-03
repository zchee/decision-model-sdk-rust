//! The schema writer's output compared with the Python adapter's.
//!
//! Each prompted cassette holds the system message upstream sent, and that
//! message carries the schema text pydantic wrote. The test writes the schema
//! of the same questions and answer mode and compares the two texts byte for
//! byte.

use std::{fs, path::Path};

use decision_model_adapter::{
    __internals::schema::schema, AnswerMode, Choice, Noul, Questions, Score,
};
use serde_json::Value;

/// The first line of upstream's schema instruction and the empty line after
/// it; the schema text starts right behind them.
const INSTRUCTION_START: &str = "Return one JSON object that matches this schema exactly:\n\n";

/// The empty line after the schema text and the last line of the instruction,
/// which ends the system message.
const INSTRUCTION_END: &str =
    "\n\nDo not include text or Markdown fencing before or after the JSON object.";

/// The cassettes recorded with `structured_outputs=False`.
const PROMPTED_CASSETTES: usize = 12;

/// `QUESTIONS` of upstream's `tests/test_client_with_live_apis.py`, as the
/// JSON the SDK prepares.
fn review_questions() -> String {
    let questions = Questions::new()
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
        );
    questions.prepare().expect("the SDK accepts the review questions").as_json().to_owned()
}

/// `CONTEXT_PROBE_QUESTIONS` of upstream's
/// `tests/test_client_with_live_apis.py`, as the JSON the SDK prepares.
fn context_probe_questions() -> String {
    let questions = Questions::new()
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
        );
    questions.prepare().expect("the SDK accepts the context probe questions").as_json().to_owned()
}

/// The system message of the one request the cassette `cassette` recorded,
/// where the vendor named between the last `-` and the `]` of `name` puts it.
fn system_message<'a>(name: &str, cassette: &'a Value) -> &'a str {
    let body = &cassette["interactions"][0]["request"]["body"];
    let message = if name.ends_with("-openai].json") {
        &body["input"][0]["content"]
    } else if name.ends_with("-anthropic].json") {
        &body["system"]
    } else if name.ends_with("-gemini].json") {
        &body["system_instruction"]
    } else {
        panic!("{name}: a cassette of a vendor this test does not know");
    };
    message.as_str().unwrap_or_else(|| panic!("{name}: the system message is not a string"))
}

#[test]
fn parity_schema_prompted_cassettes() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cassettes");
    let mut names = fs::read_dir(&directory)
        .expect("the cassette directory is readable")
        .map(|entry| {
            entry.expect("a directory entry").file_name().into_string().expect("a UTF-8 file name")
        })
        .filter(|name| name.contains("-prompted-"))
        .collect::<Vec<_>>();
    names.sort_unstable();

    let mut compared = 0;
    for name in &names {
        let questions = if name.starts_with("test_live_responses_match_reference_shape[") {
            review_questions()
        } else if name.starts_with("test_live_models_follow_question_instructions_and_criteria[") {
            context_probe_questions()
        } else {
            panic!("{name}: a prompted cassette of a test this file does not transcribe");
        };
        let mode = if name.contains("[probabilities-") {
            AnswerMode::Probabilities
        } else if name.contains("[discrete-") {
            AnswerMode::Discrete
        } else {
            panic!("{name}: no answer mode in the name");
        };
        let text = fs::read_to_string(directory.join(name)).expect("the cassette is readable");
        let cassette: Value = serde_json::from_str(&text).expect("the cassette is JSON");
        let system = system_message(name, &cassette);
        let start = system.find(INSTRUCTION_START).unwrap_or_else(|| {
            panic!("{name}: the system message lacks the first line of the instruction")
        }) + INSTRUCTION_START.len();
        let recorded = system[start..].strip_suffix(INSTRUCTION_END).unwrap_or_else(|| {
            panic!("{name}: the system message does not end with the last line of the instruction")
        });

        let written = schema(&questions, mode).expect("the transcribed questions are valid");

        let written = written.as_str();
        if written != recorded {
            let offset = written
                .bytes()
                .zip(recorded.bytes())
                .position(|(ours, theirs)| ours != theirs)
                .unwrap_or_else(|| written.len().min(recorded.len()));
            panic!(
                "{name}: the schema text differs at byte {offset}\n written: {written}\nrecorded: {recorded}"
            );
        }
        compared += 1;
    }

    assert_eq!(compared, PROMPTED_CASSETTES, "the prompted cassettes compared: {names:?}");
}
