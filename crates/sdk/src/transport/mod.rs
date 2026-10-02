//! The seam between this crate and whatever sends the bytes.
//!
//! The SDK drives a `tower`-shaped service rather than an HTTP client, so a
//! caller can put their own stack underneath it - a recorder, a proxy, an
//! in-memory harness - without this crate knowing about it, and the default
//! stack, `HyperTransport` (with the `hyper` feature), is one implementation of that seam rather than a
//! hard dependency. Any [`tower_service::Service`] over `http` requests with
//! this crate's [`Body`] is a transport; [`HttpService`] names the bounds.
//!
//! The request body is this crate's own type implementing `http-body`, not a
//! type borrowed from a pre-1.0 crate, so the public signature does not commit
//! a caller to a version of somebody else's dependency.
//!
//! A transport sends one request. Everything around that - the headers every
//! request carries, the deadline, reading the response under the size limit,
//! turning a failure into this crate's [`Error`] - happens here, once per
//! attempt, whichever transport is underneath.

#[cfg(feature = "hyper")]
mod gate;
#[cfg(feature = "hyper")]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
mod hyper;

use std::{
    convert::Infallible,
    error::Error as StdError,
    fmt,
    future::{Future, poll_fn},
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::CONTENT_TYPE,
};
use http_body::{Frame, SizeHint};
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use tower_service::Service;

#[cfg(feature = "hyper")]
pub(crate) use self::hyper::TransportSettings;
#[cfg(feature = "hyper")]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub use self::hyper::{HttpVersion, HyperResponseFuture, HyperTransport, ResponseBody};
use crate::{
    config::Config,
    constants::{
        JSON_CONTENT_TYPE, PROTECTED_HEADERS, RETRY_COUNT_HEADER, RUNTIME_IDENTIFIER,
        SDK_IDENTIFIER, TRANSPORT_HEADERS,
    },
    error::{ApiError, Error, ErrorKind, format_endpoint},
    question::upsert,
    redact::{self, Credentials, Outcome},
    telemetry,
    text::{self, Backslash, SafeText},
};

/// The error type a transport may fail with: any error that can cross
/// threads.
pub type BoxError = Box<dyn StdError + Send + Sync>;

// ------------------------------------------------------------------- Body

/// The body of a request the SDK sends: finished bytes, handed over in one
/// frame.
///
/// Its length is known before the first byte is sent, so `size_hint` is exact
/// and a transport can write a `Content-Length` without buffering. Cloning it
/// shares the bytes rather than copying them.
///
/// `Debug` prints the length only: a body carries the caller's `state`, which
/// may be personal data.
#[derive(Clone, Default)]
pub struct Body {
    /// `None` once the one frame has been handed out, or for an empty body.
    data: Option<Bytes>,
}

impl Body {
    /// A body with no bytes, as a `GET` carries.
    #[must_use]
    pub fn empty() -> Self {
        Self { data: None }
    }

    /// The bytes not yet handed out as a frame.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.as_ref().map_or(0, Bytes::len)
    }

    /// Whether no bytes are left to hand out.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<Bytes> for Body {
    fn from(bytes: Bytes) -> Self {
        // An empty buffer is no frame at all, so that `is_end_stream` is true
        // from the start and a transport sends no empty DATA frame.
        Self { data: (!bytes.is_empty()).then_some(bytes) }
    }
}

impl fmt::Debug for Body {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Body").field("len", &self.len()).finish()
    }
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        // `Body` holds nothing that cares where it lives in memory, so the
        // pinned reference can be turned back into a plain one.
        Poll::Ready(self.get_mut().data.take().map(|bytes| Ok(Frame::data(bytes))))
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.len() as u64)
    }
}

// ------------------------------------------------------------ HttpService

mod sealed {
    /// Keeps [`HttpService`](super::HttpService) implemented only through its
    /// blanket implementation.
    pub trait Sealed {}
}

