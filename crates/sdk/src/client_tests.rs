//! Tests for building a client. None of them reads the process environment:
//! the builder is given a lookup of its own.

#[cfg(feature = "hyper")]
use std::ffi::OsString;

use super::*;
use crate::ErrorKind;

#[cfg(feature = "hyper")]
#[test]
fn client_builder_new_is_client_builder() {
    assert_eq!(format!("{:?}", ClientBuilder::new()), format!("{:?}", Client::builder()));
    assert_eq!(format!("{:?}", ClientBuilder::new()), format!("{:?}", ClientBuilder::default()));
}

#[cfg(feature = "tracing")]
#[path = "../tests/support/recorder.rs"]
mod recorder;

#[cfg(feature = "tracing")]
#[tokio::test]
async fn log_endpoint_host_false_prints_the_api_path_alone_over_a_custom_service() {
    use std::{
        future::{Ready, ready},
        io,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };

    use http::{Method, Request, Response, StatusCode};
    use tracing::Level;

    use self::recorder::{Recorder, assert_timed, install};
    use crate::Body;

    #[derive(Clone)]
    struct Answering(Arc<AtomicUsize>);

    impl tower_service::Service<Request<Body>> for Answering {
        type Response = Response<Body>;
        type Error = io::Error;
        type Future = Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<Body>) -> Self::Future {
            let attempt = self.0.fetch_add(1, Ordering::SeqCst) + 1;
            let path = if request.method() == Method::POST { "systemone" } else { "models" };
            assert_eq!(request.uri(), format!("http://127.0.0.1:9/prefix/v1/{path}").as_str());
            let (status, body): (_, &'static [u8]) = match attempt {
                1 => (StatusCode::SERVICE_UNAVAILABLE, b"{}"),
                2 => (StatusCode::OK, br#"{"models":[]}"#),
                3 => (StatusCode::NOT_FOUND, br#"{"message":"gone"}"#),
                4 => (StatusCode::OK, include_bytes!("../tests/fixtures/result.json")),
                5 => {
                    return ready(Err(io::Error::new(io::ErrorKind::ConnectionRefused, "offline")));
                }
                other => panic!("unexpected attempt {other}"),
            };
            let mut response = Response::new(Body::from(Bytes::from_static(body)));
            *response.status_mut() = status;
            ready(Ok(response))
        }
    }

    let recorder = Recorder::default();
    let _installed = install(&recorder);
    let calls = Arc::new(AtomicUsize::new(0));
    let builder = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9/prefix")
        .default_model("jev-latest")
        .log_endpoint_host(false)
        .retry(RetryPolicy::new().backoff_initial(Duration::ZERO));
    assert!(format!("{builder:?}").contains("log_endpoint_host: false"));
    let client =
        builder.build_with_service(Answering(Arc::clone(&calls))).expect("the client builds");
    let endpoints = client.shared().config.endpoints();
    assert_eq!(endpoints.models_log(), "/v1/models");
    assert_eq!(endpoints.system_one_log(), "/v1/systemone");
    assert_eq!(
        format!("{:?}", ClientBuilder::default().log_endpoint_host(false).log_endpoint_host(true)),
        format!("{:?}", ClientBuilder::default()),
        "a later setter call restores the default"
    );

    let response = client.models().list().send().await.expect("the retry succeeds");
    assert!(response.models().is_empty());
    let error = client.models().list().send().await.expect_err("the next call is not found");
    assert_eq!(error.to_string(), "GET http://127.0.0.1:9/prefix/v1/models: 404 gone");
    let ErrorKind::Api(api) = error.kind() else { panic!("expected an API error: {error:?}") };
    assert_eq!(api.endpoint(), Some("GET http://127.0.0.1:9/prefix/v1/models"));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let info = recorder.at(Level::INFO);
    assert_eq!(info.len(), 4, "{info:#?}");
    assert_timed(&info[0], "message=GET /v1/models <- 503 in ", " (request -)");
    assert_eq!(info[1], "message=GET /v1/models retry 1");
    assert_timed(&info[2], "message=GET /v1/models <- 200 in ", " (request -)");
    assert_timed(&info[3], "message=GET /v1/models <- 404 in ", " (request -)");

    // Exercise the POST request-body event and the connection-failure field too.
    let questions =
        crate::Questions::new().noul("q", crate::Noul::new()).prepare().expect("prepares");
    client.system_one("state", &questions).send().await.expect("the POST succeeds");
    let error =
        client.models().list().retry(RetryPolicy::none()).send().await.expect_err("offline");
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    assert_eq!(error.to_string(), "Connection error: offline");
    let info = recorder.at(Level::INFO);
    assert_eq!(info.len(), 6, "{info:#?}");
    assert_timed(&info[4], "message=POST /v1/systemone <- 200 in ", " (request -)");
    assert_eq!(info[5], "message=GET /v1/models <- connection error");
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    for level in [Level::INFO, Level::DEBUG, Level::TRACE] {
        let lines = recorder.at(level);
        assert!(!lines.is_empty(), "{level}: expected captured events");
        for line in lines {
            for hidden in ["127.0.0.1", "http://", "prefix"] {
                assert!(!line.contains(hidden), "{level}: {hidden} reached {line}");
            }
            if level != Level::INFO {
                let path =
                    if line.contains("method=POST") { "/v1/systemone" } else { "/v1/models" };
                assert!(line.contains(&format!("endpoint={path}")), "{level}: {line}");
            }
        }
    }
}

/// An environment with none of the SDK's variables.
#[cfg(feature = "hyper")]
fn empty(_: &str) -> Option<String> {
    None
}

/// The config error `builder` fails to build with, rendered.
#[cfg(feature = "hyper")]
fn config_error(builder: ClientBuilder) -> String {
    match builder.build_with_env(empty) {
        Ok(client) => panic!("expected a config error, built {client:?}"),
        Err(error) => {
            assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
            error.to_string()
        }
    }
}

/// An in-memory transport for configuration tests that never send a request.
#[derive(Debug, Clone)]
struct InMemory;

impl tower_service::Service<http::Request<crate::Body>> for InMemory {
    type Response = http::Response<crate::Body>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<crate::Body>) -> Self::Future {
        std::future::ready(Ok(http::Response::new(request.into_body())))
    }
}

