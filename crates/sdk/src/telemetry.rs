//! What the SDK reports about itself while it works.
//!
//! The crate emits events and never installs a subscriber: choosing where logs
//! go is the application's decision, and a library that made it would take it
//! away from every other library in the process.
//!
//! A state may carry personal data and a header may carry a credential, so a
//! body is reported by its length at the ordinary level and in full only at the
//! most verbose one, and the headers that carry secrets are redacted wherever
//! they are printed.
//!
//! Every event has the target `typesafe_sdk`, so one filter directive selects
//! all of them. At `INFO` each attempt gets one line, as the Python SDK
//! writes it: `GET <url> <- 200 in 12ms (request <id>)` for a response of any
//! status, or `GET <url> <- timeout` - a fixed word per kind of failure, never
//! its message - for an attempt that ended without one; a retry is announced
//! as `GET <url> retry 1` before it is sent. At `DEBUG` a request
//! is reported as it leaves and as its answer arrives - method, endpoint,
//! status, request id, the headers with their secrets redacted, and the
//! body's length. At `TRACE` the bodies themselves follow.
//!
//! Without the `tracing` feature every function here is empty and the
//! compiler removes the calls. With it, and with no subscriber interested, a
//! call costs a check per event and allocates nothing: the fields are values
//! that know how to print themselves, and nothing is printed unless an event
//! is recorded.

#[cfg(feature = "tracing")]
use std::fmt;
use std::time::Instant;

#[cfg(feature = "tracing")]
use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
#[cfg(feature = "tracing")]
use http::{HeaderName, HeaderValue};

#[cfg(feature = "tracing")]
use tracing::Level;

use crate::error::Error;
#[cfg(feature = "tracing")]
use crate::{
    constants::{MODELS_PATH, SYSTEM_ONE_PATH, request_id},
    error::format_endpoint,
    redact::is_secret,
    text::{Backslash, MAX_NAME_CHARS, SafeText},
};

/// The target every event of this crate is emitted under, those raised
/// outside this module included.
#[cfg(feature = "tracing")]
pub(crate) const TARGET: &str = "typesafe_sdk";

/// One request, as the events about it name it.
#[derive(Clone, Copy)]
pub(crate) struct Exchange<'a> {
    #[cfg(feature = "tracing")]
    method: &'a Method,
    #[cfg(feature = "tracing")]
    uri: &'a Uri,
    #[cfg(feature = "tracing")]
    retry: u32,
    #[cfg(feature = "tracing")]
    omit_endpoint_host: bool,
    /// Holds the lifetime when the fields above are compiled out.
    #[cfg(not(feature = "tracing"))]
    request: std::marker::PhantomData<&'a ()>,
}

impl<'a> Exchange<'a> {
    /// The attempt numbered `retry` of a request to `method uri`.
    #[cfg(feature = "tracing")]
    pub(crate) fn new(
        method: &'a Method,
        uri: &'a Uri,
        retry: u32,
        omit_endpoint_host: bool,
    ) -> Self {
        Self { method, uri, retry, omit_endpoint_host }
    }

    /// Without events there is nothing to name a request for.
    #[cfg(not(feature = "tracing"))]
    pub(crate) fn new(_: &'a Method, _: &'a Uri, _: u32, _: bool) -> Self {
        Self { request: std::marker::PhantomData }
    }
}

/// A request is about to be handed to the transport.
#[cfg(feature = "tracing")]
pub(crate) fn sending(exchange: Exchange<'_>, headers: &HeaderMap, body: Option<&Bytes>) {
    tracing::debug!(
        target: TARGET,
        method = %exchange.method,
        endpoint = %EndpointUri(exchange),
        retry = exchange.retry,
        headers = ?redact(headers),
        body_len = body.map_or(0, Bytes::len),
        "sending request"
    );
    if let Some(body) = body {
        tracing::trace!(
            target: TARGET,
            method = %exchange.method,
            endpoint = %EndpointUri(exchange),
            body = %Lossy(body),
            "request body"
        );
    }
}