/// What a transport has to be: a `tower` service that takes an `http` request
/// with this crate's [`Body`] and answers with an `http` response.
///
/// It is implemented for every [`tower_service::Service`] that fits, and for
/// nothing else, so it is a name for a set of bounds rather than a trait to
/// implement: implement `Service` and a type is a transport. The service is
/// cloned for every request and driven with its own `poll_ready`, so a
/// service with back-pressure keeps it; `HyperTransport` (with the `hyper`
/// feature) is always ready.
///
/// The error of the service, and of its response body, is anything that can
/// cross threads. It is kept as the [`source`](StdError::source) of the
/// connection [`Error`] the call fails with.
pub trait HttpService: sealed::Sealed + Clone + Send + Sync + 'static {
    /// The body of the responses the service answers with.
    type ResponseBody: http_body::Body<Data: Send, Error: Into<BoxError>> + Send + 'static;
    /// What the service fails with when it cannot produce a response.
    type Error: Into<BoxError>;
    /// The future a call returns.
    type Future: Future<Output = Result<Response<Self::ResponseBody>, Self::Error>> + Send;

    /// [`Service::poll_ready`], forwarded.
    ///
    /// # Errors
    ///
    /// Returns the service's error when it can take no more requests.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;

    /// [`Service::call`], forwarded.
    fn call(&mut self, request: Request<Body>) -> Self::Future;
}

impl<S, B> sealed::Sealed for S
where
    S: Service<Request<Body>, Response = Response<B>> + Clone + Send + Sync + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send,
    B: http_body::Body<Data: Send, Error: Into<BoxError>> + Send + 'static,
{
}

impl<S, B> HttpService for S
where
    S: Service<Request<Body>, Response = Response<B>> + Clone + Send + Sync + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send,
    B: http_body::Body<Data: Send, Error: Into<BoxError>> + Send + 'static,
{
    type ResponseBody = B;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        Service::poll_ready(self, cx)
    }

    fn call(&mut self, request: Request<Body>) -> S::Future {
        Service::call(self, request)
    }
}

// ------------------------------------------------------- header assembly

/// The headers every request of one kind carries, built once per client.
///
/// Precedence, lowest first: the client's default headers, then the SDK's
/// own, which no default can replace, and, when the request has a body,
/// `Content-Type: application/json`. A default `X-TypeSafe-Retry-Count` is
/// dropped: the SDK sets that header on retries and only there. So is a
/// default framing or connection header ([`TRANSPORT_HEADERS`]). Per-call
/// headers are applied on top of this map for each attempt; see
/// [`call_headers`]. `User-Agent` carries the value the configuration built
/// once, and `X-TypeSafe-Runtime` is left out when the configuration says so;
/// a caller's header of either name is dropped all the same.
pub(crate) fn base_headers(config: &Config, with_body: bool) -> HeaderMap {
    let defaults = config.default_headers();
    // Room for every default, the protected five and `Content-Type`, so
    // building the map never grows it.
    let mut headers = HeaderMap::with_capacity(defaults.len() + PROTECTED_HEADERS.len() + 1);
    for (name, value) in defaults {
        if !is_sdk_owned(name, with_body) {
            headers.append(name, value.clone());
        }
    }
    let [authorization, accept, user_agent, sdk, runtime] = PROTECTED_HEADERS;
    headers.insert(authorization, config.authorization().clone());
    headers.insert(accept, JSON_CONTENT_TYPE);
    headers.insert(user_agent, config.user_agent().clone());
    headers.insert(sdk, SDK_IDENTIFIER);
    if config.send_runtime_header() {
        headers.insert(runtime, RUNTIME_IDENTIFIER.clone());
    }
    if with_body {
        headers.insert(CONTENT_TYPE, JSON_CONTENT_TYPE);
    }
    headers
}

/// Whether the SDK or its transport owns `name` on a request, so a caller
/// cannot set it.
fn is_sdk_owned(name: &HeaderName, with_body: bool) -> bool {
    PROTECTED_HEADERS.contains(name)
        || *name == RETRY_COUNT_HEADER
        || (with_body && *name == CONTENT_TYPE)
        || TRANSPORT_HEADERS.contains(name)
}

