//! The models endpoint.
//!
//! Listing the models is also how a client checks that its API key works and
//! opens its connection before the first real call: the request carries no
//! body, so a key that is rejected is rejected cheaply.
//!
//! [`Models`] is the resource, reached through [`Client::models`]; a model
//! card carries three strings, and members the API adds later are ignored by
//! the decoder and stay readable in the raw body.

use std::{borrow::Cow, fmt, time::Duration};

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, IgnoredAny, MapAccess, Visitor},
    ser::SerializeStruct,
};

use crate::{
    client::Client,
    codec,
    de::{KeyIn, invalid_response},
    error::Error,
    request::{CallHeaders, Deadline},
    response::ResponseMeta,
    retry::{self, RetryPolicy},
    transport::{self, Exchange, HttpService},
};

// ------------------------------------------------------------- resource

/// The models endpoint of a client, from [`Client::models`].
pub struct Models<'a, S> {
    client: &'a Client<S>,
}

impl<'a, S> Models<'a, S> {
    pub(crate) fn new(client: &'a Client<S>) -> Self {
        Self { client }
    }

    /// A request for the models the account can use.
    pub fn list(&self) -> ListModels<'a, S> {
        ListModels {
            client: self.client,
            deadline: Deadline::Client,
            headers: CallHeaders::default(),
            retry: None,
        }
    }
}

impl<S> fmt::Debug for Models<'_, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Models").finish_non_exhaustive()
    }
}

/// A request for the model list, ready to be configured and sent.
///
/// Made by [`Models::list`]. Nothing is checked until
/// [`send`](Self::send).
#[must_use = "a request does nothing until it is sent"]
pub struct ListModels<'a, S> {
    client: &'a Client<S>,
    deadline: Deadline,
    headers: CallHeaders<'a>,
    /// This call's own policy, in place of the client's.
    retry: Option<RetryPolicy>,
}

impl<'a, S> ListModels<'a, S> {
    /// The deadline of each attempt of this call, instead of the client's.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.deadline = Deadline::After(timeout);
        self
    }

    /// No deadline on any attempt of this call.
    pub fn no_timeout(mut self) -> Self {
        self.deadline = Deadline::Never;
        self
    }

    /// A header for this call only; see
    /// [`SystemOne::header`](crate::request::SystemOne::header).
    pub fn header(mut self, name: impl Into<Cow<'a, str>>, value: impl Into<Cow<'a, str>>) -> Self {
        self.headers.push(name.into(), value.into());
        self
    }

    /// The retry policy of this call, in place of the client's; see
    /// [`SystemOne::retry`](crate::request::SystemOne::retry).
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }
}

impl<S> ListModels<'_, S>
where
    S: HttpService,
{
    /// Sends the request and decodes the list, retrying a failure as the
    /// call's retry policy, or else the client's, says. When retrying stops,
    /// the error is the one the last attempt failed with.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest),
    ///   before anything is sent: a header that is not a valid header, or a
    ///   deadline of zero.
    /// - [`ErrorKind::Api`](crate::ErrorKind::Api) for a status outside 2xx.
    /// - [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the attempt
    ///   ran past its deadline.
    /// - [`ErrorKind::Connection`](crate::ErrorKind::Connection) when no
    ///   response could be read.
    /// - [`ErrorKind::ResponseTooLarge`](crate::ErrorKind::ResponseTooLarge)
    ///   when a success response's body was larger than the client's limit.
    /// - [`ErrorKind::ResponseValidation`](crate::ErrorKind::ResponseValidation)
    ///   when the body is not a model list.
    pub async fn send(self) -> Result<ListModelsResponse, Error> {
        let shared = self.client.shared();
        let deadline = self.deadline.resolve(shared.config.timeout())?;
        let headers = self.headers.parse(false)?;

        let uri = shared.config.endpoints().models();
        let exchange = Exchange {
            method: &Method::GET,
            uri,
            base_headers: &shared.get_headers,
            call_headers: &headers,
            deadline,
            config: &shared.config,
        };
        let policy = self.retry.as_ref().unwrap_or(&shared.retry);
        retry::run(
            policy,
            &Method::GET,
            shared.config.endpoints().models_log(),
            |retry| async move {
                let (status, headers, body) =
                    transport::attempt(&shared.service, exchange, retry, None).await?;
                // Decoding is part of the attempt, so a retry predicate sees a
                // response that did not decode, as the Python SDK's does.
                decode_list_models(body, status, headers, Some((&Method::GET, uri)))
            },
        )
        .await
    }
}

