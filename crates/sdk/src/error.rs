//! What a call can fail with, and how a failure renders.
//!
//! [`Error`] is the one error type every fallible operation returns. It is a
//! single boxed pointer, so `Result<T, Error>` costs a pointer beside `T`
//! rather than the size of the largest failure; the detail lives behind
//! [`Error::kind`].
//!
//! Four things here are not what a plain `thiserror` enum would do:
//!
//! * **An API failure's message is extracted from the server's body in a fixed
//!   order.** Servers put the human-readable sentence under `error`,
//!   `error.message`, `message`, `detail`, `detail.message` or a list under
//!   `detail`, and the SDK reads them in that order so the message a caller
//!   sees does not depend on which shape the endpoint chose. See
//!   [`ApiError::message`].
//! * **Nothing here prints a header value or a codec error's input.** An
//!   `Authorization` header and a decode error's excerpt of the body are both
//!   in reach of these types, and neither appears in `Debug` or `Display`.
//! * **Text the server chose is escaped and cut before it is printed.** An
//!   API failure's message and the request id are the server's text, and
//!   either would otherwise put a line break, a terminal colour or 16 MiB into
//!   every log line that prints the error. The accessors for the body and the
//!   headers still return them as they arrived.
//! * **`Retry-After` is parsed against a caller-supplied `now`.** Reading the
//!   clock inside the parser would make the HTTP-date case untestable without
//!   mocking time, so the parser takes the instant to measure against and
//!   [`ApiError::retry_after`] is the thin wrapper that reads the clock.

use std::{
    borrow::Cow,
    error::Error as StdError,
    fmt,
    time::{Duration, SystemTime},
};

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri, header};
use serde::{Deserialize, de::IgnoredAny};

use crate::{
    codec::{self, DecodeError, DecodeErrorKind, RawJson},
    constants::{RETRY_AFTER_MS_HEADER, request_id},
    text,
};

/// What a failure this crate could not attribute to itself was caused by.
type Cause = Box<dyn StdError + Send + Sync>;

// ------------------------------------------------------------------- Error

/// Anything a call to the API can fail with.
///
/// The type is one pointer wide whatever the failure was, so a `Result` from
/// this crate is the size of its success value plus a pointer. Match on
/// [`kind`](Error::kind) to tell the failures apart; the `Display` of this
/// type is the sentence to show a user, and [`source`](StdError::source)
/// leads to the failure underneath when there was one.
pub struct Error(Box<Inner>);

/// The heap half of [`Error`], so that the stack half stays a pointer.
struct Inner {
    kind: ErrorKind,
    /// The sentence for the kinds that do not carry a payload of their own.
    /// Empty for [`ErrorKind::Api`], [`ErrorKind::ResponseValidation`],
    /// [`ErrorKind::Timeout`] and [`ErrorKind::ResponseTooLarge`], which render
    /// from their payload instead.
    message: Box<str>,
    source: Option<Cause>,
}

/// Which kind of failure an [`Error`] is.
///
/// New variants are added as the SDK learns to distinguish more failures, so
/// a `match` over this enum needs a catch-all arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The client could not be built: a missing or malformed API key, a base
    /// URL that is not a URL, a timeout that is zero or not finite.
    Config,
    /// The request could not be built from what the caller supplied, and was
    /// never sent.
    InvalidRequest,
    /// The server answered, and the answer was not a success status.
    Api(ApiError),
    /// The request never produced an HTTP response: the connection failed,
    /// was refused, or was lost mid-flight.
    Connection,
    /// The attempt exceeded its deadline.
    Timeout {
        /// The deadline the attempt was given.
        timeout: Duration,
    },
    /// The server answered with a success status and a body this SDK could
    /// not read as the response it expected.
    ResponseValidation(ResponseValidationError),
    /// The server answered with a success status and a body larger than the
    /// client's limit, so the body was not read past the limit and nothing
    /// was decoded.
    ///
    /// The limit is 16 MiB unless
    /// [`ClientBuilder::max_response_bytes`](crate::ClientBuilder::max_response_bytes)
    /// set another. Retrying the same request cannot help: the answer will be
    /// as large again. A failure status with a body over the limit is an
    /// [`ErrorKind::Api`] instead, which keeps its status and headers.
    ResponseTooLarge {
        /// The limit the body exceeded, in bytes.
        limit: usize,
    },
}

