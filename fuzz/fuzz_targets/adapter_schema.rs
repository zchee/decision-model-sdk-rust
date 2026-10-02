//! Arbitrary text as a question set, through the adapter's schema writer in
//! both answer modes.
//!
//! The property has two parts. The writer returns - `Ok` with a schema or
//! `Err` for a question set the adapter does not take - and never panics,
//! aborts or hangs. And a schema it returns is the text of one JSON object,
//! whatever the question names, instructions and criteria hold: that text is
//! sent to a vendor as the schema of a request and pasted into the system
//! message of a prompted one, so a name or a description that escaped its
//! string would change what the model is asked.
//!
//! The whole input is the question set, the JSON object
//! `PreparedQuestions::as_json` writes; nothing is split off. The writer
//! takes text, so an input that is not UTF-8 is skipped.

#![no_main]

use std::hint::black_box;

use libfuzzer_sys::fuzz_target;
use serde_json::{Map, Value};
use system_one_adapter::{__internals::schema::schema, AnswerMode};

fuzz_target!(|data: &[u8]| {
    let Ok(questions) = str::from_utf8(data) else { return };

    for mode in [AnswerMode::Probabilities, AnswerMode::Discrete] {
        match schema(questions, mode) {
            Ok(schema) => {
                let text = schema.as_str();
                if let Err(error) = serde_json::from_str::<Map<String, Value>>(text) {
                    panic!("the {mode:?} schema is not a JSON object: {error}");
                }
            }
            Err(error) => {
                black_box(error.to_string());
                black_box(format!("{error:?}"));
            }
        }
    }
});
