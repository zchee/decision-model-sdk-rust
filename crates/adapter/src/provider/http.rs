//! The HTTP request the built-in providers share: the key header, one POST,
//! the per-attempt deadline, the response-size cap and the mapping of
//! failures to the adapter's errors.
//!
//! Ported from `_utils/error_handling.py` of system-one-adapter-python, whose
//! `map_provider_error` turns a vendor SDK's exception into the TypeSafe
//! SDK's. There is no vendor SDK here: the three providers send their own
//! JSON through [`post`], and the mapping is made from the response itself.
//!
//! What a provider builds once, in its `build()`: the key header
//! ([`key_header`]), the base URL ([`BaseUrl`]) and the [`Endpoint`] made from
//! it, the [`Limits`], and the header map ([`request_headers`]). What it does
//! per attempt: one [`post`].
//!
//! The key is turned into a header value in one place, and this module puts
//! it into no text of its own: no error message, `Debug` or event that this
//! module writes holds it. Text that others wrote can still hold it: a
//! response, and the error of a caller's own service, which may hold the
//! request's headers. So a failed call's error chain is searched before it
//! is kept, and the header names and values of a response with a status
//! outside 2xx, and its body when that was read, are searched before the
//! API error is built from them, and the error that was built is searched
//! as an error chain is; a body over the size limit is not read to its end,
//! not searched and not kept. The body of a 2xx response is searched only
//! when the provider reads a non-answer from it ([`Exchange::screened`]),
//! and only to decide the non-answer's text and the recorded stop reason:
//! so a non-answer's message can be the fixed [`REASON_NOT_SHOWN`] after
//! the vendor's name instead of a status or stop reason. A success that
//! yields answers is not searched, and the trace holds every body as
//! received.
//!
//! The search looks for the key as written, as `Debug` writes it and as a
//! JSON string holds it, the spellings listed at [`KeyHeader::holds_key`],
//! and for nothing else: a key that a service or a server transformed in
//! another way, into hex or base64 say, or split over two links of the
//! chain, is not found.

use std::{
    error::Error as StdError,
    fmt::{self, Write as _},
    future::{Future, poll_fn},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
    header::{ACCEPT, CONTENT_TYPE, USER_AGENT},
    uri::{Authority, Scheme},
};
use http_body::{Frame, SizeHint};
use http_body_util::{BodyExt as _, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{self, connect::HttpConnector},
    rt::{TokioExecutor, TokioTimer},
};
use rustls::{ClientConfig, pki_types::CertificateDer};
use rustls_platform_verifier::{BuilderVerifierExt as _, Verifier};
use secrecy::{ExposeSecret as _, SecretString};
use serde::de::IgnoredAny;
use tower_service::Service;
use typesafe_sdk::{ApiError, Body, BoxError, Error as SdkError, HttpService};

use crate::{
    error::Error,
    provider::{AttemptTrace, NonAnswer, ProviderResult},
};

/// How long one attempt may take unless the builder says otherwise: the
/// default of the OpenAI and Anthropic Python SDKs.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The largest response body read unless the builder says otherwise: the
/// SDK's own default.
pub(crate) const DEFAULT_MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

/// The most characters of text this crate did not write that a message
/// holds, as the SDK cuts server text.
const MAX_MESSAGE_CHARS: usize = 200;

/// The value of `content-type` and `accept` on every request.
const JSON_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("application/json");

/// The value of `user-agent` on every request.
const USER_AGENT_VALUE: HeaderValue =
    HeaderValue::from_static(concat!("typesafe-sdk-rust-adapter/", env!("CARGO_PKG_VERSION")));

// ------------------------------------------------------------ the key header

/// A provider's key as the header it is sent in.
///
/// `Debug` prints the header's name only.
#[derive(Clone)]
pub(crate) struct KeyHeader {
    name: HeaderName,
    /// Marked sensitive, so `http` and hyper print `Sensitive` for it and
    /// HTTP/2 never puts it into the header compression table.
    value: HeaderValue,
    /// The key without its `Bearer ` prefix, in the spellings [`key_forms`]
    /// lists. An error chain is searched for each of them and for nothing
    /// else.
    forms: Arc<[Box<str>]>,
}

/// Builds the header a provider's key is sent in: `name`, and the key after
/// `Bearer ` when `bearer` is set.
///
/// It is the one place of the crate that reads the key out of its
/// [`SecretString`], and a provider calls it from `build()`, so a key that
/// cannot be sent is refused before any request.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when the
/// key is empty or holds a byte that is not legal in a header value, such as
/// a line feed. The text never repeats the key.
pub(crate) fn key_header(
    name: HeaderName,
    bearer: bool,
    key: &SecretString,
) -> Result<KeyHeader, Error> {
    let key = key.expose_secret();
    if key.is_empty() {
        return Err(Error::config("The API key is empty."));
    }
    let text = if bearer { format!("Bearer {key}") } else { key.to_owned() };
    // `from_str` (http 1.5.0, `src/header/value.rs`, `is_valid`) admits the
    // bytes 0x20 to 0x7e (the space and visible ASCII), the tab, and every
    // byte from 0x80 up, so any character outside ASCII as well. It refuses
    // the other control bytes, 0x00 to 0x1f without the tab, and DEL (0x7f):
    // a key with a line feed would otherwise end the header early. A space
    // at the start or the end of the key is admitted and not trimmed here:
    // the header value holds it as it is.
    let Ok(mut value) = HeaderValue::from_str(&text) else {
        return Err(Error::config(
            "The API key holds a character that cannot be sent in an HTTP header.",
        ));
    };
    value.set_sensitive(true);
    Ok(KeyHeader { name, value, forms: key_forms(key).into() })
}