/// Parses the headers a caller set on one call, dropping the ones the SDK
/// owns.
///
/// A later header of a name replaces an earlier one, as a later key does in a
/// Python mapping. The protected headers, `X-TypeSafe-Retry-Count`, and
/// `Content-Type` on a request with a body are dropped without an error, as
/// the Python SDK overrides them. The framing and connection headers
/// ([`TRANSPORT_HEADERS`]) are dropped the same way: they belong to the
/// transport. `Host` is kept.
///
/// # Errors
///
/// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
/// error when a name is not a valid header name or a value is not a valid
/// header value. The message names the header and never repeats a value.
pub(crate) fn call_headers<'a, I>(
    raw: I,
    with_body: bool,
) -> Result<Vec<(HeaderName, HeaderValue)>, Error>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
    I::IntoIter: ExactSizeIterator,
{
    let raw = raw.into_iter();
    let mut parsed: Vec<(HeaderName, HeaderValue)> = Vec::with_capacity(raw.len());
    for (name, value) in raw {
        let (name, value) = parse_header(name, value, "").map_err(Error::invalid_request)?;
        if is_sdk_owned(&name, with_body) {
            continue;
        }
        upsert(&mut parsed, name, value);
    }
    Ok(parsed)
}

/// Parses one header, or says which part of it is not valid.
///
/// The message names the header by its name - escaped, and cut at 128
/// characters, since a name that fails here can be anything - and never
/// repeats the value: a value is where a caller puts a token. `whose` is the word before `header` in the message: `default ` for
/// a client default, empty for a per-call header.
pub(crate) fn parse_header(
    name: &str,
    value: &str,
    whose: &str,
) -> Result<(HeaderName, HeaderValue), String> {
    let parsed = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
        format!("The {whose}header name {} is not a valid HTTP header name.", text::quoted(name))
    })?;
    let value = HeaderValue::from_str(value).map_err(|_| {
        format!(
            "The value of the {whose}header {} is not a valid HTTP header value.",
            text::quoted(name)
        )
    })?;
    Ok((parsed, value))
}

// ------------------------------------------------------------ one attempt

/// Everything one request needs that does not change between its attempts.
#[derive(Clone, Copy)]
pub(crate) struct Exchange<'a> {
    pub(crate) method: &'a Method,
    pub(crate) uri: &'a Uri,
    /// The client's headers for this kind of request; see [`base_headers`].
    pub(crate) base_headers: &'a HeaderMap,
    /// The call's own headers, already parsed; see [`call_headers`].
    pub(crate) call_headers: &'a [(HeaderName, HeaderValue)],
    /// The deadline of one attempt, or `None` for no deadline.
    pub(crate) deadline: Option<Duration>,
    pub(crate) config: &'a Config,
}

/// What a successful attempt returns: a success status, the headers and the
/// whole body.
type Received = (StatusCode, HeaderMap, Bytes);

