//! What decoding one response costs in allocations.
//!
//! The budget is stated in dhat's `total_blocks` and `total_bytes` deltas, and
//! it is measured on the identical decodes after one warm-up of the same
//! shape, exactly as the encode budget is: [`support::RUNS`] of them, held to
//! the stable minimum, for the reason `support` gives.
//!
//! The fixture is the upstream `RESULT` (three answers: a noul, a choice, a
//! score). Every block it costs is one the answer representation asks for -
//! decoding it into a fully borrowed type costs none - so the count below is a
//! count of `Vec`s and of strings too long to be stored inline. The names (the
//! model, the question names, the choice's pick and option names) are stored
//! inline up to 24 bytes on a 64-bit target, and every name in the fixture is
//! shorter; the level descriptions are `Content`, which owns a `String`:
//!
//! | item | blocks |
//! | --- | ---: |
//! | model name (inline) | 0 |
//! | answers vector, sized from the question count | 1 |
//! | three question names (inline) | 0 |
//! | the choice's pick (inline) | 0 |
//! | the choice's probabilities vector | 1 |
//! | the choice's two option names (inline) | 0 |
//! | the score's legend vector | 1 |
//! | the score's three level descriptions | 3 |
//! | the score's probabilities vector | 1 |
//! | **total** | **7** |
//!
//! One profiler exists per process, so all of it runs in a single test.

mod support;

use std::fmt;

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use serde::{
    Deserialize,
    de::{self, Deserializer, IgnoredAny, MapAccess, Visitor},
};
use typesafe_sdk::{
    __internals as sdk,
    de::{AnswerContext, AnswerSet},
    response::{Answers, ChoiceAnswer, NoulAnswer, ScoreAnswer, SystemOneResponse},
};

use crate::support::{Measured, measure_min};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `RESULT` of `tests/test_clients.py:42-56`, as the upstream test sends it.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// The number of questions the request asked.
const QUESTIONS: usize = 3;

/// The decode budget: blocks, bytes, and the ratio to the comparator's blocks.
/// The block count is exact, so one more `String` or `Vec` fails it. The byte
/// bound is the measured 578 bytes plus 12.5%, rounded down, close to the
/// headroom the first budget had (700 over a measured 626, 11.8%), so a
/// regression of 100 bytes that adds no block fails it.
const MAX_BLOCKS: u64 = 7;
const MAX_BYTES: u64 = 650;
const MAX_RATIO_TO_NAIVE: f64 = 0.7;

/// Decodes the fixture once through `decode` to warm up and reports what each
/// identical decode after it costs. The response and the inputs are made
/// outside the measured section: a response body arrives as `Bytes` and the
/// headers as a `HeaderMap`, and both are moved in, not copied.
fn second_decode<T, F>(label: &str, decode: F) -> Measured
where
    T: fmt::Debug,
    F: Fn(Bytes, HeaderMap) -> T,
{
    drop(decode(Bytes::from_static(RESULT), HeaderMap::new()));

    let (measured, value) = measure_min(
        label,
        || (Bytes::from_static(RESULT), HeaderMap::new()),
        |(body, headers)| decode(body, headers),
    );
    println!("{label:<44} blocks={:>3} bytes={:>5}", measured.blocks, measured.bytes);
    drop(value);
    measured
}

// ------------------------------------------------------ naive comparator

// A comparator that is not a strawman: the same codec, answers internally
// tagged by `type` in a `HashMap<String, _>`, maps for every container.
#[path = "../benches/support/naive_response.rs"]
mod naive_response;

use naive_response::NaiveResponse;

// ------------------------------------------------- a struct answer set

#[path = "support/ticket.rs"]
mod ticket;

use ticket::Ticket;

// ------------------------------------------------------------------ test

#[test]
fn decoding_the_three_answer_fixture_stays_within_its_budget() {
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
    let _profiler = dhat::Profiler::builder().testing().build();

    let answers = second_decode("SystemOneResponse<Answers>", |body, headers| {
        sdk::decode_system_one::<Answers>(body, StatusCode::OK, headers, QUESTIONS)
            .expect("the fixture decodes")
    });
    let naive = if arbitrary {
        println!(
            "naive comparator skipped: serde_json arbitrary_precision turns floats into token maps \
             an internally tagged derive cannot read"
        );
        None
    } else {
        Some(second_decode("naive: #[serde(tag)] answers in a HashMap", |_, _| {
            sdk::decode::<NaiveResponse>(RESULT).expect("the fixture decodes as the comparator")
        }))
    };
    let typed = second_decode("SystemOneResponse<Ticket>, field dispatch", |body, headers| {
        let response: SystemOneResponse<Ticket> =
            sdk::decode_system_one(body, StatusCode::OK, headers, QUESTIONS)
                .expect("the fixture decodes as a Ticket");
        response
    });

    if let Some(naive) = &naive {
        let ratio = answers.blocks as f64 / naive.blocks as f64;
        println!(
            "budget: blocks {} <= {MAX_BLOCKS}, bytes {} <= {MAX_BYTES}, ratio to naive {ratio:.2} <= \
             {MAX_RATIO_TO_NAIVE}; struct set {} < {}",
            answers.blocks, answers.bytes, typed.blocks, answers.blocks
        );
    } else {
        println!(
            "budget: blocks {} <= {MAX_BLOCKS}, bytes {} <= {MAX_BYTES}; struct set {} < {}",
            answers.blocks, answers.bytes, typed.blocks, answers.blocks
        );
    }

    assert!(
        answers.blocks <= MAX_BLOCKS,
        "decoding into Answers cost {} blocks, over the budget of {MAX_BLOCKS}",
        answers.blocks
    );
    assert!(
        answers.bytes <= MAX_BYTES,
        "decoding into Answers allocated {} bytes, over the budget of {MAX_BYTES}",
        answers.bytes
    );
    if let Some(naive) = naive {
        let ratio = answers.blocks as f64 / naive.blocks as f64;
        assert!(
            ratio <= MAX_RATIO_TO_NAIVE,
            "decoding into Answers cost {} blocks against the comparator's {}: a ratio of {ratio:.2}",
            answers.blocks,
            naive.blocks
        );
    }
    assert!(
        typed.blocks < answers.blocks,
        "a struct answer set cost {} blocks, not fewer than Answers' {}",
        typed.blocks,
        answers.blocks
    );

    // The cheaper decode has to be the same decode: each field holds the
    // answer the lookup finds under its name.
    let runtime: SystemOneResponse<Answers> = sdk::decode_system_one(
        Bytes::from_static(RESULT),
        StatusCode::OK,
        HeaderMap::new(),
        QUESTIONS,
    )
    .expect("the fixture decodes");
    let ticket: SystemOneResponse<Ticket> = sdk::decode_system_one(
        Bytes::from_static(RESULT),
        StatusCode::OK,
        HeaderMap::new(),
        QUESTIONS,
    )
    .expect("the fixture decodes as a Ticket");
    assert_eq!(Some(&ticket.answers().spam), runtime.answers().noul("spam"));
    assert_eq!(Some(&ticket.answers().tone), runtime.answers().choice("tone"));
    assert_eq!(Some(&ticket.answers().quality), runtime.answers().score("quality"));
}