/// The spellings of a key that an error's rendering is searched for, the
/// ones the SDK searches a transport's error for
/// (`crates/sdk/src/redact.rs`, `push_variants`).
///
/// The first six are the key as written, and what `Debug` of a `str`,
/// `str::escape_debug`, `Debug` of a [`HeaderValue`] that is not marked
/// sensitive, `Debug` of a [`Bytes`] and a JSON string as `serde_json` writes
/// one hold for it, each without the quotes around it. The others are each
/// of those six as `Debug` of a `str` writes it once more, without the
/// quotes: a derived `Debug` prints a `String` field that already holds one
/// of the six in that way.
///
/// Equal spellings are kept once: for a key of ASCII letters, digits, `-`,
/// `_` and `.` every one of them is the key as written.
fn key_forms(key: &str) -> Vec<Box<str>> {
    let mut forms = vec![
        key.to_owned(),
        unquoted(&format!("{key:?}"), "\"").to_owned(),
        key.escape_debug().to_string(),
        unquoted(&format!("{:?}", Bytes::copy_from_slice(key.as_bytes())), "b\"").to_owned(),
        // The value of a `Value::String` is written as a JSON string.
        unquoted(&serde_json::Value::from(key).to_string(), "\"").to_owned(),
    ];
    if let Ok(value) = HeaderValue::from_str(key) {
        forms.push(unquoted(&format!("{value:?}"), "\"").to_owned());
    }
    let escaped_again: Vec<String> =
        forms.iter().map(|form| unquoted(&format!("{form:?}"), "\"").to_owned()).collect();
    forms.extend(escaped_again);
    forms.sort_unstable();
    forms.dedup();
    forms.into_iter().map(String::into_boxed_str).collect()
}

/// `text` without the `open` it starts with and the double quote it ends
/// with.
fn unquoted<'a>(text: &'a str, open: &str) -> &'a str {
    text.strip_prefix(open).and_then(|rest| rest.strip_suffix('"')).unwrap_or(text)
}

/// How many links of an error chain are searched for the key. A longer chain
/// is treated as a hit of the search.
const MAX_SCANNED_LINKS: usize = 32;

impl KeyHeader {
    /// Whether `text` holds, byte for byte, one of the spellings of the key
    /// that [`key_forms`] lists. A key written in any other way is not found.
    fn occurs_in(&self, text: &str) -> bool {
        self.occurs_in_bytes(text.as_bytes())
    }

    /// The same for bytes that need not be text: a response body, a header
    /// value.
    fn occurs_in_bytes(&self, text: &[u8]) -> bool {
        self.forms.iter().any(|form| {
            let form = form.as_bytes();
            !form.is_empty() && text.windows(form.len()).any(|window| window == form)
        })
    }

    /// Whether the key search has a hit in `error` or in a link below it.
    ///
    /// Every link is rendered three ways, `Display`, `{:?}` and `{:#?}`: an
    /// error's `Debug` prints its source's `Debug`, and `{:#?}` passes the
    /// alternate flag down, so each of the three can reach a caller. Each
    /// rendering is searched, byte for byte, for these spellings of the key
    /// and for nothing else:
    ///
    /// - the key as written;
    /// - what `Debug` of a `str` writes for it, without the quotes;
    /// - what `str::escape_debug` writes for it;
    /// - what `Debug` of a [`HeaderValue`] that is not marked sensitive
    ///   writes for it, without the quotes;
    /// - what `Debug` of a [`Bytes`] writes for its bytes, without the `b`
    ///   and the quotes;
    /// - what a JSON string holds for it, as `serde_json` writes one,
    ///   without the quotes;
    /// - each of those six as `Debug` of a `str` writes it once more,
    ///   without the quotes: a derived `Debug` prints a `String` field that
    ///   already holds one of them in that way.
    ///
    /// A key transformed in any other way is not found: split over two
    /// links, in hex, in base64, cut to a prefix, reversed, percent-encoded,
    /// as a list of its bytes, or with a JSON `\u` escape for a character
    /// `serde_json` writes as it is. A rendering that holds the key only in
    /// such a shape is not a hit, unless that shape happens to hold one of
    /// the listed spellings as well.
    ///
    /// A chain longer than [`MAX_SCANNED_LINKS`] cannot be searched to its
    /// end and counts as a hit.
    fn holds_key(&self, error: &(dyn StdError + 'static)) -> bool {
        let mut link = Some(error);
        for _ in 0..MAX_SCANNED_LINKS {
            let Some(current) = link else {
                return false;
            };
            let renderings = [current.to_string(), format!("{current:?}"), format!("{current:#?}")];
            if renderings.iter().any(|text| self.occurs_in(text)) {
                return true;
            }
            link = current.source();
        }
        link.is_some()
    }
}

impl fmt::Debug for KeyHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("KeyHeader").field("name", &self.name).finish_non_exhaustive()
    }
}

