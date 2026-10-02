//! Tests for the shared HTTP module: the key header, the base URL and the
//! log URI, one POST against a real loopback server, the size cap, the
//! deadline, a foreign service's error, the event, and the default transport.

use std::{
    collections::VecDeque,
    future::{Ready, ready},
    io,
};

use http::{
    Version,
    header::{AUTHORIZATION, LOCATION},
};
use test_support::{Protocol, RefusingPort, SilentServer, TestServer, json_response};
use typesafe_sdk::{ApiErrorKind, ErrorKind as SdkErrorKind};

use super::*;
use crate::error::ErrorKind;

#[cfg(feature = "tracing")]
#[path = "../../tests/support/recorder.rs"]
mod recorder;

/// The key every test sends. No test may find it in an error or an event.
const KEY: &str = "sk-test-1f6c0a9d7b3e";

/// What a provider builds once: the key header, the endpoint and the
/// headers.
struct Fixture {
    endpoint: Endpoint,
    headers: HeaderMap,
    key: KeyHeader,
    limits: Limits,
}

impl Fixture {
    /// A provider at `base_url` whose operation is `/responses` under it and
    /// `/v1/responses` in a log.
    fn new(base_url: &str, limits: Limits) -> Self {
        let key = key_header(AUTHORIZATION, true, &SecretString::from(KEY)).expect("a legal key");
        let endpoint = BaseUrl::parse(base_url)
            .expect("a legal base URL")
            .endpoint("/responses", "/v1/responses")
            .expect("an endpoint");
        Self { endpoint, headers: request_headers(&key), key, limits }
    }

