//! Thin wrappers around the codec and the retry delay, for allocation tests
//! and benchmarks, and the built-in transport's count of open streams, for
//! the connection tests.
//!
//! This module exists only because an allocation budget has to be asserted
//! against the same code the SDK runs, and that code is crate-private. It is
//! hidden from the documentation, it is behind the non-default `internals`
//! feature, and it carries **no semver promise**: anything here may change or
//! disappear in a patch release. Nothing outside these wrappers is made public
//! for the sake of tests.

use bytes::Bytes;
use serde::Serialize;

use crate::codec::{self, DecodeError, EncodeError};

/// The nesting depth beyond which [`check_depth`] rejects a document.
pub const MAX_JSON_DEPTH: usize = codec::MAX_JSON_DEPTH;

/// See `codec::encode_into`.
///
/// # Errors
///
/// Returns [`EncodeError`] when the value cannot be represented as JSON.
pub fn encode_into<T>(buffer: &mut Vec<u8>, value: &T) -> Result<(), EncodeError>
where
    T: Serialize + ?Sized,
{
    codec::encode_into(buffer, value)
}

/// See `codec::encode_body`: the body encoder whose allocation behaviour the
/// budget is stated on.
///
/// # Errors
///
/// Returns whatever `fill` returns.
pub fn encode_body<F>(fill: F) -> Result<Bytes, EncodeError>
where
    F: FnOnce(&mut Vec<u8>) -> Result<(), EncodeError>,
{
    codec::encode_body(fill)
}

/// See `codec::check_depth`.
///
/// # Errors
///
/// Returns a too-deep [`DecodeError`] when the input crosses
/// [`MAX_JSON_DEPTH`].
pub fn check_depth(json: &[u8]) -> Result<(), DecodeError> {
    codec::check_depth(json)
}

/// Bytes the encode scratch of the calling thread is holding on to.
#[must_use]
pub fn scratch_capacity() -> usize {
    codec::scratch_capacity()
}

/// The decayed size hint of the calling thread.
#[must_use]
pub fn scratch_hint() -> usize {
    codec::scratch_hint()
}

/// Drops the encode scratch of the calling thread, so that the next call
/// starts from the state of a fresh thread.
pub fn reset_scratch() {
    codec::reset_scratch();
}

/// See `codec::decode`: the decoder every response goes through, for
/// comparing a type of the test's own on the same codec.
///
/// # Errors
///
/// Returns [`DecodeError`] when the input is not UTF-8, is nested too deeply,
/// is not valid JSON, or does not have the shape `T` expects.
pub fn decode<'de, T>(bytes: &'de [u8]) -> Result<T, DecodeError>
where
    T: serde::Deserialize<'de>,
{
    codec::decode(bytes)
}

/// See `de::decode_system_one_with`: the response decoder whose allocation
/// behaviour the decode budget is stated on.
///
/// It passes no level hint. A call passes the largest score its questions
/// ask, which sizes a score's first level list from the start; without it
/// the list starts at 4, so each score of 5 to 8 levels costs one block more
/// here than in a call, and the three-level fixture the budget is stated on
/// costs the same blocks either way.
///
/// # Errors
///
/// Returns a response-validation [`Error`](crate::Error) when the body does
/// not decode.
pub fn decode_system_one<A>(
    body: Bytes,
    status: http::StatusCode,
    headers: http::HeaderMap,
    questions: usize,
) -> Result<crate::response::SystemOneResponse<A>, crate::Error>
where
    A: crate::de::AnswerSet,
{
    crate::de::decode_system_one_with(
        body,
        status,
        headers,
        crate::de::AnswerContext::new(questions),
        None,
    )
}

/// See `models::decode_list_models`: the decoder a models response goes
/// through.
///
/// # Errors
///
/// Returns a response-validation [`Error`](crate::Error) when the body does
/// not decode.
pub fn decode_list_models(
    body: Bytes,
    status: http::StatusCode,
    headers: http::HeaderMap,
) -> Result<crate::models::ListModelsResponse, crate::Error> {
    crate::models::decode_list_models(body, status, headers, None)
}

/// See `ApiError::new`: the error a non-success response becomes, with its
/// message read out of `body`.
#[must_use]
pub fn api_error(
    status: http::StatusCode,
    body: Bytes,
    headers: http::HeaderMap,
) -> crate::ApiError {
    crate::error::ApiError::new(status, body, headers, None)
}

/// See `error::parse_retry_after`: the wait the response headers ask for,
/// with an HTTP date measured against `now`.
#[must_use]
pub fn parse_retry_after(
    headers: &http::HeaderMap,
    now: std::time::SystemTime,
) -> Option<std::time::Duration> {
    crate::error::parse_retry_after(headers, now)
}

/// The streams the built-in transport of `client` has open: every connection
/// it made that is not dropped yet, one the server closed included until the
/// client has noticed. At zero, the next request of `client` opens a
/// connection.
#[cfg(feature = "hyper")]
#[must_use]
pub fn open_streams(client: &crate::Client<crate::HyperTransport>) -> usize {
    client.shared().service.open_streams()
}

/// Forwards to `retry::backoff_seconds`, unchanged: the delay in seconds
/// before the attempt after attempt number `attempt` failed, with `draw`
/// standing in for the random number in `[0, 1)`.
///
/// Inlined, so the wrapper adds no call of its own: a benchmark measures
/// `retry::backoff_seconds` as the retry loop calls it.
#[inline]
#[must_use]
pub fn backoff_seconds(attempt: u32, initial: f64, max: f64, jitter: f64, draw: f64) -> f64 {
    crate::retry::backoff_seconds(attempt, initial, max, jitter, draw)
}
