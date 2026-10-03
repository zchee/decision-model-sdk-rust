//! What the tests against the live TypeSafe API share.
//!
//! **These tests make real, billed calls.** They run only when BOTH
//! `DECISION_MODEL_LIVE_TESTS=1` and `DECISION_MODEL_API_KEY` are in the environment; then
//! `cargo test --workspace`, `cargo nextest run --workspace` or any command
//! naming `-p decision-model-sdk-live-tests` sends requests to the live API on
//! that key's account. A key alone is not enough, so a contributor who has one
//! exported for other work is not billed by a stray `--workspace`. This crate
//! is a workspace member (so its code is linted and compiled) but not a
//! default member: a plain `cargo test` or `cargo nextest run` never builds or
//! runs it. Run it on purpose, with a key meant for it.
//!
//! The tests themselves are in `tests/live.rs`. Each one builds its client
//! through [`live_client`], which panics when either variable is missing,
//! before any client is built or any request is made, so such a run fails
//! with a message naming both variables instead of passing without having
//! asked the API anything.

use std::{env, time::Duration};

use decision_model_sdk::{Client, constants::API_KEY_ENV};

/// The deadline of one live request: the Python SDK's live tests give their
/// clients 120 seconds, far more than a System One answer takes.
pub const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// The API the live tests call: TypeSafe AI's, named here because the SDK
/// has no default base URL.
pub const LIVE_BASE_URL: &str = "https://api.typesafe.ai";

/// The model the live tests ask, named here because the SDK has no default
/// model.
pub const LIVE_MODEL: &str = "jev-latest";

/// The variable that opts in to the live tests; it must be exactly `1`.
pub const LIVE_TESTS_ENV: &str = "DECISION_MODEL_LIVE_TESTS";

/// A client for the live API at [`LIVE_BASE_URL`], asking [`LIVE_MODEL`] by
/// default, with the key from the environment as [`Client::from_env`] reads
/// it and [`LIVE_TIMEOUT`] as its deadline.
///
/// # Panics
///
/// When `DECISION_MODEL_LIVE_TESTS` is not `1` or `DECISION_MODEL_API_KEY` is unset or
/// empty - checked before the client is built, with a message naming both
/// variables and never the key's value - and when the client cannot be built
/// for another reason, with the SDK's own configuration error.
#[must_use]
pub fn live_client() -> Client {
    let opted_in = env::var_os(LIVE_TESTS_ENV).is_some_and(|value| value == "1");
    // Only whether the key is present is read here, never its text.
    let has_key = env::var_os(API_KEY_ENV).is_some_and(|value| !value.is_empty());
    if !(opted_in && has_key) {
        let missing = match (opted_in, has_key) {
            (false, false) => format!("{LIVE_TESTS_ENV}=1 and {API_KEY_ENV} are"),
            (false, true) => format!("{LIVE_TESTS_ENV}=1 is"),
            _ => format!("{API_KEY_ENV} is"),
        };
        panic!(
            "the live tests make billed calls and run only with both {LIVE_TESTS_ENV}=1 and \
             {API_KEY_ENV} set; {missing} missing, so they fail rather than skip"
        );
    }
    match Client::builder()
        .base_url(LIVE_BASE_URL)
        .default_model(LIVE_MODEL)
        .timeout(LIVE_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(error) => panic!("the live client could not be built from the environment: {error}"),
    }
}