impl ErrorKind {
    /// The fixed log word and user-facing sentence for this kind.
    pub(crate) fn words(&self) -> (&'static str, &'static str) {
        match self {
            Self::Timeout { .. } => ("timeout", "The request timed out."),
            Self::Connection => ("connection error", "The connection to the API failed."),
            Self::ResponseTooLarge { .. } => {
                ("response too large", "The response was larger than the size limit.")
            }
            Self::Api(_) => ("api error", "The API answered with an error."),
            Self::ResponseValidation(_) => {
                ("invalid response", "The response did not have the expected shape.")
            }
            Self::InvalidRequest => {
                ("invalid request", "The request was invalid and was not sent.")
            }
            Self::Config => ("config error", "The client configuration is invalid."),
        }
    }
}

impl Error {
    /// Which kind of failure this is.
    pub fn kind(&self) -> &ErrorKind {
        &self.0.kind
    }

    /// A fixed sentence for this kind of failure, safe to show a user.
    ///
    /// It never contains server or transport text. Unlike `Display`, it
    /// names only the kind, not this failure's message or metadata.
    pub fn summary(&self) -> &'static str {
        self.kind().words().1
    }

    /// The client could not be built from the configuration it was given.
    pub(crate) fn config(message: impl Into<Box<str>>) -> Self {
        Self::plain(ErrorKind::Config, message, None)
    }

    /// The caller's arguments do not describe a request that can be sent.
    pub(crate) fn invalid_request(message: impl Into<Box<str>>) -> Self {
        Self::plain(ErrorKind::InvalidRequest, message, None)
    }

    /// The request failed without an HTTP response.
    ///
    /// `cause` is the transport's own error, kept as the
    /// [`source`](StdError::source) so a caller can downcast to it - unless it
    /// held a credential of the request, when it is a private redacted copy
    /// that cannot be downcast.
    pub(crate) fn connection(message: impl Into<Box<str>>, cause: Option<Cause>) -> Self {
        Self::plain(ErrorKind::Connection, message, cause)
    }

    /// The message and the cause, taken apart so that one of them can be
    /// replaced without touching the other.
    pub(crate) fn into_parts(self) -> (Box<str>, Option<Cause>) {
        let Inner { message, source, .. } = *self.0;
        (message, source)
    }

    /// The attempt ran past `timeout`.
    pub(crate) fn timeout(timeout: Duration) -> Self {
        Self::plain(ErrorKind::Timeout { timeout }, "", None)
    }

    /// A success response's body was larger than `limit` bytes.
    pub(crate) fn response_too_large(limit: usize) -> Self {
        Self::plain(ErrorKind::ResponseTooLarge { limit }, "", None)
    }

    /// Builds one of the kinds whose sentence is not derived from a payload.
    fn plain(kind: ErrorKind, message: impl Into<Box<str>>, source: Option<Cause>) -> Self {
        Self(Box::new(Inner { kind, message: message.into(), source }))
    }
}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::plain(ErrorKind::Api(error), "", None)
    }
}

