//! Where a client's settings come from, and what makes them usable.
//!
//! Resolution is a pure function over a lookup closure rather than a reader of
//! the process environment, for two reasons: `std::env::set_var` is `unsafe` in
//! edition 2024, so a test that wanted to set a variable could not do it
//! without breaking the crate's `#![forbid(unsafe_code)]`; and the process
//! environment is global mutable state that two tests running in parallel
//! would fight over. The closure makes each case its own input.
//!
//! An explicit setting wins over the environment, an environment value is
//! trimmed, a blank one counts as unset, a trailing slash comes off the base
//! URL, and a deadline must be above zero when there is one.

use std::{ffi::OsString, fmt, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, Uri, uri::PathAndQuery};
use secrecy::{ExposeSecret, SecretString};

use crate::{
    constants::{
        API_KEY_ENV, BASE_URL_ENV, DEFAULT_BASE_URL, DEFAULT_MAX_RESPONSE_BYTES, DEFAULT_MODEL,
        DEFAULT_MODEL_ENV, DEFAULT_TIMEOUT, MODELS_PATH, SDK_IDENTIFIER, SYSTEM_ONE_PATH,
    },
    error::{Error, format_endpoint},
};

/// The settings a caller passed explicitly. Anything left unset falls back to
/// its environment variable, then to the SDK's default.
#[derive(Default)]
pub(crate) struct Explicit {
    pub(crate) api_key: Option<SecretString>,
    pub(crate) base_url: Option<String>,
    pub(crate) default_model: Option<String>,
    /// `None` leaves the default; `Some(None)` asks for no deadline at all.
    pub(crate) timeout: Option<Option<Duration>>,
    pub(crate) default_headers: HeaderMap,
    pub(crate) max_response_bytes: Option<usize>,
    /// The caller's product, put in front of the SDK's own in `User-Agent`.
    pub(crate) user_agent_product: Option<String>,
    /// Whether `X-TypeSafe-Runtime` is left out; `false`, the default, sends it.
    pub(crate) omit_runtime_header: bool,
    /// Whether events omit the scheme, host and base URL's path prefix.
    pub(crate) omit_endpoint_host: bool,
}

/// A client's settings once every source has been consulted and every value
/// checked.
///
/// The API key survives only as the `Authorization` value built from it,
/// flagged sensitive, and `Debug` prints neither that value nor the values of
/// the default headers.
pub(crate) struct Config {
    authorization: HeaderValue,
    endpoints: Endpoints,
    default_model: Box<str>,
    timeout: Option<Duration>,
    default_headers: HeaderMap,
    max_response_bytes: usize,
    /// The whole `User-Agent` value, built once.
    user_agent: HeaderValue,
    send_runtime_header: bool,
    omit_endpoint_host: bool,
}

