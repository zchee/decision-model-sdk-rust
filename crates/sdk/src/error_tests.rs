//! Tests for the error types.
//!
//! The body cases are ports of the upstream Python SDK's `tests/test_errors.py`
//! and the `Retry-After` cases are ports of its `tests/test_retry.py`, so a
//! divergence from the SDK this one is a port of shows up as a failing test
//! rather than as a difference nobody looked for.

use std::{error::Error as StdError, time::UNIX_EPOCH};

use http::{HeaderName, HeaderValue};

use super::*;
use crate::{DecodeErrorKind, constants::REQUEST_ID_HEADER, rendering_tests::assert_printable};

// ------------------------------------------------------------- fixtures

/// The instant the `Retry-After` date cases measure against, chosen so the
/// dates in them are far from any real clock.
const NOW: Duration = Duration::from_secs(1_000_000);

/// The body the live API answers a request with no API key with.
const LIVE_403_BODY: &str = concat!(
    r#"{"detail":{"error_type":"authentication_error","message":"Must supply an API key! "#,
    r#"Check your request and try again."}}"#
);

fn at(offset: u64) -> SystemTime {
    UNIX_EPOCH + NOW + Duration::from_secs(offset)
}

fn now() -> SystemTime {
    UNIX_EPOCH + NOW
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes())
            .expect("invariant: the test names a real header");
        let value = HeaderValue::from_str(value)
            .expect("invariant: the test uses a printable header value");
        map.insert(name, value);
    }
    map
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).expect("invariant: the test uses a status in the valid range")
}

/// An API failure with no headers and no endpoint: the shape the message
/// cases care about.
fn api(code: u16, body: &str) -> ApiError {
    ApiError::new(status(code), Bytes::copy_from_slice(body.as_bytes()), HeaderMap::new(), None)
}

/// The message an API failure with this body reports.
fn message_of(body: &str) -> String {
    api(400, body).message().to_owned()
}

fn uri(text: &str) -> Uri {
    text.parse::<Uri>().expect("invariant: the test uses a URL the http crate accepts")
}

// --------------------------------------------------------- status mapping

#[test]
fn every_status_lands_in_the_class_the_python_sdk_puts_it_in() {
    let expected = [
        (400, ApiErrorKind::BadRequest),
        (401, ApiErrorKind::Authentication),
        (403, ApiErrorKind::PermissionDenied),
        (404, ApiErrorKind::NotFound),
        (422, ApiErrorKind::UnprocessableEntity),
        (429, ApiErrorKind::RateLimit),
        (500, ApiErrorKind::InternalServer),
        // 529 is not a status the `http` crate names, and it is still a
        // server failure: the rule is "at or above 500", not a list.
        (529, ApiErrorKind::InternalServer),
        (418, ApiErrorKind::Other),
    ];
    for (code, kind) in expected {
        let error = api(code, "{}");
        assert_eq!(error.kind(), kind, "status {code}");
        assert_eq!(error.status(), status(code), "status {code}");
    }
}