/// The headers every request of a provider carries: JSON in and out, this
/// crate's `user-agent`, and the key. A provider adds its own fixed headers,
/// such as `anthropic-version`, to the map once.
pub(crate) fn request_headers(key: &KeyHeader) -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(4);
    headers.insert(CONTENT_TYPE, JSON_CONTENT_TYPE);
    headers.insert(ACCEPT, JSON_CONTENT_TYPE);
    headers.insert(USER_AGENT, USER_AGENT_VALUE);
    headers.insert(key.name.clone(), key.value.clone());
    headers
}

// ------------------------------------------------------------- environment

/// The variable `name` of the environment a provider reads, or `None` when
/// it is unset or empty there.
///
/// A provider reads its key and its base URL through this function, once,
/// when it is built. Which environment that is depends on the build, so that
/// no test build can pick up a key of the machine it runs on:
///
/// - with the `internals` feature, the replacement a test set up through
///   `__internals::env::replace`; while no replacement is in force every
///   variable is unset;
/// - in this crate's unit-test build without that feature, none: every
///   variable is unset;
/// - in every other build, the process environment.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when the
/// value is not valid UTF-8. The text names the variable and never repeats
/// the value.
pub(crate) fn env_var(name: &str) -> Result<Option<String>, Error> {
    #[cfg(feature = "internals")]
    let value = crate::__internals::env::lookup(name);
    #[cfg(all(test, not(feature = "internals")))]
    let value: Option<std::ffi::OsString> = None;
    #[cfg(not(any(test, feature = "internals")))]
    let value = std::env::var_os(name);

    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.into_string().map_err(|_| {
        Error::config(format!("The {name} environment variable is not valid UTF-8."))
    })?;
    Ok((!value.is_empty()).then_some(value))
}

// ------------------------------------------------------------ the base URL

/// A provider's base URL, checked by the SDK's rules.
///
/// `Debug` prints the host only: a path prefix can hold a caller's secret.
#[derive(Clone)]
pub(crate) struct BaseUrl {
    scheme: Scheme,
    authority: Authority,
    /// The path without a trailing slash; empty for a URL with no path.
    prefix: Box<str>,
}

impl BaseUrl {
    /// Checks `base_url`: absolute, `http` or `https`, a non-empty host, and
    /// no userinfo, query or fragment.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error for a
    /// URL that breaks one of the rules. No error text repeats the URL or a
    /// part of it: its userinfo is a credential, and a URL that failed to
    /// parse cannot be trusted to have had its userinfo found.
    pub(crate) fn parse(base_url: &str) -> Result<Self, Error> {
        // `http::Uri` drops a fragment without a word when it parses, so the
        // only place left to see one is the text. The first `#` or `?` of a
        // URL always starts the fragment or the query.
        if base_url.contains('#') {
            return Err(Error::config("The base URL must not carry a fragment ('#...')."));
        }
        if base_url.contains('?') {
            return Err(Error::config("The base URL must not carry a query ('?...')."));
        }
        let base: Uri =
            base_url.parse().map_err(|_| Error::config("The base URL is not a valid URL."))?;
        let parts = base.into_parts();
        let (Some(scheme), Some(authority)) = (parts.scheme, parts.authority) else {
            return Err(Error::config(
                "The base URL must be absolute, with a scheme and a host, \
                 such as https://api.example.com/v1.",
            ));
        };
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err(Error::config("The base URL must use http or https."));
        }
        if authority.as_str().contains('@') {
            return Err(Error::config(
                "The base URL must not carry credentials; pass the API key on its own instead.",
            ));
        }
        if authority.host().is_empty() {
            return Err(Error::config("The base URL has an empty host."));
        }
        // `http` gives a URL with no path the path `/`, which would double
        // the slash in front of the operation's path.
        let prefix = parts
            .path_and_query
            .as_ref()
            .map_or("", |path| path.path().trim_end_matches('/'))
            .into();
        Ok(Self { scheme, authority, prefix })
    }

    /// The host, without a port.
    pub(crate) fn host(&self) -> &str {
        self.authority.host()
    }

    /// The endpoint of one vendor operation under this base URL.
    ///
    /// `path` is appended to the base URL to give the URI a request is sent
    /// to. `log_path` is the fixed path of the operation as the vendor
    /// documents it, `/v1/responses` say, and gives the URI that is logged.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// the base URL and `path` do not join into a URI.
    pub(crate) fn endpoint(&self, path: &str, log_path: &'static str) -> Result<Endpoint, Error> {
        let invalid = |_| Error::config("The base URL is not a valid URL.");
        let wire = Uri::builder()
            .scheme(self.scheme.clone())
            .authority(self.authority.clone())
            .path_and_query(format!("{}{path}", self.prefix))
            .build()
            .map_err(invalid)?;
        // Scheme, host and the fixed path; the port only when it is not the
        // scheme's own. Nothing of the caller's path is copied.
        let default_port = if self.scheme == Scheme::HTTPS { 443 } else { 80 };
        let authority = match self.authority.port_u16() {
            Some(port) if port != default_port => format!("{}:{port}", self.authority.host()),
            _ => self.authority.host().to_owned(),
        };
        let log = Uri::builder()
            .scheme(self.scheme.clone())
            .authority(authority)
            .path_and_query(log_path)
            .build()
            .map_err(invalid)?;
        Ok(Endpoint { wire, log })
    }
}

