//! What a derived question set costs in allocations.
//!
//! Budgets are dhat `total_blocks` and `total_bytes` deltas, as in
//! `alloc_decode.rs`, each held to the stable minimum of [`support::RUNS`]
//! identical runs after a warm-up, for the reason `support` gives, and one
//! profiler exists per process, so all of it runs in a single test:
//!
//! - `prepared()` allocates nothing, on its first call or any later one: the
//!   set is a `static` the compiler initialized.
//! - Decoding the upstream `RESULT` fixture into a derived set costs fewer
//!   blocks than decoding it into `Answers` (7): there is no map, no answers
//!   vector and no question name, so what is left is the model name and the
//!   answers' own storage. The names are stored inline, as in
//!   `alloc_decode.rs`:
//!
//!   | item | blocks |
//!   | --- | ---: |
//!   | model name (inline) | 0 |
//!   | the choice's pick (inline) | 0 |
//!   | the choice's probabilities vector | 1 |
//!   | the choice's two option names (inline) | 0 |
//!   | the score's legend vector | 1 |
//!   | the score's three level descriptions | 3 |
//!   | the score's probabilities vector | 1 |
//!   | **total** | **6** |
//!
//! - `Questions::prepare()` on the runtime example set keeps its cost of 3
//!   blocks: the worst-case buffer, its shrink to exact size, and the name
//!   ends.

mod support;

use std::hint::black_box;

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use typesafe_sdk::{
    __internals as sdk, Answers, Choice, ChoiceAnswer, Noul, NoulAnswer, QuestionSet, Questions,
    Score, ScoreAnswer, SystemOneResponse,
};

use crate::support::{Measured, measure, measure_min};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// AC-P2's budget for `Answers`, which a derived set must beat.
const ANSWERS_BUDGET: u64 = 7;
/// What decoding into a struct costs when the implementation is written by
/// hand (`alloc_decode.rs`); the derived one is no worse.
const HAND_WRITTEN_BLOCKS: u64 = 6;
/// `Questions::prepare()` on the runtime example set.
const PREPARE_BLOCKS: u64 = 3;
const PREPARE_BYTES: u64 = 1524;

/// The questions `RESULT` answers.
#[derive(Debug, QuestionSet)]
struct Review {
    #[noul(instructions = "Spam?")]
    spam: NoulAnswer,
    #[choice(instructions = "Tone?", options("friendly", "hostile"))]
    tone: ChoiceAnswer,
    #[score(instructions = "Quality?", levels("bad", "ok", "great"))]
    quality: ScoreAnswer,
}

/// Runs `body` once to warm up, then measures the identical runs after it,
/// as the other allocation budgets are measured.
fn second_run<F, T>(label: &str, body: F) -> Measured
where
    F: Fn() -> T,
{
    drop(body());
    let (measured, value) = measure_min(label, || (), |()| body());
    drop(value);
    println!("{label:<48} blocks={:>3} bytes={:>5}", measured.blocks, measured.bytes);
    measured
}

fn decode<A: typesafe_sdk::AnswerSet>() -> SystemOneResponse<A> {
    sdk::decode_system_one(Bytes::from_static(RESULT), StatusCode::OK, HeaderMap::new(), 3)
        .expect("the fixture decodes")
}

#[test]
fn a_derived_set_allocates_nothing_to_ask_and_less_to_decode() {
    let _profiler = dhat::Profiler::builder().testing().build();

    // The very first call: there is no initializer to run. Being the first,
    // it cannot be repeated, so it is the one section measured once.
    let (first, _) = measure(|| black_box(Review::prepared()).len());
    let (thousand, _) = measure_min(
        "prepared() x 1000 with names() walked",
        || (),
        |()| (0..1000).map(|_| black_box(Review::prepared()).names().count()).sum::<usize>(),
    );
    println!(
        "prepared(), first call                           blocks={:>3} bytes={:>5}",
        first.blocks, first.bytes
    );
    println!(
        "prepared() x 1000 with names() walked            blocks={:>3} bytes={:>5}",
        thousand.blocks, thousand.bytes
    );

    let answers = second_run("SystemOneResponse<Answers>", decode::<Answers>);
    let derived = second_run("SystemOneResponse<Review>, derived", decode::<Review>);

    // Only `prepare()` is measured: building the set it consumes is not part
    // of it, so each run's set is built before the measured section.
    let example = || {
        Questions::new()
            .noul("billing", Noul::new().instructions("Is this about billing?"))
            .choice("tone", Choice::new(["calm", "angry"]).instructions("What is the tone?"))
            .score("urgency", Score::new(["can wait", "this week", "today"]))
    };
    drop(example().prepare().expect("the runtime set is valid"));
    let (prepare, prepared) =
        measure_min("Questions::prepare(), runtime example set", example, |set| {
            set.prepare().expect("the runtime set is valid")
        });
    drop(prepared);
    println!(
        "{:<48} blocks={:>3} bytes={:>5}",
        "Questions::prepare(), runtime example set", prepare.blocks, prepare.bytes
    );

    assert_eq!(first, Measured { blocks: 0, bytes: 0 }, "the first prepared() allocated");
    assert_eq!(thousand, Measured { blocks: 0, bytes: 0 }, "prepared() allocated");
    assert!(
        derived.blocks < ANSWERS_BUDGET && derived.blocks < answers.blocks,
        "the derived set cost {} blocks, not fewer than Answers' {} (budget {ANSWERS_BUDGET})",
        derived.blocks,
        answers.blocks
    );
    assert!(
        derived.blocks <= HAND_WRITTEN_BLOCKS,
        "the derived set cost {} blocks, more than a hand-written one's {HAND_WRITTEN_BLOCKS}",
        derived.blocks
    );
    assert_eq!(
        prepare,
        Measured { blocks: PREPARE_BLOCKS, bytes: PREPARE_BYTES },
        "Questions::prepare() changed cost"
    );

    // The cheaper decode is the same decode.
    let typed = decode::<Review>();
    let looked_up = decode::<Answers>();
    assert_eq!(Some(&typed.answers().spam), looked_up.answers().noul("spam"));
    assert_eq!(Some(&typed.answers().tone), looked_up.answers().choice("tone"));
    assert_eq!(Some(&typed.answers().quality), looked_up.answers().score("quality"));
}
