//! Tests for the transport seam: the request body, header assembly, one
//! attempt against a real loopback server, and what becomes of the SDK's own
//! errors a custom service fails with.

use std::{
    error::Error as StdError,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use http::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
#[cfg(feature = "hyper")]
use test_support::{Protocol, TestServer};

use super::*;
use crate::{
    ClientBuilder, ErrorKind, RetryPolicy,
    config::Explicit,
    constants::{RUNTIME_HEADER, SDK_HEADER},
    rendering_tests::assert_printable,
};

/// A configuration with a key, a base URL and the given default headers.
fn config(base_url: &str, defaults: &[(&str, &str)]) -> Config {
    config_with(base_url, defaults, None, true)
}

/// [`config`] with a `User-Agent` product and the runtime header switch.
fn config_with(
    base_url: &str,
    defaults: &[(&str, &str)],
    product: Option<&str>,
    send_runtime_header: bool,
) -> Config {
    let mut headers = HeaderMap::new();
    for (name, value) in defaults {
        headers.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("a test header name is valid"),
            HeaderValue::from_str(value).expect("a test header value is valid"),
        );
    }
    let explicit = Explicit {
        api_key: Some("test-key".into()),
        base_url: Some(base_url.into()),
        default_model: Some("jev-latest".into()),
        default_headers: headers,
        user_agent_product: product.map(str::to_owned),
        omit_runtime_header: !send_runtime_header,
        ..Explicit::default()
    };
    Config::resolve(explicit, |_: &str| None::<String>)
        .unwrap_or_else(|error| panic!("the test configuration resolves: {error:?}"))
}

/// Every value of `name` in `headers`, as text.
fn values<'a>(headers: &'a HeaderMap, name: &str) -> Vec<&'a str> {
    headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().expect("a test header value is text"))
        .collect()
}

// ------------------------------------------------------------------- Body

#[tokio::test]
async fn a_body_is_one_frame_of_an_exact_length() {
    let body = Body::from(Bytes::from_static(b"{\"state\":\"x\"}"));
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(13));
    assert!(!http_body::Body::is_end_stream(&body));
    assert_eq!(body.len(), 13);

    let mut body = body;
    let frame = body.frame().await.expect("one frame").expect("infallible");
    assert_eq!(frame.into_data().expect("a data frame"), &b"{\"state\":\"x\"}"[..]);
    assert!(http_body::Body::is_end_stream(&body), "nothing follows the one frame");
    assert!(body.frame().await.is_none());
    assert_eq!(http_body::Body::size_hint(&body).exact(), Some(0));
}

#[tokio::test]
async fn an_empty_body_ends_before_its_first_frame() {
    for mut body in [Body::empty(), Body::from(Bytes::new()), Body::default()] {
        assert!(http_body::Body::is_end_stream(&body), "{body:?}");
        assert!(body.is_empty());
        assert_eq!(http_body::Body::size_hint(&body).exact(), Some(0));
        assert!(body.frame().await.is_none(), "an empty body sends no empty frame");
    }
}

#[test]
fn a_body_prints_its_length_and_never_its_bytes() {
    let body = Body::from(Bytes::from_static(b"{\"state\":\"my card is 4242\"}"));
    assert_eq!(format!("{body:?}"), "Body { len: 27 }");
}

// ------------------------------------------------------- header assembly

#[test]
fn the_sdk_headers_win_over_every_client_default() {
    let config = config(
        "https://example.test",
        &[
            ("authorization", "injected-secret"),
            ("accept", "text/plain"),
            ("user-agent", "wrong"),
            ("x-typesafe-sdk", "wrong"),
            ("x-typesafe-runtime", "wrong"),
            ("x-typesafe-retry-count", "99"),
            ("content-type", "text/plain"),
            ("x-team", "default"),
        ],
    );

    for with_body in [false, true] {
        let headers = base_headers(&config, with_body);
        assert_eq!(values(&headers, "authorization"), ["Bearer test-key"], "body {with_body}");
        assert!(headers[AUTHORIZATION].is_sensitive());
        assert_eq!(values(&headers, "accept"), ["application/json"]);
        let identifier = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(values(&headers, "user-agent"), [identifier.as_str()]);
        assert_eq!(values(&headers, "x-typesafe-sdk"), [identifier.as_str()]);
        let runtime = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
        assert_eq!(values(&headers, "x-typesafe-runtime"), [runtime.as_str()]);
        assert!(headers.get(RETRY_COUNT_HEADER).is_none(), "body {with_body}");
        assert_eq!(values(&headers, "x-team"), ["default"]);
        // `Content-Type` is the SDK's only on a request that has a body.
        let content_type = if with_body { "application/json" } else { "text/plain" };
        assert_eq!(values(&headers, "content-type"), [content_type], "body {with_body}");
    }
    assert_eq!(
        PROTECTED_HEADERS,
        [AUTHORIZATION, ACCEPT, USER_AGENT, SDK_HEADER, RUNTIME_HEADER],
        "the protected set is upstream's five"
    );
}

