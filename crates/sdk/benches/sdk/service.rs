//! The in-memory transport of B4 and B5, and the body splice B1 and B3
//! measure.

use std::{
    convert::Infallible,
    future::{Ready, ready},
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{HeaderValue, Request, Response, StatusCode};
use serde::Serialize;
use tokio::runtime::Runtime;
use tower_service::Service;
use typesafe_sdk::{__internals as sdk, Body, Client, ClientBuilder};

use crate::{MODEL, QUESTIONS_JSON};

/// A transport that answers every request with the same canned response and
/// allocates the same blocks whatever the request: the response's one
/// header. The request is dropped unread, so the SDK and the naive client
/// are charged only for what they do themselves.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InMemory {
    status: StatusCode,
    header: (&'static str, &'static str),
    body: &'static [u8],
}

impl InMemory {
    /// A 200 with `body` and an `x-typesafe-request-id`.
    pub(crate) const fn ok(body: &'static [u8]) -> Self {
        Self { status: StatusCode::OK, header: ("x-typesafe-request-id", "req-1"), body }
    }

    /// A failure `status` carrying one header, with an error body.
    pub(crate) const fn failing(status: StatusCode, header: (&'static str, &'static str)) -> Self {
        Self {
            status,
            header,
            body: br#"{"detail":{"error_type":"rate_limit_error","message":"Slow down."}}"#,
        }
    }
}

impl Service<Request<Body>> for InMemory {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        drop(request);
        let mut response = Response::new(Body::from(Bytes::from_static(self.body)));
        *response.status_mut() = self.status;
        response.headers_mut().insert(self.header.0, HeaderValue::from_static(self.header.1));
        ready(Ok(response))
    }
}

/// A current-thread runtime with its time driver, as a call's deadline needs.
pub(crate) fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread().enable_time().build().expect("the runtime builds")
}

/// A client over `service` with the settings every bench here shares.
pub(crate) fn client<S: typesafe_sdk::HttpService>(service: S) -> Client<S> {
    ClientBuilder::new()
        .api_key("bench-key")
        .base_url("http://127.0.0.1:9")
        .default_model(MODEL)
        .build_with_service(service)
        .expect("the client builds")
}

/// [`MODEL`] as a JSON string, escaped once as the client escapes it when it
/// is built.
const MODEL_JSON: &[u8] = br#""jev-latest""#;

/// The body `SystemOne::send` builds for `state` and the prepared questions,
/// through the same encoder and the same splice: `{"state":` + the encoded
/// state + `,"model":` + the model + `,"questions":` + the prepared bytes +
/// `}`. `assembly` proves the output equal to a body the SDK sent.
pub(crate) fn body<T>(state: &T) -> Bytes
where
    T: Serialize + ?Sized,
{
    sdk::encode_body(|buffer| {
        buffer.extend_from_slice(br#"{"state":"#);
        sdk::encode_into(buffer, state)?;
        buffer.extend_from_slice(br#","model":"#);
        buffer.extend_from_slice(MODEL_JSON);
        buffer.extend_from_slice(br#","questions":"#);
        buffer.extend_from_slice(QUESTIONS_JSON);
        buffer.push(b'}');
        Ok(())
    })
    .expect("the body encodes")
}
