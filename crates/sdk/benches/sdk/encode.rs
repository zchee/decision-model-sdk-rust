//! B1: the request body encode.
//!
//! `prepared` is the SDK's hot path: the state goes through the codec into
//! the thread's retained scratch, and the finished body is copied out into a
//! right-sized buffer. `unprepared` adds what a caller pays for building and
//! preparing the questions on every call instead of once. The state is text
//! with quotes, newlines and multi-byte characters, so the escaper does real
//! work; a state of one repeated letter would flatter every codec.
//!
//! How this can mislead: every run after the first reuses a warm scratch, so
//! these are steady-state numbers. The scratch's first growth and its
//! one-step shrink are what `mixed_sizes` shows, and the allocation side of
//! both is asserted by `tests/alloc_encode.rs`, not here.
//!
//! `codec` compares sonic-rs with serde_json on the state alone, each writing
//! into a retained buffer, so neither pays for growing it: this isolates the
//! escaper, which is the part of the encode the codec choice decides.

use divan::{Bencher, black_box};
use serde::Serialize;
use typesafe_sdk::__internals as sdk;

use crate::{
    service::body,
    support::{questions, text},
};

/// 1 KB, 64 KB and 1 MB states.
const SIZES: [usize; 3] = [1 << 10, 64 << 10, 1 << 20];

/// [`SIZES`] and a 4 MB state, whose scratch is over the size a thread keeps
/// between calls: it is dropped after every call and grown again by the next,
/// so that row times what a state that large pays on every call.
const PREPARED_SIZES: [usize; 4] = [1 << 10, 64 << 10, 1 << 20, 4 << 20];

/// A state object holding one large string field, AC-P1's object case.
#[derive(Serialize)]
struct Ticket {
    subject: String,
    body: String,
}

#[divan::bench(args = PREPARED_SIZES)]
fn prepared(bencher: Bencher<'_, '_>, len: usize) {
    let state = text(len);
    drop(body(state.as_str()));
    bencher.bench_local(|| body(black_box(state.as_str())));
}

#[divan::bench(args = SIZES)]
fn unprepared(bencher: Bencher<'_, '_>, len: usize) {
    let state = text(len);
    drop(body(state.as_str()));
    bencher.bench_local(|| (black_box(questions()), body(black_box(state.as_str()))));
}

#[divan::bench]
fn object_1mb(bencher: Bencher<'_, '_>) {
    let state = Ticket { subject: text(64), body: text(1 << 20) };
    drop(body(&state));
    bencher.bench_local(|| body(black_box(&state)));
}

/// One 1 MB call and then sixteen 1 KB calls, from a fresh scratch each
/// time: the growth to the large body, the decaying hint, and the one-step
/// shrink that the fifth small call triggers.
#[divan::bench]
fn mixed_sizes(bencher: Bencher<'_, '_>) {
    let large = text(1 << 20);
    let small = text(1 << 10);
    bencher.with_inputs(sdk::reset_scratch).bench_local_values(|()| {
        drop(body(black_box(large.as_str())));
        for _ in 0..16 {
            drop(body(black_box(small.as_str())));
        }
    });
}

mod codec {
    use divan::{Bencher, black_box};

    use super::SIZES;
    use crate::support::text;

    #[divan::bench(args = SIZES)]
    fn sonic_rs(bencher: Bencher<'_, '_>, len: usize) {
        let state = text(len);
        let mut buffer = Vec::new();
        sonic_rs::to_writer(&mut buffer, state.as_str()).expect("the state encodes");
        bencher.bench_local(|| {
            buffer.clear();
            sonic_rs::to_writer(&mut buffer, black_box(state.as_str())).expect("the state encodes");
            black_box(buffer.len())
        });
    }

    #[divan::bench(args = SIZES)]
    fn serde_json(bencher: Bencher<'_, '_>, len: usize) {
        let state = text(len);
        let mut buffer = Vec::new();
        serde_json::to_writer(&mut buffer, state.as_str()).expect("the state encodes");
        bencher.bench_local(|| {
            buffer.clear();
            serde_json::to_writer(&mut buffer, black_box(state.as_str()))
                .expect("the state encodes");
            black_box(buffer.len())
        });
    }
}