impl From<ResponseValidationError> for Error {
    fn from(error: ResponseValidationError) -> Self {
        Self::plain(ErrorKind::ResponseValidation(error), "", None)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0.kind {
            ErrorKind::Api(error) => error.fmt(formatter),
            ErrorKind::ResponseValidation(error) => error.fmt(formatter),
            ErrorKind::Timeout { timeout } => {
                write!(formatter, "Request timed out (timeout={}s).", timeout.as_secs_f64())
            }
            ErrorKind::ResponseTooLarge { limit } => write!(
                formatter,
                "The response body exceeded the limit of {limit} bytes and was not read."
            ),
            ErrorKind::Config | ErrorKind::InvalidRequest | ErrorKind::Connection => {
                formatter.write_str(&self.0.message)
            }
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = formatter.debug_struct("Error");
        shown.field("kind", &self.0.kind);
        if !self.0.message.is_empty() {
            shown.field("message", &self.0.message);
        }
        if let Some(source) = &self.0.source {
            shown.field("source", source);
        }
        shown.finish()
    }
}

impl StdError for Error {
    /// The failure underneath this one, when there is one.
    ///
    /// An [`ErrorKind::Api`] has none: the server's answer is the failure, and
    /// it is already what `Display` prints. A response-validation failure
    /// leads to the decode error that names the offending field, which carries
    /// a position `Display` leaves out. A connection failure leads to the
    /// transport's own error, which a caller can downcast to - unless it held
    /// a credential of the request, when it is a private redacted copy that
    /// cannot be downcast.
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.0.kind {
            ErrorKind::Api(_) => None,
            ErrorKind::ResponseValidation(error) => Some(error.decode_error()),
            _ => self.0.source.as_ref().map(|cause| &**cause as &(dyn StdError + 'static)),
        }
    }
}

// ---------------------------------------------------------------- ApiError

/// Which class of API failure a status code puts a response in.
///
/// The mapping is by status alone, so an endpoint answering a documented
/// status with an undocumented body still lands in the right class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ApiErrorKind {
    /// 400: the request was malformed.
    BadRequest,
    /// 401: the API key was missing, malformed or rejected.
    Authentication,
    /// 403: the key is valid and is not allowed to do this.
    PermissionDenied,
    /// 404: no such endpoint or resource.
    NotFound,
    /// 422: the request parsed and failed the server's validation.
    UnprocessableEntity,
    /// 429: the rate limit was exceeded. [`ApiError::retry_after`] may say
    /// how long to wait.
    RateLimit,
    /// Any status at or above 500: the server failed to answer the request.
    InternalServer,
    /// Any other unsuccessful status.
    Other,
}

impl ApiErrorKind {
    /// The class `status` falls in.
    fn of(status: StatusCode) -> Self {
        match status.as_u16() {
            400 => Self::BadRequest,
            401 => Self::Authentication,
            403 => Self::PermissionDenied,
            404 => Self::NotFound,
            422 => Self::UnprocessableEntity,
            429 => Self::RateLimit,
            500.. => Self::InternalServer,
            _ => Self::Other,
        }
    }
}

/// An unsuccessful HTTP response, with the body and metadata it came with.
///
/// Reach it through [`ErrorKind::Api`].
#[derive(Clone)]
pub struct ApiError {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    endpoint: Option<Box<str>>,
    message: Box<str>,
    error_type: Option<Box<str>>,
}

impl ApiError {
    /// Builds the error for a response the server refused, reading its message
    /// out of `body`.
    pub(crate) fn new(
        status: StatusCode,
        body: Bytes,
        headers: HeaderMap,
        endpoint: Option<Box<str>>,
    ) -> Self {
        let reading = BodyReading::of(&body);
        Self {
            status,
            headers,
            body,
            endpoint,
            message: reading.message,
            error_type: reading.error_type,
        }
    }

    /// The same, with a message the caller supplies instead of the one the
    /// body would give.
    pub(crate) fn with_message(
        status: StatusCode,
        body: Bytes,
        headers: HeaderMap,
        endpoint: Option<Box<str>>,
        message: impl Into<Box<str>>,
    ) -> Self {
        let reading = BodyReading::of(&body);
        Self {
            status,
            headers,
            body,
            endpoint,
            message: message.into(),
            error_type: reading.error_type,
        }
    }

    /// The response status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Which class of failure the status puts this response in.
    pub fn kind(&self) -> ApiErrorKind {
        ApiErrorKind::of(self.status)
    }

    /// The response headers, as they arrived.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The server's identifier for this request, from `x-typesafe-request-id`.
    ///
    /// `None` when the header is absent or is not text. This is the header's
    /// text exactly as it arrived; `Display` and `Debug` show it escaped and
    /// cut at 128 characters instead.
    pub fn request_id(&self) -> Option<&str> {
        request_id(&self.headers)
    }

