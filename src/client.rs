//! The client: what a caller holds, clones and shares.
//!
//! A client owns its transport and is cheap to clone, so passing one to every
//! task is the intended use rather than something to work around with a shared
//! reference. The API key enters as a secret and is kept only as the finished
//! `Authorization` header value, marked sensitive, so no formatting of the
//! client or its configuration can print it.
//!
//! The key is copied once into the `Authorization` header value, which is not zeroed
//! and lives as long as the client and every request built from it; `HeaderValue`
//! is `Bytes`-backed and shared by reference count, so it cannot be zeroed on drop.

#[cfg(feature = "hyper")]
use std::ffi::OsString;
use std::{fmt, sync::Arc, time::Duration};

use bytes::Bytes;
use http::HeaderMap;
#[cfg(feature = "hyper")]
use http::uri::Scheme;
use secrecy::SecretString;
use serde::Serialize;

#[cfg(feature = "hyper")]
use crate::transport::{HttpVersion, HyperTransport, TransportSettings};
use crate::{
    codec,
    config::{Config, Explicit},
    error::Error,
    models::Models,
    question::PreparedQuestions,
    request::SystemOne,
    retry::RetryPolicy,
    transport::{self, HttpService},
};

/// A client of the TypeSafe API.
///
/// Build one with [`ClientBuilder::new`] and [`ClientBuilder::build_with_service`],
/// or use `Client::builder()` / `Client::from_env()` with the `hyper` feature.
/// Cloning a client is cheap - the settings and
/// the transport are shared behind one reference count - so a clone per task
/// is the way to use one from many tasks, and all of them share one
/// connection pool.
///
/// `S` is the transport. With the `hyper` feature its default is
/// `HyperTransport`, an HTTP/2 client over TLS; any `tower` service over
/// `http` requests is accepted through [`ClientBuilder::build_with_service`].
///
/// Every request runs on the caller's Tokio runtime, which needs its time
/// driver enabled: each attempt has a deadline, and HTTP/2 keep-alive pings
/// run on a timer.
pub struct Client<#[cfg(feature = "hyper")] S = HyperTransport, #[cfg(not(feature = "hyper"))] S> {
    shared: Arc<Shared<S>>,
}

/// What every clone of one client shares.
pub(crate) struct Shared<S> {
    pub(crate) service: S,
    pub(crate) config: Config,
    /// The headers of a request without a body, built once.
    pub(crate) get_headers: HeaderMap,
    /// The headers of a request with a JSON body, built once.
    pub(crate) post_headers: HeaderMap,
    /// The default model as a JSON string, escaped once.
    pub(crate) model_json: Bytes,
    /// The retry policy of every call that does not bring its own.
    pub(crate) retry: RetryPolicy,
}

impl<S> Clone for Client<S> {
    fn clone(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }
}

impl<S: fmt::Debug> fmt::Debug for Client<S> {
    /// The endpoints, the default model, the deadline, the response limit and
    /// the names of the default headers, then the transport. Never the API key
    /// and never a header value.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("config", &self.shared.config)
            .field("transport", &self.shared.service)
            .finish()
    }
}

#[cfg(feature = "hyper")]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
impl Client<HyperTransport> {
    /// A builder for a client; every setting it leaves unset comes from the
    /// environment, then from the SDK's default.
    #[must_use]
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// A client configured by the environment alone: `TYPESAFE_API_KEY`, and
    /// optionally `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL`.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// no API key is set, a value is unusable, or a variable is not UTF-8.
    pub fn from_env() -> Result<Self, Error> {
        Self::builder().build()
    }
}

impl<S> Client<S> {
    /// Assembles a client around a resolved configuration.
    fn assemble(config: Config, retry: RetryPolicy, service: S) -> Self {
        let get_headers = transport::base_headers(&config, false);
        let post_headers = transport::base_headers(&config, true);
        let mut model = Vec::with_capacity(config.default_model().len() + 2);
        codec::write_json_string(&mut model, config.default_model());
        Self {
            shared: Arc::new(Shared {
                service,
                config,
                get_headers,
                post_headers,
                model_json: Bytes::from(model),
                retry,
            }),
        }
    }