impl Config {
    /// Resolves the settings from what the caller passed and what `env` finds.
    ///
    /// `env` looks a variable up by name; the client passes
    /// `std::env::var_os`. It is consulted only for settings the caller left
    /// unset. A value it returns is trimmed, and a blank one counts as unset;
    /// one that is not UTF-8 is refused, by the name of the variable. An
    /// explicit value always wins. The API key, from either source, is trimmed
    /// as Python's `str.strip()` trims, as the Python SDK trims it; an explicit
    /// key that is blank is the missing-key error, and the environment is not
    /// consulted for it. Every other explicit value is taken as given, without
    /// trimming, as the Python SDK takes it - except that an explicit default
    /// model that is blank is refused rather than sent, where the Python SDK
    /// would send it.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when no
    /// API key is found or the given one is blank, when the trimmed key holds
    /// anything but printable ASCII without whitespace (see
    /// [`validate_api_key`]), when an explicit default model is blank, when
    /// the base URL is not
    /// an absolute `http` or `https` URL without credentials, query or
    /// fragment, when the timeout or the response size limit is zero, when the
    /// `User-Agent` product is not a product token (see [`user_agent`]), or
    /// when a variable `env` reads is not UTF-8.
    pub(crate) fn resolve<V>(
        explicit: Explicit,
        env: impl Fn(&str) -> Option<V>,
    ) -> Result<Self, Error>
    where
        V: Into<OsString>,
    {
        let Explicit {
            api_key,
            base_url,
            default_model,
            timeout,
            default_headers,
            max_response_bytes,
            user_agent_product,
            omit_runtime_header,
            omit_endpoint_host,
        } = explicit;

        // An explicit key, even a blank one, is the key: the environment is
        // not consulted for it. An unset or blank variable becomes the empty
        // key, which `validate_api_key` refuses as missing. `String::new()`
        // does not allocate, and the secret wrapper owns the bytes until
        // `bearer` has copied them.
        let api_key = match api_key {
            Some(key) => key,
            None => SecretString::from(from_env(&env, API_KEY_ENV)?.unwrap_or_default()),
        };
        let authorization = bearer(validate_api_key(api_key.expose_secret())?);

        let base_url = match base_url {
            Some(url) => Some(url),
            None => from_env(&env, BASE_URL_ENV)?,
        };
        let mut base_url = base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_owned());
        base_url.truncate(base_url.trim_end_matches('/').len());
        let mut endpoints = endpoints(&base_url)?;
        if omit_endpoint_host {
            endpoints.system_one_log = Uri::from_static(SYSTEM_ONE_PATH);
            endpoints.models_log = Uri::from_static(MODELS_PATH);
        }

        let default_model = match default_model {
            Some(model) if is_blank(&model) => {
                return Err(Error::config(format!(
                    "The default model is empty. \
                     Pass a non-empty default_model or set the {DEFAULT_MODEL_ENV} \
                     environment variable."
                )));
            }
            Some(model) => model,
            None => from_env(&env, DEFAULT_MODEL_ENV)?.unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
        };

        let timeout = timeout.unwrap_or(Some(DEFAULT_TIMEOUT));
        // A `Duration` cannot be negative, NaN or infinite, so zero is the one
        // unusable deadline left to refuse. No deadline at all is asked for
        // by name, never by a sentinel value.
        if timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(Error::config(ZERO_TIMEOUT));
        }

        let max_response_bytes = max_response_bytes.unwrap_or(DEFAULT_MAX_RESPONSE_BYTES);
        if max_response_bytes == 0 {
            return Err(Error::config(
                "max_response_bytes must be at least 1: every response carries a body.",
            ));
        }

        let user_agent = user_agent(user_agent_product.as_deref())?;

        Ok(Self {
            authorization,
            endpoints,
            default_model: default_model.into_boxed_str(),
            timeout,
            default_headers,
            max_response_bytes,
            user_agent,
            send_runtime_header: !omit_runtime_header,
            omit_endpoint_host,
        })
    }

    /// The `Authorization: Bearer <key>` value, flagged sensitive so that
    /// `http` and `hyper` keep it out of their own `Debug` output and out of
    /// HTTP/2 header compression tables.
    pub(crate) fn authorization(&self) -> &HeaderValue {
        &self.authorization
    }

    /// The two endpoint URLs, parsed once.
    pub(crate) fn endpoints(&self) -> &Endpoints {
        &self.endpoints
    }

    /// The model a request names when the call names none.
    pub(crate) fn default_model(&self) -> &str {
        &self.default_model
    }

    /// The deadline of each attempt, or `None` for no deadline.
    pub(crate) fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// The largest response body a request reads, in bytes.
    pub(crate) fn max_response_bytes(&self) -> usize {
        self.max_response_bytes
    }

    /// The caller's default headers, as given.
    pub(crate) fn default_headers(&self) -> &HeaderMap {
        &self.default_headers
    }

    /// The `User-Agent` value: the SDK's identifier, after the caller's
    /// product when there is one.
    pub(crate) fn user_agent(&self) -> &HeaderValue {
        &self.user_agent
    }

    /// Whether requests carry `X-TypeSafe-Runtime`.
    pub(crate) fn send_runtime_header(&self) -> bool {
        self.send_runtime_header
    }

    /// Whether an event prints only the fixed API path for its endpoint.
    pub(crate) fn omit_endpoint_host(&self) -> bool {
        self.omit_endpoint_host
    }
}