    /// The method and URL the request went to, without credentials, query or
    /// fragment, as `GET https://api.typesafe.ai/v1/models`.
    ///
    /// `None` when the failure was built without a request to name.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The sentence the server gave for this failure.
    ///
    /// It is read from the body at the first of these that holds a string:
    /// `error`, `error.message`, `message`, `detail`, `detail.message`, or a
    /// list under `detail` whose entries are joined with `; ` as
    /// `<loc>: <msg>`. Failing all of those, it is the body itself, compacted
    /// when it is JSON; an empty body, or a body that is the JSON `null`,
    /// gives `status code (no body)`.
    ///
    /// Whichever part of the body it came from, the text is the server's, so
    /// it is made safe to print: a control character, or a format character
    /// that reorders or hides the text around it, is written as a Rust escape
    /// (`\n`, `\u{1b}`), and the text is cut at 200 characters, counted after
    /// escaping, and marked with U+2026. The Python SDK keeps a member's text
    /// as the server sent it and cuts only a body standing in for a message;
    /// here every path is cut, so a server cannot break, recolour or flood
    /// the log line of a caller that prints the error.
    /// [`body_text`](Self::body_text) still returns every byte.
    ///
    /// It can be empty, which is how a caller-supplied empty message survives
    /// to `Display`, where the status then stands alone.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The server's machine-readable name for this failure, from
    /// `detail.error_type`.
    ///
    /// The live API answers a request with no key with 403 and
    /// `authentication_error` here, which is the only way to tell that case
    /// apart from a key that exists and lacks a permission.
    ///
    /// This is the server's text as it arrived, for a caller to compare. It is
    /// never part of `Display`; `Debug` shows it escaped and cut at 128
    /// characters, as it does the request id.
    pub fn error_type(&self) -> Option<&str> {
        self.error_type.as_deref()
    }

    /// Whether the status or the server's error type names an authentication failure.
    ///
    /// True for 401, or for any status whose [`error_type`](Self::error_type)
    /// is exactly `authentication_error`. The live API uses 403 with that
    /// error type for a request without a key; [`kind`](Self::kind) still
    /// returns [`ApiErrorKind::PermissionDenied`] for that status.
    /// Only `detail.error_type` is read, not a top-level `error_type`.
    pub fn is_authentication(&self) -> bool {
        self.kind() == ApiErrorKind::Authentication
            || self.error_type() == Some("authentication_error")
    }

    /// The response body, exactly as it arrived.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The response body as text, with anything that is not UTF-8 replaced by
    /// `U+FFFD`.
    pub fn body_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// The response body, decoded as JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] when the body is not JSON, is nested deeper
    /// than this crate's limit, or does not fit `T`.
    pub fn body_json<'de, T>(&'de self) -> Result<T, DecodeError>
    where
        T: Deserialize<'de>,
    {
        codec::decode(&self.body)
    }

    /// How long the server asked the caller to wait, from `retry-after-ms` or
    /// `Retry-After`.
    ///
    /// Read for any status, not only 429: a 503 may carry the same headers.
    /// Reads the system clock, because `Retry-After` may be an HTTP date.
    pub fn retry_after(&self) -> Option<Duration> {
        parse_retry_after(&self.headers, SystemTime::now())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(formatter, self.endpoint(), self.status, &self.message, self.request_id())
    }
}

impl fmt::Debug for ApiError {
    /// Prints what identifies the failure, and nothing that could be a secret.
    ///
    /// The headers are reduced to their count and the body to its length: a
    /// response carries back whatever the request sent under `Authorization`
    /// or a cookie in some proxy configurations, and a `Debug` that is printed
    /// into a log must not be the thing that puts it there.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiError")
            .field("status", &self.status.as_u16())
            .field("kind", &self.kind())
            .field("endpoint", &self.endpoint())
            .field("request_id", &self.request_id().map(shown_name))
            .field("message", &self.message)
            .field("error_type", &self.error_type().map(shown_name))
            .field("headers", &HeaderCount(self.headers.len()))
            .field("body", &ByteCount(self.body.len()))
            .finish()
    }
}

impl StdError for ApiError {}

// ------------------------------------------------- ResponseValidationError

/// A successful HTTP response whose body this SDK could not read.
///
/// Reach it through [`ErrorKind::ResponseValidation`]. The raw body is kept,
/// so a caller can recover data this version of the SDK does not model.
#[derive(Clone)]
pub struct ResponseValidationError {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
    endpoint: Option<Box<str>>,
    source: DecodeError,
    message: Box<str>,
}

impl ResponseValidationError {
    /// Builds the error for a body that did not fit the response type.
    pub(crate) fn new(
        status: StatusCode,
        body: Bytes,
        headers: HeaderMap,
        endpoint: Option<Box<str>>,
        source: DecodeError,
    ) -> Self {
        let message = format!("Invalid response data at '{}'.", source.path()).into_boxed_str();
        Self { status, headers, body, endpoint, source, message }
    }