/// Sends one attempt of a request and reads its response.
///
/// `retry` is how many attempts came before this one: from 1 it is sent as
/// `X-TypeSafe-Retry-Count`, and at 0 the header is absent. `body` is the
/// finished request body, handed over by value so that a first attempt costs
/// no copy and a later one only a reference count.
///
/// The deadline covers the whole attempt: waiting for the transport to be
/// ready, connecting, sending, and reading the response body.
///
/// # Errors
///
/// - [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the deadline
///   passes first.
/// - [`ErrorKind::Api`](crate::ErrorKind::Api) for any status outside 2xx.
/// - [`ErrorKind::Connection`](crate::ErrorKind::Connection) when the
///   transport fails or the body cannot be read; the transport's own error, or
///   its redacted copy when it held a credential of the request, is the
///   [`source`](StdError::source).
/// - [`ErrorKind::ResponseTooLarge`](crate::ErrorKind::ResponseTooLarge) when
///   a success response's body is larger than the limit. A body over the
///   limit is not read past it, and a failure response whose body is over the
///   limit is an API error with the status and headers and no body.
pub(crate) async fn attempt<S>(
    service: &S,
    exchange: Exchange<'_>,
    retry: u32,
    body: Option<Bytes>,
) -> Result<Received, Error>
where
    S: HttpService,
{
    let events = telemetry::Exchange::new(
        exchange.method,
        exchange.uri,
        retry,
        exchange.config.omit_endpoint_host(),
    );
    // The header map and the request are built inside a block so that their
    // storage ends before the await. An async function keeps every local of
    // a scope that is still open when it suspends, even one whose value was
    // moved out, so written at the top level the two would ride in the
    // future beside the copy the transport call already holds: 352 bytes of
    // every call's future, which over a custom transport tips it over the
    // size at which tokio boxes a spawned future in a debug build (2,048
    // bytes). Over the default transport hyper's response future keeps a
    // System One call over that size either way (2,328 bytes), so a
    // debug-build System One call spawned on the default client is boxed; a
    // model listing (2,016 bytes) is not.
    let (started, exchanged) = {
        let mut headers = exchange.base_headers.clone();
        for (name, value) in exchange.call_headers {
            headers.insert(name.clone(), value.clone());
        }
        if retry > 0 {
            headers.insert(RETRY_COUNT_HEADER, HeaderValue::from(retry));
        }
        telemetry::sending(events, &headers, body.as_ref());
        let started = telemetry::clock();

        let mut request = Request::new(body.map_or_else(Body::empty, Body::from));
        *request.method_mut() = exchange.method.clone();
        *request.uri_mut() = exchange.uri.clone();
        *request.headers_mut() = headers;
        (started, exchange_once(service, request, exchange.config.max_response_bytes()))
    };
    let outcome = match exchange.deadline {
        Some(deadline) => tokio::time::timeout(deadline, exchanged)
            .await
            .unwrap_or_else(|_| Err(Failure::Error(Error::timeout(deadline)))),
        None => exchanged.await,
    };

    // A response is reported by its status; an attempt that ended without
    // one is reported by what ended it. An API error is not reported a second
    // time as a failure: its message can come from the body.
    let failure = match outcome {
        Ok((status, headers, body)) => {
            telemetry::responded(events, status, &headers, started);
            telemetry::received(events, status, &headers, &body, started);
            if status.is_success() {
                return Ok((status, headers, body));
            }
            return Err(ApiError::new(status, body, headers, Some(endpoint(exchange))).into());
        }
        Err(Failure::TooLarge { status, headers }) if !status.is_success() => {
            telemetry::responded(events, status, &headers, started);
            return Err(ApiError::with_message(
                status,
                Bytes::new(),
                headers,
                Some(endpoint(exchange)),
                Error::response_too_large(exchange.config.max_response_bytes()).to_string(),
            )
            .into());
        }
        Err(Failure::TooLarge { .. }) => {
            Error::response_too_large(exchange.config.max_response_bytes())
        }
        Err(Failure::Error(error)) => redacted(error, exchange),
    };
    telemetry::failed(events, &failure, started);
    Err(failure)
}

/// How an attempt can end short of a response body.
enum Failure {
    /// Anything that becomes an [`Error`] without needing the response.
    Error(Error),
    /// The body was larger than the limit. Whether that is a response too
    /// large or an API error depends on the status, which is kept.
    TooLarge { status: StatusCode, headers: HeaderMap },
}