impl fmt::Debug for Config {
    /// Prints the settings a reader needs to tell two clients apart, and no
    /// value that could be a credential: the `Authorization` value is replaced
    /// by a marker and the default headers are reduced to their names, since a
    /// caller may pass a token there under any name at all. The endpoints are
    /// printed as an error names them, which is the base URL without a default
    /// port; a base URL can carry no userinfo, query or fragment, but it keeps
    /// its path, so a credential put into that path would show here. The
    /// `User-Agent` value and the runtime header switch are printed only when
    /// they differ from the default; the value is then a checked product
    /// token followed by the SDK's identifier, so it holds nothing to escape.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = formatter.debug_struct("Config");
        shown
            .field("endpoints", &self.endpoints)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("authorization", &Hidden)
            .field("default_headers", &HeaderNames(&self.default_headers));
        if self.user_agent != SDK_IDENTIFIER {
            shown.field("user_agent", &self.user_agent);
        }
        if !self.send_runtime_header {
            shown.field("send_runtime_header", &false);
        }
        if self.omit_endpoint_host {
            shown.field("log_endpoint_host", &false);
        }
        shown.finish()
    }
}

/// Stands in for a credential in a `Debug`.
struct Hidden;

impl fmt::Debug for Hidden {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

/// Stands in for a header map in a `Debug`: the names, once each, in the order
/// they were first inserted.
struct HeaderNames<'a>(&'a HeaderMap);

impl fmt::Debug for HeaderNames<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.0.keys()).finish()
    }
}

/// The URLs of the two API endpoints under one base URL.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Endpoints {
    system_one: Uri,
    models: Uri,
    system_one_log: Uri,
    models_log: Uri,
}

impl fmt::Debug for Endpoints {
    /// `["POST <system one URL>", "GET <models URL>"]`, each as an error names
    /// the endpoint it failed at.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entry(&format_endpoint(&Method::POST, &self.system_one))
            .entry(&format_endpoint(&Method::GET, &self.models))
            .finish()
    }
}

impl Endpoints {
    /// The System One endpoint, `<base>/v1/systemone`.
    pub(crate) fn system_one(&self) -> &Uri {
        &self.system_one
    }

    /// The model listing endpoint, `<base>/v1/models`.
    pub(crate) fn models(&self) -> &Uri {
        &self.models
    }

    /// The System One endpoint as the retry event names it.
    pub(crate) fn system_one_log(&self) -> &Uri {
        &self.system_one_log
    }

    /// The models endpoint as the retry event names it.
    pub(crate) fn models_log(&self) -> &Uri {
        &self.models_log
    }
}