    /// The dotted path to the field that was missing or of the wrong type,
    /// such as `answers.spam.noul`.
    ///
    /// Empty when the body failed before any field could be named, which is
    /// what a syntax error or a document nested too deep gives.
    pub fn field_path(&self) -> &str {
        self.source.path()
    }

    /// The response status, which was a success status.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The response headers, as they arrived.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The server's identifier for this request, from `x-typesafe-request-id`.
    ///
    /// The header's text exactly as it arrived; `Display` and `Debug` show it
    /// escaped and cut at 128 characters instead.
    pub fn request_id(&self) -> Option<&str> {
        request_id(&self.headers)
    }

    /// The method and URL the request went to, without credentials, query or
    /// fragment.
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// The sentence for this failure, `Invalid response data at '<path>'.`.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The response body, exactly as it arrived.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// The response body as text, with anything that is not UTF-8 replaced by
    /// `U+FFFD`.
    pub fn body_text(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }

    /// The response body, decoded as JSON.
    ///
    /// This is how a caller recovers data the SDK's own response type dropped,
    /// including whatever made the decode fail.
    ///
    /// # Errors
    ///
    /// Returns a [`DecodeError`] when the body is not JSON, is nested deeper
    /// than this crate's limit, or does not fit `T`.
    pub fn body_json<'de, T>(&'de self) -> Result<T, DecodeError>
    where
        T: Deserialize<'de>,
    {
        codec::decode(&self.body)
    }

    /// The decode failure underneath, which carries the position as well as
    /// the path.
    pub fn decode_error(&self) -> &DecodeError {
        &self.source
    }
}

impl fmt::Display for ResponseValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(formatter, self.endpoint(), self.status, &self.message, self.request_id())
    }
}

impl fmt::Debug for ResponseValidationError {
    /// Prints what identifies the failure, and nothing that could be a secret;
    /// see the note on [`ApiError`]'s `Debug`.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseValidationError")
            .field("status", &self.status.as_u16())
            .field("endpoint", &self.endpoint())
            .field("request_id", &self.request_id().map(shown_name))
            .field("field_path", &self.field_path())
            .field("source", &self.source)
            .field("headers", &HeaderCount(self.headers.len()))
            .field("body", &ByteCount(self.body.len()))
            .finish()
    }
}

impl StdError for ResponseValidationError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

// --------------------------------------------------------------- rendering

/// Writes the one line an API-shaped failure renders as.
///
/// `<endpoint>: <status> <message> (request_id=<id>)`, with each optional part
/// left out when it is absent. The status is the bare number, not the number
/// and its reason phrase, so the line reads the same whether or not the status
/// is one the `http` crate has a name for. The message arrives already made
/// safe to print; the request id is the header's raw text and is made safe
/// here.
fn render(
    formatter: &mut fmt::Formatter<'_>,
    endpoint: Option<&str>,
    status: StatusCode,
    message: &str,
    request_id: Option<&str>,
) -> fmt::Result {
    if let Some(endpoint) = endpoint {
        write!(formatter, "{endpoint}: ")?;
    }
    write!(formatter, "{}", status.as_u16())?;
    if !message.is_empty() {
        write!(formatter, " {message}")?;
    }
    if let Some(request_id) = request_id {
        write!(formatter, " (request_id={})", shown_name(request_id))?;
    }
    Ok(())
}

/// A name the server chose - the request id, the error type - as a message
/// or a `Debug` shows it: escaped, and cut at 128 characters, as the SDK's log
/// lines show the request id.
///
/// `http` hands a header value over as text only when it is visible ASCII and
/// tabs, so for the request id this escapes the tabs and bounds the length; a
/// body member can hold anything.
fn shown_name(name: &str) -> String {
    text::bounded(&name, text::MAX_NAME_CHARS)
}

/// Stands in for the headers in a `Debug`, so their values never reach it.
struct HeaderCount(usize);

impl fmt::Debug for HeaderCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "<{} redacted>", self.0)
    }
}

/// Stands in for a body in a `Debug`, so its bytes never reach it.
struct ByteCount(usize);

impl fmt::Debug for ByteCount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "<{} bytes>", self.0)
    }
}

