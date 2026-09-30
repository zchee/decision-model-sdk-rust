//! The models resource against a real loopback server: ports of the models
//! cases of the upstream `tests/test_clients.py` (upstream has no
//! `test_models.py`), and `warm_up`, which is a models call.

use http::StatusCode;
use test_support::{Protocol, TestServer};
use typesafe_sdk::{ApiErrorKind, Client, ErrorKind};

const MODELS: &[u8] = include_bytes!("fixtures/models.json");
const MODELS_EXTRA_FIELDS: &[u8] = include_bytes!("fixtures/models-extra-fields.json");

#[path = "support/answering.rs"]
mod answering;
#[path = "support/loopback_builder.rs"]
mod loopback_builder;

use answering::answering;
use loopback_builder::loopback_builder;

fn client_for(server: &TestServer, protocol: Protocol) -> Client {
    loopback_builder(server, protocol).build().expect("the client builds")
}

/// Upstream `test_models_shape`: a `GET` of `/v1/models` with no body, and
/// the cards it answers with.
#[tokio::test]
async fn models_shape() {
    for protocol in Protocol::ALL {
        let server = answering(protocol, StatusCode::OK, MODELS).await;
        let response = client_for(&server, protocol).models().list().send().await.expect("listed");

        let [request] = &server.requests()[..] else { panic!("{protocol:?}: one request") };
        assert_eq!(request.method, "GET", "{protocol:?}");
        assert_eq!(request.uri.path(), "/v1/models");
        assert!(request.body.is_empty(), "{protocol:?}: a GET carries no body");
        assert!(request.headers.get("content-type").is_none(), "{protocol:?}");

        let [model] = response.models() else { panic!("{protocol:?}: one card") };
        assert_eq!(model.name(), "jev-latest");
        assert_eq!(model.description(), "Fast model");
        assert_eq!(model.release_date(), "2026-08-01");
        assert_eq!(response.meta().status(), StatusCode::OK);
    }
}

/// Upstream `test_models_ignore_unknown_fields`: members a card does not
/// model are dropped from it and stay in the raw body.
#[tokio::test]
async fn models_ignore_unknown_fields() {
    let server = answering(Protocol::Http1, StatusCode::OK, MODELS_EXTRA_FIELDS).await;
    let response =
        client_for(&server, Protocol::Http1).models().list().send().await.expect("listed");

    assert_eq!(response.models().len(), 1);
    assert_eq!(response.models()[0].name(), "jev-latest");
    let raw: serde_json::Value =
        serde_json::from_slice(response.meta().raw_body()).expect("the raw body is JSON");
    assert_eq!(raw["models"][0]["context_window"], 128_000);
}

/// Upstream `test_invalid_models_response[None, {}, bad, missing field]`: a
/// body of the wrong shape is a response-validation error naming the field.
/// A `null` body fails at `.` (README deviation row "A body that is not an
/// object fails at path `''`").
#[tokio::test]
async fn invalid_models_response() {
    let rows: [(&'static [u8], &str); 4] = [
        (b"null", "."),
        (b"{}", "models"),
        (br#"{"models":"bad"}"#, "models"),
        (br#"{"models":[{"name":"x"}]}"#, "models[0].description"),
    ];
    for (body, path) in rows {
        let server = answering(Protocol::Http1, StatusCode::OK, body).await;
        let error = client_for(&server, Protocol::Http1)
            .models()
            .list()
            .send()
            .await
            .expect_err("the body does not fit");
        let ErrorKind::ResponseValidation(failure) = error.kind() else {
            panic!(
                "{}: expected a response-validation error, got {error:?}",
                String::from_utf8_lossy(body)
            )
        };
        assert_eq!(failure.field_path(), path, "{}", String::from_utf8_lossy(body));
        assert_eq!(failure.status(), StatusCode::OK);
        assert_eq!(failure.body(), body);
        assert_eq!(
            error.to_string(),
            format!("GET {}/v1/models: 200 Invalid response data at '{path}'.", server.base_url())
        );
    }
}

/// `warm_up` lists the models once: it succeeds on a key the API accepts,
/// and fails as the API's own answer on one it refuses - the live API
/// answers a missing key with 403 and `authentication_error`.
#[tokio::test]
async fn warm_up_lists_the_models_and_reports_a_refused_key() {
    let server = answering(Protocol::Http1, StatusCode::OK, MODELS).await;
    client_for(&server, Protocol::Http1).warm_up().await.expect("the key is accepted");
    assert_eq!(server.requests()[0].uri.path(), "/v1/models");

    const REFUSED: &[u8] = br#"{"detail":{"error_type":"authentication_error","message":"Must supply an API key! Check your request and try again."}}"#;
    let server = answering(Protocol::Http1, StatusCode::FORBIDDEN, REFUSED).await;
    let error = client_for(&server, Protocol::Http1).warm_up().await.expect_err("refused");
    let ErrorKind::Api(api) = error.kind() else { panic!("expected an API error: {error:?}") };
    assert_eq!(api.kind(), ApiErrorKind::PermissionDenied);
    assert_eq!(api.error_type(), Some("authentication_error"));
    assert_eq!(
        error.to_string(),
        format!(
            "GET {}/v1/models: 403 Must supply an API key! Check your request and try again.",
            server.base_url()
        )
    );
}

/// A per-call header on a models request replaces a client default, the
/// SDK's own headers win over both, and `Content-Type` is the caller's to
/// set on a request without a body.
#[tokio::test]
async fn a_models_call_takes_headers_of_its_own() {
    let server = answering(Protocol::Http1, StatusCode::OK, MODELS).await;
    let client = Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_header("x-team", "default")
        .build()
        .expect("the client builds");
    client
        .models()
        .list()
        .header("x-team", "call")
        .header("accept", "text/plain")
        .header("content-type", "text/plain")
        .send()
        .await
        .expect("listed");

    let request = &server.requests()[0];
    assert_eq!(request.headers["x-team"], "call");
    assert_eq!(request.headers["accept"], "application/json");
    assert_eq!(request.headers["content-type"], "text/plain");

    let error = client
        .models()
        .list()
        .header("x-token", "sk-live-secret\n")
        .send()
        .await
        .expect_err("not a header value");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    assert_eq!(
        error.to_string(),
        r#"The value of the header "x-token" is not a valid HTTP header value."#
    );
    assert_eq!(server.request_count(), 1, "an invalid header never reaches the network");
}
