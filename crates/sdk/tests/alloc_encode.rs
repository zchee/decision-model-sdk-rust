//! What one request body costs in allocations.
//!
//! The budget is stated in dhat's `total_blocks` and `total_bytes` deltas
//! rather than in live bytes, because every re-allocation is charged as one
//! block whose byte contribution is the full new size - which is exactly what
//! makes a buffer that grows inside a call visible here.
//!
//! Everything is measured after one warm-up call of the same shape. The
//! first call of a shape fills the thread's encode scratch; the SDK's steady
//! state is what follows it. Each asserted section is measured
//! [`support::RUNS`] times and held to the stable minimum, for the reason
//! `support` gives: libtest's own thread can allocate inside a section.
//!
//! One profiler exists per process, so all of it runs in a single test.

mod support;

use bytes::Bytes;
use serde::Serialize;
use typesafe_sdk::__internals as codec;

use crate::support::{Measured, RUNS, measure, measure_min, stable_min};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The model name as the SDK holds it: already a JSON string literal.
const MODEL: &[u8] = b"\"jev-latest\"";

/// A prepared question set, already serialized. 297 bytes, spliced in without
/// being encoded again.
const QUESTIONS: &[u8] = br#"{"billing":{"type":"noul","instructions":"Is this about billing?","yes":"payments or invoices"},"tone":{"type":"choice","instructions":"What is the tone?","criteria":{"calm":"neutral or polite","angry":"annoyed or hostile"}},"urgency":{"type":"score","criteria":["can wait","this week","today"]}}"#;

/// An object `state`, the shape whose length cannot be known before it is
/// encoded.
#[derive(Serialize)]
struct Ticket<'a> {
    subject: &'a str,
    body: &'a str,
}

/// `{"state":<state>,"model":"jev-latest","questions":<prepared>}`.
fn encode_body<T>(state: &T) -> Bytes
where
    T: Serialize + ?Sized,
{
    codec::encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"state":"#);
        codec::encode_into(buffer, state)?;
        buffer.extend_from_slice(br#","model":"#);
        buffer.extend_from_slice(MODEL);
        buffer.extend_from_slice(br#","questions":"#);
        buffer.extend_from_slice(QUESTIONS);
        buffer.push(b'}');
        Ok(())
    })
    .expect("the body encodes")
}

/// Text of roughly `len` bytes that the encoder has real work to do on: a
/// quote to escape and a multi-byte character to copy.
fn filler(len: usize) -> String {
    const PATTERN: &str = "The ticket said \"it broke\" again, caf\u{e9} closed. ";
    let mut text = PATTERN.repeat(len.div_ceil(PATTERN.len()));
    while !text.is_char_boundary(len) {
        text.push(' ');
    }
    text.truncate(len);
    text
}

/// Encodes the body once to warm up, then reports what each identical call
/// after it costs.
fn steady_state<F>(label: &str, encode: F) -> Measured
where
    F: Fn() -> Bytes,
{
    drop(encode());
    let (measured, body) = measure_min(label, || (), |()| encode());

    let allowance = (body.len() as f64 * 1.05) as u64 + 4096;
    println!(
        "{label:<14} body={:>9} blocks={} bytes={:>9} allowance={allowance:>9} \
         scratch={} hint={}",
        body.len(),
        measured.blocks,
        measured.bytes,
        codec::scratch_capacity(),
        codec::scratch_hint()
    );

    assert_eq!(
        measured.blocks, 1,
        "{label}: a repeated body must cost the one copy into the request buffer"
    );
    assert!(
        measured.bytes <= allowance,
        "{label}: allocated {} bytes for a body of {}",
        measured.bytes,
        body.len()
    );
    assert!(
        codec::scratch_capacity() <= codec::scratch_hint().saturating_mul(8),
        "{label}: the retained scratch is {} bytes against a hint of {}",
        codec::scratch_capacity(),
        codec::scratch_hint()
    );

    measured
}