/// The caller's defaults that name the two headers these settings shape,
/// and one that is the caller's own.
const SHAPED_DEFAULTS: [(&str, &str); 3] =
    [("user-agent", "wrong"), ("x-typesafe-runtime", "wrong"), ("x-team", "default")];

#[test]
fn a_user_agent_product_goes_in_front_of_the_sdk_identifier_and_leaves_x_typesafe_sdk_alone() {
    let config = config_with("https://example.test", &SHAPED_DEFAULTS, Some("my-app/1.2.0"), true);
    let identifier = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    let runtime = format!("rust ({}; {})", std::env::consts::OS, std::env::consts::ARCH);
    for with_body in [false, true] {
        let headers = base_headers(&config, with_body);
        let product = format!("my-app/1.2.0 {identifier}");
        assert_eq!(values(&headers, "user-agent"), [product.as_str()], "body {with_body}");
        assert_eq!(values(&headers, "x-typesafe-sdk"), [identifier.as_str()], "body {with_body}");
        assert_eq!(values(&headers, "x-typesafe-runtime"), [runtime.as_str()], "body {with_body}");
        assert_eq!(values(&headers, "x-team"), ["default"], "body {with_body}");
    }
}

#[test]
fn with_the_runtime_header_off_none_is_built_and_the_other_sdk_headers_are() {
    let identifier = format!("decision-model-sdk/{}", env!("CARGO_PKG_VERSION"));
    for product in [None, Some("my-app/1.2.0")] {
        let config = config_with("https://example.test", &SHAPED_DEFAULTS, product, false);
        let user_agent =
            product.map_or(identifier.clone(), |product| format!("{product} {identifier}"));
        for with_body in [false, true] {
            let context = format!("product {product:?}, body {with_body}");
            let headers = base_headers(&config, with_body);
            assert!(headers.get(RUNTIME_HEADER).is_none(), "{context}: {headers:?}");
            assert_eq!(values(&headers, "authorization"), ["Bearer test-key"], "{context}");
            assert_eq!(values(&headers, "accept"), ["application/json"], "{context}");
            assert_eq!(values(&headers, "user-agent"), [user_agent.as_str()], "{context}");
            assert_eq!(values(&headers, "x-typesafe-sdk"), [identifier.as_str()], "{context}");
            assert_eq!(values(&headers, "x-team"), ["default"], "{context}");
        }
    }
}

/// Neither setting opens a way in: whatever they are, a per-call
/// `User-Agent` or `X-TypeSafe-Runtime` is dropped, as a default of either
/// name is above.
#[test]
fn a_call_header_never_reaches_user_agent_or_x_typesafe_runtime() {
    let raw = [("User-Agent", "wrong"), ("X-TypeSafe-Runtime", "wrong"), ("x-team", "call")];
    for with_body in [false, true] {
        let parsed = call_headers(raw, with_body).expect("every header is valid");
        let names: Vec<&str> = parsed.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["x-team"], "body {with_body}");
    }
}

