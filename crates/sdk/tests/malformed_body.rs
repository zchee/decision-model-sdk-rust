//! A response body that is not UTF-8, through the public API over a real
//! connection.
//!
//! JSON text is UTF-8, and a body that is not is a server's or a proxy's
//! mistake, not the caller's: every call here must come back as an `Err` the
//! caller can inspect - an API error keeping its status, headers and bytes for
//! a failure status, a response-validation error for a success status - and
//! never as a panic. The bodies are the ones the fuzzer's finding reduced to,
//! one per decode path it reached inside the codec.

use std::time::Duration;

use http::{StatusCode, header::RETRY_AFTER};
use test_support::{Protocol, TestServer, json_response};
use typesafe_sdk::{
    ApiErrorKind, Client, DecodeErrorKind, ErrorKind, Questions, RetryPolicy, Score,
};

/// An error body whose one member holds a byte that is not UTF-8: the error
/// reader keeps each member as raw text, so the codec must check that the
/// bytes are UTF-8 before it reads them as text.
const ERROR_BODY: &[u8] = b"{\"error\":\"\xC9\"}";

/// A success body whose score legend holds an object with such a byte: the
/// answer decoder keeps a structured legend value as raw text.
const SUCCESS_BODY: &[u8] = b"{\"model\":\"jev-latest\",\"usage\":{},\"answers\":{\"urgency\":\
    {\"type\":\"score\",\"score\":0,\"confidence\":1,\"legend\":{\"0\":{\"a\":\"\xC9\"}},\
    \"probabilities\":{\"0\":1}}}}";

/// A models list whose one card has such a byte in its name.
const MODELS_BODY: &[u8] = b"{\"models\":[{\"name\":\"jev-\xC9\",\"description\":\"d\",\
    \"release_date\":\"2026-08-01\"}]}";

/// A server answering every request with `status`, `body` and a
/// `Retry-After` of three seconds.
async fn answering_with_retry_after(
    protocol: Protocol,
    status: StatusCode,
    body: &'static [u8],
) -> TestServer {
    TestServer::start(protocol, move |_| async move {
        let mut response = json_response(status, body);
        response.headers_mut().insert(RETRY_AFTER, "3".parse().expect("valid"));
        response
    })
    .await
    .expect("the test server starts")
}

#[path = "support/loopback_builder.rs"]
mod loopback_builder;

use loopback_builder::loopback_builder;

/// A client of `server` that makes one attempt per call.
fn client_for(server: &TestServer, protocol: Protocol) -> Client {
    loopback_builder(server, protocol)
        .retry(RetryPolicy::default().max_retries(0))
        .build()
        .expect("the client builds")
}

fn questions() -> typesafe_sdk::PreparedQuestions {
    Questions::new()
        .score("urgency", Score::new(["can wait"]))
        .prepare()
        .expect("the questions are valid")
}

#[tokio::test]
async fn a_failure_status_with_a_body_that_is_not_utf8_is_an_api_error() {
    for protocol in Protocol::ALL {
        let server =
            answering_with_retry_after(protocol, StatusCode::UNPROCESSABLE_ENTITY, ERROR_BODY)
                .await;
        let client = client_for(&server, protocol);
        let questions = questions();

        let failure =
            client.system_one("ticket", &questions).send().await.expect_err("a 422 is an error");
        println!("{protocol:?}: {failure}");
        let ErrorKind::Api(error) = failure.kind() else {
            panic!("{protocol:?}: expected an API error, got {failure:?}");
        };
        assert_eq!(error.status(), StatusCode::UNPROCESSABLE_ENTITY, "{protocol:?}");
        assert_eq!(error.kind(), ApiErrorKind::UnprocessableEntity, "{protocol:?}");
        assert_eq!(error.body(), ERROR_BODY, "{protocol:?}: the body is kept byte for byte");
        assert_eq!(error.message(), "{\"error\":\"\u{FFFD}\"}", "{protocol:?}");
        assert_eq!(error.error_type(), None, "{protocol:?}");
        let wait = error.retry_after().expect("the Retry-After header is still read");
        assert_eq!(wait, Duration::from_secs(3), "{protocol:?}");
        assert!(
            failure.to_string().ends_with("/v1/systemone: 422 {\"error\":\"\u{FFFD}\"}"),
            "{protocol:?}: {failure}"
        );
        assert_eq!(server.request_count(), 1, "{protocol:?}: one attempt, no retry");
    }
}

#[tokio::test]
async fn a_success_status_with_a_body_that_is_not_utf8_is_a_validation_error() {
    for protocol in Protocol::ALL {
        let server = answering_with_retry_after(protocol, StatusCode::OK, SUCCESS_BODY).await;
        let client = client_for(&server, protocol);
        let questions = questions();

        let failure = client
            .system_one("ticket", &questions)
            .send()
            .await
            .expect_err("the body does not decode");
        println!("{protocol:?}: {failure}");
        let ErrorKind::ResponseValidation(error) = failure.kind() else {
            panic!("{protocol:?}: expected a response-validation error, got {failure:?}");
        };
        let column = 1 + SUCCESS_BODY.iter().position(|&byte| byte == 0xC9).expect("present");
        assert_eq!(error.status(), StatusCode::OK, "{protocol:?}");
        assert_eq!(error.field_path(), "", "{protocol:?}");
        assert_eq!(error.decode_error().kind(), DecodeErrorKind::Syntax, "{protocol:?}");
        assert_eq!(
            (error.decode_error().line(), error.decode_error().column()),
            (1, column),
            "{protocol:?}"
        );
        assert_eq!(error.body(), SUCCESS_BODY, "{protocol:?}: the body is kept byte for byte");
        assert!(
            failure.to_string().ends_with("/v1/systemone: 200 Invalid response data at ''."),
            "{protocol:?}: {failure}"
        );
    }
}

#[tokio::test]
async fn a_models_list_that_is_not_utf8_is_a_validation_error() {
    for protocol in Protocol::ALL {
        let server = answering_with_retry_after(protocol, StatusCode::OK, MODELS_BODY).await;
        let client = client_for(&server, protocol);

        let failure = client.models().list().send().await.expect_err("the body does not decode");
        println!("{protocol:?}: {failure}");
        let ErrorKind::ResponseValidation(error) = failure.kind() else {
            panic!("{protocol:?}: expected a response-validation error, got {failure:?}");
        };
        assert_eq!(error.decode_error().kind(), DecodeErrorKind::Syntax, "{protocol:?}");
        assert_eq!(error.body(), MODELS_BODY, "{protocol:?}");
    }
}