    /// What every clone of this client shares.
    pub(crate) fn shared(&self) -> &Shared<S> {
        &self.shared
    }
}

impl<S> Client<S>
where
    S: HttpService,
{
    /// A System One request asking `questions` about `state`.
    ///
    /// `state` is anything that serializes to a JSON string, object or array;
    /// it is encoded when the request is sent, straight into the body. The
    /// request is configured with the builder's methods and sent with
    /// [`send`](SystemOne::send).
    pub fn system_one<'a, T>(
        &'a self,
        state: &'a T,
        questions: &'a PreparedQuestions,
    ) -> SystemOne<'a, S, T>
    where
        T: Serialize + ?Sized,
    {
        SystemOne::new(self, state, questions)
    }

    /// The models resource.
    pub fn models(&self) -> Models<'_, S> {
        Models::new(self)
    }

    /// Lists the models once and drops the answer.
    ///
    /// That checks the API key and leaves an open connection in the pool, so
    /// requests started together afterwards share it instead of each opening
    /// one. Call it before a burst of concurrent requests.
    ///
    /// # Errors
    ///
    /// Returns what [`ListModels::send`](crate::models::ListModels::send)
    /// returns: an authentication failure, for a key the API refuses.
    pub async fn warm_up(&self) -> Result<(), Error> {
        self.models().list().send().await.map(drop)
    }
}

/// Configures a [`Client`].
///
/// Every setting is optional. The API key, base URL and default model fall
/// back to `TYPESAFE_API_KEY`, `TYPESAFE_BASE_URL` and
/// `TYPESAFE_DEFAULT_MODEL`, then to the SDK's defaults (no key, which fails;
/// `https://api.typesafe.ai`; `jev-latest`). The methods never fail: what
/// they are given is checked by `build()` (with the `hyper` feature) or
/// [`build_with_service`](ClientBuilder::build_with_service).
#[derive(Default)]
pub struct ClientBuilder {
    api_key: Option<SecretString>,
    base_url: Option<String>,
    default_model: Option<String>,
    /// `None` leaves the default; `Some(None)` asks for no deadline.
    timeout: Option<Option<Duration>>,
    default_headers: Vec<(String, String)>,
    max_response_bytes: Option<usize>,
    #[cfg(feature = "hyper")]
    extra_roots: Vec<Vec<u8>>,
    #[cfg(feature = "hyper")]
    http_version: Option<HttpVersion>,
    #[cfg(feature = "hyper")]
    connect_timeout: Option<Duration>,
    retry: Option<RetryPolicy>,
    user_agent_product: Option<String>,
    /// `false`, the default, sends `X-TypeSafe-Runtime`.
    omit_runtime_header: bool,
    /// `false`, the default, logs the full endpoint URL.
    omit_endpoint_host: bool,
}