#[test]
fn a_call_header_is_dropped_when_the_sdk_owns_it_and_the_last_of_a_name_wins() {
    let raw = [
        ("Authorization", "injected-secret"),
        ("ACCEPT", "text/plain"),
        ("user-agent", "wrong"),
        ("X-TypeSafe-SDK", "wrong"),
        ("x-typesafe-runtime", "wrong"),
        ("X-TypeSafe-Retry-Count", "99"),
        ("content-type", "wrong"),
        ("X-Team", "first"),
        ("x-other", "kept"),
        ("x-team", "call"),
    ];

    let with_body = call_headers(raw, true).expect("every header is valid");
    let shown: Vec<(&str, &str)> = with_body
        .iter()
        .map(|(name, value)| (name.as_str(), value.to_str().expect("text")))
        .collect();
    assert_eq!(shown, [("x-team", "call"), ("x-other", "kept")]);

    let without_body = call_headers(raw, false).expect("every header is valid");
    let shown: Vec<(&str, &str)> = without_body
        .iter()
        .map(|(name, value)| (name.as_str(), value.to_str().expect("text")))
        .collect();
    assert_eq!(shown, [("content-type", "wrong"), ("x-team", "call"), ("x-other", "kept")]);
}

#[test]
fn a_call_header_that_cannot_be_sent_is_refused_without_its_value() {
    let secret = "sk-live-do-not-log";
    let rows = [
        (("x team", "fine"), r#"The header name "x team" is not a valid HTTP header name."#),
        (
            ("x-bad\nname", "fine"),
            r#"The header name "x-bad\nname" is not a valid HTTP header name."#,
        ),
        (
            ("x-token", "sk-live-do-not-log\r\nInjected: yes"),
            r#"The value of the header "x-token" is not a valid HTTP header value."#,
        ),
        (
            ("x-token", "sk-live-do-not-log\u{0}"),
            r#"The value of the header "x-token" is not a valid HTTP header value."#,
        ),
    ];
    for ((name, value), message) in rows {
        let error = call_headers([("x-fine", "ok"), (name, value)], true)
            .expect_err("the header cannot be sent");
        assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
        assert_eq!(error.to_string(), message);
        assert_eq!(
            format!("{error:?}"),
            format!("Error {{ kind: InvalidRequest, message: {message:?} }}")
        );
        assert!(!format!("{error}{error:?}").contains(secret), "the value leaked: {error:?}");
    }
}

// ------------------------------------------------------------ one attempt

/// Answers every request with `200 {}`.
#[cfg(feature = "hyper")]
async fn empty_object_server(protocol: Protocol) -> TestServer {
    TestServer::start(protocol, |_| async {
        http::Response::new(http_body_util::Full::new(Bytes::from_static(b"{}")))
    })
    .await
    .expect("the test server starts")
}

#[cfg(feature = "hyper")]
#[tokio::test]
async fn the_retry_count_is_sent_from_the_second_attempt_on_and_never_taken_from_a_caller() {
    let server = empty_object_server(Protocol::Http1).await;
    let config = Config::resolve(
        Explicit {
            api_key: Some("test-key".into()),
            base_url: Some(server.base_url().into()),
            default_model: Some("jev-latest".into()),
            default_headers: HeaderMap::from_iter([(
                RETRY_COUNT_HEADER,
                HeaderValue::from_static("7"),
            )]),
            max_response_bytes: Some(1024),
            ..Explicit::default()
        },
        |_: &str| None::<String>,
    )
    .expect("the test configuration resolves");
    let transport = HyperTransport::new(TransportSettings {
        version: HttpVersion::Auto,
        extra_roots: Vec::new(),
        connect_timeout: None,
        attempt_deadline: None,
    })
    .expect("the transport builds");
    let base = base_headers(&config, true);
    let call = call_headers([("x-typesafe-retry-count", "99"), ("x-call", "yes")], true)
        .expect("valid headers");
    let exchange = Exchange {
        method: &Method::POST,
        uri: config.endpoints().system_one(),
        base_headers: &base,
        call_headers: &call,
        deadline: Some(Duration::from_secs(5)),
        config: &config,
    };

    let body = Bytes::from_static(b"{\"state\":\"x\"}");
    for retry in [0, 2, 1] {
        let (status, _, received) = attempt(&transport, exchange, retry, Some(body.clone()))
            .await
            .unwrap_or_else(|error| panic!("attempt {retry}: {error}"));
        assert_eq!(status, StatusCode::OK);
        assert_eq!(received, &b"{}"[..]);
    }

    let requests = server.requests();
    let counts: Vec<Vec<&str>> =
        requests.iter().map(|request| values(&request.headers, "x-typesafe-retry-count")).collect();
    assert_eq!(counts, [vec![], vec!["2"], vec!["1"]]);
    for request in &requests {
        assert_eq!(request.body, body, "every attempt sends the same bytes");
        assert_eq!(values(&request.headers, "x-call"), ["yes"]);
        assert_eq!(values(&request.headers, "content-type"), ["application/json"]);
        assert_eq!(request.uri.path(), "/v1/systemone");
    }
}

#[test]
fn a_transport_failure_reads_as_its_chain_of_messages() {
    let refused = io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused");
    let error = connection(Box::new(Wrapper { message: "tcp connect error", source: refused }));

    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: tcp connect error: Connection refused");
    let wrapper = error.source().expect("the cause is kept");
    assert_eq!(wrapper.to_string(), "tcp connect error");
    let io = wrapper.source().and_then(|source| source.downcast_ref::<io::Error>());
    assert_eq!(io.map(io::Error::kind), Some(io::ErrorKind::ConnectionRefused));

    // An error this crate raised inside a transport passes through unchanged.
    let ours: BoxError = Box::new(Error::timeout(Duration::from_millis(250)));
    let timeout = connection(ours);
    assert!(
        matches!(timeout.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_millis(250)),
        "{timeout:?}"
    );
    assert!(timeout.source().is_none());
}

/// A transport error that says whatever it likes: a newline, an ANSI
/// colour, a right-to-left override, and 100,000 characters.
#[derive(Debug)]
struct Loud;

impl fmt::Display for Loud {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "bad\nline \u{1b}[31mred \u{202e}rtl {}", "x".repeat(100_000))
    }
}