impl fmt::Debug for BaseUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("BaseUrl").field("host", &self.host()).finish_non_exhaustive()
    }
}

/// Where a provider's requests go, and how that place is named in a log.
///
/// `Debug` prints the log URI only.
#[derive(Clone)]
pub(crate) struct Endpoint {
    wire: Uri,
    log: Uri,
}

impl Endpoint {
    /// The URI the retry policy and the events name: scheme, host, the port
    /// when it is not the scheme's default, and the fixed path of the vendor
    /// operation. It never holds userinfo, a query or the path prefix of a
    /// caller's base URL, so a secret placed in a base URL is not logged.
    pub(crate) fn log_uri(&self) -> &Uri {
        &self.log
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Endpoint").field("log", &self.log).finish_non_exhaustive()
    }
}

// ----------------------------------------------------------------- limits

/// The two bounds of one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Limits {
    timeout: Duration,
    max_response_bytes: usize,
}

impl Limits {
    /// The bounds a builder was given, with [`DEFAULT_TIMEOUT`] and
    /// [`DEFAULT_MAX_RESPONSE_BYTES`] for the ones it was not.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error for a
    /// zero timeout or a limit of zero bytes.
    pub(crate) fn new(
        timeout: Option<Duration>,
        max_response_bytes: Option<usize>,
    ) -> Result<Self, Error> {
        let timeout = timeout.unwrap_or(DEFAULT_TIMEOUT);
        if timeout.is_zero() {
            return Err(Error::config("timeout must be greater than zero."));
        }
        let max_response_bytes = max_response_bytes.unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
        if max_response_bytes == 0 {
            return Err(Error::config(
                "max_response_bytes must be at least 1: every response carries a body.",
            ));
        }
        Ok(Self { timeout, max_response_bytes })
    }
}

// ------------------------------------------------------------ one attempt

/// Everything one request needs that does not change between its attempts.
#[derive(Clone, Copy)]
pub(crate) struct Exchange<'a> {
    /// The vendor's name as a non-answer names it, `OpenAI` say.
    pub(crate) vendor: &'static str,
    pub(crate) endpoint: &'a Endpoint,
    /// The provider's headers; see [`request_headers`].
    pub(crate) headers: &'a HeaderMap,
    /// The key those headers carry, searched for in a failed call's error
    /// and in a non-answer's response.
    pub(crate) key: &'a KeyHeader,
    pub(crate) limits: Limits,
}

impl Exchange<'_> {
    /// `outcome`, read by the provider from a success response, unchanged
    /// unless it is a non-answer and the key search has a hit in the
    /// response body recorded in `trace`, in the stop reason recorded there,
    /// or in the non-answer itself, rendered as [`KeyHeader::holds_key`]
    /// renders an error. On a hit the non-answer is [`REASON_NOT_SHOWN`]
    /// after the vendor's name and the recorded stop reason is cleared; the
    /// recorded body stays as received.
    ///
    /// The body is searched as it arrived, because a non-answer cuts its
    /// reason at 200 characters and a key cut there is no longer found; the
    /// stop reason as decoded, because a long one spelled with a `\u`
    /// escape is in neither of the other two; the non-answer, because it
    /// holds a decoded field that the recorded stop reason does not.
    pub(crate) fn screened(
        &self,
        outcome: Result<ProviderResult, NonAnswer>,
        trace: &mut AttemptTrace,
    ) -> Result<ProviderResult, NonAnswer> {
        let Err(reply) = &outcome else {
            return outcome;
        };
        let hit = trace.response().is_some_and(|body| self.key.occurs_in(body))
            || trace.finish_reason().is_some_and(|reason| self.key.occurs_in(reason))
            || self.key.holds_key(&Error::non_answer(reply.clone()));
        if !hit {
            return outcome;
        }
        trace.finish_reason = Some(None);
        Err(non_answer(self.vendor, REASON_NOT_SHOWN))
    }
}