/// A request is about to be handed to the transport.
#[cfg(not(feature = "tracing"))]
pub(crate) fn sending(_: Exchange<'_>, _: &HeaderMap, _: Option<&bytes::Bytes>) {}

/// When an attempt started, read only when an `INFO` event of this crate can
/// be recorded: the elapsed time is printed by events and by nothing else.
type Started = Option<Instant>;

/// The start of an attempt, if anything will print how long it took.
#[cfg(feature = "tracing")]
pub(crate) fn clock() -> Started {
    // `INFO` is the least verbose level this crate's timed events use, so
    // when it is off every one of them is off.
    tracing::enabled!(target: TARGET, Level::INFO).then(Instant::now)
}

/// The start of an attempt, if anything will print how long it took.
#[cfg(not(feature = "tracing"))]
pub(crate) fn clock() -> Started {
    None
}

/// A response arrived, whatever its status: the `INFO` summary line,
/// `GET https://api.typesafe.ai/v1/models <- 200 in 12ms (request req_1)`.
#[cfg(feature = "tracing")]
pub(crate) fn responded(
    exchange: Exchange<'_>,
    status: StatusCode,
    headers: &HeaderMap,
    started: Started,
) {
    tracing::info!(
        target: TARGET,
        "{} <- {} in {} (request {})",
        Endpoint(exchange),
        status.as_u16(),
        Elapsed(started),
        RequestId(headers),
    );
}

/// A response arrived, whatever its status.
#[cfg(not(feature = "tracing"))]
pub(crate) fn responded(_: Exchange<'_>, _: StatusCode, _: &HeaderMap, _: Started) {}

/// A response arrived and its body was read in full.
#[cfg(feature = "tracing")]
pub(crate) fn received(
    exchange: Exchange<'_>,
    status: StatusCode,
    headers: &HeaderMap,
    body: &Bytes,
    started: Started,
) {
    tracing::debug!(
        target: TARGET,
        method = %exchange.method,
        endpoint = %EndpointUri(exchange),
        status = status.as_u16(),
        request_id = %RequestId(headers),
        elapsed = %Elapsed(started),
        headers = ?redact(headers),
        body_len = body.len(),
        "received response"
    );
    tracing::trace!(
        target: TARGET,
        method = %exchange.method,
        endpoint = %EndpointUri(exchange),
        body = %Lossy(body),
        "response body"
    );
}

/// A response arrived and its body was read in full.
#[cfg(not(feature = "tracing"))]
pub(crate) fn received(
    _: Exchange<'_>,
    _: StatusCode,
    _: &HeaderMap,
    _: &bytes::Bytes,
    _: Started,
) {
}

/// An attempt ended without a response this crate could read: the `INFO`
/// line `POST https://api.typesafe.ai/v1/systemone <- timeout`.
///
/// Only a fixed word for the kind is printed, never the error's message or
/// cause: a connection failure's message is the transport's chain, and that
/// can carry text the server chose.
#[cfg(feature = "tracing")]
pub(crate) fn failed(exchange: Exchange<'_>, error: &Error, started: Started) {
    tracing::info!(target: TARGET, "{} <- {}", Endpoint(exchange), error.kind().words().0);
    tracing::debug!(
        target: TARGET,
        method = %exchange.method,
        endpoint = %EndpointUri(exchange),
        elapsed = %Elapsed(started),
        failure = error.kind().words().0,
        "request failed"
    );
}

/// An attempt ended without a response this crate could read.
#[cfg(not(feature = "tracing"))]
pub(crate) fn failed(_: Exchange<'_>, _: &Error, _: Started) {}

/// A failed request is about to be sent again: the `INFO` line
/// `POST https://api.typesafe.ai/v1/systemone retry 1`, the retry numbered as
/// `X-TypeSafe-Retry-Count` numbers it. What the last attempt failed with has
/// its own line already.
#[cfg(feature = "tracing")]
pub(crate) fn retrying(exchange: Exchange<'_>) {
    tracing::info!(target: TARGET, "{} retry {}", Endpoint(exchange), exchange.retry);
}

/// A failed request is about to be sent again.
#[cfg(not(feature = "tracing"))]
pub(crate) fn retrying(_: Exchange<'_>) {}

/// An event's URI, optionally reduced to the fixed API path.
#[cfg(feature = "tracing")]
struct EndpointUri<'a>(Exchange<'a>);

#[cfg(feature = "tracing")]
impl fmt::Display for EndpointUri<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.omit_endpoint_host {
            // Only these two API operations reach telemetry. The wire URI's
            // path can include a caller's prefix, which must not be logged here.
            let path = if *self.0.method == Method::POST { SYSTEM_ONE_PATH } else { MODELS_PATH };
            formatter.write_str(path)
        } else {
            self.0.uri.fmt(formatter)
        }
    }
}

/// A request's method and URL, or its fixed API path when the host is omitted.
/// Built only when an event that prints it is recorded.
#[cfg(feature = "tracing")]
struct Endpoint<'a>(Exchange<'a>);

#[cfg(feature = "tracing")]
impl fmt::Display for Endpoint<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.omit_endpoint_host {
            write!(formatter, "{} {}", self.0.method, EndpointUri(self.0))
        } else {
            formatter.write_str(&format_endpoint(self.0.method, self.0.uri))
        }
    }
}

/// How long an attempt took, in whole milliseconds, or `-` when the start was
/// not read because no event could print it then.
#[cfg(feature = "tracing")]
struct Elapsed(Started);

#[cfg(feature = "tracing")]
impl fmt::Display for Elapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(started) => write!(formatter, "{}ms", started.elapsed().as_millis()),
            None => formatter.write_str("-"),
        }
    }
}