/// Renders the endpoint of a request as an error names it: the method, then
/// the URL without userinfo, query or fragment.
///
/// A URL's userinfo is a credential and its query may carry one, so neither
/// belongs in an error that will be logged. `http`'s `Uri` drops the fragment
/// when it parses, so there is none left to strip. A port is kept only when it
/// is not the default for the scheme, which is how the same endpoint reads the
/// same whether or not the caller spelled `:443` out.
pub(crate) fn format_endpoint(method: &Method, uri: &Uri) -> String {
    let mut out = String::with_capacity(method.as_str().len() + 1 + uri.path().len() + 32);
    out.push_str(method.as_str());
    out.push(' ');
    if let Some(scheme) = uri.scheme() {
        out.push_str(scheme.as_str());
        out.push_str("://");
    }
    if let Some(host) = uri.host() {
        out.push_str(host);
        if let Some(port) = uri.port_u16()
            && default_port(uri.scheme_str()) != Some(port)
        {
            out.push(':');
            out.push_str(&port.to_string());
        }
    }
    out.push_str(uri.path());
    out
}

/// The port a scheme implies, and so does not need spelling out.
fn default_port(scheme: Option<&str>) -> Option<u16> {
    match scheme {
        Some("http") => Some(80),
        Some("https") => Some(443),
        _ => None,
    }
}

// ------------------------------------------------------------ retry-after

/// How long the server asked the caller to wait, from the response headers.
///
/// `retry-after-ms` is read first and is a count of milliseconds;
/// `Retry-After` is read second and is either a count of seconds or an HTTP
/// date, measured against `now`. A value the first header cannot supply falls
/// through to the second, with one exception: a negative `Retry-After` ends
/// the search, because a server asking for a wait in the past is asking for no
/// wait at all and should not then be given the backoff a missing header would
/// produce.
///
/// The result is truncated to whole milliseconds, which is the precision the
/// millisecond header carries and far finer than any retry schedule needs.
pub(crate) fn parse_retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    for (name, per_unit) in
        [(RETRY_AFTER_MS_HEADER, 1.0_f64), (header::RETRY_AFTER.as_str(), 1000.0_f64)]
    {
        let Some(raw) = headers.get(name).and_then(|value| value.to_str().ok()) else {
            continue;
        };
        let trimmed = raw.trim();
        // An empty header is a request to wait no time at all, not a parse
        // failure: `Retry-After:` with nothing after it means zero.
        let spelled = if trimmed.is_empty() { "0" } else { trimmed };
        match spelled.parse::<f64>() {
            // A value that is not finite says nothing about how long to wait,
            // so the next header gets its turn.
            Ok(seconds) if !seconds.is_finite() => {}
            Ok(seconds) if seconds >= 0.0 => {
                let millis = seconds * per_unit;
                if millis.is_finite() {
                    return Some(millis_to_duration(millis));
                }
            }
            Ok(_) if name == header::RETRY_AFTER.as_str() => return None,
            Ok(_) => {}
            Err(_) if name == header::RETRY_AFTER.as_str() => {
                if let Ok(when) = httpdate::parse_http_date(raw) {
                    // A date already past means the wait is over, not that the
                    // header was unusable, so it answers zero rather than
                    // falling through to a backoff.
                    let wait = when.duration_since(now).unwrap_or(Duration::ZERO);
                    return Some(Duration::from_millis(
                        u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                    ));
                }
            }
            Err(_) => {}
        }
    }
    None
}

/// Turns a finite, non-negative count of milliseconds into a [`Duration`],
/// saturating rather than wrapping on a value no `Duration` can hold.
fn millis_to_duration(millis: f64) -> Duration {
    // A float-to-integer cast saturates in Rust, so a value past `u64::MAX`
    // lands on `u64::MAX` rather than wrapping to a short wait.
    Duration::from_millis(millis as u64)
}

// ------------------------------------------------------- the error message

/// What an error body says: the sentence for it, and the machine-readable
/// name beside it.
struct BodyReading {
    message: Box<str>,
    error_type: Option<Box<str>>,
}