/// Sends `body` as one `POST` and reads the response: one attempt.
///
/// The deadline covers the whole attempt: waiting for the service to be
/// ready, connecting, sending, and reading the response body. A redirect is
/// not followed: a 3xx is a failure status like any other.
///
/// The inner `Ok` is the body of a success response: one JSON value, not yet
/// read as the vendor's shape. The inner `Err` is a success response whose
/// body is not JSON, which no retry of the same request fixes.
///
/// # Errors
///
/// - [`ErrorKind::Timeout`](typesafe_sdk::ErrorKind::Timeout) when the
///   deadline passes first.
/// - [`ErrorKind::Api`](typesafe_sdk::ErrorKind::Api) for any status outside
///   2xx, with the status, the headers and the body. A failure response
///   whose body is over the limit keeps its status, and its headers unless
///   a search below has a hit; the body is not read past the limit and is
///   not kept: without such a hit, `ApiError::body()` returns a replacement
///   this module wrote, a JSON object whose message says that the response
///   body was larger than the limit, and the SDK prints that message after
///   the status. Before the error is built, the
///   bytes of the body, when it was read, and of each header name and
///   value are searched for the key in the spellings listed at
///   [`KeyHeader::holds_key`]; a name is searched as `http` holds it, in
///   lower case. The error built from the response is then searched as
///   that method describes, in its `Display` and both forms of its
///   `Debug`: the SDK decodes the message out of the body, so a JSON
///   `\u` escape of one character of the key, which the first search does
///   not find, ends as the key in the message. On a hit of either search
///   the error keeps the status and neither the body nor the headers: its
///   header map is empty, and `ApiError::body()` returns a replacement this
///   module wrote, not the server's bytes: a JSON object whose message says
///   that the response's body and headers are not shown, because showing
///   them could reveal the API key. Without a hit a body that was read
///   is kept as it arrived: one that spells the key in a way neither
///   search finds, in a member the SDK reads no message from say, is
///   returned by `ApiError::body()` as the server sent it.
/// - [`ErrorKind::Connection`](typesafe_sdk::ErrorKind::Connection) when the
///   service fails or the body cannot be read. The service's own error is
///   the [`source`](StdError::source), unless the search described at
///   [`KeyHeader::holds_key`] has a hit in it or in the message built from
///   it: then the error has a fixed text and no source. That search finds
///   the key in the spellings listed there, not a key transformed in
///   another way.
/// - [`ErrorKind::ResponseTooLarge`](typesafe_sdk::ErrorKind::ResponseTooLarge)
///   when a success response's body is larger than the limit. A body over
///   the limit is not read past it.
pub(crate) async fn post<S>(
    service: &S,
    exchange: Exchange<'_>,
    body: Bytes,
) -> Result<Result<String, NonAnswer>, SdkError>
where
    S: HttpService,
{
    let limit = exchange.limits.max_response_bytes;
    let started = Started::now();
    // The request is built inside a block so that its storage ends before
    // the await: the transport call owns its own copy by then.
    let exchanged = {
        let mut request = Request::new(Body::from(body));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = exchange.endpoint.wire.clone();
        *request.headers_mut() = exchange.headers.clone();
        exchange_once(service, request, limit, exchange.key)
    };
    let deadline = exchange.limits.timeout;
    let outcome = tokio::time::timeout(deadline, exchanged)
        .await
        .unwrap_or_else(|_| Err(Failure::Error(SdkError::timeout(deadline))));

    let (status, result) = match outcome {
        Ok((status, _, body)) if status.is_success() => {
            (Some(status), Ok(json(exchange.vendor, status, body)))
        }
        Ok((status, headers, body)) => {
            (Some(status), Err(api_error(status, headers, Some(body), exchange.key).into()))
        }
        Err(Failure::TooLarge { status, .. }) if status.is_success() => {
            (Some(status), Err(SdkError::response_too_large(limit)))
        }
        Err(Failure::TooLarge { status, headers }) => {
            (Some(status), Err(api_error(status, headers, None, exchange.key).into()))
        }
        Err(Failure::Error(error)) => (None, Err(error)),
    };
    exchanged_event(exchange.endpoint, status, result.as_ref().err(), started);
    result
}

/// The message of an API error whose response the key search had a hit
/// for. The SDK prints it after the status.
///
/// One text for every way the search has a hit: the body, a header name or
/// a header value holds the key as it arrived, or the error built from the
/// response would show it. The last can be a key the response spelled with
/// an escape that the SDK decoded, and it can be a key the response does
/// not hold at all:
/// the SDK escapes a control character of the message, and a tab written
/// as `\t` can spell a key that holds those two characters.
const NOT_SHOWN: &str =
    "The response's body and headers are not shown, because showing them could reveal the API key.";

/// The message of an API error whose response body was over the size
/// limit, when the key search has no hit for the response: larger by the
/// length the response declared, which is refused before a byte is read,
/// or by the bytes that arrived. The SDK prints it after the status; with
/// no body at all it would print `status code (no body)`, which is not
/// what happened.
const BODY_TOO_LARGE: &str = "The response body was larger than the limit and is not shown.";

/// The reason a non-answer gives when the key search has a hit for its
/// response; [`non_answer`] puts the vendor's name before it. One text for
/// every vendor and every way the search has a hit.
const REASON_NOT_SHOWN: &str =
    "the reason is not shown, because showing it could reveal the API key.";

/// The API error a response with a status outside 2xx becomes. `body` is
/// `None` for a body over the size limit, which was not read to its end;
/// unless the search has a hit, the error's message is then
/// [`BODY_TOO_LARGE`].
///
/// A server can send back what it was sent, a gateway that quotes a refused
/// `authorization` header say, and the SDK prints the message it reads out
/// of the body. So the body's bytes and each header's name and value are
/// searched for the key first, in the spellings [`key_forms`] lists; a
/// body that is `None` is not searched, only the headers are. A header
/// name is searched too, in the lower case `http` holds it in: the error
/// prints no name, but [`ApiError::headers`] returns the map. The bytes
/// are searched as they arrived, not the message: the SDK cuts a message
/// at 200 characters, and a key cut there is no longer found.
///
/// Then the error is built and searched as [`KeyHeader::holds_key`]
/// describes. The bytes alone are not enough: the SDK decodes the message
/// out of a JSON body, so a body that writes one character of the key as a
/// `\u` escape holds the key in none of the searched spellings and gives a
/// message that holds it as written.
///
/// On a hit of either search the error is built from the status alone,
/// with [`NOT_SHOWN`] as its message.
fn api_error(
    status: StatusCode,
    headers: HeaderMap,
    body: Option<Bytes>,
    key: &KeyHeader,
) -> ApiError {
    let not_shown = || ApiError::from_response(status, replacement(NOT_SHOWN), HeaderMap::new());
    let received = body.as_deref().is_some_and(|body| key.occurs_in_bytes(body))
        || headers.iter().any(|(name, value)| {
            key.occurs_in(name.as_str()) || key.occurs_in_bytes(value.as_bytes())
        });
    if received {
        return not_shown();
    }
    let body = body.unwrap_or_else(|| replacement(BODY_TOO_LARGE));
    let error = ApiError::from_response(status, body, headers);
    if key.holds_key(&error) {
        return not_shown();
    }
    error
}