/// Joins the API paths onto `base_url`, which must already have lost its
/// trailing slashes.
///
/// The base URL may carry a path prefix, which the API paths are appended to.
/// None of the error messages repeats the URL: its userinfo, when it has any,
/// is a credential, and a URL that failed to parse cannot be trusted to have
/// had its userinfo found and cut out.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
/// `base_url` does not parse, is not absolute, is not `http` or `https`, has an
/// empty host, or carries userinfo, a query or a fragment.
pub(crate) fn endpoints(base_url: &str) -> Result<Endpoints, Error> {
    // `http::Uri` drops a fragment without a word when it parses, so the only
    // place left to see one is the text. The first `#` or `?` of a URL always
    // starts the fragment or the query, so a plain search is exact.
    if base_url.contains('#') {
        return Err(Error::config("The base URL must not carry a fragment ('#...')."));
    }
    if base_url.contains('?') {
        return Err(Error::config("The base URL must not carry a query ('?...')."));
    }
    let base: Uri =
        base_url.parse().map_err(|_| Error::config("The base URL is not a valid URL."))?;
    let (Some(scheme), Some(authority)) = (base.scheme_str(), base.authority()) else {
        return Err(Error::config(
            "The base URL must be absolute, with a scheme and a host, \
             such as https://api.typesafe.ai.",
        ));
    };
    if !matches!(scheme, "http" | "https") {
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

    // `http` gives a URL with no path the path `/`, which would double the
    // slash in front of the API path; a real prefix has no trailing slash left.
    let prefix = base.path().trim_end_matches('/');
    let join = |path: &str| {
        let mut parts = base.clone().into_parts();
        parts.path_and_query = Some(
            PathAndQuery::from_maybe_shared(Bytes::from(format!("{prefix}{path}")))
                .expect("invariant: a parsed path followed by a fixed API path is a valid path"),
        );
        Uri::from_parts(parts).expect(
            "invariant: the scheme and authority were checked present, and the path is valid",
        )
    };
    let system_one = join(SYSTEM_ONE_PATH);
    let models = join(MODELS_PATH);
    Ok(Endpoints {
        system_one_log: system_one.clone(),
        models_log: models.clone(),
        system_one,
        models,
    })
}

/// Builds the `Authorization` value for `key`, flagged sensitive.
///
/// `key` is what [`validate_api_key`] returned: printable ASCII without
/// whitespace, which `http` always accepts. The key is copied once, into a
/// buffer that becomes the header value's own storage; that value is not
/// zeroed and lives as long as the client.
fn bearer(key: &str) -> HeaderValue {
    const SCHEME: &[u8] = b"Bearer ";
    let key = key.as_bytes();
    debug_assert!(key.iter().all(|byte| (b'!'..=b'~').contains(byte)), "unvalidated API key");
    let mut value = Vec::with_capacity(SCHEME.len() + key.len());
    value.extend_from_slice(SCHEME);
    value.extend_from_slice(key);
    let mut value = HeaderValue::from_maybe_shared(Bytes::from(value))
        .expect("invariant: validate_api_key admits only printable ASCII without whitespace");
    value.set_sensitive(true);
    value
}

/// The most bytes a caller's `User-Agent` product may hold.
pub(crate) const MAX_USER_AGENT_PRODUCT_BYTES: usize = 64;

/// The `User-Agent` value: [`SDK_IDENTIFIER`] alone, or `product` in front of
/// it, the more significant product first (RFC 9110, section 10.1.5).
///
/// Without a product this is the constant itself, which costs no allocation;
/// with one the value is built once here and shared by every request.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
/// `product` is not `token "/" token` (RFC 9110, section 5.6.2) of at most
/// [`MAX_USER_AGENT_PRODUCT_BYTES`] bytes. The message names the rule and
/// never repeats the value, which can hold anything at all.
pub(crate) fn user_agent(product: Option<&str>) -> Result<HeaderValue, Error> {
    let Some(product) = product else {
        return Ok(SDK_IDENTIFIER);
    };
    if let Err(rule) = check_product(product) {
        return Err(Error::config(format!(
            "The user_agent_product must be a product token, name/version \
             (RFC 9110, section 10.1.5): {rule}."
        )));
    }
    let sdk = SDK_IDENTIFIER;
    let mut value = Vec::with_capacity(product.len() + 1 + sdk.len());
    value.extend_from_slice(product.as_bytes());
    value.push(b' ');
    value.extend_from_slice(sdk.as_bytes());
    Ok(HeaderValue::from_maybe_shared(Bytes::from(value))
        .expect("invariant: a checked token, a space and the SDK identifier are visible ASCII"))
}

/// Which rule `product` breaks, if any, as the end of a sentence.
///
/// The rules are checked from the most general to the most specific, so the
/// first one named is the one a reader fixes first: a pasted line with a
/// newline in it is reported as whitespace, not as a missing version.
fn check_product(product: &str) -> Result<(), &'static str> {
    let bytes = product.as_bytes();
    if bytes.is_empty() {
        return Err("it is empty");
    }
    if bytes.len() > MAX_USER_AGENT_PRODUCT_BYTES {
        return Err("it is longer than 64 bytes");
    }
    if !product.is_ascii() {
        return Err("it contains a character that is not ASCII");
    }
    if bytes.iter().any(u8::is_ascii_whitespace) {
        return Err("it contains whitespace");
    }
    if bytes.iter().any(u8::is_ascii_control) {
        return Err("it contains a control character");
    }
    let Some((name, version)) = product.split_once('/') else {
        return Err("it has no '/' between the name and the version");
    };
    if version.contains('/') {
        return Err("it has more than one '/'");
    }
    if name.is_empty() {
        return Err("the name before the '/' is empty");
    }
    if version.is_empty() {
        return Err("the version after the '/' is empty");
    }
    if !name.bytes().chain(version.bytes()).all(is_tchar) {
        return Err("it contains a character a token cannot hold (RFC 9110, section 5.6.2)");
    }
    Ok(())
}