impl<S> fmt::Debug for ListModels<'_, S> {
    /// The deadline and the header names; never a header value. A retry
    /// policy is shown when the call has one of its own.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = formatter.debug_struct("ListModels");
        shown.field("deadline", &self.deadline).field("headers", &self.headers);
        if let Some(retry) = &self.retry {
            shown.field("retry", retry);
        }
        shown.finish_non_exhaustive()
    }
}

// ------------------------------------------------------------ payload

/// One model the account can use.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ModelMetadata {
    name: String,
    description: String,
    release_date: String,
}

impl ModelMetadata {
    /// The name or alias a request's `model` accepts, such as `jev-latest`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the model is for.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The release date, as the API writes it: `YYYY-MM-DD`.
    #[must_use]
    pub fn release_date(&self) -> &str {
        &self.release_date
    }
}

/// The models available to the account, with the HTTP response they came in.
///
/// Serializing it writes `models` only; the HTTP metadata is runtime state,
/// not part of the API payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ListModelsResponse {
    models: Vec<ModelMetadata>,
    meta: ResponseMeta,
}

impl ListModelsResponse {
    /// The models, in the order the API listed them.
    #[must_use]
    pub fn models(&self) -> &[ModelMetadata] {
        &self.models
    }

    /// The status, headers and raw body of the HTTP response.
    #[must_use]
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// Gives up everything but the models.
    #[must_use]
    pub fn into_models(self) -> Vec<ModelMetadata> {
        self.models
    }
}

impl Serialize for ListModelsResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("ListModelsResponse", 1)?;
        out.serialize_field("models", &self.models)?;
        out.end()
    }
}

impl<'de> Deserialize<'de> for ModelMetadata {
    /// Reads a model card from an object; members this version does not know
    /// are ignored.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(CardVisitor)
    }
}

/// Reads a model card as an object only. serde's derived reader would also
/// take a JSON array positionally, which the API's schema does not allow.
struct CardVisitor;

impl<'de> Visitor<'de> for CardVisitor {
    type Value = ModelMetadata;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a model card")
    }

    fn visit_map<M>(self, mut map: M) -> Result<ModelMetadata, M::Error>
    where
        M: MapAccess<'de>,
    {
        let (mut name, mut description, mut release_date) = (None, None, None);
        while let Some(index) =
            map.next_key_seed(KeyIn(&["name", "description", "release_date"]))?
        {
            match index {
                Some(0) => name = Some(map.next_value()?),
                Some(1) => description = Some(map.next_value()?),
                Some(2) => release_date = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ModelMetadata {
            name: name.ok_or_else(|| de::Error::missing_field("name"))?,
            description: description.ok_or_else(|| de::Error::missing_field("description"))?,
            release_date: release_date.ok_or_else(|| de::Error::missing_field("release_date"))?,
        })
    }
}

/// The body of a models response.
struct ModelList {
    models: Vec<ModelMetadata>,
}

impl<'de> Deserialize<'de> for ModelList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ModelListVisitor)
    }
}

struct ModelListVisitor;

impl<'de> Visitor<'de> for ModelListVisitor {
    type Value = ModelList;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a list of models")
    }

    fn visit_map<M>(self, mut map: M) -> Result<ModelList, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut models = None;
        while let Some(index) = map.next_key_seed(KeyIn(&["models"]))? {
            match index {
                Some(_) => models = Some(map.next_value()?),
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(ModelList { models: models.ok_or_else(|| de::Error::missing_field("models"))? })
    }
}

/// Decodes the body of a successful models response.
///
/// # Errors
///
/// Returns [`ErrorKind::ResponseValidation`](crate::ErrorKind::ResponseValidation)
/// when the body does not have the documented shape; its field path names the
/// card and the member, such as `models[1].name`.
pub(crate) fn decode_list_models(
    body: Bytes,
    status: StatusCode,
    headers: HeaderMap,
    endpoint: Option<(&Method, &Uri)>,
) -> Result<ListModelsResponse, Error> {
    let meta = ResponseMeta::new(status, headers, body);
    let decoded = codec::decode::<ModelList>(meta.raw_body());
    match decoded {
        Ok(ModelList { models }) => Ok(ListModelsResponse { models, meta }),
        Err(source) => Err(invalid_response(meta, endpoint, source)),
    }
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
