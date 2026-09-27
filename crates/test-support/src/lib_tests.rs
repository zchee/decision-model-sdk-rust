use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use hyper_util::client::legacy::Client;

use super::*;

/// Builds a plain hyper-util pooled client over cleartext TCP. `http2_only`
/// selects h2c with prior knowledge, which is what makes a cold fan-out share
/// one connection: hyper-util only takes its single-connection lock when it
/// already knows the version is HTTP/2.
fn cleartext_client(
    http2_only: bool,
) -> Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>> {
    let mut builder = Client::builder(TokioExecutor::new());
    builder.http2_only(http2_only);
    builder.build_http()
}

/// Builds a pooled client that speaks HTTP/2 over TLS and trusts exactly the
/// certificates in `roots`. hyper-rustls fills in `alpn_protocols` from
/// `enable_http2`, and panics if the config already lists any, so the config
/// handed to it must leave that field empty.
fn tls_client(
    roots: rustls::RootCertStore,
) -> Result<
    Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        Full<Bytes>,
    >,
    rustls::Error,
> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(config)
        .https_only()
        .enable_http2()
        .build();
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

fn text_response(status: StatusCode, body: &'static str) -> TestResponse {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    response
}

async fn read_body(response: Response<Incoming>) -> Bytes {
    response
        .into_body()
        .collect()
        .await
        .expect("a response served by the test server has a readable body")
        .to_bytes()
}