impl ClientBuilder {
    /// A builder with no explicit settings, as [`Default`] also gives.
    ///
    /// Use [`build_with_service`](Self::build_with_service) with a custom
    /// transport, or `build()` with the `hyper` feature.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The API key. It is sent as `Authorization: Bearer <key>` and is
    /// printed nowhere. Leading and trailing whitespace is stripped; an empty
    /// key, internal whitespace, control and non-ASCII characters are refused.
    ///
    /// The key is copied once into the `Authorization` header value, which is not zeroed
    /// and lives as long as the client and every request built from it; `HeaderValue`
    /// is `Bytes`-backed and shared by reference count, so it cannot be zeroed on drop.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(SecretString::from(key.into()));
        self
    }

    /// The API root, such as `https://api.typesafe.ai`; trailing slashes are
    /// removed and a path prefix is kept.
    ///
    /// It must be an absolute `http` or `https` URL without userinfo, query or
    /// fragment. An `http://` base URL sends the API key unencrypted; use it
    /// only for a local proxy or a test server. Do not put a credential in its
    /// path: the path is printed by the client's `Debug` and in every error
    /// message that names an endpoint, as the Python SDK prints it.
    #[must_use]
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = Some(url.into());
        self
    }

    /// The model a request names when the call does not name one.
    #[must_use]
    pub fn default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = Some(model.into());
        self
    }

    /// The deadline of each whole attempt: transport readiness, connect,
    /// send and body read. With a custom transport it includes the caller's
    /// own pool wait and connect. The default is 10 seconds.
    ///
    /// A large `state` on a slow link can take longer than that to upload;
    /// raise the deadline for it, or use [`no_timeout`](Self::no_timeout).
    ///
    /// The budget is checked only before a retry and never cuts an attempt short,
    /// so a call lasts at most the budget plus one per-attempt deadline; with
    /// `RetryPolicy::none()` it lasts at most one per-attempt deadline.
    /// Dropping a call's future cancels the attempt in flight, and nothing of
    /// that call is sent or retried after the drop. Under
    /// `HttpVersion::Http2Only` (with the `hyper` feature) a connect the call
    /// had started goes on in a task of the transport's for the calls waiting
    /// for it, and without a `connect_timeout` this deadline also bounds that
    /// connect: it is given up once this long has passed since it began and
    /// the call that started it is gone.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(Some(timeout));
        self
    }

    /// No deadline on any attempt.
    ///
    /// Under `HttpVersion::Http2Only` (with the `hyper` feature) a connect is
    /// then given up with the call that started it, as nothing else bounds
    /// it; a `connect_timeout` bounds it instead, and lets it outlive that
    /// call.
    #[must_use]
    pub fn no_timeout(mut self) -> Self {
        self.timeout = Some(None);
        self
    }

    /// A header sent on every request. A per-call header of the same name
    /// replaces it; the SDK's own headers - `Authorization`, `Accept`,
    /// `User-Agent`, `X-TypeSafe-SDK`, `X-TypeSafe-Runtime`, and
    /// `Content-Type` on a request with a body - always win, and
    /// `X-TypeSafe-Retry-Count` is dropped. The headers that frame a message
    /// or manage its connection belong to the transport and are dropped too:
    /// `Content-Length`, `Transfer-Encoding`, `Connection`, `Keep-Alive`,
    /// `Proxy-Connection`, `TE`, `Trailer` and `Upgrade`. `Host` is sent as
    /// given, on every protocol; over HTTP/2 the request's `:authority` still
    /// comes from the base URL. Over HTTP/2 a `Host` that differs from the
    /// base URL's authority is outside RFC 9113 (section 8.3.1), and a
    /// conforming server may refuse the request as malformed. A caller that
    /// needs another `Host` routes by the base URL instead, or speaks
    /// HTTP/1.1: `HttpVersion::Auto` (with the `hyper` feature) does on an `http` base URL, and on an
    /// `https` one only when the server picks HTTP/1.1 through ALPN. A later
    /// call with the same name replaces an earlier one.
    ///
    /// No header set here or on a call reaches `User-Agent` or
    /// `X-TypeSafe-Runtime`: [`user_agent_product`](Self::user_agent_product)
    /// and [`send_runtime_header`](Self::send_runtime_header) are the only
    /// ways to change what they carry.
    #[must_use]
    pub fn default_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.default_headers.push((name.into(), value.into()));
        self
    }

    /// The largest response body a request reads, in bytes; 16 MiB unless
    /// set. A larger body is not read past the limit: a success response
    /// fails with [`ErrorKind::ResponseTooLarge`](crate::ErrorKind::ResponseTooLarge),
    /// a failure response is an API error without its body.
    #[must_use]
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Trusts `der`, a DER-encoded certificate, in addition to the operating
    /// system's roots: for a corporate CA the system store lacks, or a test
    /// server's own certificate.
    ///
    /// The default transport only; see
    /// [`build_with_service`](Self::build_with_service).
    #[must_use]
    #[cfg(feature = "hyper")]
    #[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
    pub fn add_root_certificate(mut self, der: impl Into<Vec<u8>>) -> Self {
        self.extra_roots.push(der.into());
        self
    }

    /// Which HTTP versions the default transport speaks. The default is
    /// [`HttpVersion::Http2Only`] for an `https` base URL and
    /// [`HttpVersion::Auto`] for an `http` one.
    ///
    /// The default transport only; see
    /// [`build_with_service`](Self::build_with_service).
    #[must_use]
    #[cfg(feature = "hyper")]
    #[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
    pub fn http_version(mut self, version: HttpVersion) -> Self {
        self.http_version = Some(version);
        self
    }

    /// A deadline for opening a TCP connection, inside the deadline of the
    /// whole attempt. None unless set. When it passes, the request fails with
    /// [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) carrying this value.
    ///
    /// Under [`HttpVersion::Http2Only`] it also bounds the whole connect - TCP,
    /// TLS and the HTTP/2 handshake - once the call that started it has given
    /// up: the connect goes on for the calls waiting for it until this long
    /// after it began, and those still waiting then fail with
    /// `ErrorKind::Timeout` carrying this value. While the call that started
    /// it waits, it bounds the TCP connect only, as above. Without it, the
    /// client's [`timeout`](Self::timeout) bounds the connect in the same
    /// way, and with [`no_timeout`](Self::no_timeout) nothing does, so the
    /// connect is given up with the call that started it: set a connect
    /// timeout to let the connect outlive that call.
    ///
    /// Set above the deadline of an attempt, it lets the retries of the calls
    /// that gave up at an endpoint slower than that deadline be served on the
    /// connect their first attempts started: at an endpoint whose TLS and
    /// HTTP/2 handshake end 1.5 s after it accepted the connection, 64 calls
    /// with a 1 s deadline and the default retry policy all succeed on one
    /// connection with a 2 s connect timeout, and none does without one. The
    /// price is paid at an endpoint whose first connect stalls while a new
    /// one would be served: the calls wait for the stalled connect until the
    /// connect timeout, so they recover about one retry wave later (the first
    /// success after 2.8 s with 2 s, 1.4 s without), and a connect timeout
    /// longer than the retries last loses the burst (none of 64 with 10 s).
    ///
    /// The default transport only; see
    /// [`build_with_service`](Self::build_with_service).
    #[must_use]
    #[cfg(feature = "hyper")]
    #[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = Some(timeout);
        self
    }

    /// The retry policy of every call made through the client;
    /// [`RetryPolicy::default`] unless set. A call can replace it for itself
    /// with its own `retry`.
    #[must_use]
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// A product that names the application, sent in `User-Agent` in front
    /// of the SDK's own: `user_agent_product("my-app/1.2.0")` sends
    /// `User-Agent: my-app/1.2.0 typesafe-sdk-rust/<version>`, the more
    /// significant product first as RFC 9110 (section 10.1.5) orders them.
    /// Unset, `User-Agent` is the SDK's identifier alone. `X-TypeSafe-SDK`
    /// always names the SDK alone. A later call replaces an earlier one.
    ///
    /// The product must be `name/version`, both parts tokens (RFC 9110,
    /// section 5.6.2: letters, digits and ``!#$%&'*+-.^_`|~``), with exactly
    /// one `/` and at most 64 bytes in all. That rules out whitespace,
    /// control characters, anything outside ASCII, a comment in parentheses
    /// and a product without a version.
    ///
    /// # Errors
    ///
    /// This method never fails. A product that breaks those rules makes
    /// `build()` (with the `hyper` feature) and
    /// [`build_with_service`](Self::build_with_service)
    /// return an [`ErrorKind::Config`](crate::ErrorKind::Config) error naming
    /// the rule, before anything is sent.
    ///
    /// ```
    /// use typesafe_sdk::{Client, ErrorKind};
    ///
    /// // Building connects to nothing.
    /// let client = Client::builder()
    ///     .api_key("your-api-key")
    ///     .user_agent_product("my-app/1.2.0")
    ///     .build()?;
    /// # drop(client);
    ///
    /// let error = Client::builder()
    ///     .api_key("your-api-key")
    ///     .user_agent_product("my app")
    ///     .build()
    ///     .expect_err("a product with a space is refused");
    /// assert!(matches!(error.kind(), ErrorKind::Config));
    /// # Ok::<(), typesafe_sdk::Error>(())
    /// ```
    #[must_use]
    pub fn user_agent_product(mut self, product: impl Into<String>) -> Self {
        self.user_agent_product = Some(product.into());
        self
    }

    /// Whether requests carry `X-TypeSafe-Runtime: rust (<os>; <arch>)`,
    /// which tells the API the operating system and architecture the SDK was
    /// compiled for. The default is `true`; `false` leaves the header out of
    /// every request, so an application can keep its platform to itself.
    /// `X-TypeSafe-SDK` is sent either way. A later call replaces an earlier
    /// one.
    ///
    /// ```
    /// use typesafe_sdk::Client;
    ///
    /// // Building connects to nothing.
    /// let client = Client::builder().api_key("your-api-key").send_runtime_header(false).build()?;
    /// # drop(client);
    /// # Ok::<(), typesafe_sdk::Error>(())
    /// ```
    #[must_use]
    pub fn send_runtime_header(mut self, send: bool) -> Self {
        self.omit_runtime_header = !send;
        self
    }

    /// Whether log events name the full endpoint URL. The default is `true`.
    ///
    /// With `false`, events print only `/v1/systemone` or `/v1/models`, not
    /// the scheme, host or the base URL's path prefix. A later call replaces
    /// an earlier one. This setting is available without `tracing` too.
    ///
    /// Errors (`Display`, [`ApiError::endpoint`](crate::ApiError::endpoint),
    /// [`ResponseValidationError`](crate::ResponseValidationError)) still name
    /// the scheme and host, so a caller logging `%error` puts the host back.
    /// Request and response bodies still print at `TRACE` under the target
    /// `typesafe_sdk` whatever this flag; cap that target to keep them out.
    /// No credential from userinfo, query or fragment can reach the default
    /// endpoint line, because those URL components are refused at build.
    ///
    /// ```
    /// use typesafe_sdk::Client;
    ///
    /// // Building connects to nothing.
    /// let client = Client::builder().api_key("your-api-key").log_endpoint_host(false).build()?;
    /// # drop(client);
    /// # Ok::<(), typesafe_sdk::Error>(())
    /// ```
    #[must_use]
    pub fn log_endpoint_host(mut self, log: bool) -> Self {
        self.omit_endpoint_host = !log;
        self
    }

    /// Builds a client with the default transport.
    ///
    /// Settings left unset are read from the environment. Nothing connects
    /// here: the first request, or [`Client::warm_up`], does.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// no API key is found or the key is empty after trimming or holds
    /// whitespace, a control or a non-ASCII character; when
    /// the base URL is not an absolute `http` or `https` URL without
    /// userinfo, query or fragment; when the default model is blank; when a
    /// deadline or the response limit is zero; when a default header is not a
    /// valid header; when the [`user_agent_product`](Self::user_agent_product)
    /// is not a product token; when an environment variable is not UTF-8; or
    /// when the certificate verifier cannot be built, for an added root that
    /// is not a certificate among other causes. No message repeats the key, a
    /// header value or the URL.
    #[cfg(feature = "hyper")]
    #[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
    pub fn build(self) -> Result<Client<HyperTransport>, Error> {
        self.build_with_env(|name: &str| std::env::var_os(name))
    }

    /// Builds a client that sends its requests through `service`.
    ///
    /// This is how a client runs over a transport of the caller's own: a
    /// proxy, a recorder, a `tower` stack with its own middleware. The
    /// service owns its connections and their timeouts; the SDK still wraps
    /// each attempt in its own deadline and reads the response under its own
    /// limit.
    ///
    /// When the service fails, its error's text becomes the connection
    /// error's message, escaped and cut at 200 characters. The request's
    /// credentials are replaced by `***` first: the API key, and the value of
    /// every header whose name is a secret one (`authorization`,
    /// `proxy-authorization`, `x-api-key`, `api-key`, `cookie`, `set-cookie`,
    /// or any name containing `token` or `secret`) or that is flagged
    /// sensitive, as it is, as `{:?}` of a `str`, `str::escape_debug`, `{:?}`
    /// of a `HeaderValue` and of `Bytes`, and a JSON string write it, and each
    /// of those escaped once more as `{:?}` of a `str` writes it, which is how
    /// a derived `Debug` prints a `String` field holding one. When
    /// any of those occurs in the error's `Display`, `{:?}` or `{:#?}`, or in
    /// any error below it, the [`source`](std::error::Error::source) is a
    /// redacted copy that cannot be downcast. The message is redacted after
    /// escaping as well, so a covered form the escaping creates by chance is
    /// `***` too. The value of any other header a service prints stays in the
    /// message, and so does a credential written in a form not listed here:
    /// as a list of byte values, `{:x?}`, percent-encoded or in base64.
    ///
    /// # Errors
    ///
    /// A config error for missing or invalid settings, as `build()` (with the
    /// `hyper` feature) reports, except that no certificate verifier is built.
    /// With `hyper`, `add_root_certificate`, `http_version` and
    /// `connect_timeout` are also config errors: they configure the default
    /// transport, which this client does not have, and are refused rather
    /// than ignored.
    pub fn build_with_service<S>(self, service: S) -> Result<Client<S>, Error>
    where
        S: HttpService,
    {
        #[cfg(feature = "hyper")]
        {
            let mut unused = Vec::new();
            if !self.extra_roots.is_empty() {
                unused.push("add_root_certificate");
            }
            if self.http_version.is_some() {
                unused.push("http_version");
            }
            if self.connect_timeout.is_some() {
                unused.push("connect_timeout");
            }
            if !unused.is_empty() {
                let verb = if unused.len() == 1 { "configures" } else { "configure" };
                return Err(Error::config(format!(
                    "{} {verb} the default transport, and a client built with \
                     build_with_service has a transport of its own.",
                    unused.join(", ")
                )));
            }
        }
        let (explicit, retry) = self.split()?;
        let config = Config::resolve(explicit, |name: &str| std::env::var_os(name))?;
        Ok(Client::assemble(config, retry, service))
    }

    /// [`build`](Self::build) with the environment read through `env`.
    #[cfg(feature = "hyper")]
    pub(crate) fn build_with_env<V>(
        mut self,
        env: impl Fn(&str) -> Option<V>,
    ) -> Result<Client<HyperTransport>, Error>
    where
        V: Into<OsString>,
    {
        let transport = TransportChoices {
            version: self.http_version,
            extra_roots: std::mem::take(&mut self.extra_roots),
            connect_timeout: self.connect_timeout,
        };
        let (explicit, retry) = self.split()?;
        let config = Config::resolve(explicit, env)?;
        let https = config.endpoints().system_one().scheme() == Some(&Scheme::HTTPS);
        let settings = TransportSettings {
            version: transport.version.unwrap_or(if https {
                HttpVersion::Http2Only
            } else {
                HttpVersion::Auto
            }),
            extra_roots: transport.extra_roots,
            connect_timeout: transport.connect_timeout,
            attempt_deadline: config.timeout(),
        };
        Ok(Client::assemble(config, retry, HyperTransport::new(settings)?))
    }

    /// Checks what only the builder can check and separates the configuration
    /// from the retry policy.
    fn split(self) -> Result<(Explicit, RetryPolicy), Error> {
        let Self {
            api_key,
            base_url,
            default_model,
            timeout,
            default_headers,
            max_response_bytes,
            #[cfg(feature = "hyper")]
                extra_roots: _,
            #[cfg(feature = "hyper")]
                http_version: _,
            #[cfg(feature = "hyper")]
            connect_timeout,
            retry,
            user_agent_product,
            omit_runtime_header,
            omit_endpoint_host,
        } = self;

        #[cfg(feature = "hyper")]
        if connect_timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err(Error::config("connect_timeout must be a positive number of seconds."));
        }
        let mut headers = HeaderMap::with_capacity(default_headers.len());
        for (name, value) in &default_headers {
            let (name, value) =
                transport::parse_header(name, value, "default ").map_err(Error::config)?;
            headers.insert(name, value);
        }

        let explicit = Explicit {
            api_key,
            base_url,
            default_model,
            timeout,
            default_headers: headers,
            max_response_bytes,
            user_agent_product,
            omit_runtime_header,
            omit_endpoint_host,
        };
        Ok((explicit, retry.unwrap_or_default()))
    }
}