/// Whether `byte` is a `tchar` of RFC 9110, section 5.6.2: a letter, a digit
/// or one of ``!#$%&'*+-.^_`|~``.
///
/// Written out here because no crate the SDK depends on exposes this set:
/// `http` checks header names against a table of its own, which lowercases
/// what it accepts and is not public, and no maintained crate on crates.io
/// offers the predicate alone.
fn is_tchar(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte)
}

/// The message a zero deadline is refused with, the Python SDK's wording.
pub(crate) const ZERO_TIMEOUT: &str = "timeout must be a positive, finite number of seconds.";

/// Whether `c` is whitespace to Python's `str.strip()`, which is what the
/// Python SDK trims with: Unicode whitespace plus the four ASCII separators
/// U+001C to U+001F, which Python counts as whitespace and Rust's
/// `char::is_whitespace` does not.
pub(crate) fn is_python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Whether `text` is empty once Python's `str.strip()` has trimmed it.
fn is_blank(text: &str) -> bool {
    text.chars().all(is_python_space)
}

/// `key` trimmed as Python's `str.strip()` trims it, once it is known to be
/// usable: not empty, and every character printable ASCII other than the
/// space (`'!'..='~'`), which is what the Python SDK accepts since 0.7.1.
///
/// The result borrows from `key`; nothing is copied.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error with the
/// Python SDK's missing-key message when the trimmed key is empty, and with
/// its invalid-key message when a character is outside that range. Neither
/// repeats the key.
fn validate_api_key(key: &str) -> Result<&str, Error> {
    let key = key.trim_matches(is_python_space);
    if key.is_empty() {
        return Err(Error::config(format!(
            "No API key was provided. \
             Pass api_key or set the {API_KEY_ENV} environment variable."
        )));
    }
    if !key.bytes().all(|byte| (b'!'..=b'~').contains(&byte)) {
        return Err(Error::config(
            "API key must contain only printable ASCII characters without whitespace.",
        ));
    }
    Ok(key)
}

/// The variable `name` as `env` finds it, trimmed as [`is_python_space`]
/// says, or `None` when it is unset or blank.
///
/// # Errors
///
/// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error naming the
/// variable when its value is not UTF-8. The value itself is not repeated: it
/// may be the API key.
fn from_env<V>(env: &impl Fn(&str) -> Option<V>, name: &str) -> Result<Option<String>, Error>
where
    V: Into<OsString>,
{
    let Some(value) = env(name) else {
        return Ok(None);
    };
    let mut value = value.into().into_string().map_err(|_| {
        Error::config(format!("The {name} environment variable is not valid UTF-8."))
    })?;
    let end = value.trim_end_matches(is_python_space).len();
    value.truncate(end);
    let start = value.len() - value.trim_start_matches(is_python_space).len();
    value.drain(..start);
    Ok(if value.is_empty() { None } else { Some(value) })
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