/// The members of an error body this crate reads, each captured as raw JSON so
/// that a member of an unexpected type costs a failed decode of that member
/// rather than a failed decode of the whole body.
#[derive(Deserialize)]
struct Envelope {
    error: Option<RawJson>,
    message: Option<RawJson>,
    detail: Option<RawJson>,
}

/// The two members read from an object under `detail`.
#[derive(Deserialize)]
struct Detail {
    message: Option<RawJson>,
    error_type: Option<RawJson>,
}

/// The one member read from an object under `error`.
#[derive(Deserialize)]
struct MessageMember {
    message: Option<RawJson>,
}

/// One entry of a list under `detail`, in the shape a validation framework
/// reports a field error.
#[derive(Deserialize)]
struct DetailEntry {
    msg: Option<RawJson>,
    loc: Option<RawJson>,
}

impl BodyReading {
    /// Reads `body` the way the API's own SDKs do.
    fn of(body: &[u8]) -> Self {
        if body.is_empty() {
            return Self::no_body();
        }
        match first_token(body) {
            Some(b'{') => Self::of_object(body),
            // A JSON string body is its own message. An empty one leaves the
            // status to stand alone, which is what a caller-supplied empty
            // message does too.
            Some(b'"') => match codec::decode::<String>(body) {
                Ok(text) => Self::said(bounded(&text)),
                Err(_) => Self::said(text_message(body)),
            },
            // Everything else is a number, a boolean, a list or `null` - or it
            // is not JSON at all, which only parsing can tell.
            _ => match codec::decode::<Option<IgnoredAny>>(body) {
                Ok(None) => Self::no_body(),
                Ok(Some(_)) => Self::said(json_message(body)),
                Err(failure) => Self::said(unparsed_message(body, &failure)),
            },
        }
    }

    /// The reading for a body that is empty, or is the JSON `null` that stands
    /// for an empty one.
    fn no_body() -> Self {
        Self { message: "status code (no body)".into(), error_type: None }
    }

    /// The reading for a body with a message and nothing else to report.
    fn said(message: impl Into<Box<str>>) -> Self {
        Self { message: message.into(), error_type: None }
    }

    /// The object case, where the message can be in any of six places.
    fn of_object(body: &[u8]) -> Self {
        let envelope = match codec::decode::<Envelope>(body) {
            Ok(envelope) => envelope,
            Err(failure) => return Self::said(unparsed_message(body, &failure)),
        };
        let detail = envelope.detail.as_ref().and_then(|raw| raw.decode::<Detail>().ok());
        let error_type = detail
            .as_ref()
            .and_then(|detail| detail.error_type.as_ref())
            .and_then(as_text)
            .map(Into::into);

        let message = as_text_of(&envelope.error)
            .or_else(|| {
                member_text(&envelope.error, |raw| {
                    raw.decode::<MessageMember>().ok().and_then(|it| it.message)
                })
            })
            .or_else(|| as_text_of(&envelope.message))
            .or_else(|| as_text_of(&envelope.detail))
            .or_else(|| {
                detail.as_ref().and_then(|detail| detail.message.as_ref()).and_then(as_text)
            })
            .or_else(|| joined_detail_list(&envelope.detail))
            .filter(|message| !message.is_empty());

        Self {
            message: message.map_or_else(|| json_message(body), |message| bounded(&message)),
            error_type,
        }
    }
}

/// The value as a JSON string, or nothing when it is any other shape.
fn as_text(raw: &RawJson) -> Option<String> {
    raw.decode::<String>().ok()
}

/// The same, for a member that may be absent.
fn as_text_of(raw: &Option<RawJson>) -> Option<String> {
    raw.as_ref().and_then(as_text)
}

/// A string reached by descending one level into a member.
fn member_text(
    raw: &Option<RawJson>,
    member: impl FnOnce(&RawJson) -> Option<RawJson>,
) -> Option<String> {
    raw.as_ref().and_then(member).as_ref().and_then(as_text)
}