/// Waits for the transport, sends `request`, and reads the whole response.
async fn exchange_once<S>(
    service: &S,
    request: Request<Body>,
    limit: usize,
) -> Result<Received, Failure>
where
    S: HttpService,
{
    // A clone per call, as `tower` intends: readiness belongs to the handle
    // that is then called, and a shared one would let another task take the
    // slot this one waited for. The handle is dropped once the call has been
    // made: the response future owns what it needs, and a handle kept to the
    // end would be stored in this future through the whole body read.
    let called = {
        let mut service = service.clone();
        poll_fn(|cx| service.poll_ready(cx))
            .await
            .map_err(|error| Failure::Error(connection(error)))?;
        service.call(request)
    };
    let response = called.await.map_err(|error| Failure::Error(connection(error)))?;
    let (parts, body) = response.into_parts();

    // A declared length over the limit is refused before a byte is read.
    if http_body::Body::size_hint(&body).lower() > limit as u64 {
        return Err(Failure::TooLarge { status: parts.status, headers: parts.headers });
    }
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok((parts.status, parts.headers, collected.to_bytes())),
        Err(error) if error.is::<LengthLimitError>() => {
            Err(Failure::TooLarge { status: parts.status, headers: parts.headers })
        }
        Err(error) => Err(Failure::Error(connection(error))),
    }
}

/// The error a transport failure becomes.
///
/// An error of this crate's own type, raised by its transport or built by a
/// caller's, is passed through as it is. Anything else is a connection
/// failure whose message is the chain of the transport's own messages - the
/// cause stays reachable as the [`source`](StdError::source).
pub(crate) fn connection(error: impl Into<BoxError>) -> Error {
    match error.into().downcast::<Error>() {
        Ok(ours) => *ours,
        Err(other) => Error::connection(connection_message(&*other), Some(other)),
    }
}