impl StdError for Loud {}

/// The message a [`Loud`] error renders as: escaped, and cut 200 characters
/// after the prefix. The escapes count as the characters they print as:
/// 36 before the run of `x`, so 164 of those, then the mark.
pub(super) fn loud_message() -> String {
    format!(
        "Connection error: bad\\nline \\u{{1b}}[31mred \\u{{202e}}rtl {}\u{2026}",
        "x".repeat(164)
    )
}

#[test]
fn text_a_transport_chose_is_escaped_and_cut_and_its_error_kept_whole() {
    let error = connection(Box::new(Loud));

    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    let rendered = error.to_string();
    assert_eq!(rendered, loud_message());
    assert_eq!(rendered.chars().count(), "Connection error: ".len() + text::MAX_MESSAGE_CHARS + 1);
    for shown in [rendered.clone(), format!("{rendered:?}")] {
        assert_printable(&shown);
    }
    let source = error.source().expect("the transport's error is kept");
    assert_eq!(source.to_string(), Loud.to_string(), "the cause keeps its full text");
    assert_eq!(source.to_string().chars().count(), 100_023);
}

#[test]
fn a_chain_longer_than_eight_links_is_cut() {
    let mut error: BoxError = Box::new(io::Error::other("root"));
    for _ in 0..20 {
        error = Box::new(Wrapper { message: "link", source: error });
    }
    let rendered = connection_message(&*error);
    assert_eq!(rendered, format!("Connection error: {}", ["link"; 8].join(": ")));
}

/// An error with a message of its own and a cause under it.
#[derive(Debug)]
struct Wrapper<E> {
    message: &'static str,
    source: E,
}

impl<E: fmt::Debug> fmt::Display for Wrapper<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl StdError for Wrapper<io::Error> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.source)
    }
}

impl StdError for Wrapper<BoxError> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&*self.source)
    }
}

// ------------------------------------------- a custom service's own errors

/// The key of the client under test, with a quote, a double quote and a
/// backslash, which every escaping form writes differently.
const SERVICE_KEY: &str = "ts_live_quo'te\"slash\\tail";

/// The value of the client's secret default header.
const SERVICE_SECRET: &str = "provider-credential";

/// A custom transport that fails every request with the SDK error `fail`
/// builds, in `poll_ready` or in `call`.
#[derive(Clone)]
struct Failing {
    in_poll_ready: bool,
    fail: Arc<dyn Fn() -> Error + Send + Sync>,
    calls: Arc<AtomicUsize>,
}

impl Failing {
    fn new(in_poll_ready: bool, fail: impl Fn() -> Error + Send + Sync + 'static) -> Self {
        Self { in_poll_ready, fail: Arc::new(fail), calls: Arc::default() }
    }

    fn error(&self) -> BoxError {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::new((self.fail)())
    }
}