/// The body this module puts in the place of a response body it does not
/// show: a JSON object in the shape the SDK reads an error's message from,
/// `{"error":{"message":...}}`.
fn replacement(message: &'static str) -> Bytes {
    Bytes::from(serde_json::json!({ "error": { "message": message } }).to_string())
}

/// The body of a success response as JSON text, or the non-answer for a body
/// that is not one JSON value.
fn json(vendor: &'static str, status: StatusCode, body: Bytes) -> Result<String, NonAnswer> {
    // UTF-8 is checked on its own: skipping a value does not validate the
    // strings inside it.
    match String::from_utf8(Vec::from(body)) {
        Ok(json) if serde_json::from_str::<IgnoredAny>(&json).is_ok() => Ok(json),
        _ => Err(non_answer(
            vendor,
            &format!("status {} with a body that is not JSON", status.as_u16()),
        )),
    }
}

/// What a success status, the headers and the whole body are returned as.
type Received = (StatusCode, HeaderMap, Bytes);

/// How an attempt can end short of a response body.
enum Failure {
    /// Anything that is an error without needing the response.
    Error(SdkError),
    /// The body was larger than the limit. Whether that is a response too
    /// large or an API error depends on the status, which is kept.
    TooLarge { status: StatusCode, headers: HeaderMap },
}

/// Waits for the service, sends `request`, and reads the whole response.
async fn exchange_once<S>(
    service: &S,
    request: Request<Body>,
    limit: usize,
    key: &KeyHeader,
) -> Result<Received, Failure>
where
    S: HttpService,
{
    // A clone per call, as `tower` intends: readiness belongs to the handle
    // that is then called. The handle is dropped once the call has been
    // made: the response future owns what it needs.
    let called = {
        let mut service = service.clone();
        poll_fn(|cx| service.poll_ready(cx))
            .await
            .map_err(|error| Failure::Error(connection(error, key)))?;
        service.call(request)
    };
    let response = called.await.map_err(|error| Failure::Error(connection(error, key)))?;
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
        Err(error) => Err(Failure::Error(connection(error, key))),
    }
}

// ----------------------------------------------------- connection failures

/// What every connection error's message starts with, as in the SDK.
const CONNECTION_PREFIX: &str = "Connection error: ";

/// The message of a connection error whose cause the key search withheld.
/// One text for every way the search has a hit: a link of the chain holds
/// the key, the message built from the chain would spell it, or the chain
/// is longer than [`MAX_SCANNED_LINKS`] and may hold no key at all.
const WITHHELD: &str = "Connection error: the transport's error is not shown, because showing it could reveal the API key or its chain of causes was too long to search.";

/// How many links of an error chain a connection error's message names.
const MAX_MESSAGE_LINKS: usize = 8;

/// The error a failure of the service becomes.
///
/// A service's error may hold the request's headers, and so the key: the
/// default [`Transport`] formats no header value, but a caller's own service
/// can. So the chain is searched first, as [`KeyHeader::holds_key`]
/// describes: for the key in the spellings listed there, not for a key
/// transformed in another way. On a hit the whole chain is dropped for a
/// fixed text. Otherwise an SDK error the service raised itself, a timeout
/// say, is passed through as it is, and anything else is a connection error
/// whose message is the chain's own messages and whose source is the chain;
/// that message is searched in the same way before it is used.
fn connection(error: impl Into<BoxError>, key: &KeyHeader) -> SdkError {
    let error = error.into();
    if key.holds_key(&*error) {
        return SdkError::connection(WITHHELD, None);
    }
    match error.downcast::<SdkError>() {
        Ok(ours) => *ours,
        Err(other) => {
            let message = connection_message(&*other);
            // Escaping can spell a key no link holds: a tab written as `\t`.
            if key.occurs_in(&message) {
                return SdkError::connection(WITHHELD, None);
            }
            SdkError::connection(message, Some(other))
        }
    }
}