/// The builder's settings for the default transport, before the base URL
/// decides the default version.
#[cfg(feature = "hyper")]
struct TransportChoices {
    version: Option<HttpVersion>,
    extra_roots: Vec<Vec<u8>>,
    connect_timeout: Option<Duration>,
}

impl fmt::Debug for ClientBuilder {
    /// What was set, without the key, without header values, and with the
    /// roots as a count. The base URL is shown only once it has passed the
    /// checks `build()` runs, and then as its endpoints,
    /// the way an error names them: a URL that failed them may still hold
    /// userinfo. A retry policy, a `User-Agent` product and a runtime header
    /// switched off are shown when they were set; the product is quoted and
    /// escaped, since until it is built it may hold anything.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoints =
            self.base_url.as_deref().map(|url| crate::config::endpoints(url.trim_end_matches('/')));
        let base_url = endpoints.as_ref().map(|endpoints| {
            endpoints.as_ref().map_or_else(|_| Shown::Text("<not a usable URL>"), Shown::Endpoints)
        });
        let mut shown = formatter.debug_struct("ClientBuilder");
        shown
            .field("api_key", &self.api_key.as_ref().map(|_| Shown::Text("<redacted>")))
            .field("base_url", &base_url)
            .field("default_model", &self.default_model)
            .field("timeout", &self.timeout)
            .field(
                "default_headers",
                &self.default_headers.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("max_response_bytes", &self.max_response_bytes);
        #[cfg(feature = "hyper")]
        shown
            .field("extra_roots", &self.extra_roots.len())
            .field("http_version", &self.http_version)
            .field("connect_timeout", &self.connect_timeout);
        if let Some(retry) = &self.retry {
            shown.field("retry", retry);
        }
        if let Some(product) = &self.user_agent_product {
            shown.field("user_agent_product", &Shown::Quoted(crate::text::quoted(product)));
        }
        if self.omit_runtime_header {
            shown.field("send_runtime_header", &false);
        }
        if self.omit_endpoint_host {
            shown.field("log_endpoint_host", &false);
        }
        shown.finish()
    }
}

/// A value the builder's `Debug` prints in place of the one it holds.
enum Shown<'a> {
    Text(&'static str),
    /// Text already quoted and escaped by [`crate::text::quoted`].
    Quoted(String),
    Endpoints(&'a crate::config::Endpoints),
}

impl fmt::Debug for Shown<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => formatter.write_str(text),
            Self::Quoted(text) => formatter.write_str(text),
            Self::Endpoints(endpoints) => fmt::Debug::fmt(endpoints, formatter),
        }
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
