//! Comparator (A): a System One client written the obvious way.
//!
//! It is held to the rules that keep it from being a strawman. It uses the
//! same selected codec as the SDK and the same in-memory service. What it
//! does differently is what a first implementation does:
//!
//! - the body is built as a value tree (`state`, `model`, and a copy of the
//!   questions kept as a value tree) and then serialized;
//! - the URI is formatted and parsed, and the header map built from strings,
//!   on every call;
//! - the response is read whole and decoded with serde's derive, answers
//!   internally tagged by `type` (`#[serde(tag = "type")]`, which buffers each
//!   answer before it can pick the variant) in a `HashMap<String, _>`, with a
//!   map for every container.
//!
//! It does the same work the SDK does for this scenario - a deadline, a
//! status check, a whole body read before decoding - and nothing more: no
//! retry, no telemetry. Where it could be slower without being wrong, it is
//! not made slower.

use std::{error::Error, future::poll_fn, time::Duration};

use bytes::Bytes;
use http::{
    HeaderMap, HeaderValue, Method, Request, Response, Uri,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT},
};
use http_body_util::BodyExt as _;
#[cfg(not(feature = "sonic"))]
use serde_json as codec;
#[cfg(feature = "sonic")]
use sonic_rs as codec;
use tower_service::Service;
use typesafe_sdk::Body;

use crate::naive_response::NaiveResponse;

/// What the naive client returns for anything that is not a decoded answer.
pub(crate) type NaiveError = Box<dyn Error + Send + Sync>;

/// The client's configuration, as a naive client keeps it: strings, and the
/// questions as a value tree built once from the caller's definitions.
#[derive(Debug)]
pub(crate) struct NaiveClient {
    base_url: String,
    api_key: String,
    model: String,
    questions: codec::Value,
    timeout: Duration,
}

impl NaiveClient {
    /// A client for `base_url` asking the questions of `questions`, a JSON
    /// object in the API's `questions` shape.
    pub(crate) fn new(base_url: &str, api_key: &str, model: &str, questions: &[u8]) -> Self {
        Self {
            base_url: base_url.to_owned(),
            api_key: api_key.to_owned(),
            model: model.to_owned(),
            questions: codec::from_slice(questions).expect("the questions are JSON"),
            timeout: Duration::from_secs(10),
        }
    }

    /// Sends one System One request through `service` and decodes the
    /// answer.
    ///
    /// # Errors
    ///
    /// Anything the URI, the headers, the service, the body or the codec
    /// reports, and a status outside 2xx.
    pub(crate) async fn call<S>(
        &self,
        service: &mut S,
        state: &str,
    ) -> Result<NaiveResponse, NaiveError>
    where
        S: Service<Request<Body>, Response = Response<Body>>,
        S::Error: Into<NaiveError>,
    {
        let tree = codec::json!({
            "state": state,
            "model": self.model.as_str(),
            "questions": self.questions.clone(),
        });
        let body = codec::to_vec(&tree)?;

        let uri: Uri = format!("{}/v1/systemone", self.base_url).parse()?;
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", self.api_key))?);
        headers.insert(ACCEPT, HeaderValue::from_str("application/json")?);
        headers.insert(CONTENT_TYPE, HeaderValue::from_str("application/json")?);
        headers.insert(USER_AGENT, HeaderValue::from_str("typesafe-naive/0.1.0")?);
        headers.insert("x-typesafe-sdk", HeaderValue::from_str("typesafe-naive/0.1.0")?);
        headers.insert("x-typesafe-runtime", HeaderValue::from_str("rust")?);
        let mut request = Request::new(Body::from(Bytes::from(body)));
        *request.method_mut() = Method::POST;
        *request.uri_mut() = uri;
        *request.headers_mut() = headers;

        let exchange = async {
            poll_fn(|cx| service.poll_ready(cx)).await.map_err(Into::into)?;
            let response = service.call(request).await.map_err(Into::into)?;
            let (parts, body) = response.into_parts();
            let bytes = body.collect().await?.to_bytes();
            Ok::<_, NaiveError>((parts.status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(self.timeout, exchange).await??;
        if !status.is_success() {
            return Err(format!("the server answered {status}").into());
        }
        Ok(codec::from_slice(&bytes)?)
    }
}