/// [`CONNECTION_PREFIX`] and then the messages of the chain, at most
/// [`MAX_MESSAGE_LINKS`] of them, joined with `: `.
///
/// The chain can hold text the server chose, an HTTP/2 GOAWAY's debug data
/// or a certificate's subject, so every link is escaped and the whole of it
/// after the prefix is cut at [`MAX_MESSAGE_CHARS`] characters; the full
/// chain stays reachable through [`source`](StdError::source). A backslash
/// is written as it is: h2 and rustls print their own text through `Debug`
/// already.
fn connection_message(error: &(dyn StdError + 'static)) -> String {
    let mut message = Capped::after(String::from(CONNECTION_PREFIX), MAX_MESSAGE_CHARS);
    let mut link = Some(error);
    for index in 0..MAX_MESSAGE_LINKS {
        let Some(current) = link else { break };
        if index > 0 {
            message.fixed(": ");
        }
        message.untrusted(&current.to_string(), Backslash::Keep);
        link = current.source();
    }
    message.text
}

// ------------------------------------------------------------ non-answers

/// The non-answer of `vendor` described by `reason`: a status, a stop reason
/// or the word `refusal`.
///
/// `reason` may be text the vendor chose, a stop reason say, so it is escaped
/// and cut at 200 characters as the SDK cuts server text. It must never be
/// the refusal's own text or an error message from the body: those stay in
/// the attempt's recorded response.
pub(crate) fn non_answer(vendor: &'static str, reason: &str) -> NonAnswer {
    let mut message = Capped::after(format!("{vendor} did not answer: "), MAX_MESSAGE_CHARS);
    message.untrusted(reason, Backslash::Double);
    NonAnswer::new(message.text)
}

// ------------------------------------------------------- text not ours

/// What a backslash in escaped text becomes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backslash {
    /// Written as `\\`, so text spelling an escape cannot be mistaken for
    /// text holding the character.
    Double,
    /// Written as it is, for text another layer has already escaped once.
    Keep,
}

/// A rendering capped at a number of characters, built from this crate's
/// text and text it did not write. The rules are the SDK's
/// (`crates/sdk/src/text.rs`), which is private to it: a control character,
/// or a format character that reorders or hides the text around it, is
/// written as a Rust escape; the count is taken after escaping; a cut never
/// splits a character or an escape, and is marked with U+2026.
struct Capped {
    text: String,
    /// Characters written after the prefix.
    chars: usize,
    limit: usize,
    /// Set once the limit is reached; nothing is written after it.
    full: bool,
}

impl Capped {
    /// A rendering that starts with `prefix`, which does not count against
    /// `limit`.
    fn after(prefix: String, limit: usize) -> Self {
        Self { text: prefix, chars: 0, limit, full: false }
    }

    /// Text of this crate's own, written as it is but counted, and whole or
    /// not at all.
    fn fixed(&mut self, text: &str) {
        self.put(&text, text.chars().count());
    }

    /// Text this crate did not write, escaped.
    fn untrusted(&mut self, text: &str, backslash: Backslash) {
        for character in text.chars() {
            let written = match character {
                '\\' if backslash == Backslash::Keep => self.put(&character, 1),
                '\\' | '\n' | '\r' | '\t' => {
                    let short = character.escape_default();
                    self.put(&short, short.len())
                }
                _ if character.is_control() || hides_text(character) => {
                    let code = character.escape_unicode();
                    self.put(&code, code.len())
                }
                _ => self.put(&character, 1),
            };
            if !written {
                return;
            }
        }
    }

    /// Appends `piece`, which renders as `len` characters, whole; or, when
    /// it would cross the limit, U+2026 instead, after which nothing more is
    /// written. Returns whether `piece` was written.
    fn put(&mut self, piece: &dyn fmt::Display, len: usize) -> bool {
        if self.full {
            return false;
        }
        if self.chars + len > self.limit {
            self.text.push('\u{2026}');
            self.full = true;
            return false;
        }
        write!(self.text, "{piece}").expect("invariant: writing to a String cannot fail");
        self.chars += len;
        true
    }
}

