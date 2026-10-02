//! What the adapter's tests against the live vendor APIs share.
//!
//! **These tests make real, billed calls** to OpenAI, Anthropic and Gemini.
//! This crate is a workspace member (so its code is linted and compiled) but
//! not a default member: a plain `cargo test` or `cargo nextest run` never
//! builds or runs it. The tests themselves are in `tests/live.rs`; they are
//! ported from `tests/test_client_with_live_apis.py` of
//! system-one-adapter-python.
