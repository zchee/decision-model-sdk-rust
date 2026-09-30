//! CPU benchmarks of the SDK's own work, with no I/O: body encode (B1),
//! response decode (B2), request assembly (B3), `Retry-After` and backoff
//! (B4), and a whole call through an in-memory service against its floor and
//! against a naive client (B5).
//!
//! Everything here is deterministic and single-threaded, so it is what the
//! CodSpeed instrumented run measures. Loopback I/O lives in the `loopback`
//! target instead, because an instruction count of a socket round trip
//! measures the kernel and the scheduler as much as the SDK.
//!
//! Run with `cargo bench --all-features --bench sdk`, with no `RUSTFLAGS`:
//! the numbers are meant to be what a consumer's default build gets.

// The CodSpeed layer is used under divan's own name, as its documentation
// asks: its attribute macros expand to paths that start with `::divan`. A
// dependency inherited from the workspace cannot be renamed in the manifest,
// so the rename happens here; `extern crate ... as` puts the new name in
// the extern prelude, where a `::divan` path looks.
extern crate codspeed_divan_compat as divan;

#[path = "../support/naive.rs"]
mod naive;
#[path = "../support/naive_response.rs"]
mod naive_response;
#[path = "../support/mod.rs"]
mod support;

mod assembly;
mod call;
mod decode;
mod encode;
mod retry;
mod service;

/// The prepared form of [`support::questions`], byte for byte: what the SDK
/// splices into every body. `assembly` checks it against a body the SDK
/// actually sent before anything is measured.
const QUESTIONS_JSON: &[u8] = br#"{"spam":{"type":"noul","instructions":"Spam?"},"tone":{"type":"choice","instructions":"Tone?","criteria":{"friendly":null,"hostile":null}},"quality":{"type":"score","instructions":"Quality?","criteria":["bad","ok","great"]}}"#;

/// The default model of every client here.
const MODEL: &str = "jev-latest";

fn main() {
    divan::main();
}