/// Whether `character` is a Unicode format character that reorders, joins or
/// hides the text around it: the bidirectional controls, the zero-width
/// characters, the byte-order mark, the line and paragraph separators, the
/// interlinear annotation marks and the invisible tag characters.
/// `char::is_control` covers none of them.
fn hides_text(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

// ------------------------------------------------------------------ events

/// The target of this crate's events.
#[cfg(feature = "tracing")]
const TARGET: &str = "system_one_adapter";

/// When an exchange began. The clock is read only with the `tracing`
/// feature, where an event prints the elapsed time; without it this holds
/// nothing.
struct Started {
    #[cfg(feature = "tracing")]
    at: std::time::Instant,
}

impl Started {
    fn now() -> Self {
        Self {
            #[cfg(feature = "tracing")]
            at: std::time::Instant::now(),
        }
    }
}

/// One `DEBUG` event per exchange: the method, the log URI, the status when
/// a response arrived, the fixed name of the error's kind when it failed,
/// and the elapsed milliseconds. Never a header, a body or an error's text.
#[cfg(feature = "tracing")]
fn exchanged_event(
    endpoint: &Endpoint,
    status: Option<StatusCode>,
    failure: Option<&SdkError>,
    started: Started,
) {
    tracing::debug!(
        target: TARGET,
        method = %Method::POST,
        uri = %endpoint.log,
        status = status.map(|status| status.as_u16()),
        error = failure.map(crate::error::provider_kind_name),
        elapsed_ms = u64::try_from(started.at.elapsed().as_millis()).unwrap_or(u64::MAX),
    );
}

/// One event per exchange: nothing, without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
fn exchanged_event(_: &Endpoint, _: Option<StatusCode>, _: Option<&SdkError>, _: Started) {}

// ------------------------------------------------------ the default transport

/// How long an idle pooled connection is kept before it is closed.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How often an HTTP/2 connection is pinged to keep it open.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// The transport a built-in provider sends its requests through unless it is
/// given another service: a pooled HTTP client over TLS.
///
/// It speaks HTTP/2 or HTTP/1.1 as the server chooses through ALPN on
/// `https`, and HTTP/1.1 on `http`. It trusts the operating system's
/// certificate roots, and any roots the provider's builder added on top of
/// them. It follows no redirect and formats no header value.
///
/// Cloning it shares the connection pool. A provider's builder creates it;
/// the type is named here so that a provider's type can be written down.
#[derive(Clone)]
pub struct Transport {
    client: legacy::Client<HttpsConnector<HttpConnector>, Body>,
    /// How many roots were added to the operating system's.
    extra_roots: usize,
}

impl Transport {
    /// Builds the transport and its TLS configuration, trusting the
    /// DER-encoded certificates `extra_roots` in addition to the operating
    /// system's roots.
    ///
    /// Needs no runtime: nothing connects until the first request, which
    /// must then run on a Tokio runtime with its time driver enabled.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// the certificate verifier cannot be built: an added root is not a
    /// certificate, or the operating system's roots cannot be loaded.
    pub(crate) fn new(extra_roots: Vec<Vec<u8>>) -> Result<Self, Error> {
        let root_count = extra_roots.len();
        let tls = tls_config(extra_roots.into_iter().map(CertificateDer::from).collect())?;

        let mut http = HttpConnector::new();
        // The TLS layer above decides between `http` and `https`; the TCP
        // layer has to accept both.
        http.enforce_http(false);
        http.set_nodelay(true);

        // hyper-rustls fills ALPN from the versions enabled here.
        let connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http);

        let mut builder = legacy::Client::builder(TokioExecutor::new());
        // hyper panics on a time-based option that has no timer to run on.
        builder
            .timer(TokioTimer::new())
            .pool_timer(TokioTimer::new())
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
            .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
            .http2_keep_alive_while_idle(true);
        Ok(Self { client: builder.build(connector), extra_roots: root_count })
    }
}

impl fmt::Debug for Transport {
    /// The added roots as a count.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Transport").field("extra_roots", &self.extra_roots).finish()
    }
}

impl Service<Request<Body>> for Transport {
    type Response = Response<TransportBody>;
    type Error = BoxError;
    type Future = TransportFuture;

    /// Always ready: the pool takes any number of requests.
    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> TransportFuture {
        TransportFuture(self.client.request(request))
    }
}

/// The response of one request sent by [`Transport`].
#[must_use = "futures do nothing unless polled"]
pub struct TransportFuture(legacy::ResponseFuture);

impl fmt::Debug for TransportFuture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TransportFuture").finish_non_exhaustive()
    }
}

impl Future for TransportFuture {
    type Output = Result<Response<TransportBody>, BoxError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // The inner future is `Unpin`, so the pinned reference can be turned
        // back into a plain one and the future pinned in place again.
        Pin::new(&mut self.get_mut().0)
            .poll(cx)
            .map(|sent| sent.map(|response| response.map(TransportBody)).map_err(Into::into))
    }
}

/// The body of a response [`Transport`] received, read frame by frame as it
/// arrives.
///
/// It is hyper's own body under a name of this crate's, so that a new major
/// version of hyper is not a breaking change here. The length the server
/// declared is what [`size_hint`](http_body::Body::size_hint) reports, which
/// lets a response over the size limit be refused before a byte is read.
///
/// `Debug` prints no part of the body.
pub struct TransportBody(Incoming);

impl fmt::Debug for TransportBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("TransportBody").finish_non_exhaustive()
    }
}

impl http_body::Body for TransportBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        // hyper's body is `Unpin`; its error is boxed only when one occurs.
        Pin::new(&mut self.get_mut().0).poll_frame(cx).map_err(Into::into)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

/// The rustls configuration: TLS 1.2 and 1.3 with aws-lc-rs, the operating
/// system's roots, and `extra_roots` on top of them.
///
/// The crypto provider is named rather than taken from the process default,
/// so a second provider elsewhere in the program cannot change it. ALPN is
/// left empty: hyper-rustls sets it from the versions the connector enables.
///
/// `dangerous()` is rustls' name for installing any verifier of one's own.
/// The one installed here is the platform verifier with the added roots: it
/// verifies every certificate, against the operating system's roots and the
/// added ones, and replaces neither.
fn tls_config(extra_roots: Vec<CertificateDer<'static>>) -> Result<ClientConfig, Error> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(verifier_error)?;
    let config = if extra_roots.is_empty() {
        builder.with_platform_verifier().map_err(verifier_error)?.with_no_client_auth()
    } else {
        let verifier =
            Verifier::new_with_extra_roots(extra_roots, provider).map_err(verifier_error)?;
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth()
    };
    Ok(config)
}

/// The error for a TLS configuration that cannot be built.
///
/// The verifier's text can quote a certificate the caller added or the
/// platform's own diagnostics, so it is escaped and cut like any text this
/// crate did not write.
fn verifier_error(error: rustls::Error) -> Error {
    let mut message = Capped::after(
        String::from("The TLS certificate verifier could not be built: "),
        MAX_MESSAGE_CHARS,
    );
    message.untrusted(&error.to_string(), Backslash::Keep);
    message.text.push('.');
    Error::config(message.text)
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