#[tokio::test]
async fn http1_round_trip_is_recorded() {
    let server = TestServer::start(Protocol::Http1, |request| async move {
        assert_eq!(request.body, Bytes::from_static(br#"{"ping":true}"#));
        text_response(StatusCode::OK, r#"{"pong":true}"#)
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let request = Request::post(format!("{}/v1/systemone", server.base_url()))
        .header("content-type", "application/json")
        .header("x-team", "billing")
        .body(Full::new(Bytes::from_static(br#"{"ping":true}"#)))
        .expect("the request parts are valid");
    let response = cleartext_client(false)
        .request(request)
        .await
        .expect("the HTTP/1.1 request reaches the test server");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(read_body(response).await, Bytes::from_static(br#"{"pong":true}"#));

    let recorded = server.requests();
    assert_eq!(recorded.len(), 1, "exactly one request was served");
    assert_eq!(server.request_count(), 1);
    let first = &recorded[0];
    assert_eq!(first.method, Method::POST);
    assert_eq!(first.uri.path(), "/v1/systemone");
    assert_eq!(first.version, Version::HTTP_11);
    assert_eq!(first.body, Bytes::from_static(br#"{"ping":true}"#));
    assert_eq!(
        first.headers.get("x-team").map(http::HeaderValue::as_bytes),
        Some(b"billing".as_slice()),
        "caller headers reach the recorder unchanged",
    );
    assert_eq!(server.accepted_connections(), 1);
}

#[tokio::test]
async fn h2c_multiplexes_fifty_requests_over_one_connection() {
    const REQUESTS: usize = 50;

    let server =
        TestServer::start(Protocol::H2c, |_request| async { text_response(StatusCode::OK, "ok") })
            .await
            .expect("an h2c server binds on loopback");

    // One client, cold: nothing has connected yet when all 50 calls start.
    let client = cleartext_client(true);
    let mut calls = Vec::with_capacity(REQUESTS);
    for index in 0..REQUESTS {
        let client = client.clone();
        let url = format!("{}/v1/models?call={index}", server.base_url());
        calls.push(tokio::spawn(async move {
            let request = Request::get(url)
                .body(Full::new(Bytes::new()))
                .expect("the request parts are valid");
            client.request(request).await
        }));
    }

    for call in calls {
        let response = call
            .await
            .expect("no request task panicked")
            .expect("every h2c request reaches the test server");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.version(), Version::HTTP_2);
    }

    assert_eq!(server.request_count(), REQUESTS);
    assert_eq!(
        server.accepted_connections(),
        1,
        "50 concurrent h2c requests are multiplexed over a single connection",
    );
}

#[tokio::test]
async fn tls_client_that_trusts_the_certificate_negotiates_http2() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");

    let certificate = server.certificate_der().expect("a TLS server exposes its certificate");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).expect("the generated certificate is a usable trust anchor");

    let request = Request::get(format!("{}/v1/models", server.base_url()))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    let response = tls_client(roots)
        .expect("the client TLS configuration is valid")
        .request(request)
        .await
        .expect("a client trusting the server certificate completes the handshake");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.version(),
        Version::HTTP_2,
        "ALPN selected h2 rather than falling back to HTTP/1.1",
    );
    assert_eq!(server.request_count(), 1);
    assert_eq!(server.accepted_connections(), 1);
}

#[tokio::test]
async fn tls_client_without_the_certificate_fails_the_handshake() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");

    let request = Request::get(format!("{}/v1/models", server.base_url()))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    // An empty root store trusts nothing, so the self-signed certificate has no
    // path to an anchor.
    let error = tls_client(rustls::RootCertStore::empty())
        .expect("the client TLS configuration is valid")
        .request(request)
        .await
        .expect_err("a client trusting nothing must not complete the handshake");

    assert!(
        error.is_connect(),
        "the failure is a connect-time TLS failure, not an HTTP response: {error}",
    );
    assert_eq!(server.request_count(), 0, "a rejected handshake never reaches the handler",);
    assert_eq!(
        server.accepted_connections(),
        1,
        "the TCP connection is still counted, because accept precedes the handshake",
    );
}

#[tokio::test]
async fn handler_can_be_stateful_and_can_delay() {
    let attempts = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&attempts);

    let server = TestServer::start(Protocol::Http1, move |_request| {
        let attempts = Arc::clone(&attempts);
        async move {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                // Holding the first answer back is how a per-attempt deadline
                // is exercised without any clock injection.
                tokio::time::sleep(Duration::from_millis(50)).await;
                text_response(StatusCode::SERVICE_UNAVAILABLE, "retry")
            } else {
                text_response(StatusCode::OK, "ok")
            }
        }
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let client = cleartext_client(false);
    let mut statuses = Vec::new();
    for _ in 0..2 {
        let request = Request::get(format!("{}/v1/models", server.base_url()))
            .body(Full::new(Bytes::new()))
            .expect("the request parts are valid");
        let response = client.request(request).await.expect("the request reaches the test server");
        statuses.push(response.status());
    }

    assert_eq!(
        statuses,
        vec![StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK],
        "the handler answered the two attempts differently",
    );
    assert_eq!(observed.load(Ordering::SeqCst), 2);
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn dropping_the_server_releases_the_port() {
    let server = TestServer::start(Protocol::Http1, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");
    let addr = server.addr();
    drop(server);

    // Rebinding the exact port proves the listener is gone rather than merely
    // idle. A short retry covers the scheduler not having run the abort yet.
    let mut rebound = None;
    for _ in 0..50 {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                rebound = Some(listener);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    assert!(rebound.is_some(), "the port {addr} was still held after the server was dropped",);
}

#[tokio::test]
async fn every_protocol_starts_a_server_of_its_scheme() {
    for protocol in Protocol::ALL {
        let server =
            TestServer::start(protocol, |_request| async { text_response(StatusCode::OK, "ok") })
                .await
                .expect("a server binds on loopback");
        let tls = protocol == Protocol::Http2Tls;
        let scheme = if tls { "https://" } else { "http://" };
        assert!(server.base_url().starts_with(scheme), "{protocol:?}: {server:?}");
        assert_eq!(server.certificate_der().is_some(), tls, "{protocol:?}");
    }
}

#[tokio::test]
async fn a_counted_handler_is_told_which_request_it_answers() {
    let server = TestServer::start_nth(Protocol::Http1, |n, request| {
        json_response(StatusCode::OK, format!("{n} {:?}", request.header_values("x-team")))
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");

    let client = cleartext_client(false);
    for expected in [r#"1 ["a", "b"]"#, r#"2 ["a", "b"]"#] {
        let request = Request::get(format!("{}/v1/models", server.base_url()))
            .header("x-team", "a")
            .header("x-team", "b")
            .body(Full::new(Bytes::new()))
            .expect("the request parts are valid");
        let response = client.request(request).await.expect("the request reaches the test server");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).map(HeaderValue::as_bytes),
            Some(b"application/json".as_slice()),
        );
        assert_eq!(read_body(response).await, expected.as_bytes());
    }
    assert_eq!(server.requests()[1].header_values("x-team"), ["a", "b"]);
    assert_eq!(server.requests()[1].header_values("x-absent"), Vec::<&str>::new());
}

/// The server's records once it has accepted `count` connections and none of
/// them is still in its TLS handshake, waited for under a bound of 5 s.
async fn settled_connections(server: &TestServer, count: usize) -> Vec<ConnectionRecord> {
    let settled = async {
        loop {
            let records = server.connections();
            if records.len() == count
                && records.iter().all(|record| *record.tls() != Tls::InProgress)
            {
                return records;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), settled).await.unwrap_or_else(|_| {
        panic!(
            "the server did not hold {count} settled connection(s) within 5 s: {:#?}",
            server.connections()
        )
    })
}

/// Writes `request` to `stream` and reads until the answer ends with `ok`,
/// under a bound of 5 s.
async fn exchange_ok(stream: &mut TcpStream, request: &[u8]) -> Vec<u8> {
    stream.write_all(request).await.expect("the request is written");
    let read = async {
        let mut answer = Vec::new();
        let mut buffer = [0; 1024];
        while !answer.ends_with(b"ok") {
            let read = stream.read(&mut buffer).await.expect("the answer reads");
            assert_ne!(read, 0, "the server closed before answering: {answer:?}");
            answer.extend_from_slice(&buffer[..read]);
        }
        answer
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .expect("the server answered within 5 s")
}

#[tokio::test]
async fn a_tls_connection_is_recorded_with_its_alpn_and_its_requests() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");
    let certificate = server.certificate_der().expect("a TLS server exposes its certificate");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).expect("the generated certificate is a usable trust anchor");
    let client = tls_client(roots).expect("the client TLS configuration is valid");

    for _ in 0..3 {
        let request = Request::get(format!("{}/v1/models", server.base_url()))
            .body(Full::new(Bytes::new()))
            .expect("the request parts are valid");
        let response = client.request(request).await.expect("the request is served");
        assert_eq!(response.status(), StatusCode::OK);
    }

    let records = server.connections();
    assert_eq!(records.len(), 1, "{records:#?}");
    let record = &records[0];
    assert!(record.peer().ip().is_loopback(), "{record:?}");
    assert_ne!(record.peer().port(), server.addr().port(), "the peer is the client");
    assert_eq!(*record.tls(), Tls::Completed { alpn: Some(b"h2".to_vec()) });
    assert_eq!(record.requests(), 3);
    assert_eq!(format!("{record:?}"), format!("{}, TLS, ALPN \"h2\", 3 request(s)", record.peer()));
    assert_eq!(server.accepted_connections(), 1, "the length of the records");
}

#[tokio::test]
async fn a_plaintext_request_at_a_tls_port_is_recorded_with_its_first_bytes() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");
    // 76 bytes, of which the record keeps the first 64: the request line, the
    // host, and the credential's scheme.
    let request =
        b"GET /v1/models HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer test-key\r\n\r\n";
    let mut stream = TcpStream::connect(server.addr()).await.expect("the server accepts");
    stream.write_all(request).await.expect("the request is written");

    let records = settled_connections(&server, 1).await;
    let record = &records[0];
    assert_eq!(record.peer(), stream.local_addr().expect("the client's address"));
    assert_eq!(
        *record.tls(),
        Tls::Failed {
            error: String::from("received corrupt message of type InvalidContentType"),
            first_bytes: request[..FIRST_BYTES].to_vec(),
        }
    );
    assert_eq!(record.requests(), 0);
    assert_eq!(
        format!("{record:?}"),
        format!(
            "{}, TLS failed: received corrupt message of type InvalidContentType; first bytes {}, 0 request(s)",
            record.peer(),
            r#""GET /v1/models HTTP/1.1\r\nhost: 127.0.0.1\r\nauthorization: Bearer ""#
        )
    );
}

#[tokio::test]
async fn a_handshake_the_client_rejects_is_recorded_with_its_client_hello() {
    let server = TestServer::start(Protocol::Http2Tls, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/2-over-TLS server binds on loopback");
    let request = Request::get(format!("{}/v1/models", server.base_url()))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    tls_client(rustls::RootCertStore::empty())
        .expect("the client TLS configuration is valid")
        .request(request)
        .await
        .expect_err("a client trusting nothing must not complete the handshake");

    let records = settled_connections(&server, 1).await;
    let record = &records[0];
    let Tls::Failed { error, first_bytes } = record.tls() else {
        panic!("the handshake failed on the server as well: {record:?}");
    };
    assert_eq!(error, "received fatal alert: UnknownCA", "{record:?}");
    // A TLS record of type handshake (22), then the version of the record
    // layer: binary, so the rendering escapes it.
    assert_eq!(first_bytes.len(), FIRST_BYTES, "{record:?}");
    assert_eq!(first_bytes[..2], [0x16, 0x03], "{record:?}");
    let rendered = format!("{record:?}");
    assert!(
        rendered.contains(r#"; first bytes "\x16\x03"#) && rendered.ends_with(", 0 request(s)"),
        "{rendered}"
    );
    assert!(rendered.is_ascii() && !rendered.contains('\n'), "one printable line: {rendered}");
}

#[tokio::test]
async fn close_connections_ends_every_held_connection_and_keeps_listening() {
    let server = TestServer::start(Protocol::Http1, |_request| async {
        text_response(StatusCode::OK, "ok")
    })
    .await
    .expect("an HTTP/1.1 server binds on loopback");
    let request = b"GET /v1/models HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n";

    let mut first = TcpStream::connect(server.addr()).await.expect("the server accepts");
    let answer = exchange_ok(&mut first, request).await;
    assert!(answer.starts_with(b"HTTP/1.1 200 OK\r\n"), "{:?}", answer.escape_ascii().to_string());

    server.close_connections();
    let mut buffer = [0; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), first.read(&mut buffer))
        .await
        .expect("the server closed the connection within 5 s");
    assert_eq!(read.expect("a clean close, not a reset"), 0, "the connection ended");

    // The listener is still there, and a new connection is served.
    let mut second = TcpStream::connect(server.addr()).await.expect("the server still accepts");
    let answer = exchange_ok(&mut second, request).await;
    assert!(answer.starts_with(b"HTTP/1.1 200 OK\r\n"), "{:?}", answer.escape_ascii().to_string());

    let records = server.connections();
    let peers = [
        first.local_addr().expect("the first client's address"),
        second.local_addr().expect("the second client's address"),
    ];
    assert_eq!(
        records.iter().map(|record| format!("{record:?}")).collect::<Vec<_>>(),
        peers.map(|peer| format!("{peer}, cleartext, 1 request(s)")),
    );
    assert_eq!(server.accepted_connections(), 2);
}

#[tokio::test]
async fn the_refusing_port_is_port_one_and_refuses() {
    let port = RefusingPort::new().await.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(port.addr(), SocketAddr::from((Ipv4Addr::LOCALHOST, 1)));
    assert_eq!(port.base_url(), "http://127.0.0.1:1");
    for attempt in 1..=3 {
        let error = TcpStream::connect(port.addr()).await.expect_err("nothing answers at port 1");
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused, "attempt {attempt}: {error}");
    }
}

#[tokio::test]
async fn an_address_that_accepts_is_not_a_refusing_port() {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.expect("a loopback listener binds");
    let addr = listener.local_addr().expect("its address");

    let error = RefusingPort::at(addr, Duration::from_secs(2))
        .await
        .expect_err("a connect to a listener is not refused");
    let Error::NotRefused { addr: named, outcome } = &error else {
        panic!("the error says the port was not refused: {error:?}");
    };
    assert_eq!(*named, addr);
    assert_eq!(outcome, "connected");
    assert_eq!(
        error.to_string(),
        format!("{addr} should refuse every connection, but a connect to it connected")
    );
}

#[tokio::test]
async fn a_silent_server_accepts_and_never_answers() {
    let server = SilentServer::start().await.expect("a silent server binds on loopback");
    assert!(server.addr().ip().is_loopback(), "{server:?}");
    let mut written = TcpStream::connect(server.addr()).await.expect("the server accepts");
    let mut quiet = TcpStream::connect(server.addr()).await.expect("the server accepts again");
    written.write_all(b"GET / HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n").await.expect("it is written");

    let accepted = async {
        while server.accepted_connections() < 2 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), accepted)
        .await
        .unwrap_or_else(|_| panic!("the server did not accept both connections: {server:?}"));

    // Nothing comes back, neither an answer nor a close: the read is still
    // waiting when its own bound ends.
    let mut buffer = [0; 64];
    let read = tokio::time::timeout(Duration::from_millis(200), written.read(&mut buffer)).await;
    assert!(read.is_err(), "the silent server answered or closed: {read:?}");
    assert_eq!(server.accepted_connections(), 2, "{server:?}");

    // Dropping the server closes what it held.
    drop(server);
    let read = tokio::time::timeout(Duration::from_secs(5), quiet.read(&mut buffer))
        .await
        .expect("the held connection closed within 5 s of the drop");
    assert_eq!(read.expect("a clean close"), 0);
}

#[tokio::test]
async fn a_closing_server_closes_each_connection_after_its_delay_without_a_word() {
    const DELAY: Duration = Duration::from_millis(200);

    let server = SilentServer::closing_after(DELAY).await.expect("a silent server binds");
    let started = std::time::Instant::now();
    let mut first = TcpStream::connect(server.addr()).await.expect("the server accepts");
    let mut second = TcpStream::connect(server.addr()).await.expect("the server accepts again");

    for (name, stream) in [("first", &mut first), ("second", &mut second)] {
        let mut buffer = [0; 64];
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
            .await
            .unwrap_or_else(|_| panic!("the {name} connection was not closed within 5 s"));
        // A clean close with no byte before it: the server wrote nothing.
        assert_eq!(read.expect("a clean close"), 0, "the {name} connection");
        let elapsed = started.elapsed();
        assert!(elapsed >= DELAY, "the {name} connection closed after {elapsed:?}");
    }
    assert_eq!(server.accepted_connections(), 2, "{server:?}");
}

#[tokio::test]
async fn a_tls_handshake_with_a_closing_server_fails_after_its_delay() {
    const DELAY: Duration = Duration::from_millis(200);

    let server = SilentServer::closing_after(DELAY).await.expect("a silent server binds");
    let request = Request::get(format!("https://{}/v1/models", server.addr()))
        .body(Full::new(Bytes::new()))
        .expect("the request parts are valid");
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        tls_client(rustls::RootCertStore::empty())
            .expect("the client TLS configuration is valid")
            .request(request),
    )
    .await
    .expect("the handshake ended within 5 s")
    .expect_err("the server closes the connection instead of answering the handshake");

    let elapsed = started.elapsed();
    assert!(error.is_connect(), "a connect-time failure, not an HTTP response: {error:?}");
    assert!(elapsed >= DELAY, "the handshake failed after {elapsed:?}, before the close");
    assert_eq!(server.accepted_connections(), 1, "{server:?}");
}

#[tokio::test]
async fn dropping_a_closing_server_closes_what_it_holds_at_once() {
    // A delay no test waits for: only the drop can close the connection.
    let server = SilentServer::closing_after(Duration::from_secs(3600))
        .await
        .expect("a silent server binds");
    let mut stream = TcpStream::connect(server.addr()).await.expect("the server accepts");
    let accepted = async {
        while server.accepted_connections() < 1 {
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), accepted)
        .await
        .unwrap_or_else(|_| panic!("the server did not accept within 5 s: {server:?}"));

    drop(server);
    let mut buffer = [0; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .expect("the held connection closed within 5 s of the drop");
    assert_eq!(read.expect("a clean close"), 0);
}

#[tokio::test]
async fn a_raw_server_answers_every_connection_with_its_bytes() {
    let reply = b"SSH-2.0-OpenSSH_9.9\r\n\r\n";
    let addr = raw_server(reply).await.expect("a raw server binds on loopback");
    assert!(addr.ip().is_loopback(), "{addr}");
    for connection in 1..=2 {
        let mut stream = TcpStream::connect(addr).await.expect("the raw server accepts");
        stream.write_all(b"GET / HTTP/1.1\r\n\r\n").await.expect("the request is written");
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.expect("the raw server closes after its reply");
        assert_eq!(answer, reply, "connection {connection}");
    }
}