impl Service<Request<Body>> for Failing {
    type Response = Response<http_body_util::Empty<Bytes>>;
    type Error = BoxError;
    type Future = std::future::Ready<Result<Self::Response, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(if self.in_poll_ready { Err(self.error()) } else { Ok(()) })
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        std::future::ready(Err(self.error()))
    }
}

/// Lists the models through `service` under `retry`, with [`SERVICE_KEY`] as
/// the key and [`SERVICE_SECRET`] as a secret default header, and returns
/// the error the call fails with.
async fn fail_through(service: Failing, retry: RetryPolicy) -> Error {
    ClientBuilder::new()
        .api_key(SERVICE_KEY)
        .base_url("https://api.typesafe.ai")
        .default_header("x-client-secret", SERVICE_SECRET)
        .retry(retry.backoff_initial(Duration::ZERO).backoff_max(Duration::ZERO))
        .build_with_service(service)
        .expect("the client builds")
        .models()
        .list()
        .send()
        .await
        .expect_err("the service fails")
}

/// Every form in which `secret` can be printed: as it is, as `{:?}` of a
/// `str`, `escape_debug`, `{:?}` of a `HeaderValue` and of `Bytes`, and a
/// JSON string write it, without their quotes; and each of those once more as
/// `{:?}` of a `str` writes it.
fn spellings(secret: &str) -> Vec<String> {
    let unquote = |text: String, open: usize| text[open..text.len() - 1].to_owned();
    let forms = [
        secret.to_owned(),
        unquote(format!("{secret:?}"), 1),
        secret.escape_debug().to_string(),
        unquote(format!("{:?}", HeaderValue::from_str(secret).expect("a header value")), 1),
        unquote(format!("{:?}", Bytes::copy_from_slice(secret.as_bytes())), 2),
        unquote(serde_json::to_string(secret).expect("a string encodes"), 1),
    ];
    let again = forms.clone().map(|form| unquote(format!("{form:?}"), 1));
    forms.into_iter().chain(again).collect()
}

/// The credentials of the client [`fail_through`] builds, as a service could
/// copy them out of the request: the key, the whole `Authorization` value and
/// the secret header's value.
fn service_credentials() -> [String; 3] {
    [SERVICE_KEY.to_owned(), format!("Bearer {SERVICE_KEY}"), SERVICE_SECRET.to_owned()]
}

/// `Display`, `{:?}` and `{:#?}` of `error`.
fn renderings(error: &Error) -> [String; 3] {
    [error.to_string(), format!("{error:?}"), format!("{error:#?}")]
}

/// A connection error a custom service builds without a cause, and whose
/// message copies a credential of the request in any spelling the SDK knows,
/// reaches the caller with every one replaced by `***`: in `Display`, `Debug`
/// and the alternate `Debug`, and still a connection error. A message that
/// holds none is kept byte for byte, neither escaped nor cut.
#[tokio::test]
async fn a_causeless_connection_error_from_a_service_has_its_credentials_replaced() {
    let mut cases = Vec::new();
    for credential in service_credentials() {
        for spelling in spellings(&credential) {
            cases.push((
                format!("proxy refused {spelling} for /v1/models"),
                String::from("proxy refused *** for /v1/models"),
            ));
        }
    }
    let plain = [
        String::from("Connection error: refused"),
        format!("bad\nline \u{1b}[31mred {}", "x".repeat(300)),
    ];
    for message in plain {
        cases.push((message.clone(), message));
    }

    for (message, expected) in cases {
        for in_poll_ready in [false, true] {
            let case = format!("{message:?}, in poll_ready {in_poll_ready}");
            let built = message.clone();
            let service =
                Failing::new(in_poll_ready, move || Error::connection(built.clone(), None));
            let error = fail_through(service, RetryPolicy::default().max_retries(0)).await;

            assert!(matches!(error.kind(), ErrorKind::Connection), "{case}: {error:?}");
            assert!(error.source().is_none(), "{case}: {error:?}");
            assert_eq!(error.to_string(), expected, "{case}");
            assert_eq!(
                format!("{error:?}"),
                format!("Error {{ kind: Connection, message: {expected:?} }}"),
                "{case}"
            );
            for rendering in renderings(&error) {
                for credential in service_credentials() {
                    for spelling in spellings(&credential) {
                        assert!(
                            !rendering.contains(&spelling),
                            "{case}: {spelling:?} in {rendering}"
                        );
                    }
                }
            }
        }
    }
}

