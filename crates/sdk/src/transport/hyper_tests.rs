//! Tests for the default transport's construction, error mapping and response
//! body.

use std::io;

use http_body::Body as _;
use http_body_util::BodyExt as _;
use test_support::raw_server;

use super::*;
use crate::ErrorKind;

fn settings(version: HttpVersion) -> TransportSettings {
    TransportSettings {
        version,
        extra_roots: Vec::new(),
        connect_timeout: None,
        attempt_deadline: None,
    }
}

/// Sends one `GET` through the transport itself, with no SDK around it, and
/// returns the body of the answer.
async fn body_from(reply: &'static [u8]) -> ResponseBody {
    let mut transport = HyperTransport::new(settings(HttpVersion::Auto)).expect("it builds");
    let address = raw_server(reply).await.expect("a loopback port");
    let request =
        Request::get(format!("http://{address}/")).body(Body::empty()).expect("a request");
    let response = transport.call(request).await.expect("the server answers");
    assert_eq!(response.status(), http::StatusCode::OK);
    response.into_body()
}

#[tokio::test]
async fn the_response_body_forwards_the_declared_length_the_frames_and_the_end() {
    let mut body = body_from(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello").await;
    // The declared length is known before a byte of the body is read: that
    // is what the size limit is checked against.
    assert_eq!(body.size_hint().exact(), Some(5));
    assert!(!body.is_end_stream());
    assert_eq!(format!("{body:?}"), "ResponseBody { .. }", "no part of the body is shown");

    let frame = body.frame().await.expect("one frame").expect("it reads");
    assert_eq!(frame.into_data().expect("a data frame"), Bytes::from_static(b"hello"));
    assert!(body.is_end_stream(), "the declared length has been read");
    assert_eq!(body.size_hint().exact(), Some(0));
    assert!(body.frame().await.is_none());
}

#[tokio::test]
async fn a_response_body_without_a_declared_length_reports_none_and_streams() {
    let body =
        body_from(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n")
            .await;
    assert_eq!(body.size_hint().lower(), 0);
    assert_eq!(body.size_hint().exact(), None);
    assert_eq!(body.collect().await.expect("it reads").to_bytes(), Bytes::from_static(b"abc"));
}

#[tokio::test]
async fn a_body_cut_short_fails_with_hyper_error_as_the_boxed_cause() {
    let body = body_from(b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\nabc").await;
    let error = body.collect().await.expect_err("the server closed 97 bytes early");
    let cause = error.downcast_ref::<::hyper::Error>().expect("hyper's own error, boxed once");
    let underneath = cause.source().and_then(|inner| inner.downcast_ref::<io::Error>());
    assert_eq!(underneath.map(io::Error::kind), Some(io::ErrorKind::UnexpectedEof), "{cause:?}");
}

#[test]
fn the_transport_prints_its_settings_and_counts_its_roots() {
    let transport = HyperTransport::new(settings(HttpVersion::Http2Only)).expect("it builds");
    assert_eq!(
        format!("{transport:?}"),
        "HyperTransport { http_version: Http2Only, extra_roots: 0, connect_timeout: None }"
    );

    let transport = HyperTransport::new(TransportSettings {
        version: HttpVersion::Auto,
        extra_roots: Vec::new(),
        connect_timeout: Some(Duration::from_millis(1500)),
        attempt_deadline: None,
    })
    .expect("it builds");
    assert_eq!(
        format!("{transport:?}"),
        "HyperTransport { http_version: Auto, extra_roots: 0, connect_timeout: Some(1.5s) }"
    );
}

#[test]
fn a_root_that_is_not_a_certificate_is_a_config_error() {
    let garbage = b"-----BEGIN CERTIFICATE----- not DER at all".to_vec();
    // What the platform verifier says about these bytes differs by operating
    // system, so the expected message is built from its own answer.
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let refusal = Verifier::new_with_extra_roots([CertificateDer::from(garbage.clone())], provider)
        .expect_err("the bytes are not a certificate");

    let error = HyperTransport::new(TransportSettings {
        version: HttpVersion::Http2Only,
        extra_roots: vec![garbage],
        connect_timeout: None,
        attempt_deadline: None,
    })
    .expect_err("a bad root must not build");

    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert_eq!(
        error.to_string(),
        format!("The TLS certificate verifier could not be built: {refusal}.")
    );
}

#[test]
fn a_timed_out_connect_is_the_connect_timeout() {
    let expired = io::Error::new(io::ErrorKind::TimedOut, "connect timeout");
    let refused = io::Error::new(io::ErrorKind::ConnectionRefused, "refused");
    let wrapped: Box<dyn StdError + Send + Sync> = Box::new(io::Error::other(Chained(expired)));

    assert!(timed_out(&Chained(io::Error::new(io::ErrorKind::TimedOut, "t"))));
    assert!(timed_out(&*wrapped), "found below another I/O error");
    assert!(!timed_out(&refused));
    assert!(!timed_out(&Chained(refused)));
}

/// An error whose cause is an I/O error, as a connector reports one.
#[derive(Debug)]
struct Chained(io::Error);

impl fmt::Display for Chained {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("tcp connect error")
    }
}

impl StdError for Chained {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(&self.0)
    }
}
