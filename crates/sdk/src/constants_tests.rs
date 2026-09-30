//! Tests for the SDK's names and defaults.
//!
//! Most of these values are the contract with the server and with callers'
//! environments, so each is pinned by its literal text: a change to one should
//! fail here and be a decision, not a side effect.

use super::*;

#[test]
fn environment_variable_names_match_the_python_sdk() {
    assert_eq!(API_KEY_ENV, "TYPESAFE_API_KEY");
    assert_eq!(BASE_URL_ENV, "TYPESAFE_BASE_URL");
    assert_eq!(DEFAULT_MODEL_ENV, "TYPESAFE_DEFAULT_MODEL");
}

#[test]
fn defaults_match_the_python_sdk_and_the_documented_cap() {
    assert_eq!(DEFAULT_BASE_URL, "https://api.typesafe.ai");
    assert_eq!(DEFAULT_MODEL, "jev-latest");
    assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(10));
    assert_eq!(DEFAULT_MAX_RESPONSE_BYTES, 16_777_216);
}

#[test]
fn api_paths_are_rooted_and_carry_no_trailing_slash() {
    assert_eq!(SYSTEM_ONE_PATH, "/v1/systemone");
    assert_eq!(MODELS_PATH, "/v1/models");
}

#[test]
fn header_names_are_the_python_sdk_names_lower_cased() {
    assert_eq!(SDK_HEADER.as_str(), "x-typesafe-sdk");
    assert_eq!(RUNTIME_HEADER.as_str(), "x-typesafe-runtime");
    assert_eq!(RETRY_COUNT_HEADER.as_str(), "x-typesafe-retry-count");
    assert_eq!(JSON_CONTENT_TYPE, "application/json");
    assert_eq!(
        SECRET_HEADERS,
        ["authorization", "proxy-authorization", "x-api-key", "api-key", "cookie", "set-cookie"]
    );
}

#[test]
fn sdk_identifier_names_this_port_and_its_version() {
    let expected = format!("typesafe-sdk-rust/{}", env!("CARGO_PKG_VERSION"));
    assert_eq!(SDK_IDENTIFIER, expected.as_str());
}

#[test]
fn runtime_identifier_names_rust_and_the_compiled_target() {
    let expected = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    assert_eq!(*RUNTIME_IDENTIFIER, expected.as_str());
    assert!(!RUNTIME_IDENTIFIER.is_sensitive(), "an identifier is not a secret");
}