/// A cause-less connection error whose message is far longer than any cut,
/// holds characters `Debug` would escape, and copies a credential, comes back
/// with the credential replaced by `***` and every other byte as the service
/// wrote it: neither escaped nor cut, in `Display` and in `Debug`.
#[tokio::test]
async fn a_long_causeless_message_with_a_credential_is_neither_escaped_nor_cut() {
    let head = format!("tab\there \"quoted\" back\\slash \u{1b}[31mred {}", "x".repeat(300));
    let tail = format!("{} new\nline \u{202e}end", "y".repeat(250));
    for credential in service_credentials() {
        let message = format!("{head} {credential} {tail}");
        let expected = format!("{head} *** {tail}");
        for in_poll_ready in [false, true] {
            let case = format!("{credential:?}, in poll_ready {in_poll_ready}");
            let built = message.clone();
            let service =
                Failing::new(in_poll_ready, move || Error::connection(built.clone(), None));
            let error = fail_through(service, RetryPolicy::default().max_retries(0)).await;

            assert!(matches!(error.kind(), ErrorKind::Connection), "{case}: {error:?}");
            assert!(error.source().is_none(), "{case}: {error:?}");
            assert_eq!(error.to_string(), expected, "{case}");
            assert_eq!(
                format!("{error:?}"),
                format!("Error {{ kind: Connection, message: {expected:?} }}"),
                "{case}"
            );
        }
    }
}

/// The replacement keeps the retry class: the built-in rule retries the
/// redacted connection error exactly while it retries connection errors, and
/// a caller's rule sees the redacted error, still a connection error.
#[tokio::test]
async fn a_redacted_connection_error_is_retried_as_any_connection_error_is() {
    let message = format!("proxy refused Bearer {SERVICE_KEY}");
    for (retried, attempts) in [(true, 3), (false, 1)] {
        let built = message.clone();
        let service = Failing::new(false, move || Error::connection(built.clone(), None));
        let calls = Arc::clone(&service.calls);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        let policy = RetryPolicy::default().max_retries(2).api_connection_error(retried).predicate(
            move |error| {
                let connection = matches!(error.kind(), ErrorKind::Connection);
                recorded.lock().expect("not poisoned").push((connection, error.to_string()));
                false
            },
        );

        let error = fail_through(service, policy).await;

        assert!(matches!(error.kind(), ErrorKind::Connection), "retried {retried}: {error:?}");
        assert_eq!(error.to_string(), "proxy refused ***", "retried {retried}");
        assert_eq!(calls.load(Ordering::SeqCst), attempts, "retried {retried}");
        let seen = seen.lock().expect("not poisoned").clone();
        // The built-in rule decides first; the caller's rule is asked only
        // when it says no.
        let asked =
            if retried { Vec::new() } else { vec![(true, String::from("proxy refused ***"))] };
        assert_eq!(seen, asked, "retried {retried}");
    }
}