    fn exchange(&self) -> Exchange<'_> {
        Exchange {
            vendor: "OpenAI",
            endpoint: &self.endpoint,
            headers: &self.headers,
            key: &self.key,
            limits: self.limits,
        }
    }

    /// One attempt over `service`.
    async fn post<S: HttpService>(
        &self,
        service: &S,
    ) -> Result<Result<String, NonAnswer>, SdkError> {
        post(service, self.exchange(), Bytes::from_static(br#"{"input":"hello"}"#)).await
    }
}

/// A five-second deadline and a limit of `max_response_bytes`, or the
/// default limit.
fn limits(max_response_bytes: Option<usize>) -> Limits {
    Limits::new(Some(Duration::from_secs(5)), max_response_bytes).expect("legal limits")
}

fn transport() -> Transport {
    Transport::new(Vec::new()).expect("the default transport builds")
}

/// A server that answers every request with `status` and `body`.
async fn answering(status: StatusCode, body: impl Into<Bytes>) -> TestServer {
    let body = body.into();
    TestServer::start(Protocol::Http1, move |_| {
        let body = body.clone();
        async move { json_response(status, body) }
    })
    .await
    .expect("a loopback server")
}

/// How often `needle` occurs in `text`.
fn occurrences(text: &str, needle: &str) -> usize {
    text.matches(needle).count()
}

/// How often the key occurs in every rendering of `error` and of each link
/// of its `source()` chain.
fn key_occurrences(error: &(dyn StdError + 'static)) -> usize {
    let mut count = 0;
    let mut link = Some(error);
    while let Some(current) = link {
        for text in [current.to_string(), format!("{current:?}"), format!("{current:#?}")] {
            count += occurrences(&text, KEY);
        }
        link = current.source();
    }
    count
}

// ------------------------------------------------------------ the key header

#[test]
fn key_header_sensitive() {
    let secret = SecretString::from(KEY);

    let bearer = key_header(AUTHORIZATION, true, &secret).expect("a legal key");
    assert!(bearer.value.is_sensitive());
    assert_eq!(bearer.value.as_bytes(), format!("Bearer {KEY}").as_bytes());
    assert_eq!(bearer.name, AUTHORIZATION);

    let bare =
        key_header(HeaderName::from_static("x-api-key"), false, &secret).expect("a legal key");
    assert!(bare.value.is_sensitive());
    assert_eq!(bare.value.as_bytes(), KEY.as_bytes());

    // Neither the key header nor the header map built from it prints the key.
    for text in [
        format!("{bearer:?}"),
        format!("{bare:?}"),
        format!("{:?}", request_headers(&bearer)),
        format!("{:?}", bearer.value),
    ] {
        assert_eq!(occurrences(&text, KEY), 0, "{text}");
    }
    assert_eq!(format!("{bearer:?}"), r#"KeyHeader { name: "authorization", .. }"#);
}

#[test]
fn key_header_illegal_byte() {
    let secret = SecretString::from("sk-first\nsk-second");

    for bearer in [true, false] {
        let error = key_header(AUTHORIZATION, bearer, &secret).expect_err("a line feed is illegal");
        assert!(matches!(error.kind(), ErrorKind::Config));
        assert_eq!(
            error.to_string(),
            "The API key holds a character that cannot be sent in an HTTP header."
        );
        for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
            assert_eq!(occurrences(&text, "sk-first"), 0, "{text}");
            assert_eq!(occurrences(&text, "sk-second"), 0, "{text}");
        }
        assert!(error.source().is_none());
    }
}

#[test]
fn an_empty_key_is_refused() {
    let error = key_header(AUTHORIZATION, true, &SecretString::from("")).expect_err("no key");
    assert!(matches!(error.kind(), ErrorKind::Config));
    assert_eq!(error.to_string(), "The API key is empty.");
}

#[test]
fn a_key_is_found_in_the_forms_debug_writes_it_in() {
    // A quote, a backslash and a tab are among the characters `Debug` of a
    // string and of a header value write differently from the key itself.
    let key = "ab\"cd\\ef\tgh";
    let header = key_header(AUTHORIZATION, true, &SecretString::from(key)).expect("a legal key");

    assert!(header.occurs_in(&format!("the header was {key}")));
    assert!(header.occurs_in(&format!("{key:?}")));
    assert!(header.occurs_in(&format!("{:?}", String::from(key))));
    let unflagged = HeaderValue::from_str(key).expect("a legal value");
    assert!(header.occurs_in(&format!("{unflagged:?}")));
    assert!(header.occurs_in(&format!("{:?}", format!("Bearer {key}"))));

    assert!(!header.occurs_in("ab\"cd"));
    assert!(!header.occurs_in(""));
}

#[test]
fn the_request_headers_are_json_the_user_agent_and_the_key() {
    let key =
        key_header(HeaderName::from_static("x-goog-api-key"), false, &SecretString::from(KEY))
            .expect("a legal key");
    let headers = request_headers(&key);

    assert_eq!(headers.len(), 4);
    assert_eq!(headers[CONTENT_TYPE], "application/json");
    assert_eq!(headers[ACCEPT], "application/json");
    assert_eq!(
        headers[USER_AGENT],
        concat!("typesafe-sdk-rust-adapter/", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(headers["x-goog-api-key"], KEY);
    assert!(headers["x-goog-api-key"].is_sensitive());
}

// ------------------------------------------------- base URL and log URI

/// The `Config` error `base_url` is refused with, after checking that no
/// rendering of it repeats a part of the URL.
#[track_caller]
fn refused(base_url: &str, parts: &[&str]) -> String {
    let error = BaseUrl::parse(base_url).expect_err("the base URL is refused");
    assert!(matches!(error.kind(), ErrorKind::Config));
    for text in [error.to_string(), format!("{error:?}"), format!("{error:#?}")] {
        assert_eq!(occurrences(&text, base_url), 0, "{text}");
        for part in parts {
            assert_eq!(occurrences(&text, part), 0, "{text}");
        }
    }
    error.to_string()
}

#[test]
fn http_base_url_with_userinfo_is_refused() {
    assert_eq!(
        refused("https://alice:hunter2@proxy.example/v1", &["alice", "hunter2", "proxy.example"]),
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
}

#[test]
fn http_base_url_with_a_fragment_is_refused() {
    assert_eq!(
        refused("https://proxy.example/v1#token-9f2", &["token-9f2", "proxy.example"]),
        "The base URL must not carry a fragment ('#...')."
    );
}

#[test]
fn http_base_url_with_a_query_is_refused() {
    assert_eq!(
        refused("https://proxy.example/v1?api_key=k-77", &["api_key", "k-77", "proxy.example"]),
        "The base URL must not carry a query ('?...')."
    );
}

#[test]
fn a_base_url_that_is_relative_or_not_for_the_web_is_refused() {
    assert_eq!(refused("not a url", &["not a url"]), "The base URL is not a valid URL.");
    assert_eq!(
        refused("/chat", &[]),
        "The base URL must be absolute, with a scheme and a host, \
         such as https://api.example.com/v1."
    );
    assert_eq!(
        refused("ftp://files.example/v1", &["files.example"]),
        "The base URL must use http or https."
    );
}

#[test]
fn http_log_uri_has_no_query_and_no_userinfo() {
    let base = BaseUrl::parse("https://api.openai.com/v1").expect("a legal base URL");
    let endpoint = base.endpoint("/responses", "/v1/responses").expect("an endpoint");
    let log = endpoint.log_uri();

    assert_eq!(log.query(), None);
    let authority = log.authority().expect("an authority").as_str();
    assert!(!authority.contains('@'), "{authority}");
    assert_eq!(log.to_string(), "https://api.openai.com/v1/responses");
    assert_eq!(endpoint.wire.to_string(), "https://api.openai.com/v1/responses");
    assert_eq!(base.host(), "api.openai.com");

    // A port is named only when it is not the scheme's own.
    for (base_url, expected) in [
        ("https://api.openai.com:443/v1", "https://api.openai.com/v1/responses"),
        ("http://127.0.0.1:80", "http://127.0.0.1/v1/responses"),
        ("http://127.0.0.1:8080/", "http://127.0.0.1:8080/v1/responses"),
        ("https://proxy.example:8443", "https://proxy.example:8443/v1/responses"),
    ] {
        let endpoint = BaseUrl::parse(base_url)
            .expect("a legal base URL")
            .endpoint("/responses", "/v1/responses")
            .expect("an endpoint");
        assert_eq!(endpoint.log_uri().to_string(), expected, "{base_url}");
        assert_eq!(endpoint.log_uri().query(), None);
    }
}

#[test]
fn http_log_uri_leaves_out_a_path_prefix() {
    let base = BaseUrl::parse("https://proxy.example:8443/tenant-4b1e/secret-path/v1/")
        .expect("a legal base URL");
    let endpoint = base.endpoint("/chat/completions", "/v1/chat/completions").expect("an endpoint");

    // The request goes to the caller's prefix; the log names the vendor's
    // fixed path and nothing of the prefix.
    assert_eq!(
        endpoint.wire.to_string(),
        "https://proxy.example:8443/tenant-4b1e/secret-path/v1/chat/completions"
    );
    let log = endpoint.log_uri().to_string();
    assert_eq!(log, "https://proxy.example:8443/v1/chat/completions");
    assert_eq!(occurrences(&log, "tenant-4b1e"), 0);
    assert_eq!(occurrences(&log, "secret-path"), 0);

    // Neither `Debug` shows the prefix.
    for text in [format!("{base:?}"), format!("{endpoint:?}")] {
        assert_eq!(occurrences(&text, "tenant-4b1e"), 0, "{text}");
        assert_eq!(occurrences(&text, "secret-path"), 0, "{text}");
    }
}

// ----------------------------------------------------------------- limits

#[test]
fn response_cap_default() {
    assert_eq!(DEFAULT_MAX_RESPONSE_BYTES, 16 * 1024 * 1024);
    let limits = Limits::new(None, None).expect("the defaults are legal");
    assert_eq!(limits.max_response_bytes, 16 * 1024 * 1024);
    assert_eq!(limits.timeout, Duration::from_secs(600));

    let limits = Limits::new(Some(Duration::from_secs(3)), Some(1)).expect("legal limits");
    assert_eq!(limits.max_response_bytes, 1);
    assert_eq!(limits.timeout, Duration::from_secs(3));
}

#[test]
fn response_cap_zero() {
    let error = Limits::new(None, Some(0)).expect_err("a limit of zero bytes");
    assert!(matches!(error.kind(), ErrorKind::Config));
    assert_eq!(
        error.to_string(),
        "max_response_bytes must be at least 1: every response carries a body."
    );
}

#[test]
fn a_zero_timeout_is_refused() {
    let error = Limits::new(Some(Duration::ZERO), None).expect_err("a zero timeout");
    assert!(matches!(error.kind(), ErrorKind::Config));
    assert_eq!(error.to_string(), "timeout must be greater than zero.");
}

/// A JSON string of exactly `bytes` bytes, quotes included.
fn json_string(bytes: usize) -> String {
    format!("\"{}\"", "a".repeat(bytes - 2))
}

#[tokio::test]
async fn response_cap_exact() {
    let body = json_string(64);
    assert_eq!(body.len(), 64);
    let server = answering(StatusCode::OK, body.clone()).await;
    let fixture = Fixture::new(server.base_url(), limits(Some(64)));

    let reply = fixture
        .post(&transport())
        .await
        .expect("a body of exactly the limit is read")
        .expect("it is JSON");

    assert_eq!(reply, body);
}

#[tokio::test]
async fn response_cap_plus_one() {
    let body = json_string(65);
    assert_eq!(body.len(), 65);
    let server = answering(StatusCode::OK, body).await;
    let fixture = Fixture::new(server.base_url(), limits(Some(64)));

    let error = fixture.post(&transport()).await.expect_err("one byte over the limit");

    assert!(matches!(error.kind(), SdkErrorKind::ResponseTooLarge { limit: 64 }), "{error:?}");
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_failure_status_over_the_limit_is_still_an_api_error() {
    let server = answering(StatusCode::BAD_GATEWAY, json_string(65)).await;
    let fixture = Fixture::new(server.base_url(), limits(Some(64)));

    let error = fixture.post(&transport()).await.expect_err("a failure status");

    let SdkErrorKind::Api(api) = error.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(api.body(), b"", "a body over the limit is not kept");
    assert_eq!(api.kind(), ApiErrorKind::InternalServer);
}

// ----------------------------------------------------- status, deadline, connect

/// Sends one request to a server answering `status` with upstream's body and
/// checks the API error it becomes.
async fn status_case(status: u16, kind: ApiErrorKind) {
    const BODY: &str = r#"{"error":{"message":"boom"}}"#;
    let status = StatusCode::from_u16(status).expect("a status");
    let server = answering(status, BODY).await;
    let fixture = Fixture::new(server.base_url(), limits(None));

    let error = fixture.post(&transport()).await.expect_err("a failure status");

    let SdkErrorKind::Api(api) = error.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), status);
    assert_eq!(api.body(), BODY.as_bytes());
    assert_eq!(api.kind(), kind);
    assert_eq!(api.message(), "boom");
    assert_eq!(api.headers()[CONTENT_TYPE], "application/json");
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_400_is_a_bad_request() {
    status_case(400, ApiErrorKind::BadRequest).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_401_is_an_authentication_error() {
    status_case(401, ApiErrorKind::Authentication).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_403_is_a_permission_error() {
    status_case(403, ApiErrorKind::PermissionDenied).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_429_is_a_rate_limit_error() {
    status_case(429, ApiErrorKind::RateLimit).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_500_is_a_server_error() {
    status_case(500, ApiErrorKind::InternalServer).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_status_errors_map_and_preserve_status_and_body
async fn http_status_418_is_another_api_error() {
    status_case(418, ApiErrorKind::Other).await;
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_timeout_and_connection_errors_map
async fn http_deadline_passing_is_a_timeout() {
    let server = SilentServer::start().await.expect("a silent server");
    let deadline = Duration::from_millis(100);
    let fixture = Fixture::new(
        &format!("http://{}", server.addr()),
        Limits::new(Some(deadline), None).expect("legal limits"),
    );

    let error = fixture.post(&transport()).await.expect_err("the server never answers");

    let SdkErrorKind::Timeout { timeout } = error.kind() else {
        panic!("expected a timeout, got {error:?}");
    };
    assert_eq!(*timeout, deadline);
}

#[tokio::test]
// Upstream: tests/utils/test_error_handling.py::test_timeout_and_connection_errors_map
async fn http_connect_refused_is_a_connection_error() {
    let port = RefusingPort::new().await.expect("a refusing port");
    let fixture = Fixture::new(port.base_url(), limits(None));

    let error = fixture.post(&transport()).await.expect_err("the connect is refused");

    assert!(matches!(error.kind(), SdkErrorKind::Connection), "{error:?}");
    let message = error.to_string();
    assert!(message.starts_with("Connection error: "), "{message}");
    assert!(error.source().is_some(), "the transport's error is the source");
    assert_eq!(key_occurrences(&error), 0);
}

#[tokio::test]
async fn http_redirect_is_not_followed() {
    let server = TestServer::start(Protocol::Http1, |_| async {
        let mut response = json_response(StatusCode::FOUND, "{}");
        response.headers_mut().insert(LOCATION, HeaderValue::from_static("/v1/responses"));
        response
    })
    .await
    .expect("a loopback server");
    let fixture = Fixture::new(server.base_url(), limits(None));

    let error = fixture.post(&transport()).await.expect_err("a redirect is a failure status");

    let SdkErrorKind::Api(api) = error.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.status(), StatusCode::FOUND);
    assert_eq!(api.headers()[LOCATION], "/v1/responses");
    assert_eq!(server.request_count(), 1, "the redirect was followed");
}

#[tokio::test]
async fn http_non_json_success_body_is_a_non_answer() {
    let server = answering(StatusCode::OK, "<html>upstream gateway page</html>").await;
    let fixture = Fixture::new(server.base_url(), limits(None));

    let non_answer = fixture
        .post(&transport())
        .await
        .expect("a success status is not an exchange failure")
        .expect_err("the body is not JSON");

    let message = non_answer.to_string();
    assert_eq!(message, "OpenAI did not answer: status 200 with a body that is not JSON");
    assert_eq!(occurrences(&message, "gateway"), 0, "the body is not quoted");
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn a_request_carries_the_key_the_headers_and_the_body() {
    let server = answering(StatusCode::OK, r#"{"output":"hi"}"#).await;
    let fixture = Fixture::new(&format!("{}/team-a/v1", server.base_url()), limits(None));

    let reply = fixture.post(&transport()).await.expect("it answers").expect("it is JSON");

    assert_eq!(reply, r#"{"output":"hi"}"#);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, Method::POST);
    assert_eq!(request.uri.path(), "/team-a/v1/responses");
    assert_eq!(request.uri.query(), None);
    assert_eq!(request.header_values("authorization"), [format!("Bearer {KEY}")]);
    assert_eq!(request.header_values("content-type"), ["application/json"]);
    assert_eq!(request.header_values("accept"), ["application/json"]);
    assert_eq!(
        request.header_values("user-agent"),
        [concat!("typesafe-sdk-rust-adapter/", env!("CARGO_PKG_VERSION"))]
    );
    assert_eq!(request.body, Bytes::from_static(br#"{"input":"hello"}"#));
}

#[tokio::test]
async fn a_success_body_that_is_not_utf8_or_is_empty_is_a_non_answer() {
    for body in [Bytes::from_static(b"\"\xff\""), Bytes::new(), Bytes::from_static(b"{} {}")] {
        let service = Scripted::answering(StatusCode::OK, vec![body]);
        let fixture = Fixture::new("http://scripted.invalid", limits(None));

        let non_answer = fixture
            .post(&service)
            .await
            .expect("a success status")
            .expect_err("the body is not one JSON value");

        assert_eq!(
            non_answer.to_string(),
            "OpenAI did not answer: status 200 with a body that is not JSON"
        );
    }
}

// ------------------------------------------------- a caller's own service

/// What a scripted service does with a request.
#[derive(Clone)]
enum Script {
    /// Fails the call with the error `fn` builds from the text of the
    /// request's `authorization` header.
    Fail(fn(&str) -> BoxError),
    /// Answers with `status` and a body that declares no length and arrives
    /// in these frames; a frame of `None` is a read error.
    Answer(StatusCode, Vec<Option<Bytes>>),
}

/// A service of the caller's own, as `build_with_service` takes one.
#[derive(Clone)]
struct Scripted(Script);

impl Scripted {
    fn answering(status: StatusCode, frames: Vec<Bytes>) -> Self {
        Self(Script::Answer(status, frames.into_iter().map(Some).collect()))
    }
}

impl Service<Request<Body>> for Scripted {
    type Response = Response<Frames>;
    type Error = BoxError;
    type Future = Ready<Result<Response<Frames>, BoxError>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ready(match &self.0 {
            Script::Fail(build) => {
                // A sensitive value prints `Sensitive`; a service that wants
                // the header's text reads its bytes, as this one does.
                let header = request
                    .headers()
                    .get(AUTHORIZATION)
                    .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                    .unwrap_or_default();
                Err(build(&header))
            }
            Script::Answer(status, frames) => {
                let mut response = Response::new(Frames(frames.iter().cloned().collect()));
                *response.status_mut() = *status;
                Ok(response)
            }
        })
    }
}

/// A response body that declares no length.
struct Frames(VecDeque<Option<Bytes>>);

impl http_body::Body for Frames {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        Poll::Ready(self.get_mut().0.pop_front().map(|frame| {
            frame.map(Frame::data).ok_or_else(|| io::Error::other("the stream was reset"))
        }))
    }
}

/// An error of a caller's service: `Display` prints `display`, the derived
/// `Debug` prints every field.
#[derive(Debug)]
struct ServiceError {
    display: String,
    debug_only: String,
    source: Option<Box<ServiceError>>,
}

impl ServiceError {
    fn new(display: &str, debug_only: &str, source: Option<ServiceError>) -> Self {
        Self {
            display: display.into(),
            debug_only: debug_only.into(),
            source: source.map(Box::new),
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl StdError for ServiceError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|source| source as &(dyn StdError + 'static))
    }
}

/// An error that shows nothing of the error below it: only a walk of the
/// `source()` chain finds what that one holds.
struct Opaque(ServiceError);

impl fmt::Display for Opaque {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("request failed")
    }
}

impl fmt::Debug for Opaque {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Opaque")
    }
}

impl StdError for Opaque {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}

#[tokio::test]
async fn http_foreign_error_holding_the_key_is_withheld() {
    // The header in `Display`; only in `Debug`; in the `Debug` of a link
    // below the top; only in the `Display` of a link the top does not print.
    let leaks: [fn(&str) -> BoxError; 4] = [
        |header| Box::new(Opaque(ServiceError::new(&format!("sent {header}"), "", None))),
        |header| Box::new(ServiceError::new(&format!("request failed: {header}"), "", None)),
        |header| Box::new(ServiceError::new("request failed", header, None)),
        |header| {
            let inner = ServiceError::new("tls alert", header, None);
            Box::new(ServiceError::new("request failed", "", Some(inner)))
        },
    ];
    let fixture = Fixture::new("http://scripted.invalid", limits(None));

    for leak in leaks {
        // The scripted error does hold the key, so the check below is not vacuous.
        assert!(key_occurrences(&*leak(&format!("Bearer {KEY}"))) > 0);

        let error = fixture
            .post(&Scripted(Script::Fail(leak)))
            .await
            .expect_err("the service fails the call");

        assert!(matches!(error.kind(), SdkErrorKind::Connection), "{error:?}");
        assert_eq!(
            error.to_string(),
            "Connection error: the transport's error held the API key and is not shown."
        );
        assert!(error.source().is_none(), "the chain is dropped whole");
        assert_eq!(key_occurrences(&error), 0);
        // And as the adapter's own error, which a caller sees.
        assert_eq!(key_occurrences(&Error::provider(error)), 0);
    }

    // A foreign error without the key is kept: its messages are the message,
    // and the chain is the source.
    let harmless: fn(&str) -> BoxError = |_| {
        let inner = ServiceError::new("tls alert", "", None);
        Box::new(ServiceError::new("request failed", "", Some(inner)))
    };
    let error = fixture
        .post(&Scripted(Script::Fail(harmless)))
        .await
        .expect_err("the service fails the call");

    assert!(matches!(error.kind(), SdkErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: request failed: tls alert");
    let source = error.source().expect("the chain is kept");
    assert!(source.downcast_ref::<ServiceError>().is_some());
    assert_eq!(key_occurrences(&error), 0);
}

#[tokio::test]
async fn an_sdk_error_a_service_raises_is_passed_through() {
    let timeout: fn(&str) -> BoxError = |_| Box::new(SdkError::timeout(Duration::from_secs(7)));
    let fixture = Fixture::new("http://scripted.invalid", limits(None));

    let error =
        fixture.post(&Scripted(Script::Fail(timeout))).await.expect_err("the service fails");

    assert!(
        matches!(error.kind(), SdkErrorKind::Timeout { timeout } if timeout.as_secs() == 7),
        "{error:?}"
    );
}

#[test]
fn a_chain_too_long_to_search_counts_as_holding_the_key() {
    fn chain(links: usize) -> ServiceError {
        let mut error = ServiceError::new("link", "", None);
        for _ in 1..links {
            error = ServiceError::new("link", "", Some(error));
        }
        error
    }
    let key = key_header(AUTHORIZATION, true, &SecretString::from(KEY)).expect("a legal key");

    assert!(!key.holds_key(&chain(MAX_SCANNED_LINKS)));
    assert!(key.holds_key(&chain(MAX_SCANNED_LINKS + 1)));
    assert_eq!(connection(chain(MAX_SCANNED_LINKS + 1), &key).to_string(), WITHHELD);
    assert!(connection(chain(2), &key).source().is_some());
}

#[test]
fn a_key_that_only_the_escaped_message_spells_is_withheld() {
    /// An error whose `Debug` is its `Display`, unescaped.
    struct Raw(&'static str);

    impl fmt::Display for Raw {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl fmt::Debug for Raw {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl StdError for Raw {}

    // No rendering of the link holds the key `a\tb` (backslash, `t`): the
    // link holds a real tab, which the message writes as that escape.
    let key = key_header(AUTHORIZATION, true, &SecretString::from("a\\tb")).expect("a legal key");
    let error = Raw("failed near a\tb");
    assert!(!key.holds_key(&error));

    assert_eq!(connection(error, &key).to_string(), WITHHELD);
}

#[tokio::test]
async fn a_body_that_declares_no_length_is_cut_at_the_limit() {
    let frames = vec![Bytes::from_static(b"\"aaaaaaaa"), Bytes::from_static(b"aaaaaaaa\"")];
    let fixture = Fixture::new("http://scripted.invalid", limits(Some(17)));

    // 18 bytes against a limit of 17.
    let over = fixture
        .post(&Scripted::answering(StatusCode::OK, frames.clone()))
        .await
        .expect_err("the body is over the limit");
    assert!(matches!(over.kind(), SdkErrorKind::ResponseTooLarge { limit: 17 }), "{over:?}");

    let failed = fixture
        .post(&Scripted::answering(StatusCode::SERVICE_UNAVAILABLE, frames.clone()))
        .await
        .expect_err("a failure status");
    let SdkErrorKind::Api(api) = failed.kind() else {
        panic!("expected an API error, got {failed:?}");
    };
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.body(), b"");

    // The same 18 bytes against a limit of 18 are read.
    let fixture = Fixture::new("http://scripted.invalid", limits(Some(18)));
    let reply = fixture
        .post(&Scripted::answering(StatusCode::OK, frames))
        .await
        .expect("the body fits")
        .expect("it is JSON");
    assert_eq!(reply, "\"aaaaaaaaaaaaaaaa\"");
}

#[tokio::test]
async fn a_body_that_cannot_be_read_is_a_connection_error() {
    let service =
        Scripted(Script::Answer(StatusCode::OK, vec![Some(Bytes::from_static(b"{")), None]));
    let fixture = Fixture::new("http://scripted.invalid", limits(None));

    let error = fixture.post(&service).await.expect_err("the body read fails");

    assert!(matches!(error.kind(), SdkErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: the stream was reset");
    assert!(error.source().is_some());
}

// ------------------------------------------------------------ message text

#[test]
fn a_connection_message_names_at_most_eight_escaped_links() {
    let mut error = ServiceError::new("l9", "", None);
    for index in (0..9).rev() {
        error = ServiceError::new(&format!("l{index}"), "", Some(error));
    }
    assert_eq!(connection_message(&error), "Connection error: l0: l1: l2: l3: l4: l5: l6: l7");

    // A control character and a character that reorders text are escaped; a
    // backslash and a non-ASCII letter are written as they are.
    let error = ServiceError::new("a\u{1b}[31mb\nc\u{202e}d\\e\u{e9}", "", None);
    assert_eq!(
        connection_message(&error),
        "Connection error: a\\u{1b}[31mb\\nc\\u{202e}d\\e\u{e9}"
    );
}

#[test]
fn a_connection_message_is_cut_at_200_characters_without_splitting_an_escape() {
    let long = ServiceError::new(&"x".repeat(300), "", None);
    let message = connection_message(&long);
    assert_eq!(message, format!("Connection error: {}\u{2026}", "x".repeat(200)));

    // 198 characters, then an escape of two: it fits whole. One more
    // character before it, and the escape is left out whole.
    let fits = ServiceError::new(&format!("{}\n", "x".repeat(198)), "", None);
    assert_eq!(connection_message(&fits), format!("Connection error: {}\\n", "x".repeat(198)));
    let split = ServiceError::new(&format!("{}\nyz", "x".repeat(199)), "", None);
    assert_eq!(
        connection_message(&split),
        format!("Connection error: {}\u{2026}", "x".repeat(199))
    );
}

#[test]
fn a_non_answer_names_the_vendor_and_escapes_and_cuts_the_reason() {
    assert_eq!(
        non_answer("Gemini", "stop reason MAX_TOKENS").to_string(),
        "Gemini did not answer: stop reason MAX_TOKENS"
    );
    // A backslash is doubled here: the reason is text the vendor chose.
    assert_eq!(
        non_answer("Anthropic", "stop reason a\\b\u{202e}c\td").to_string(),
        "Anthropic did not answer: stop reason a\\\\b\\u{202e}c\\td"
    );
    assert_eq!(
        non_answer("OpenAI", &"r".repeat(201)).to_string(),
        format!("OpenAI did not answer: {}\u{2026}", "r".repeat(200))
    );
    assert_eq!(
        non_answer("OpenAI", &"r".repeat(200)).to_string(),
        format!("OpenAI did not answer: {}", "r".repeat(200))
    );
}

// ------------------------------------------------------------------ events

#[cfg(feature = "tracing")]
#[tokio::test]
async fn http_event_holds_method_log_uri_status_and_elapsed() {
    use tracing::Level;

    let server = TestServer::start(Protocol::Http1, |request| async move {
        let status = if request.body.is_empty() { StatusCode::OK } else { StatusCode::IM_A_TEAPOT };
        json_response(status, r#"{"ok":true}"#)
    })
    .await
    .expect("a loopback server");
    // A caller's path prefix, which the event must not name.
    let fixture = Fixture::new(&format!("{}/team-a/v1", server.base_url()), limits(None));
    let service = transport();
    let recorder = recorder::Recorder::default();
    let installed = recorder::install(&recorder);

    post(&service, fixture.exchange(), Bytes::new())
        .await
        .expect("it answers")
        .expect("it is JSON");
    fixture.post(&service).await.expect_err("a failure status");
    drop(installed);

    let events = recorder.at(Level::DEBUG);
    assert_eq!(events.len(), 2, "{events:?}");
    let uri = format!("http://127.0.0.1:{}/v1/responses", server.addr().port());
    for (line, fields) in events.iter().zip([
        format!(" method=POST uri={uri} status=200 elapsed_ms="),
        format!(" method=POST uri={uri} status=418 error=\"Api\" elapsed_ms="),
    ]) {
        let elapsed = line
            .strip_prefix(fields.as_str())
            .unwrap_or_else(|| panic!("{line:?} does not start with {fields:?}"));
        assert!(!elapsed.is_empty() && elapsed.bytes().all(|byte| byte.is_ascii_digit()), "{line}");
    }
    for level in [Level::TRACE, Level::DEBUG, Level::INFO, Level::WARN, Level::ERROR] {
        for line in recorder.at(level) {
            assert_eq!(occurrences(&line, KEY), 0, "{line}");
            assert_eq!(occurrences(&line, "Bearer"), 0, "{line}");
            assert_eq!(occurrences(&line, "team-a"), 0, "{line}");
        }
    }
}

// ------------------------------------------------------ the default transport

#[tokio::test]
async fn the_transport_speaks_http2_over_tls_to_a_root_that_was_added() {
    let server = TestServer::start(Protocol::Http2Tls, |_| async {
        json_response(StatusCode::OK, r#"{"ok":true}"#)
    })
    .await
    .expect("a loopback server");
    let root = server.certificate_der().expect("a TLS server has a certificate").to_vec();
    let fixture = Fixture::new(server.base_url(), limits(None));

    let trusting = Transport::new(vec![root]).expect("the transport builds");
    let reply = fixture.post(&trusting).await.expect("the added root is trusted").expect("JSON");
    assert_eq!(reply, r#"{"ok":true}"#);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].version, Version::HTTP_2);
    assert_eq!(format!("{trusting:?}"), "Transport { extra_roots: 1 }");

    // Without the added root the certificate is unknown, and nothing is sent.
    let error = fixture.post(&transport()).await.expect_err("an unknown certificate");
    assert!(matches!(error.kind(), SdkErrorKind::Connection), "{error:?}");
    assert_eq!(server.request_count(), 1);
    assert_eq!(key_occurrences(&error), 0);
}

#[test]
fn a_root_that_is_not_a_certificate_is_a_config_error() {
    let error = Transport::new(vec![b"not a certificate".to_vec()]).expect_err("not DER");
    assert!(matches!(error.kind(), ErrorKind::Config));
    let message = error.to_string();
    assert!(message.starts_with("The TLS certificate verifier could not be built: "), "{message}");
}

#[test]
fn the_transport_types_and_an_attempt_cross_threads() {
    fn assert_service<S: HttpService>() {}
    fn assert_send<T: Send>() {}
    fn assert_send_value<T: Send>(_: &T) {}

    // A provider boxes the attempt's future as `Send`.
    let fixture = Fixture::new("http://scripted.invalid", limits(None));
    let service = transport();
    assert_send_value(&fixture.post(&service));

    assert_service::<Transport>();
    assert_send::<TransportFuture>();
    assert_send::<TransportBody>();
    assert_eq!(format!("{:?}", transport()), "Transport { extra_roots: 0 }");
}

// ------------------------------------------------------------- environment

/// A test build never reads the process environment: with the `internals`
/// feature a variable comes from a replacement only, and this test holds
/// none; without the feature every variable is unset.
#[test]
fn a_test_build_does_not_read_the_process_environment() {
    // `PATH` is set on every machine that runs cargo.
    assert!(std::env::var_os("PATH").is_some());

    assert_eq!(env_var("PATH").expect("unset"), None, "the process environment was read");
}

#[cfg(feature = "internals")]
#[test]
fn the_environment_is_read_through_its_replacement() {
    use crate::__internals::env;

    // `PATH` is set on every machine that runs cargo.
    assert!(std::env::var_os("PATH").is_some());

    let replaced = env::replace();
    assert_eq!(env_var("PATH").expect("unset"), None, "the process environment is not read");
    assert_eq!(env_var("OPENAI_API_KEY").expect("unset"), None);

    replaced.set("OPENAI_API_KEY", "first-test-key");
    assert_eq!(env_var("OPENAI_API_KEY").expect("set").as_deref(), Some("first-test-key"));
    replaced.set("OPENAI_API_KEY", "second-test-key");
    assert_eq!(env_var("OPENAI_API_KEY").expect("set").as_deref(), Some("second-test-key"));

    // An empty variable is an unset one.
    replaced.set("OPENAI_API_KEY", "");
    assert_eq!(env_var("OPENAI_API_KEY").expect("empty"), None);

    replaced.set("OPENAI_BASE_URL", "https://first.invalid/v1");
    replaced.remove("OPENAI_BASE_URL");
    assert_eq!(env_var("OPENAI_BASE_URL").expect("unset"), None);

    // When the replacement ends its variables are gone, and the process
    // environment is still not read.
    replaced.set("OPENAI_API_KEY", "third-test-key");
    drop(replaced);
    assert_ne!(env_var("OPENAI_API_KEY").expect("unset").as_deref(), Some("third-test-key"));
    assert_eq!(env_var("PATH").expect("unset"), None, "the process environment was read");
    let replaced = env::replace();

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;

        replaced.set("ANTHROPIC_API_KEY", std::ffi::OsString::from_vec(vec![b's', b'k', 0xff]));
        let error = env_var("ANTHROPIC_API_KEY").expect_err("not UTF-8");
        assert!(matches!(error.kind(), ErrorKind::Config));
        assert_eq!(
            error.to_string(),
            "The ANTHROPIC_API_KEY environment variable is not valid UTF-8."
        );
    }
}

#[cfg(feature = "internals")]
#[test]
fn a_second_replacement_waits_for_the_first_to_end() {
    use std::sync::mpsc;

    use crate::__internals::env;

    let first = env::replace();
    first.set("GEMINI_API_KEY", "first-test-key");
    let (sender, receiver) = mpsc::channel();
    let second = std::thread::spawn(move || {
        let second = env::replace();
        // A replacement starts empty: nothing of the first is left.
        sender.send(env_var("GEMINI_API_KEY").expect("unset")).expect("the test waits");
        drop(second);
    });

    assert!(
        receiver.recv_timeout(Duration::from_millis(100)).is_err(),
        "the second replacement began while the first was in force"
    );
    drop(first);
    assert_eq!(receiver.recv_timeout(Duration::from_secs(5)).expect("it began"), None);
    second.join().expect("the thread ends");
}