/// The server's identifier for a request, from `x-typesafe-request-id`, or
/// `-`. It is the server's text: `http` lets only visible ASCII and tabs
/// through `to_str`, and the rendering escapes the tabs and cuts the id at
/// 128 characters, which no real id reaches.
#[cfg(feature = "tracing")]
struct RequestId<'a>(&'a HeaderMap);

#[cfg(feature = "tracing")]
impl fmt::Display for RequestId<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match request_id(self.0) {
            Some(id) => {
                let mut shown = SafeText::new(MAX_NAME_CHARS, Backslash::Keep);
                shown.untrusted(id, MAX_NAME_CHARS);
                formatter.write_str(&shown.into_string())
            }
            None => formatter.write_str("-"),
        }
    }
}

/// A name the server chose - an answer's key, an answer's `type` - as an
/// event field: escaped as a field path is, a backslash doubled so that a
/// name spelling `\u{1b}` cannot pass for one holding the character, and cut
/// at 128 characters. JSON lets a server put a newline, an ESC or a bidi
/// override into any string, and a plain-text subscriber writes a field's
/// `Display` as it is, so a raw name could start a forged log line or
/// recolour a terminal. Nothing is built unless an event is recorded.
#[cfg(feature = "tracing")]
pub(crate) struct ServerName<'a>(pub(crate) &'a str);

#[cfg(feature = "tracing")]
impl fmt::Display for ServerName<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = SafeText::new(MAX_NAME_CHARS, Backslash::Double);
        shown.untrusted(self.0, MAX_NAME_CHARS);
        formatter.write_str(&shown.into_string())
    }
}

/// A body as text, with anything that is not UTF-8 replaced and every
/// control or format character escaped, so that a body cannot break the log
/// line it is printed on. Nothing is built unless an event is recorded; the
/// body is not cut, since logging it whole is what `TRACE` is for.
#[cfg(feature = "tracing")]
struct Lossy<'a>(&'a [u8]);

#[cfg(feature = "tracing")]
impl fmt::Display for Lossy<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write as _;

        let mut shown = SafeText::new(usize::MAX, Backslash::Keep);
        let mut writer = shown.untrusted_writer();
        for chunk in self.0.utf8_chunks() {
            writer.write_str(chunk.valid())?;
            if !chunk.invalid().is_empty() {
                writer.write_str("\u{fffd}")?;
            }
        }
        drop(writer);
        formatter.write_str(&shown.into_string())
    }
}

/// What a secret header value is printed as.
#[cfg(feature = "tracing")]
const REDACTED: &str = "***";

/// A view of `headers` that prints every secret value as `***`.
///
/// A header is secret when its name is one of the credential headers
/// (`authorization`, `proxy-authorization`, `x-api-key`, `api-key`, `cookie`,
/// `set-cookie`), when its name contains `token` or `secret`, or when its
/// value is flagged sensitive. The first two rules are the Python SDK's; the
/// third is this port's own, so a value the SDK or a caller marked sensitive
/// stays hidden under any name.
///
/// Nothing is copied: the view borrows the map and redacts as it writes, so a
/// log event that is filtered out costs nothing beyond building the view.
#[cfg(feature = "tracing")]
fn redact(headers: &HeaderMap) -> RedactedHeaders<'_> {
    RedactedHeaders(headers)
}

/// Headers as the logs show them. See [`redact`].
#[cfg(feature = "tracing")]
#[derive(Clone, Copy)]
struct RedactedHeaders<'a>(&'a HeaderMap);

#[cfg(feature = "tracing")]
impl RedactedHeaders<'_> {
    /// Each header in the map's order, a name repeated once per value, with
    /// its value or `None` when that value is secret.
    fn entries(&self) -> impl Iterator<Item = (&HeaderName, Option<&HeaderValue>)> {
        self.0.iter().map(|(name, value)| (name, (!is_secret(name, value)).then_some(value)))
    }
}

#[cfg(feature = "tracing")]
impl fmt::Debug for RedactedHeaders<'_> {
    /// `{"name": "value", "authorization": "***"}`, each value in the quoted,
    /// escaped form `HeaderValue`'s own `Debug` uses.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_map()
            .entries(self.entries().map(|(name, value)| (name, Shown(value))))
            .finish()
    }
}

/// One header value in a `Debug` map: the value, or the redaction marker.
#[cfg(feature = "tracing")]
struct Shown<'a>(Option<&'a HeaderValue>);

#[cfg(feature = "tracing")]
impl fmt::Debug for Shown<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => fmt::Debug::fmt(value, formatter),
            None => fmt::Debug::fmt(REDACTED, formatter),
        }
    }
}

#[cfg(all(test, feature = "tracing"))]
#[path = "telemetry_tests.rs"]
mod tests;