/// Any other SDK error a custom service returns is the caller's as the
/// service built it: kind, status, body, headers and every rendering, a
/// credential in its text included. The adapter's providers rely on this.
#[tokio::test]
async fn an_sdk_error_of_another_kind_from_a_service_is_kept_unsearched() {
    let body = serde_json::json!({"error": {"message": format!("key {SERVICE_KEY} was refused")}})
        .to_string();
    let mut headers = HeaderMap::new();
    headers.insert("x-echoed-key", HeaderValue::from_str(SERVICE_KEY).expect("a header value"));
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req-1"));
    let api = {
        let (body, headers) = (Bytes::from(body.clone()), headers.clone());
        move |status: StatusCode| ApiError::from_response(status, body.clone(), headers.clone())
    };
    let invalid = || {
        let decode_error = crate::codec::decode::<bool>(b"\"x\"").expect_err("not a bool");
        crate::error::ResponseValidationError::new(
            StatusCode::OK,
            Bytes::from_static(b"\"x\""),
            HeaderMap::new(),
            None,
            decode_error,
        )
    };
    // The name, the attempts the default rule makes with the timeout retry
    // off and two retries, and the error.
    type Build = Box<dyn Fn() -> Error + Send + Sync>;
    let kinds: Vec<(&str, usize, Build)> = vec![
        (
            "api 401",
            1,
            Box::new({
                let api = api.clone();
                move || api(StatusCode::UNAUTHORIZED).into()
            }),
        ),
        ("api 503", 3, Box::new(move || api(StatusCode::SERVICE_UNAVAILABLE).into())),
        ("timeout", 1, Box::new(|| Error::timeout(Duration::from_millis(250)))),
        ("too large", 1, Box::new(|| Error::response_too_large(7))),
        ("validation", 1, Box::new(move || invalid().into())),
        ("config", 1, Box::new(|| Error::config(format!("key {SERVICE_KEY}")))),
        ("invalid request", 1, Box::new(|| Error::invalid_request(format!("key {SERVICE_KEY}")))),
    ];

    for (name, attempts, build) in kinds {
        let twin = build();
        let build: Arc<dyn Fn() -> Error + Send + Sync> = Arc::from(build);
        let service = Failing::new(false, move || build());
        let calls = Arc::clone(&service.calls);
        let policy = RetryPolicy::default().max_retries(2).api_timeout_error(false);
        let error = fail_through(service, policy).await;

        assert_eq!(renderings(&error), renderings(&twin), "{name}");
        assert_eq!(format!("{:?}", error.kind()), format!("{:?}", twin.kind()), "{name}");
        assert_eq!(calls.load(Ordering::SeqCst), attempts, "{name}: retried as its kind is");
        if let (ErrorKind::Api(kept), ErrorKind::Api(built)) = (error.kind(), twin.kind()) {
            assert_eq!(kept.status(), built.status(), "{name}");
            assert_eq!(kept.body(), body.as_bytes(), "{name}");
            assert_eq!(kept.headers(), &headers, "{name}");
            let shown = error.to_string();
            assert!(shown.contains(" was refused"), "{name}: read from the body: {shown}");
            assert!(
                spellings(SERVICE_KEY).iter().any(|spelling| shown.contains(spelling)),
                "{name}: the message read from the body is not searched: {shown}"
            );
        }
    }
}

/// A connection error a custom service builds with a cause is redacted as
/// the SDK's own transport's is: kept when nothing holds a credential; its
/// message alone rewritten when only the message holds one; message and
/// cause replaced when the cause holds one.
#[tokio::test]
async fn a_connection_error_with_a_cause_from_a_service_is_redacted_as_before() {
    let refused =
        || -> BoxError { Box::new(io::Error::new(io::ErrorKind::ConnectionRefused, "refused")) };

    let kept = fail_through(
        Failing::new(false, move || Error::connection("vendor unreachable", Some(refused()))),
        RetryPolicy::default().max_retries(0),
    )
    .await;
    assert_eq!(kept.to_string(), "vendor unreachable");
    let cause = kept.source().and_then(|source| source.downcast_ref::<io::Error>());
    assert_eq!(cause.map(io::Error::kind), Some(io::ErrorKind::ConnectionRefused), "{kept:?}");

    let message_only = fail_through(
        Failing::new(false, move || {
            Error::connection(format!("Connection error: sent {SERVICE_SECRET}"), Some(refused()))
        }),
        RetryPolicy::default().max_retries(0),
    )
    .await;
    assert!(matches!(message_only.kind(), ErrorKind::Connection), "{message_only:?}");
    assert_eq!(message_only.to_string(), "Connection error: sent ***");
    let cause = message_only.source().and_then(|source| source.downcast_ref::<io::Error>());
    assert_eq!(
        cause.map(io::Error::kind),
        Some(io::ErrorKind::ConnectionRefused),
        "{message_only:?}"
    );

    let replaced = fail_through(
        Failing::new(false, move || {
            let cause: BoxError = Box::new(io::Error::other(format!("sent {SERVICE_SECRET}")));
            Error::connection("vendor unreachable", Some(cause))
        }),
        RetryPolicy::default().max_retries(0),
    )
    .await;
    assert!(matches!(replaced.kind(), ErrorKind::Connection), "{replaced:?}");
    assert_eq!(replaced.to_string(), "Connection error: sent ***");
    let cause = replaced.source().expect("a redacted copy of the cause");
    assert_eq!(cause.to_string(), "sent ***");
    assert!(cause.downcast_ref::<io::Error>().is_none(), "{cause:?}");
    for rendering in renderings(&replaced) {
        assert!(!rendering.contains(SERVICE_SECRET), "{rendering}");
    }
}