#[cfg(feature = "hyper")]
#[test]
fn settings_the_builder_leaves_unset_come_from_the_environment() {
    let environment = |name: &str| match name {
        "TYPESAFE_API_KEY" => Some(OsString::from("env-key")),
        "TYPESAFE_BASE_URL" => Some(OsString::from("http://127.0.0.1:9/prefix/")),
        "TYPESAFE_DEFAULT_MODEL" => Some(OsString::from("env-model")),
        _ => None,
    };
    let client =
        ClientBuilder::new().build_with_env(environment).expect("the environment is complete");
    let shared = client.shared();

    assert_eq!(shared.config.endpoints().system_one(), "http://127.0.0.1:9/prefix/v1/systemone");
    assert_eq!(shared.config.default_model(), "env-model");
    assert_eq!(shared.post_headers["authorization"], "Bearer env-key");
    assert_eq!(&shared.model_json[..], b"\"env-model\"");
}

#[cfg(feature = "hyper")]
#[test]
fn the_default_version_is_http2_only_for_https_and_auto_for_http_and_a_choice_wins() {
    let rows = [
        ("https://127.0.0.1:9", None, HttpVersion::Http2Only),
        ("http://127.0.0.1:9", None, HttpVersion::Auto),
        ("https://127.0.0.1:9", Some(HttpVersion::Auto), HttpVersion::Auto),
        ("http://127.0.0.1:9", Some(HttpVersion::Http2Only), HttpVersion::Http2Only),
    ];
    for (base_url, chosen, expected) in rows {
        let mut builder = ClientBuilder::new().api_key("test-key").base_url(base_url);
        if let Some(version) = chosen {
            builder = builder.http_version(version);
        }
        let client = builder.build_with_env(empty).expect("it builds");
        let transport = format!("{:?}", client.shared().service);
        assert!(
            transport.contains(&format!("http_version: {expected:?},")),
            "{base_url} with {chosen:?}: {transport}"
        );
    }
}

#[cfg(feature = "hyper")]
#[test]
fn a_custom_transport_refuses_the_settings_only_the_default_one_has() {
    type Configure = fn(ClientBuilder) -> ClientBuilder;
    // The verb agrees with the number of settings named: one setting
    // `configures`, two or more `configure`.
    let rows: [(Configure, &str); 5] = [
        (|builder| builder.add_root_certificate(vec![1, 2, 3]), "add_root_certificate configures"),
        (|builder| builder.http_version(HttpVersion::Http2Only), "http_version configures"),
        (|builder| builder.connect_timeout(Duration::from_secs(1)), "connect_timeout configures"),
        (
            |builder| {
                builder.connect_timeout(Duration::from_secs(1)).http_version(HttpVersion::Auto)
            },
            "http_version, connect_timeout configure",
        ),
        (
            |builder| {
                builder
                    .connect_timeout(Duration::from_secs(1))
                    .add_root_certificate(vec![1])
                    .http_version(HttpVersion::Auto)
            },
            "add_root_certificate, http_version, connect_timeout configure",
        ),
    ];
    for (configure, named) in rows {
        let builder = configure(ClientBuilder::new().api_key("test-key"));
        let error = builder.build_with_service(InMemory).expect_err("it must be refused");
        assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
        assert_eq!(
            error.to_string(),
            format!(
                "{named} the default transport, and a client built with \
                 build_with_service has a transport of its own."
            )
        );
    }

    // The same settings are accepted by the default transport.
    ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .http_version(HttpVersion::Http2Only)
        .connect_timeout(Duration::from_secs(1))
        .build_with_env(empty)
        .expect("the default transport takes them");
}

