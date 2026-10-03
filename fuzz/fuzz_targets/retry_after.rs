//! Arbitrary bytes as the value of the headers a server asks for a wait
//! with, through the parser that reads them.
//!
//! The property is that the parser returns `None` or a `Duration` and never
//! panics, aborts or hangs, whatever the header holds: a count of seconds or
//! milliseconds, a float, a negative or enormous number, an HTTP date, or
//! noise. The answer also has to be a function of the headers alone, so the
//! parser is asked twice and must say the same both times.
//!
//! The first byte picks which headers carry the rest of the input:
//!
//! - `M`: `retry-after-ms` alone;
//! - `B`: both, split at the first line feed into `retry-after-ms`, then
//!   `Retry-After`;
//! - `R`, or any other first byte: `Retry-After` alone (for `R` without the
//!   `R`), which is what almost every server sends.
//!
//! Bytes a header value cannot hold (a line feed, most control bytes) make
//! the header impossible to receive, so such an input is skipped rather than
//! parsed.

#![no_main]

use std::time::{Duration, SystemTime};

use decision_model_sdk::__internals;
use http::{HeaderMap, HeaderValue, header::RETRY_AFTER};
use libfuzzer_sys::fuzz_target;

/// A fixed instant to measure an HTTP date against, so a run is repeatable:
/// 2026-01-01T00:00:00Z.
const NOW_SECS: u64 = 1_767_225_600;

/// The millisecond header, read before `Retry-After`. The SDK keeps its name
/// crate-private, so it is spelled here as the server sends it.
const RETRY_AFTER_MS_HEADER: &str = "retry-after-ms";

fuzz_target!(|data: &[u8]| {
    let (millis, seconds) = match data.split_first() {
        Some((b'M', rest)) => (Some(rest), None),
        Some((b'B', rest)) => match rest.iter().position(|&byte| byte == b'\n') {
            Some(at) => (Some(&rest[..at]), Some(&rest[at + 1..])),
            None => (Some(rest), None),
        },
        Some((b'R', rest)) => (None, Some(rest)),
        _ => (None, Some(data)),
    };

    let mut headers = HeaderMap::new();
    for (name, value) in [(RETRY_AFTER_MS_HEADER, millis), (RETRY_AFTER.as_str(), seconds)] {
        let Some(value) = value else { continue };
        let Ok(value) = HeaderValue::from_bytes(value) else { return };
        headers.insert(name, value);
    }

    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS);
    let first = __internals::parse_retry_after(&headers, now);
    let second = __internals::parse_retry_after(&headers, now);
    assert_eq!(first, second, "the same headers gave two different waits");
});
