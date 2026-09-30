//! B3: request assembly - everything an attempt does before the transport is
//! called: the body encode, the retained reference to it that a retry would
//! send again, the clone of the client's header map, and the request built
//! from the pre-parsed URI.
//!
//! These steps are crate-private and run inline inside `send`, so the bench
//! rebuilds them from the same parts: the SDK's own encoder through
//! `__internals`, a header map of the six headers the SDK sends, and
//! `http::Request`. Before anything is timed, the body is checked byte for
//! byte against the body a real `send` handed its transport, so the splice
//! measured here is the one the SDK performs.
//!
//! How this can mislead: the header map is this bench's, not the client's
//! (the same names and value lengths), and the retry predicate, the deadline
//! and the telemetry events are not in it. B5's whole call has all of them.

use std::{
    convert::Infallible,
    future::Future,
    pin::{Pin, pin},
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use divan::{Bencher, black_box};
use http::{HeaderMap, HeaderValue, Method, Request, Response, Uri};
use http_body_util::BodyExt as _;
use tower_service::Service;
use typesafe_sdk::Body;

use crate::{
    service::{body, client, runtime},
    support::{RESULT, questions, text},
};

/// A transport that keeps the last request body it was sent.
#[derive(Debug, Clone, Default)]
struct Capture(Arc<Mutex<Option<Bytes>>>);

impl Service<Request<Body>> for Capture {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Response<Body>, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let slot = Arc::clone(&self.0);
        Box::pin(async move {
            let sent = request.into_body().collect().await.unwrap_or_else(|never| match never {});
            *slot.lock().expect("the slot is never poisoned") = Some(sent.to_bytes());
            Ok(Response::new(Body::from(Bytes::from_static(RESULT))))
        })
    }
}

/// The body a real call sends for `state`.
fn sent_body(state: &str) -> Bytes {
    let capture = Capture::default();
    let client = client(capture.clone());
    let questions = questions();
    let runtime = runtime();
    runtime.block_on(pin!(client.system_one(state, &questions).send())).expect("the call succeeds");
    capture.0.lock().expect("the slot is never poisoned").take().expect("a body was sent")
}

/// The six headers the SDK sends on a request with a body, at the lengths
/// its own values have.
fn header_map() -> HeaderMap {
    let mut headers = HeaderMap::with_capacity(6);
    headers.insert("authorization", HeaderValue::from_static("Bearer bench-key"));
    headers.insert("accept", HeaderValue::from_static("application/json"));
    headers.insert("user-agent", HeaderValue::from_static("typesafe-sdk-rust/0.2.0"));
    headers.insert("x-typesafe-sdk", HeaderValue::from_static("typesafe-sdk-rust/0.2.0"));
    headers.insert("x-typesafe-runtime", HeaderValue::from_static("rust"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    // The first clone of a map moves its names into shared storage, once; a
    // client's map went through that on its first call.
    drop(headers.clone());
    headers
}

#[divan::bench]
fn request(bencher: Bencher<'_, '_>) {
    let state = text(1 << 10);
    assert_eq!(body(state.as_str()), sent_body(&state), "the bench's splice is the SDK's");
    let headers = header_map();
    let uri = Uri::from_static("http://127.0.0.1:9/v1/systemone");
    bencher.bench_local(|| {
        let body = body(black_box(state.as_str()));
        let retained = body.clone();
        let mut request = Request::new(Body::from(body));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = uri.clone();
        *request.headers_mut() = headers.clone();
        (request, retained)
    });
}

#[divan::bench]
fn header_map_clone(bencher: Bencher<'_, '_>) {
    let headers = header_map();
    bencher.bench_local(|| black_box(&headers).clone());
}