/// `Connection error: ` and then every message of the error chain, joined
/// with `: `.
///
/// The chain is the transport's, not the request's: it names what failed -
/// `tcp connect error: Connection refused`, `invalid peer certificate:
/// UnknownIssuer` - and holds neither a header nor a body. It is cut after
/// eight links, which no real chain reaches.
///
/// It can still hold text the server chose: an HTTP/2 GOAWAY's debug data
/// (up to a frame, 16 KiB by default), the subject of a certificate the
/// platform refused, and whatever a caller's own transport puts in its
/// `Display`. So every link is written as text this SDK did not write -
/// control and format characters escaped - and the whole of it after the
/// prefix is cut at [`text::MAX_MESSAGE_CHARS`] characters and marked
/// with U+2026; the full chain stays reachable through
/// [`source`](StdError::source). A backslash is written as it is: h2 and
/// rustls already print their own text through `Debug`, where a doubled
/// backslash would only make an escape harder to read, and nothing is ever
/// parsed back out of this message.
fn connection_message(error: &(dyn StdError + 'static)) -> String {
    cut(&render_uncut(error))
}

/// What every connection error's message starts with.
const CONNECTION_PREFIX: &str = "Connection error: ";

/// A connection error's message before it is cut: the text, and the byte
/// offset at which each of its pieces ends. A piece is one escaped character
/// or the `": "` between two links, and a cut never splits one.
struct Uncut {
    text: String,
    ends: Vec<usize>,
}

/// The whole message for `error`: [`CONNECTION_PREFIX`], then up to eight
/// links of the chain, each escaped, joined with `": "`.
fn render_uncut(error: &(dyn StdError + 'static)) -> Uncut {
    let mut message = SafeText::after(String::from(CONNECTION_PREFIX), usize::MAX, Backslash::Keep);
    let mut ends = Vec::new();
    let mut character_bytes = [0; 4];
    let mut link = Some(error);
    for index in 0..8 {
        let Some(current) = link else { break };
        if index > 0 {
            message.fixed(": ");
            ends.push(message.byte_len());
        }
        for character in current.to_string().chars() {
            message.untrusted(character.encode_utf8(&mut character_bytes), usize::MAX);
            ends.push(message.byte_len());
        }
        link = current.source();
    }
    Uncut { text: message.into_string(), ends }
}

/// `uncut` cut at [`text::MAX_MESSAGE_CHARS`] characters after the prefix,
/// the last whole piece that fits followed by U+2026.
fn cut(uncut: &Uncut) -> String {
    let mut message = String::from(CONNECTION_PREFIX);
    let mut chars = 0;
    let mut start = CONNECTION_PREFIX.len();
    for &end in &uncut.ends {
        let piece = &uncut.text[start..end];
        let len = piece.chars().count();
        if chars + len > text::MAX_MESSAGE_CHARS {
            message.push('\u{2026}');
            break;
        }
        message.push_str(piece);
        chars += len;
        start = end;
    }
    message
}

impl Uncut {
    /// This message with every form of a credential after the prefix
    /// replaced by `***`.
    ///
    /// It runs over the escaped text, since escaping can form a credential no
    /// link holds: a tab written as `\t`, or two links joined by `": "`. A
    /// match that covers part of a piece takes the whole piece, and the
    /// replacement is one piece, so the cut can neither split it nor leave
    /// part of the credential before it.
    fn redacted(&self, credentials: &Credentials) -> Self {
        let prefix = CONNECTION_PREFIX.len();
        let mut found = credentials
            .matches(&self.text[prefix..])
            .map(|range| range.start + prefix..range.end + prefix);
        let mut next = found.next();
        let mut text = String::from(CONNECTION_PREFIX);
        let mut ends = Vec::with_capacity(self.ends.len());
        let mut start = prefix;
        let mut index = 0;
        while let Some(&end) = self.ends.get(index) {
            match &next {
                Some(first) if first.start < end => {
                    let mut group_end = first.end;
                    next = found.next();
                    while let Some(&piece_end) = self.ends.get(index) {
                        index += 1;
                        while let Some(another) = &next
                            && another.start < piece_end
                        {
                            group_end = group_end.max(another.end);
                            next = found.next();
                        }
                        start = piece_end;
                        if piece_end >= group_end {
                            break;
                        }
                    }
                    text.push_str("***");
                }
                _ => {
                    text.push_str(&self.text[start..end]);
                    start = end;
                    index += 1;
                }
            }
            ends.push(text.len());
        }
        Self { text, ends }
    }
}

/// `error` with the request's credentials kept out of it.
///
/// Only a connection error is searched. With a cause, the cause is the
/// transport's error and the message is built from it: a chain that holds
/// none is returned as it is; see [`redact::copy_chain`] for the other two
/// outcomes. Without one, a custom service built the error with
/// [`Error::connection`], and its message is all it holds: every form of a
/// credential in it is replaced by `***`, and nothing else is changed. Any
/// other kind is returned as it is: an [`ApiError`] a service built keeps the
/// body and headers it was given. The credentials are read from the headers
/// the request was built from, and only here, after the attempt failed.
pub(crate) fn redacted(error: Error, exchange: Exchange<'_>) -> Error {
    if !matches!(error.kind(), ErrorKind::Connection) {
        return error;
    }
    let credentials = Credentials::new(
        exchange
            .base_headers
            .iter()
            .chain(exchange.call_headers.iter().map(|(name, value)| (name, value))),
    );
    let Some(source) = StdError::source(&error) else {
        let message = error.to_string();
        if !credentials.occur_in(&message) {
            return error;
        }
        return Error::connection(credentials.redact(&message), None);
    };
    // The fixed prefix is the SDK's own text: only what follows it came from
    // the transport, as the Python SDK redacts the error before prefixing it.
    let message = error.to_string();
    let transport_text = message.strip_prefix(CONNECTION_PREFIX).unwrap_or(&message);
    match redact::copy_chain(source, transport_text, &credentials) {
        Outcome::Kept => error,
        Outcome::MessageOnly => {
            let (_, source) = error.into_parts();
            let redacted = credentials.redact(transport_text);
            Error::connection(format!("{CONNECTION_PREFIX}{redacted}"), source)
        }
        Outcome::Replaced(link) => {
            let message = cut(&render_uncut(&link).redacted(&credentials));
            Error::connection(message, Some(Box::new(link)))
        }
    }
}

/// The endpoint of `exchange` as an error names it.
fn endpoint(exchange: Exchange<'_>) -> Box<str> {
    format_endpoint(exchange.method, exchange.uri).into_boxed_str()
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