#[cfg(feature = "hyper")]
#[test]
fn a_default_header_that_cannot_be_sent_is_refused_without_its_value() {
    let secret = "sk-live-do-not-log";
    let rows = [
        (
            ("x team", "fine"),
            r#"The default header name "x team" is not a valid HTTP header name."#,
        ),
        (
            ("x-token", "sk-live-do-not-log\nInjected: yes"),
            r#"The value of the default header "x-token" is not a valid HTTP header value."#,
        ),
    ];
    for ((name, value), message) in rows {
        let rendered =
            config_error(ClientBuilder::new().api_key("test-key").default_header(name, value));
        assert_eq!(rendered, message);
        assert!(!rendered.contains(secret));
    }
}

#[cfg(feature = "hyper")]
#[test]
fn a_zero_connect_timeout_is_refused() {
    let rendered =
        config_error(ClientBuilder::new().api_key("test-key").connect_timeout(Duration::ZERO));
    assert_eq!(rendered, "connect_timeout must be a positive number of seconds.");
}

#[cfg(feature = "hyper")]
#[test]
fn a_later_default_header_of_a_name_replaces_an_earlier_one() {
    let client = ClientBuilder::new()
        .api_key("test-key")
        .default_header("X-Team", "first")
        .default_header("x-team", "second")
        .build_with_env(empty)
        .expect("it builds");
    let values: Vec<_> = client.shared().get_headers.get_all("x-team").iter().collect();
    assert_eq!(values, ["second"]);
}

// ---------------------------------------------------------------- Debug

#[cfg(feature = "hyper")]
const KEY: &str = "sk-live-0123456789-DO-NOT-LOG";

#[cfg(feature = "hyper")]
#[test]
fn a_builder_prints_what_was_set_and_nothing_secret() {
    let builder = ClientBuilder::new()
        .api_key(KEY)
        .base_url("https://example.test/prefix/")
        .default_model("jev-latest")
        .timeout(Duration::from_secs(3))
        .default_header("x-client-secret", "client-secret-value")
        .default_header("x-team", "team-value")
        .max_response_bytes(1024)
        .add_root_certificate(vec![0x30, 0x82])
        .http_version(HttpVersion::Auto)
        .connect_timeout(Duration::from_secs(1));

    assert_eq!(
        format!("{builder:?}"),
        concat!(
            "ClientBuilder { api_key: Some(<redacted>), base_url: Some([",
            r#""POST https://example.test/prefix/v1/systemone", "#,
            r#""GET https://example.test/prefix/v1/models"]), "#,
            r#"default_model: Some("jev-latest"), timeout: Some(Some(3s)), "#,
            r#"default_headers: ["x-client-secret", "x-team"], "#,
            "max_response_bytes: Some(1024), extra_roots: 1, http_version: Some(Auto), ",
            "connect_timeout: Some(1s) }",
        )
    );
    assert_eq!(
        format!("{:?}", ClientBuilder::default()),
        concat!(
            "ClientBuilder { api_key: None, base_url: None, default_model: None, timeout: None, ",
            "default_headers: [], max_response_bytes: None, extra_roots: 0, http_version: None, ",
            "connect_timeout: None }",
        )
    );
}

#[test]
fn a_builder_does_not_print_a_base_url_that_failed_its_checks() {
    for url in
        ["https://user:hunter2@example.test", "https://example.test/?token=hunter2", "not a url"]
    {
        let debug = format!("{:?}", ClientBuilder::new().base_url(url));
        assert!(debug.contains("base_url: Some(<not a usable URL>)"), "{url}: {debug}");
        assert!(!debug.contains("hunter2"), "{url}: {debug}");
    }
}

#[cfg(feature = "hyper")]
#[test]
fn a_client_prints_its_settings_and_transport_and_nothing_secret() {
    let client = ClientBuilder::new()
        .api_key(KEY)
        .base_url("https://example.test:443/prefix")
        .default_header("x-client-secret", "client-secret-value")
        .no_timeout()
        .build_with_env(empty)
        .expect("it builds");

    let debug = format!("{client:?}");
    assert_eq!(
        debug,
        concat!(
            r#"Client { config: Config { endpoints: ["POST https://example.test/prefix/v1/systemone", "#,
            r#""GET https://example.test/prefix/v1/models"], default_model: "jev-latest", "#,
            r#"timeout: None, max_response_bytes: 16777216, authorization: <redacted>, "#,
            r#"default_headers: ["x-client-secret"] }, "#,
            "transport: HyperTransport { http_version: Http2Only, extra_roots: 0, ",
            "connect_timeout: None } }",
        )
    );
    for secret in [KEY, "sk-live", "Bearer", "client-secret-value"] {
        assert!(!debug.contains(secret), "{secret} leaked into {debug}");
    }
}

