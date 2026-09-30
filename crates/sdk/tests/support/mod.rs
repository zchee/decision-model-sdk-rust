//! Allocation counting shared by the `alloc_*` tests.
//!
//! dhat's `HeapStats` counts the allocations of every thread in the process,
//! and a measurement is the change in those counters across a section. The
//! test's own thread is not the only one: libtest runs a test on a thread it
//! spawns, and right after the spawn its main thread allocates its own
//! bookkeeping, once per process (rustc 1.98.1 `library/test/src/lib.rs`
//! lines 460-463: `running_tests.insert(..)` and `timeout_queue.push_back(..)`,
//! then it waits in `rx.recv_timeout`). nextest runs every test in a process
//! of its own, but that process is the same libtest binary, so it does the
//! same. On an idle machine that bookkeeping is done before the test measures
//! anything. On a loaded one the main thread can be descheduled between the
//! spawn and those allocations, and they land in whatever section the test
//! is measuring: 4 blocks and 900 bytes on Linux x86_64, seen as 4 failures
//! in 31 rounds of the suite pinned to 4 CPUs, and once in CI.
//!
//! So each asserted section is measured [`RUNS`] times, after the caller's
//! warm-up, and the budget is held against the minimum. Another thread can
//! only add to a process-wide count, never take from it, and the SDK's cost
//! of a repeated identical call is the same every time by design, so the
//! minimum is that cost. The minimum alone would also hide an allocation the
//! SDK makes in some calls and not in others, so it has to be the value of
//! at least [`AGREE`] of the runs. The foreign bookkeeping happens once per
//! process, but it is several allocations spread over three calls
//! (`insert`, `push_back` and `recv_timeout`'s own), so nothing in libtest
//! stops it from straddling a run boundary. That it lands in ONE run is an
//! observation, not a guarantee: in 46 polluted windows measured on Linux
//! x86_64 it was always one run of five. If it ever splits across enough runs
//! to leave fewer than [`AGREE`] at the minimum, the test fails the stability
//! rule - a flake - and never passes falsely, because the minimum cannot fall
//! below the SDK's own cost. Every run is printed, so a polluted one is
//! visible in the output.

use std::fmt::Write as _;

/// The number of times an asserted section is measured.
pub(crate) const RUNS: usize = 5;

/// How many of the runs must equal the minimum for it to count.
const AGREE: usize = 3;

/// The change in dhat's counters across one section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Measured {
    pub(crate) blocks: u64,
    pub(crate) bytes: u64,
}

/// Measures `body` once and returns what it cost and what it returned.
pub(crate) fn measure<F, T>(body: F) -> (Measured, T)
where
    F: FnOnce() -> T,
{
    let before = dhat::HeapStats::get();
    let value = body();
    let after = dhat::HeapStats::get();
    (
        Measured {
            blocks: after.total_blocks - before.total_blocks,
            bytes: after.total_bytes - before.total_bytes,
        },
        value,
    )
}

/// Measures `body` [`RUNS`] times, each on a fresh `input()` made outside the
/// measured section, and returns the stable minimum with the last run's value.
///
/// # Panics
///
/// When fewer than [`AGREE`] runs equal the minimum (see [`stable_min`]).
pub(crate) fn measure_min<I, T>(
    label: &str,
    mut input: impl FnMut() -> I,
    mut body: impl FnMut(I) -> T,
) -> (Measured, T) {
    let mut runs = Vec::with_capacity(RUNS);
    let mut last = None;
    for _ in 0..RUNS {
        let input = input();
        let (measured, value) = measure(|| body(input));
        runs.push(measured);
        // The previous value is dropped here, outside any measured section;
        // freeing does not count in dhat's totals anyway.
        last = Some(value);
    }
    let value = last.expect("invariant: RUNS is at least one");
    (stable_min(label, &runs), value)
}

/// The minimum of `runs`, printed with every run, after checking that at
/// least [`AGREE`] of them equal it in blocks and bytes.
///
/// # Panics
///
/// When fewer than [`AGREE`] runs equal the minimum: the section does not
/// cost the same on identical calls, which no foreign allocation explains.
pub(crate) fn stable_min(label: &str, runs: &[Measured]) -> Measured {
    let min = Measured {
        blocks: runs.iter().map(|run| run.blocks).min().unwrap_or(0),
        bytes: runs.iter().map(|run| run.bytes).min().unwrap_or(0),
    };
    let mut line = String::new();
    for run in runs {
        write!(line, " {}/{}", run.blocks, run.bytes).expect("a String takes any write");
    }
    println!("  runs of {label:<38} blocks/bytes:{line}");
    let agree = runs.iter().filter(|run| **run == min).count();
    assert!(
        agree >= AGREE,
        "{label}: the count is not stable across identical calls: {agree} of {} runs equal the \
         minimum of {} blocks and {} bytes, {AGREE} must; the runs (blocks/bytes):{line}",
        runs.len(),
        min.blocks,
        min.bytes,
    );
    min
}