#[test]
fn is_authentication_holds_for_401_and_for_an_authentication_error_type_at_any_status() {
    let cases = [
        (401, "", true, ApiErrorKind::Authentication),
        (
            401,
            r#"{"detail":{"error_type":"permission_denied"}}"#,
            true,
            ApiErrorKind::Authentication,
        ),
        (403, LIVE_403_BODY, true, ApiErrorKind::PermissionDenied),
        (403, "{}", false, ApiErrorKind::PermissionDenied),
        (
            403,
            r#"{"detail":{"error_type":"permission_denied"}}"#,
            false,
            ApiErrorKind::PermissionDenied,
        ),
        (400, LIVE_403_BODY, true, ApiErrorKind::BadRequest),
        (500, LIVE_403_BODY, true, ApiErrorKind::InternalServer),
        (
            403,
            r#"{"detail":{"error_type":"Authentication_Error"}}"#,
            false,
            ApiErrorKind::PermissionDenied,
        ),
        (403, "<html><body>403 Forbidden</body></html>", false, ApiErrorKind::PermissionDenied),
        (401, "<html><body>403 Forbidden</body></html>", true, ApiErrorKind::Authentication),
        (403, r#"{"detail":{"error_type":1}}"#, false, ApiErrorKind::PermissionDenied),
        (403, r#"{"error_type":"authentication_error"}"#, false, ApiErrorKind::PermissionDenied),
    ];
    for (code, body, expected, kind) in cases {
        let error = api(code, body);
        assert_eq!(error.is_authentication(), expected, "status {code}, body {body:?}");
        assert_eq!(error.kind(), kind, "status {code}, body {body:?}");
    }
}

// ------------------------------------------------------ message extraction

#[test]
fn the_message_is_read_from_the_first_place_that_holds_a_string() {
    // Each body holds every member the ones before it hold, so the order the
    // members are read in is what decides the answer, not their presence.
    assert_eq!(
        message_of(r#"{"error":"from error","message":"from message","detail":"from detail"}"#),
        "from error"
    );
    assert_eq!(
        message_of(
            r#"{"error":{"message":"from error.message"},"message":"from message","detail":"from detail"}"#
        ),
        "from error.message"
    );
    assert_eq!(message_of(r#"{"message":"from message","detail":"from detail"}"#), "from message");
    assert_eq!(message_of(r#"{"detail":"from detail"}"#), "from detail");
    assert_eq!(
        message_of(r#"{"detail":{"message":"from detail.message"}}"#),
        "from detail.message"
    );
    assert_eq!(
        message_of(r#"{"detail":[{"loc":["body","state"],"msg":"field required"}]}"#),
        "state: field required"
    );
}

#[test]
fn a_member_of_the_wrong_type_is_skipped_rather_than_taken() {
    // `error` is a number, then an object without a string `message`, then an
    // object whose `message` is a number: none of the three is a message, and
    // each time the next place in the order answers instead.
    assert_eq!(message_of(r#"{"error":42,"message":"from message"}"#), "from message");
    assert_eq!(message_of(r#"{"error":{"code":7},"message":"from message"}"#), "from message");
    assert_eq!(message_of(r#"{"error":{"message":7},"message":"from message"}"#), "from message");
    assert_eq!(message_of(r#"{"message":["a"],"detail":"from detail"}"#), "from detail");
    assert_eq!(message_of(r#"{"detail":7,"message":"from message"}"#), "from message");
    // A member spelled `null` is absent, not present and empty.
    assert_eq!(message_of(r#"{"error":null,"message":"from message"}"#), "from message");
}

#[test]
fn a_detail_list_joins_its_entries_and_drops_the_body_segment() {
    let body = concat!(
        r#"{"detail":[{"loc":["body","questions","tone"],"msg":"field required"},"#,
        r#"{"loc":["body","state"],"msg":"must be a string"}]}"#
    );
    assert_eq!(message_of(body), "questions.tone: field required; state: must be a string");

    // An index in `loc` is a segment like any other, joined with a dot rather
    // than bracketed: this is the message the server wrote, not the SDK's own
    // field path.
    assert_eq!(
        message_of(r#"{"detail":[{"loc":["body","models",1,"name"],"msg":"bad"}]}"#),
        "models.1.name: bad"
    );
    // An entry with no usable `loc` keeps its message on its own.
    assert_eq!(message_of(r#"{"detail":[{"msg":"no location"}]}"#), "no location");
    assert_eq!(message_of(r#"{"detail":[{"loc":[],"msg":"empty location"}]}"#), "empty location");
    assert_eq!(
        message_of(r#"{"detail":[{"loc":"not a list","msg":"scalar location"}]}"#),
        "scalar location"
    );
    // Entries the shape does not fit are dropped, and the ones that fit still
    // answer.
    assert_eq!(message_of(r#"{"detail":[null,42,{"msg":4},{"msg":"kept"}]}"#), "kept");
    // A segment that is neither a field name nor an index is left out rather
    // than guessed at, and the segments around it still form the path.
    assert_eq!(
        message_of(r#"{"detail":[{"loc":["body",null,"tone",true],"msg":"bad"}]}"#),
        "tone: bad"
    );
}

#[test]
fn a_body_the_rules_do_not_recognize_becomes_the_message_itself() {
    // Ported from `test_error_body_edge_cases`, one row per body.
    assert_eq!(message_of(""), "status code (no body)");
    assert_eq!(message_of("null"), "status code (no body)");
    assert_eq!(message_of("[]"), "[]");
    assert_eq!(message_of("42"), "42");
    assert_eq!(message_of("true"), "true");
    // `error` is a string and so is taken, but an empty one is no message, so
    // the body answers for itself - and `message` is still not consulted.
    assert_eq!(
        message_of(r#"{"error":"","message":"ignored"}"#),
        r#"{"error":"","message":"ignored"}"#
    );
    assert_eq!(
        message_of(r#"{"detail":[null,42,{"msg":4}]}"#),
        r#"{"detail":[null,42,{"msg":4}]}"#
    );
    assert_eq!(message_of("{}"), "{}");
}

#[test]
fn a_body_that_is_not_json_is_reported_as_the_text_it_is() {
    let body = Bytes::from_static(b"not JSON: \xff");
    let error = ApiError::new(status(400), body, HeaderMap::new(), None);
    assert_eq!(
        error.message(),
        "not JSON: \u{fffd}",
        "a byte that is not UTF-8 becomes the replacement character"
    );
    assert_eq!(error.body(), b"not JSON: \xff", "the bytes themselves are kept whole");
    assert_eq!(error.body_text(), "not JSON: \u{fffd}");
}

#[test]
fn a_json_string_body_is_its_own_message_and_an_empty_one_leaves_the_status_alone() {
    assert_eq!(message_of(r#""plain text""#), "plain text");
    assert_eq!(message_of(r#""{\"looks\":\"like json\"}""#), r#"{"looks":"like json"}"#);
    let empty = api(400, r#""""#);
    assert_eq!(empty.message(), "");
    assert_eq!(empty.to_string(), "400", "an empty message leaves the status to stand alone");

    // Text that starts like a string and is not one is not JSON at all, so it
    // is reported as the text it is, whitespace and all.
    assert_eq!(message_of(r#""unterminated  "#), r#""unterminated  "#);
}

#[test]
fn a_long_body_used_as_its_own_message_is_cut_and_marked() {
    let long = format!(r#"{{"unknown":"{}"}}"#, "x".repeat(201));
    let message = message_of(&long);
    let expected = format!(r#"{{"unknown":"{}{}"#, "x".repeat(188), '\u{2026}');
    assert_eq!(message, expected);
    assert_eq!(
        message.chars().count(),
        201,
        "200 characters of body and the mark that says there was more"
    );

    // DIVERGENCE from the Python SDK, deliberate: there a body that is not
    // JSON is a message and is never cut, so a hostile endpoint can make an
    // arbitrarily long one. Here the cut is on every body used as its own
    // message, whether or not it parsed.
    let plain = "x".repeat(201);
    let cut_plain = message_of(&plain);
    assert_eq!(cut_plain, format!("{}\u{2026}", "x".repeat(200)));

    // DIVERGENCE from the Python SDK, deliberate: there a message the server
    // put in a member it named is never cut. Here it is cut like the body,
    // since a member of 16 MiB floods a log line exactly as a body does.
    let named = format!(r#"{{"message":"{}"}}"#, "y".repeat(500));
    assert_eq!(message_of(&named), format!("{}\u{2026}", "y".repeat(200)));
}

#[test]
fn a_body_is_compacted_but_not_re_encoded_when_it_becomes_the_message() {
    // Whitespace between tokens goes, so a pretty-printed body does not put
    // newlines in a one-line message; whitespace inside a string stays.
    assert_eq!(
        message_of("{\n  \"a\" : [ 1 , 2 ],\n  \"b\": \"two  words\"\n}"),
        r#"{"a":[1,2],"b":"two  words"}"#
    );
    // The numbers and escapes are the server's own, not this crate's rendering
    // of them: `1e2` does not become `100.0` and `A` does not become `A`.
    assert_eq!(message_of(r#"{"n": 1e2, "s": "A"}"#), r#"{"n":1e2,"s":"A"}"#);
    // A brace inside a string does not end the string, and an escaped quote
    // does not end it either.
    assert_eq!(message_of(r#"{"s": "a \" b { } c"}"#), r#"{"s":"a \" b { } c"}"#);
}

#[test]
fn a_body_nested_deeper_than_the_codec_reads_is_never_parsed() {
    // The depth guard runs before the parser, so no member of a body past it
    // is read - including one that would otherwise have answered.
    let deep =
        format!(r#"{{ "message": "hidden", "deep": {}{} }}"#, "[".repeat(40), "]".repeat(40));
    let error = api(400, &deep);
    assert_ne!(error.message(), "hidden", "a member behind the depth guard must not be read");
    // The body is still JSON - only deeper than this crate walks - so it is
    // compacted like any other body that answers for itself.
    assert_eq!(
        error.message(),
        format!(r#"{{"message":"hidden","deep":{}{}}}"#, "[".repeat(40), "]".repeat(40))
    );
    assert_eq!(error.error_type(), None);

    // Past the cut it is marked like any other over-long body.
    let padded =
        format!(r#"{{"pad":"{}","deep":{}{}}}"#, "z".repeat(300), "[".repeat(40), "]".repeat(40));
    assert!(api(400, &padded).message().ends_with('\u{2026}'));

    // Reading it back through the public accessor fails the same way, and the
    // failure says which rule refused it. The guard runs before the parser, so
    // the target type never gets a say in the answer.
    let failure = error.body_json::<bool>().expect_err("the body is past the depth guard");
    assert_eq!(failure.kind(), DecodeErrorKind::TooDeep);
}

#[test]
fn the_live_403_body_says_which_failure_it_is() {
    // The live API answers a request with no key with 403 and this body, not
    // with the 401 the documentation promises, so `error_type` is the only
    // thing that tells a missing key from a key without a permission.
    let error = ApiError::new(
        status(403),
        Bytes::from_static(LIVE_403_BODY.as_bytes()),
        headers(&[("x-typesafe-request-id", "req-live")]),
        Some("GET https://api.typesafe.ai/v1/models".into()),
    );
    assert_eq!(error.kind(), ApiErrorKind::PermissionDenied);
    assert_eq!(error.error_type(), Some("authentication_error"));
    assert_eq!(error.message(), "Must supply an API key! Check your request and try again.");
    assert_eq!(error.request_id(), Some("req-live"));
    assert_eq!(
        error.to_string(),
        concat!(
            "GET https://api.typesafe.ai/v1/models: 403 ",
            "Must supply an API key! Check your request and try again. (request_id=req-live)"
        )
    );

    // `error_type` is read whether or not the message came from beside it.
    let elsewhere =
        api(403, r#"{"message":"from message","detail":{"error_type":"authentication_error"}}"#);
    assert_eq!(elsewhere.message(), "from message");
    assert_eq!(elsewhere.error_type(), Some("authentication_error"));
    // And it is absent when the body says nothing about it.
    assert_eq!(api(403, r#"{"detail":{"error_type":7}}"#).error_type(), None);
    assert_eq!(api(403, r#"{"error_type":"top level"}"#).error_type(), None);
}

#[test]
fn a_caller_supplied_message_replaces_the_one_in_the_body() {
    // Ported from `test_message_override`.
    for (given, rendered) in [("A custom explanation", "429 A custom explanation"), ("", "429")] {
        let error = ApiError::with_message(
            status(429),
            Bytes::from_static(br#"{"message":"Server explanation"}"#),
            headers(&[("retry-after-ms", "125")]),
            None,
            given,
        );
        assert_eq!(error.message(), given);
        assert_eq!(error.to_string(), rendered);
        assert_eq!(error.status(), status(429));
        assert_eq!(error.request_id(), None);
        assert_eq!(parse_retry_after(error.headers(), now()), Some(Duration::from_millis(125)));
    }
}

// ---------------------------------------------------------------- rendering

#[test]
fn a_failure_renders_as_the_endpoint_the_status_the_message_and_the_request_id() {
    let error = ApiError::new(
        status(429),
        Bytes::from_static(br#"{"message":"Too many requests"}"#),
        headers(&[("x-typesafe-request-id", "req-context")]),
        Some("POST https://api.example.test/prefix/v1/systemone".into()),
    );
    assert_eq!(
        error.to_string(),
        "POST https://api.example.test/prefix/v1/systemone: 429 Too many requests (request_id=req-context)"
    );
    assert_eq!(error.request_id(), Some("req-context"));
    assert_eq!(error.endpoint(), Some("POST https://api.example.test/prefix/v1/systemone"));
    assert_eq!(
        error.headers().len(),
        1,
        "the headers arrive whole, for a caller this type does not serve"
    );
    assert_eq!(
        error.headers().get("x-typesafe-request-id").and_then(|value| value.to_str().ok()),
        Some("req-context")
    );

    // Each optional part disappears on its own.
    let no_endpoint = ApiError::new(
        status(429),
        Bytes::from_static(br#"{"message":"m"}"#),
        headers(&[("x-typesafe-request-id", "r")]),
        None,
    );
    assert_eq!(no_endpoint.to_string(), "429 m (request_id=r)");
    let no_request_id = ApiError::new(
        status(429),
        Bytes::from_static(br#"{"message":"m"}"#),
        HeaderMap::new(),
        Some("GET https://example.test/v1/models".into()),
    );
    assert_eq!(no_request_id.to_string(), "GET https://example.test/v1/models: 429 m");
    assert_eq!(api(500, "").to_string(), "500 status code (no body)");
}

#[test]
fn an_endpoint_drops_the_credentials_the_query_and_the_default_port() {
    // Ported from `test_api_error_endpoint_omits_url_credentials`.
    let endpoint = format_endpoint(
        &Method::GET,
        &uri("https://user:password@example.test/v1/models?token=secret#fragment"),
    );
    assert_eq!(endpoint, "GET https://example.test/v1/models");

    let error = ApiError::new(
        status(400),
        Bytes::from_static(br#"{"message":"Bad request"}"#),
        HeaderMap::new(),
        Some(endpoint.into()),
    );
    assert_eq!(error.to_string(), "GET https://example.test/v1/models: 400 Bad request");

    // A port that the scheme already implies is noise; one that does not is
    // part of the address.
    assert_eq!(
        format_endpoint(&Method::POST, &uri("https://example.test:443/v1/systemone")),
        "POST https://example.test/v1/systemone"
    );
    assert_eq!(
        format_endpoint(&Method::GET, &uri("http://example.test:80/v1/models")),
        "GET http://example.test/v1/models"
    );
    assert_eq!(
        format_endpoint(&Method::GET, &uri("https://example.test:8443/v1/models")),
        "GET https://example.test:8443/v1/models"
    );
    assert_eq!(
        format_endpoint(&Method::GET, &uri("http://127.0.0.1:9000/v1/models")),
        "GET http://127.0.0.1:9000/v1/models"
    );
    assert_eq!(
        format_endpoint(&Method::GET, &uri("http://[::1]:9000/v1/models")),
        "GET http://[::1]:9000/v1/models"
    );
    // `Uri::host` hands back an IPv6 literal still wrapped in its brackets,
    // and the brackets are what separate the address from a port, so dropping
    // a default port must not take them with it.
    assert_eq!(
        format_endpoint(&Method::GET, &uri("https://[::1]:443/v1/models")),
        "GET https://[::1]/v1/models"
    );
    assert_eq!(
        format_endpoint(&Method::GET, &uri("http://[::1]:80/v1/models")),
        "GET http://[::1]/v1/models"
    );
    // A scheme this crate knows no default port for keeps whatever port it was
    // given, and a target with no authority at all is just its path.
    assert_eq!(
        format_endpoint(&Method::GET, &uri("ftp://example.test:21/models")),
        "GET ftp://example.test:21/models"
    );
    assert_eq!(format_endpoint(&Method::GET, &uri("/v1/models")), "GET /v1/models");
}

#[test]
fn nothing_that_could_be_a_secret_reaches_a_debug_rendering() {
    let error = ApiError::new(
        status(401),
        Bytes::from_static(br#"{"message":"Invalid API key"}"#),
        headers(&[
            ("authorization", "Bearer sk-live-do-not-log-me"),
            ("x-api-key", "sk-also-secret"),
            ("set-cookie", "session=secret-session"),
        ]),
        Some("GET https://api.typesafe.ai/v1/models".into()),
    );
    let shown = format!("{error:?}");
    for secret in
        ["sk-live-do-not-log-me", "Bearer", "sk-also-secret", "secret-session", "authorization"]
    {
        assert!(!shown.contains(secret), "{secret:?} reached the Debug rendering: {shown}");
    }
    assert_eq!(
        shown,
        concat!(
            r#"ApiError { status: 401, kind: Authentication, "#,
            r#"endpoint: Some("GET https://api.typesafe.ai/v1/models"), request_id: None, "#,
            r#"message: "Invalid API key", error_type: None, headers: <3 redacted>, body: <29 bytes> }"#
        )
    );

    // The same holds one level up, where the failure is wrapped.
    let wrapped = Error::from(error);
    let shown = format!("{wrapped:?}");
    assert!(shown.starts_with("Error { kind: Api(ApiError { status: 401,"), "{shown}");
    assert!(!shown.contains("sk-live-do-not-log-me"), "{shown}");

    // And on the validation error, which reaches the same headers by a
    // different route and prints a decode failure beside them.
    let body = Bytes::from_static(br#"{"model":"jev-1","answers":{"spam":{}}}"#);
    let decode_error = codec::decode::<Fixture>(&body).expect_err("the fixture is missing a field");
    let invalid = ResponseValidationError::new(
        status(200),
        body,
        headers(&[("authorization", "Bearer sk-live-do-not-log-me"), ("cookie", "session=secret")]),
        None,
        decode_error,
    );
    let shown = format!("{invalid:?}");
    for secret in ["sk-live-do-not-log-me", "Bearer", "secret", "authorization", "cookie"] {
        assert!(!shown.contains(secret), "{secret:?} reached the Debug rendering: {shown}");
    }
    assert_eq!(
        shown,
        concat!(
            "ResponseValidationError { status: 200, endpoint: None, request_id: None, ",
            r#"field_path: "answers.spam.noul", "#,
            r#"source: DecodeError { detail: Data { path: "answers.spam.noul", line: 1, column: 37 } }, "#,
            "headers: <2 redacted>, body: <39 bytes> }"
        )
    );
}

// ------------------------------------------------- server text in a message

/// Asserts that neither rendering of `error`, nor of the [`Error`] wrapping
/// it, holds a character a log reader would act on, and returns its
/// `Display`.
fn assert_rendered_safely(error: &ApiError) -> String {
    let wrapped = Error::from(error.clone());
    for shown in
        [error.to_string(), format!("{error:?}"), wrapped.to_string(), format!("{wrapped:?}")]
    {
        assert_printable(&shown);
    }
    assert_eq!(wrapped.to_string(), error.to_string(), "wrapping does not change the sentence");
    error.to_string()
}

/// `text` as a JSON string, escapes and all, so that a body can carry any
/// character without the test source spelling a JSON escape.
fn json_string(text: &str) -> String {
    serde_json::to_string(text).expect("a string always serializes")
}

/// Text with a line break, a colour, a right-to-left override and a NUL, and
/// the message it must become: each written as a Rust escape.
const HOSTILE: &str = "a\nb\u{1b}[31mRED\u{202e}X\u{0}";
const HOSTILE_SHOWN: &str = r"a\nb\u{1b}[31mRED\u{202e}X\u{0}";

#[test]
fn every_message_read_from_a_body_is_escaped_wherever_it_came_from() {
    let hostile = json_string(HOSTILE);
    let rows: [(String, &str); 10] = [
        (format!(r#"{{"error":{hostile}}}"#), HOSTILE_SHOWN),
        (format!(r#"{{"error":{{"message":{hostile}}}}}"#), HOSTILE_SHOWN),
        (format!(r#"{{"message":{hostile}}}"#), HOSTILE_SHOWN),
        (format!(r#"{{"detail":{hostile}}}"#), HOSTILE_SHOWN),
        (format!(r#"{{"detail":{{"message":{hostile}}}}}"#), HOSTILE_SHOWN),
        // A detail list: the location and the message are both the server's.
        (
            format!(
                r#"{{"detail":[{{"loc":["body",{}],"msg":{}}}]}}"#,
                json_string("a\nb"),
                json_string("m\u{1b}n")
            ),
            r"a\nb: m\u{1b}n",
        ),
        // A JSON string body is its own message.
        (hostile.clone(), HOSTILE_SHOWN),
        // Text that is not JSON keeps its whitespace, escaped.
        ("oops\u{1b}[2J\nline2".to_owned(), r"oops\u{1b}[2J\nline2"),
        // A JSON body standing for itself already spells its escapes with a
        // backslash, and the backslash is kept rather than doubled.
        (r#"{"unknown":"a\nb"}"#.to_owned(), r#"{"unknown":"a\nb"}"#),
        // A raw tab inside a JSON string is not JSON; it is text, escaped.
        ("{\"unknown\":\"a\tb\"}".to_owned(), r#"{"unknown":"a\tb"}"#),
    ];
    for (body, shown) in rows {
        let error = api(500, &body);
        assert_eq!(error.message(), shown, "body {body:?}");
        assert_eq!(assert_rendered_safely(&error), format!("500 {shown}"), "body {body:?}");
        assert_eq!(error.body(), body.as_bytes(), "the body itself is kept whole");
        assert_eq!(error.body_text(), body);
    }
}

#[test]
fn every_message_read_from_a_body_is_cut_after_escaping_and_the_body_is_kept() {
    // 5,000 characters in a member: 200 of them and the mark.
    let error = api(500, &format!(r#"{{"error":"{}"}}"#, "x".repeat(5000)));
    assert_eq!(error.message(), format!("{}\u{2026}", "x".repeat(200)));
    assert_eq!(assert_rendered_safely(&error).chars().count(), "500 ".len() + 200 + 1);

    // A member of 1 MiB: the same 201 characters, and every byte of the body
    // still there for a caller who wants it.
    let huge = "y".repeat(1 << 20);
    let body = format!(r#"{{"message":"{huge}"}}"#);
    let error = api(500, &body);
    assert_eq!(error.message(), format!("{}\u{2026}", "y".repeat(200)));
    assert_eq!(assert_rendered_safely(&error).chars().count(), "500 ".len() + 200 + 1);
    assert_eq!(error.body().len(), body.len());
    assert_eq!(error.body_text(), body);

    // A detail list of any length joins to one sentence, cut the same way.
    let entry = r#"{"loc":["body","state"],"msg":"too long"}"#;
    let list = format!(r#"{{"detail":[{}]}}"#, vec![entry; 1000].join(","));
    let joined = "state: too long; ".repeat(12);
    assert_eq!(api(422, &list).message(), format!("{}\u{2026}", &joined[..200]));

    // The cut counts the escape, not the character, and never splits one:
    // 194 characters and a six-character `\u{1b}` fill the 200 exactly...
    let exact =
        api(500, &format!(r#"{{"error":{}}}"#, json_string(&format!("{}\u{1b}", "x".repeat(194)))));
    assert_eq!(exact.message(), format!(r"{}\u{{1b}}", "x".repeat(194)));
    // ...and at 199 the escape does not fit, so it is dropped whole.
    let over =
        api(500, &format!(r#"{{"error":{}}}"#, json_string(&format!("{}\u{1b}", "x".repeat(199)))));
    assert_eq!(over.message(), format!("{}\u{2026}", "x".repeat(199)));
}

#[test]
fn the_request_id_is_escaped_and_cut_where_it_is_shown_and_raw_where_it_is_read() {
    let endpoint = || Some(Box::<str>::from("GET https://example.test/v1/models"));
    let with_id = |id: HeaderValue| {
        let mut map = HeaderMap::new();
        map.insert(REQUEST_ID_HEADER, id);
        ApiError::new(status(503), Bytes::from_static(br#"{"message":"m"}"#), map, endpoint())
    };

    // A tab is the one control character `http` lets through as text.
    let tab = with_id(HeaderValue::from_str("req\tlog").expect("a tab is a valid header value"));
    assert_eq!(tab.request_id(), Some("req\tlog"), "the accessor returns the header as it came");
    assert_eq!(
        assert_rendered_safely(&tab),
        r"GET https://example.test/v1/models: 503 m (request_id=req\tlog)"
    );
    assert_eq!(
        format!("{tab:?}"),
        concat!(
            r#"ApiError { status: 503, kind: InternalServer, "#,
            r#"endpoint: Some("GET https://example.test/v1/models"), request_id: Some("req\\tlog"), "#,
            r#"message: "m", error_type: None, headers: <1 redacted>, body: <15 bytes> }"#
        )
    );

    // 6,000 characters: 128 of them and the mark, in both renderings.
    let long = "r".repeat(6000);
    let error = with_id(HeaderValue::from_str(&long).expect("a valid header value"));
    assert_eq!(error.request_id(), Some(long.as_str()));
    let rendered = assert_rendered_safely(&error);
    let shown_id = format!("{}\u{2026}", "r".repeat(128));
    assert_eq!(
        rendered,
        format!("GET https://example.test/v1/models: 503 m (request_id={shown_id})")
    );
    assert!(format!("{error:?}").contains(&format!("request_id: Some({shown_id:?})")));
    // The whole line is bounded whatever the server sends: the endpoint is
    // the SDK's, and the message and the id are cut.
    assert!(
        rendered.chars().count()
            <= "GET https://example.test/v1/models: 503 ".len()
                + 201
                + " (request_id=)".len()
                + 129
    );

    // No header value can hold an ESC or any other control character but a
    // tab, so none can reach the rendering through the request id.
    assert!(HeaderValue::from_bytes(b"req\x1b[31m").is_err());
    // A value that is not text is no request id at all.
    let opaque = with_id(HeaderValue::from_bytes(b"req-\xff").expect("obs-text is a valid value"));
    assert_eq!(opaque.request_id(), None);
    assert_eq!(assert_rendered_safely(&opaque), "GET https://example.test/v1/models: 503 m");

    // The validation error shows the same header the same way.
    let body = Bytes::from_static(br#"{"model":"jev-1","answers":{"spam":{}}}"#);
    let decode_error = codec::decode::<Fixture>(&body).expect_err("the fixture is missing a field");
    let mut map = HeaderMap::new();
    map.insert(REQUEST_ID_HEADER, HeaderValue::from_str("req\tv").expect("valid"));
    let invalid = ResponseValidationError::new(status(200), body, map, None, decode_error);
    assert_eq!(invalid.request_id(), Some("req\tv"));
    assert_eq!(
        invalid.to_string(),
        r"200 Invalid response data at 'answers.spam.noul'. (request_id=req\tv)"
    );
    assert!(format!("{invalid:?}").contains(r#"request_id: Some("req\\tv")"#), "{invalid:?}");
    assert_printable(&format!("{invalid:?}"));
}

#[test]
fn the_error_type_is_read_raw_and_shown_escaped_and_cut() {
    let name = "auth\u{1b}[2J\u{202e}";
    let error =
        api(403, &format!(r#"{{"message":"m","detail":{{"error_type":{}}}}}"#, json_string(name)));
    assert_eq!(error.error_type(), Some(name), "the accessor returns the server's text");
    assert_rendered_safely(&error);
    assert!(
        format!("{error:?}").contains(r#"error_type: Some("auth\\u{1b}[2J\\u{202e}")"#),
        "{error:?}"
    );
    assert!(!error.to_string().contains("auth"), "the error type is never part of Display");

    let long = "t".repeat(300);
    let error = api(403, &format!(r#"{{"detail":{{"error_type":"{long}"}}}}"#));
    assert_eq!(error.error_type(), Some(long.as_str()));
    let shown = format!("{}\u{2026}", "t".repeat(128));
    assert!(format!("{error:?}").contains(&format!("error_type: Some({shown:?})")), "{error:?}");
}

// ------------------------------------------------------- the wrapping error

/// A success response larger than the client's limit: its own kind, so that
/// a retry predicate can see it without reaching into a cause, and no cause.
#[test]
fn a_response_too_large_names_its_limit_and_has_no_cause() {
    let error = Error::response_too_large(1024);

    assert!(matches!(error.kind(), ErrorKind::ResponseTooLarge { limit: 1024 }), "{error:?}");
    assert_eq!(
        error.to_string(),
        "The response body exceeded the limit of 1024 bytes and was not read."
    );
    assert_eq!(format!("{error:?}"), "Error { kind: ResponseTooLarge { limit: 1024 } }");
    assert!(error.source().is_none());
}

#[test]
fn summary_is_one_fixed_sentence_per_kind_and_never_the_message() {
    let body = Bytes::from_static(br#"{"model":"jev-1","answers":{"spam":{}}}"#);
    let decode_error = codec::decode::<Fixture>(&body).expect_err("the noul field is missing");
    let invalid =
        ResponseValidationError::new(status(200), body, HeaderMap::new(), None, decode_error);
    let cause = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "10.0.0.7:443 refused");
    let connection = Error::connection("10.0.0.7:443 refused", Some(Box::new(cause)));
    assert_eq!(
        connection.source().expect("the transport cause is retained").to_string(),
        "10.0.0.7:443 refused"
    );
    let cases = [
        (
            Error::timeout(Duration::from_secs(10)),
            "timeout",
            "The request timed out.",
            "Request timed out (timeout=10s).",
        ),
        (
            connection,
            "connection error",
            "The connection to the API failed.",
            "10.0.0.7:443 refused",
        ),
        (
            Error::response_too_large(1024),
            "response too large",
            "The response was larger than the size limit.",
            "The response body exceeded the limit of 1024 bytes and was not read.",
        ),
        (
            Error::from(api(403, r#"{"message":"server-chosen text"}"#)),
            "api error",
            "The API answered with an error.",
            "403 server-chosen text",
        ),
        (
            Error::from(invalid),
            "invalid response",
            "The response did not have the expected shape.",
            "200 Invalid response data at 'answers.spam.noul'.",
        ),
        (
            Error::invalid_request("caller-chosen text"),
            "invalid request",
            "The request was invalid and was not sent.",
            "caller-chosen text",
        ),
        (
            Error::config("configuration detail"),
            "config error",
            "The client configuration is invalid.",
            "configuration detail",
        ),
    ];
    for (error, word, sentence, display) in cases {
        let summary: &'static str = error.summary();
        assert_eq!(summary, sentence, "{word}: {error:?}");
        assert_eq!(error.kind().words(), (word, sentence), "{word}: {error:?}");
        assert_eq!(error.to_string(), display, "{word}: Display stays unchanged");
        assert!(!summary.contains("10.0.0.7:443"), "{word}: no transport address in {summary}");
        assert!(!summary.contains("refused"), "{word}: no transport message in {summary}");
    }
}

#[test]
fn each_kind_renders_and_chains_the_way_its_caller_will_read_it() {
    let config = Error::config("TYPESAFE_API_KEY is not set");
    assert!(matches!(config.kind(), ErrorKind::Config));
    assert_eq!(config.to_string(), "TYPESAFE_API_KEY is not set");
    assert!(config.source().is_none());

    let invalid = Error::invalid_request("a question set must hold at least one question");
    assert!(matches!(invalid.kind(), ErrorKind::InvalidRequest));
    assert_eq!(invalid.to_string(), "a question set must hold at least one question");

    let timeout = Error::timeout(Duration::from_secs(10));
    assert!(
        matches!(timeout.kind(), ErrorKind::Timeout { timeout } if *timeout == Duration::from_secs(10))
    );
    assert_eq!(timeout.to_string(), "Request timed out (timeout=10s).");
    assert_eq!(
        Error::timeout(Duration::from_millis(1500)).to_string(),
        "Request timed out (timeout=1.5s)."
    );

    // A connection failure keeps the transport's own error reachable, which is
    // how a caller gets at a cause this crate has no vocabulary for.
    let cause = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
    let connection =
        Error::connection("could not reach https://api.typesafe.ai", Some(Box::new(cause)));
    assert!(matches!(connection.kind(), ErrorKind::Connection));
    assert_eq!(connection.to_string(), "could not reach https://api.typesafe.ai");
    let source = connection.source().expect("invariant: the cause was supplied");
    assert_eq!(source.to_string(), "connection refused");
    assert!(
        source.downcast_ref::<std::io::Error>().is_some(),
        "the transport's own type survives the boxing"
    );

    // A kind that carries a sentence of its own shows it, and the cause beside
    // it, rather than a bare kind name.
    assert_eq!(
        format!("{config:?}"),
        r#"Error { kind: Config, message: "TYPESAFE_API_KEY is not set" }"#
    );
    let shown = format!("{connection:?}");
    let head =
        r#"Error { kind: Connection, message: "could not reach https://api.typesafe.ai", source: "#;
    assert!(shown.starts_with(head), "{shown}");
    assert!(shown.contains("connection refused"), "{shown}");

    // An API failure is the failure, so it has no cause underneath it; the
    // sentence is already the whole story.
    let api_error = Error::from(api(404, r#"{"message":"No such model"}"#));
    assert!(matches!(api_error.kind(), ErrorKind::Api(_)));
    assert_eq!(api_error.to_string(), "404 No such model");
    assert!(api_error.source().is_none());
}

// -------------------------------------------------- response validation

#[test]
fn a_response_that_does_not_fit_names_the_field_and_keeps_the_body() {
    let body = Bytes::from_static(br#"{"model":"jev-1","answers":{"spam":{}}}"#);
    let decode_error = codec::decode::<Fixture>(&body).expect_err("the fixture is missing a field");
    assert_eq!(decode_error.path(), "answers.spam.noul");

    let error = ResponseValidationError::new(
        status(200),
        body.clone(),
        headers(&[("x-typesafe-request-id", "req-decode")]),
        Some("POST https://api.typesafe.ai/v1/systemone".into()),
        decode_error,
    );
    assert_eq!(error.field_path(), "answers.spam.noul");
    assert_eq!(error.message(), "Invalid response data at 'answers.spam.noul'.");
    assert_eq!(
        error.to_string(),
        concat!(
            "POST https://api.typesafe.ai/v1/systemone: 200 ",
            "Invalid response data at 'answers.spam.noul'. (request_id=req-decode)"
        )
    );
    assert_eq!(error.status(), status(200));
    assert_eq!(error.request_id(), Some("req-decode"));
    assert_eq!(
        error.body(),
        &body[..],
        "the body is kept so a caller can recover what the SDK dropped"
    );
    assert_eq!(error.decode_error().kind(), DecodeErrorKind::Data);
    assert_eq!(error.headers().len(), 1);
    assert_eq!(error.body_text(), r#"{"model":"jev-1","answers":{"spam":{}}}"#);

    // Reading the body back is how a caller recovers what the SDK's own
    // response type dropped, including whatever made the decode fail.
    #[derive(Debug, Deserialize)]
    struct Recovered<'a> {
        model: &'a str,
    }
    let recovered = error.body_json::<Recovered<'_>>().expect("the rest of the body is readable");
    assert_eq!(recovered.model, "jev-1");

    // The decode failure is the cause, both from the type itself and from the
    // wrapping error, so a chain printer shows the position `Display` omits.
    assert_eq!(
        error
            .source()
            .expect("invariant: a validation error always has a decode failure")
            .to_string(),
        error.decode_error().to_string()
    );
    let rendered = error.to_string();
    let wrapped = Error::from(error);
    assert!(matches!(wrapped.kind(), ErrorKind::ResponseValidation(_)));
    assert_eq!(wrapped.to_string(), rendered, "wrapping does not change the sentence");
    let source =
        wrapped.source().expect("invariant: a validation error always has a decode failure");
    assert!(source.downcast_ref::<DecodeError>().is_some());
}

/// A response-shaped target, cut down to the field the path test needs.
#[derive(Debug, Deserialize)]
struct Fixture {
    #[expect(dead_code, reason = "the field exists to be decoded into, never read")]
    answers: FixtureAnswers,
}

#[derive(Debug, Deserialize)]
struct FixtureAnswers {
    #[expect(dead_code, reason = "the field exists to be decoded into, never read")]
    spam: FixtureNoul,
}

#[derive(Debug, Deserialize)]
struct FixtureNoul {
    #[expect(dead_code, reason = "the field exists to be decoded into, never read")]
    noul: f64,
}

// ---------------------------------------------------------- body access

#[test]
fn the_body_is_reachable_as_bytes_as_text_and_as_json() {
    let error = api(422, r#"{"message":"bad","detail":{"error_type":"validation_error"}}"#);
    assert_eq!(error.message(), "bad");
    assert_eq!(error.error_type(), Some("validation_error"));
    assert_eq!(
        error.body_text(),
        r#"{"message":"bad","detail":{"error_type":"validation_error"}}"#
    );

    #[derive(Debug, Deserialize)]
    struct Shape<'a> {
        message: &'a str,
    }
    let decoded = error.body_json::<Shape<'_>>().expect("the fixture is the shape it declares");
    assert_eq!(decoded.message, "bad", "a borrowed field borrows the error's own body");

    let refused = api(400, "not json").body_json::<Shape<'_>>().expect_err("the body is not JSON");
    assert_eq!(refused.kind(), DecodeErrorKind::Syntax);
}

// ------------------------------------------------------------ retry-after

/// One row of the upstream parametrized case list: the headers to read, and
/// the wait in milliseconds they should produce.
type Case<'a> = (&'a [(&'a str, &'a str)], Option<u64>);

#[test]
fn retry_after_reproduces_every_case_the_python_sdk_pins() {
    // Ported from `test_parse_retry_after`, one row per case.
    let cases: [Case<'_>; 9] = [
        (&[], None),
        (&[("retry-after", "bad")], None),
        // A server asking to wait a negative time is asking to wait no time,
        // and must not then be given the backoff a missing header would get.
        (&[("retry-after", "-1")], None),
        (&[("retry-after-ms", "NaN"), ("retry-after", "1.5")], Some(1500)),
        (&[("retry-after-ms", "-1"), ("retry-after", "2")], Some(2000)),
        (&[("retry-after", "")], Some(0)),
        (&[("retry-after-ms", "inf")], None),
        (&[("retry-after-ms", "bad"), ("retry-after", "2")], Some(2000)),
        // A wait this long overflows to infinity once it is milliseconds, and
        // an infinite wait is no answer at all.
        (&[("retry-after", "1e308")], None),
    ];
    for (given, expected) in cases {
        let found = parse_retry_after(&headers(given), now());
        assert_eq!(found, expected.map(Duration::from_millis), "headers {given:?}");
    }
}

#[test]
fn retry_after_reads_an_http_date_against_the_instant_it_is_given() {
    let future = httpdate::fmt_http_date(at(10));
    let past = httpdate::fmt_http_date(UNIX_EPOCH + NOW - Duration::from_secs(10));
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after", &future)]), now()),
        Some(Duration::from_millis(10_000))
    );
    assert_eq!(parse_retry_after(&headers(&[("retry-after", &past)]), now()), Some(Duration::ZERO));

    // The date is only tried when the value is not a number, and only for
    // `Retry-After`: the millisecond header has no date form.
    assert_eq!(parse_retry_after(&headers(&[("retry-after-ms", &future)]), now()), None);
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after-ms", "bad"), ("retry-after", &future)]), now()),
        Some(Duration::from_millis(10_000))
    );
}

#[test]
fn retry_after_is_read_for_any_status_that_carries_it() {
    // The Python SDK reads these headers only on its rate-limit error. A 503
    // carrying them is asking for the same wait, and honouring it is strictly
    // better than the backoff that would otherwise apply.
    let error = ApiError::new(
        status(503),
        Bytes::from_static(b"{}"),
        headers(&[("retry-after-ms", "60001")]),
        None,
    );
    assert_eq!(error.kind(), ApiErrorKind::InternalServer);
    assert_eq!(parse_retry_after(error.headers(), now()), Some(Duration::from_millis(60_001)));

    let none = api(500, "{}");
    assert_eq!(parse_retry_after(none.headers(), now()), None);
    // The public accessor reads the clock; with no date in the headers the
    // answer does not depend on it.
    assert_eq!(none.retry_after(), None);
    assert_eq!(error.retry_after(), Some(Duration::from_millis(60_001)));
}

#[test]
fn a_wait_is_truncated_to_whole_milliseconds_and_saturates_rather_than_wrapping() {
    let sub_millisecond = headers(&[("retry-after-ms", "125.7")]);
    assert_eq!(parse_retry_after(&sub_millisecond, now()), Some(Duration::from_millis(125)));

    // Past what a `Duration` of milliseconds can hold, the answer saturates:
    // a wrap would turn a refusal to serve into an immediate retry.
    let enormous = headers(&[("retry-after", "1e30")]);
    assert_eq!(parse_retry_after(&enormous, now()), Some(Duration::from_millis(u64::MAX)));

    // Surrounding space is not a parse failure.
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after", "  2  ")]), now()),
        Some(Duration::from_millis(2000))
    );
    assert_eq!(
        parse_retry_after(&headers(&[("retry-after-ms", "   ")]), now()),
        Some(Duration::ZERO)
    );
}