#[test]
fn a_repeated_request_body_costs_one_block() {
    let _profiler = dhat::Profiler::builder().testing().build();

    for (label, len) in
        [("1 KB string", 1024), ("64 KB string", 64 * 1024), ("1 MB string", 1024 * 1024)]
    {
        codec::reset_scratch();
        let state = filler(len);
        steady_state(label, || encode_body(state.as_str()));
    }

    codec::reset_scratch();
    let subject = filler(64);
    let long_body = filler(1024 * 1024);
    let ticket = Ticket { subject: &subject, body: &long_body };
    steady_state("1 MB object", || encode_body(&ticket));

    mixed_sizes_return_to_one_block();
}

/// One large body followed by small ones: sixteen with sonic-rs, thirty-three
/// with serde_json. Without sonic-rs's six-fold reserve, serde_json's Vec
/// grows by doubling: shrink at 32, steady at 33 under the same 8x rule.
///
/// The scratch is retained, so the small calls start out allocating nothing
/// while holding on to megabytes. The size hint decays a sixteenth per call
/// until the buffer is more than eight times too large, and the single shrink
/// that follows goes all the way down to the hint - not to the eight-times
/// ceiling, which the still-decaying hint would cross again on the very next
/// call, re-allocating the whole buffer every time.
///
/// The sequence is what is measured, so it is not one call repeated: the
/// whole sequence runs [`RUNS`] times, each from an empty scratch, which makes
/// every run the same sequence, and each small call is held to its stable
/// minimum across the runs. The large call's line is printed, not asserted,
/// and it stays the first run's.
fn mixed_sizes_return_to_one_block() {
    #[cfg(feature = "sonic")]
    const SMALL_CALLS: usize = 16;
    #[cfg(not(feature = "sonic"))]
    const SMALL_CALLS: usize = 33;

    let large = filler(1024 * 1024);
    let small = filler(1024);

    // Per small call: its measurement in each run, and the body length,
    // scratch and hint it left, which are the same in every run.
    let mut calls: Vec<Vec<Measured>> =
        std::iter::repeat_with(|| Vec::with_capacity(RUNS)).take(SMALL_CALLS).collect();
    let mut states = [(0, 0, 0); SMALL_CALLS];
    let mut first_run_large = None;
    for _ in 0..RUNS {
        codec::reset_scratch();
        let (first, large_body) = measure(|| encode_body(large.as_str()));
        first_run_large.get_or_insert((first, large_body.len(), codec::scratch_capacity()));
        drop(large_body);
        for (call, state) in calls.iter_mut().zip(&mut states) {
            let (measured, body) = measure(|| encode_body(small.as_str()));
            call.push(measured);
            *state = (body.len(), codec::scratch_capacity(), codec::scratch_hint());
        }
        assert!(
            codec::scratch_capacity() <= codec::scratch_hint().saturating_mul(8),
            "the scratch still holds {} bytes against a hint of {}",
            codec::scratch_capacity(),
            codec::scratch_hint()
        );
        assert!(
            codec::scratch_capacity() < large.len(),
            "the megabyte buffer is still retained: {} bytes",
            codec::scratch_capacity()
        );
    }

    let (first, large_len, large_scratch) =
        first_run_large.expect("invariant: RUNS is at least one");
    println!(
        "mixed: 1 MB     body={large_len:>9} blocks={} bytes={:>9} scratch={large_scratch}",
        first.blocks, first.bytes
    );

    let mut shrinks = 0;
    let mut last = Measured { blocks: 0, bytes: 0 };
    for (index, (runs, (len, scratch, hint))) in calls.iter().zip(states).enumerate() {
        let call = index + 1;
        let measured = stable_min(&format!("mixed: 1 KB #{call}"), runs);
        println!(
            "mixed: 1 KB #{call:<2} body={len:>9} blocks={} bytes={:>9} scratch={scratch} \
             hint={hint}",
            measured.blocks, measured.bytes
        );
        assert!(
            measured.blocks <= 2,
            "call {call} cost {} blocks; only a shrink may add one",
            measured.blocks
        );
        if measured.blocks == 2 {
            shrinks += 1;
        }
        last = measured;
    }

    assert!(
        shrinks <= 1,
        "the scratch shrank {shrinks} times; shrinking to the ceiling instead of \
         to the hint is what makes it repeat"
    );
    assert_eq!(last.blocks, 1, "after the outlier the per-call cost must be back to one block");
}