/// A list under `detail` rendered as one sentence.
///
/// Entries that are not objects, and objects whose `msg` is not a string, are
/// dropped; an entry's `loc` becomes a dotted prefix with the segment `body`
/// left out, because it names the request part rather than the field.
fn joined_detail_list(raw: &Option<RawJson>) -> Option<String> {
    let entries = raw.as_ref()?.decode::<Vec<RawJson>>().ok()?;
    let mut parts = Vec::with_capacity(entries.len());
    for entry in &entries {
        let Ok(entry) = entry.decode::<DetailEntry>() else {
            continue;
        };
        let Some(message) = entry.msg.as_ref().and_then(as_text) else {
            continue;
        };
        let path = entry.loc.as_ref().map(location_path).unwrap_or_default();
        parts.push(if path.is_empty() { message } else { format!("{path}: {message}") });
    }
    if parts.is_empty() { None } else { Some(parts.join("; ")) }
}

/// The dotted form of a validation framework's `loc` list.
fn location_path(raw: &RawJson) -> String {
    let Ok(segments) = raw.decode::<Vec<RawJson>>() else {
        return String::new();
    };
    let mut path = String::new();
    for segment in &segments {
        // A segment is a field name or an index into a list. Anything else is
        // left out rather than guessed at.
        let rendered = match segment.decode::<String>() {
            Ok(name) if name == "body" => continue,
            Ok(name) => name,
            Err(_) => match segment.decode::<i64>() {
                Ok(index) => index.to_string(),
                Err(_) => continue,
            },
        };
        if !path.is_empty() {
            path.push('.');
        }
        path.push_str(&rendered);
    }
    path
}

/// A JSON body used as its own message, for a body that says nothing this
/// crate recognizes.
///
/// The text is compacted, so a pretty-printed body does not put newlines into
/// a one-line message, and bounded like every message read from a body.
fn json_message(body: &[u8]) -> Box<str> {
    let text = String::from_utf8_lossy(body);
    bounded(&compact(&text))
}

/// A body that is not JSON at all, used as its own message.
///
/// Nothing is compacted here: the bytes are not JSON, so whitespace between
/// them is not punctuation and dropping it would change what the server said.
/// A line break or any other control character among them is escaped instead.
fn text_message(body: &[u8]) -> Box<str> {
    bounded(&String::from_utf8_lossy(body))
}

/// A body no member could be read out of because it did not parse.
///
/// A body the depth guard turned away is JSON all the same - it is only more
/// deeply nested than this crate will walk - so it is compacted like any other
/// JSON body. Anything else did not parse because it is not JSON, and its
/// whitespace is kept.
fn unparsed_message(body: &[u8], failure: &DecodeError) -> Box<str> {
    if failure.kind() == DecodeErrorKind::TooDeep { json_message(body) } else { text_message(body) }
}

/// The server's `text` as a message holds it: escaped and cut at
/// [`text::MAX_MESSAGE_CHARS`] characters as `crate::text` describes, with a
/// backslash kept as it is.
///
/// The backslash is kept because a JSON body standing in for a message already
/// spells its escapes with one (`"a\nb"`), and a second pass would turn every
/// one of them into `\\n`; what the message loses in exactness `body_text`
/// keeps.
fn bounded(text: &str) -> Box<str> {
    text::bounded(&text, text::MAX_MESSAGE_CHARS).into_boxed_str()
}

/// The first byte of `body` that is not JSON whitespace.
fn first_token(body: &[u8]) -> Option<u8> {
    body.iter().copied().find(|byte| !byte.is_ascii_whitespace())
}

/// Drops the whitespace between JSON tokens, leaving the whitespace inside
/// strings alone.
///
/// The text is not re-encoded: numbers, escapes and key order come out exactly
/// as the server wrote them, so the message shows what arrived rather than
/// what a round trip through this crate's codec would have made of it. Text
/// that is not JSON comes back unchanged, because nothing in it is outside a
/// string that a JSON reader would recognize.
fn compact(text: &str) -> Cow<'_, str> {
    let mut in_string = false;
    let mut escaped = false;
    // Stays `None` while nothing has been dropped, which is what lets a body
    // that is already compact come back borrowed.
    let mut kept: Option<String> = None;
    let mut copied = 0;
    for (at, byte) in text.bytes().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b' ' | b'\t' | b'\n' | b'\r' => {
                let kept = kept.get_or_insert_with(|| String::with_capacity(text.len()));
                kept.push_str(&text[copied..at]);
                copied = at + 1;
            }
            _ => {}
        }
    }
    match kept {
        Some(mut kept) => {
            kept.push_str(&text[copied..]);
            Cow::Owned(kept)
        }
        None => Cow::Borrowed(text),
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