#[cfg(feature = "hyper")]
#[test]
fn a_clone_shares_one_client() {
    let client = ClientBuilder::new().api_key("test-key").build_with_env(empty).expect("it builds");
    let clone = client.clone();
    assert!(std::ptr::eq(client.shared(), clone.shared()));
}

// ---------------------------------------------- User-Agent and runtime

/// `User-Agent` of a client's requests, both kinds, which must agree.
#[cfg(feature = "hyper")]
fn user_agent<S>(client: &Client<S>) -> String {
    let shared = client.shared();
    assert_eq!(shared.get_headers["user-agent"], shared.post_headers["user-agent"]);
    shared.get_headers["user-agent"].to_str().expect("ASCII").to_owned()
}

/// Whether both kinds of request of a client carry `X-TypeSafe-Runtime`.
#[cfg(feature = "hyper")]
fn sends_runtime<S>(client: &Client<S>) -> bool {
    let shared = client.shared();
    let get = shared.get_headers.contains_key("x-typesafe-runtime");
    assert_eq!(get, shared.post_headers.contains_key("x-typesafe-runtime"));
    get
}

/// Like every other setter, a later call replaces an earlier one: a refused
/// product followed by a good one builds, a good one followed by a refused
/// one does not, and the last runtime switch decides.
#[cfg(feature = "hyper")]
#[test]
fn a_later_user_agent_product_or_runtime_switch_replaces_an_earlier_one() {
    let sdk = format!("typesafe-sdk-rust/{}", env!("CARGO_PKG_VERSION"));
    let client = ClientBuilder::new()
        .api_key("test-key")
        .user_agent_product("not a token")
        .user_agent_product("app/2")
        .build_with_env(empty)
        .expect("the last product is a token");
    assert_eq!(user_agent(&client), format!("app/2 {sdk}"));

    let rendered = config_error(
        ClientBuilder::new()
            .api_key("test-key")
            .user_agent_product("app/2")
            .user_agent_product("app"),
    );
    assert_eq!(
        rendered,
        "The user_agent_product must be a product token, name/version \
         (RFC 9110, section 10.1.5): it has no '/' between the name and the version."
    );

    let rows = [(vec![false], false), (vec![false, true], true), (vec![true, false], false)];
    for (switches, expected) in rows {
        let mut builder = ClientBuilder::new().api_key("test-key");
        for send in &switches {
            builder = builder.send_runtime_header(*send);
        }
        let client = builder.build_with_env(empty).expect("it builds");
        assert_eq!(sends_runtime(&client), expected, "switches {switches:?}");
        assert_eq!(user_agent(&client), sdk, "switches {switches:?}");
    }
}

/// A product that is not a token fails `build` and `build_with_service` with
/// the same config error, and neither repeats it.
#[test]
fn a_user_agent_product_that_is_not_a_token_fails_either_way_of_building() {
    let product = "app/1.0\r\nX-Injected: yes";
    let message = "The user_agent_product must be a product token, name/version \
                   (RFC 9110, section 10.1.5): it contains whitespace.";

    #[cfg(feature = "hyper")]
    let built = config_error(ClientBuilder::new().api_key("test-key").user_agent_product(product));
    #[cfg(feature = "hyper")]
    assert_eq!(built, message);

    let error = ClientBuilder::new()
        .api_key("test-key")
        .user_agent_product(product)
        .build_with_service(InMemory)
        .expect_err("it must be refused");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert_eq!(error.to_string(), message);
    for rendered in [
        #[cfg(feature = "hyper")]
        built,
        error.to_string(),
        format!("{error:?}"),
    ] {
        crate::rendering_tests::assert_printable(&rendered);
        assert!(!rendered.contains("X-Injected"), "{rendered}");
    }
}

/// The product is printed quoted and escaped, since before `build` it can
/// hold anything; a runtime header switched off is printed, one left on is
/// not.
#[cfg(feature = "hyper")]
#[test]
fn a_builder_prints_its_user_agent_product_escaped_and_a_runtime_header_switched_off() {
    let builder =
        ClientBuilder::new().user_agent_product("app\u{1b}[31m/1").send_runtime_header(false);
    let debug = format!("{builder:?}");
    assert!(
        debug.ends_with(
            r#"connect_timeout: None, user_agent_product: "app\u{1b}[31m/1", send_runtime_header: false }"#
        ),
        "{debug}"
    );
    crate::rendering_tests::assert_printable(&debug);

    assert_eq!(
        format!("{:?}", ClientBuilder::new().send_runtime_header(true)),
        format!("{:?}", ClientBuilder::default())
    );
}
