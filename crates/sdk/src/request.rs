//! Building one request and sending it.
//!
//! The body is assembled by splicing fragments of finished JSON - the encoded
//! state, the pre-escaped model name, the prepared questions - rather than by
//! building a value and encoding it, so a call costs one pass over the state
//! and nothing over anything else.
//!
//! The builder ends in a plain `async fn`, not an `IntoFuture`: on the pinned
//! toolchain an unboxed `IntoFuture` needs an unstable associated type, so the
//! choice is between a boxed future on every call and a method call the caller
//! writes. The method call is free.

use std::{borrow::Cow, fmt, marker::PhantomData, time::Duration};

use bytes::Bytes;
use http::Method;
use serde::Serialize;

use crate::{
    client::Client,
    codec::{self, EncodeError},
    config::ZERO_TIMEOUT,
    de::{self, AnswerSet},
    error::Error,
    question::{PreparedQuestions, upsert},
    response::{Answers, SystemOneResponse},
    retry::{self, RetryPolicy},
    text,
    transport::{self, Exchange, HttpService},
};

/// The deadline a call asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Deadline {
    /// The client's deadline.
    #[default]
    Client,
    /// This deadline instead.
    After(Duration),
    /// No deadline.
    Never,
}

impl Deadline {
    /// The deadline one attempt gets, given the client's.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
    /// error, with the Python SDK's message, for a deadline of zero.
    pub(crate) fn resolve(self, client: Option<Duration>) -> Result<Option<Duration>, Error> {
        match self {
            Self::Client => Ok(client),
            Self::After(timeout) if timeout.is_zero() => Err(Error::invalid_request(ZERO_TIMEOUT)),
            Self::After(timeout) => Ok(Some(timeout)),
            Self::Never => Ok(None),
        }
    }
}

/// The headers one call adds, as given; they are checked when it is sent.
#[derive(Clone, Default)]
pub(crate) struct CallHeaders<'a>(Vec<(Cow<'a, str>, Cow<'a, str>)>);

impl<'a> CallHeaders<'a> {
    pub(crate) fn push(&mut self, name: Cow<'a, str>, value: Cow<'a, str>) {
        self.0.push((name, value));
    }

    /// Parses them, dropping the ones the SDK owns.
    ///
    /// # Errors
    ///
    /// See [`transport::call_headers`].
    pub(crate) fn parse(
        &self,
        with_body: bool,
    ) -> Result<Vec<(http::HeaderName, http::HeaderValue)>, Error> {
        transport::call_headers(
            self.0.iter().map(|(name, value)| (name.as_ref(), value.as_ref())),
            with_body,
        )
    }
}

impl fmt::Debug for CallHeaders<'_> {
    /// The names only: a header value is where a caller puts a token.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.0.iter().map(|(name, _)| name)).finish()
    }
}

/// The built-in members of a System One body, which an `extra_body` entry of
/// the same name replaces.
const STATE: &str = "state";
const MODEL: &str = "model";
const QUESTIONS: &str = "questions";

/// An extra top-level member of the body: its name, and its value encoded
/// when it was added - or the encoding error, kept for `send` to report.
type ExtraMember<'a> = (Cow<'a, str>, Result<Vec<u8>, EncodeError>);

/// A System One request, ready to be configured and sent.
///
/// Made by [`Client::system_one`]. `A` is what the answers decode into:
/// [`Answers`], a lookup by question name, unless [`typed`](Self::typed)
/// names a type of the caller's own.
///
/// Nothing is checked or encoded until [`send`](Self::send): the methods
/// never fail, and a header or a body member that cannot be sent is reported
/// by `send`.
#[must_use = "a request does nothing until it is sent"]
pub struct SystemOne<'a, S, T: ?Sized, A = Answers> {
    client: &'a Client<S>,
    state: &'a T,
    questions: &'a PreparedQuestions,
    model: Option<Cow<'a, str>>,
    deadline: Deadline,
    headers: CallHeaders<'a>,
    extra: Vec<ExtraMember<'a>>,
    /// This call's own policy, in place of the client's.
    retry: Option<RetryPolicy>,
    /// `fn() -> A` rather than `A`: the request holds no `A`, so it must not
    /// inherit `A`'s auto traits or drop behaviour.
    answers: PhantomData<fn() -> A>,
}

impl<'a, S, T> SystemOne<'a, S, T>
where
    T: ?Sized,
{
    pub(crate) fn new(
        client: &'a Client<S>,
        state: &'a T,
        questions: &'a PreparedQuestions,
    ) -> Self {
        Self {
            client,
            state,
            questions,
            model: None,
            deadline: Deadline::Client,
            headers: CallHeaders::default(),
            extra: Vec::new(),
            retry: None,
            answers: PhantomData,
        }
    }
}

impl<'a, S, T, A> SystemOne<'a, S, T, A>
where
    T: ?Sized,
{
    /// The model to ask, instead of the client's default.
    pub fn model(mut self, model: impl Into<Cow<'a, str>>) -> Self {
        self.model = Some(model.into());
        self
    }

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

    /// A header for this call only. It replaces a client default of the same
    /// name; the SDK's own headers still win over it, as they do over a
    /// default, and a later header of the same name replaces an earlier one.
    /// The headers the SDK or its transport owns are dropped without an
    /// error, as they are from a default; see
    /// [`ClientBuilder::default_header`](crate::ClientBuilder::default_header)
    /// for the list, and for what `Host` does.
    pub fn header(mut self, name: impl Into<Cow<'a, str>>, value: impl Into<Cow<'a, str>>) -> Self {
        self.headers.push(name.into(), value.into());
        self
    }

    /// The retry policy of this call, in place of the client's; the client
    /// and its other calls keep theirs.
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = Some(policy);
        self
    }

    /// A top-level member of the request body beside `state`, `model` and
    /// `questions`, for a parameter the API has and this SDK does not model.
    ///
    /// Merging is last-write-wins, as in the Python SDK: a later member of
    /// the same name replaces an earlier one, and a member named `state`,
    /// `model` or `questions` replaces that built-in one in place. The value
    /// is encoded now; a value that cannot be encoded fails the call when it
    /// is sent.
    pub fn extra_body<V>(mut self, name: impl Into<Cow<'a, str>>, value: &V) -> Self
    where
        V: Serialize + ?Sized,
    {
        let mut encoded = Vec::new();
        let value = codec::encode_into(&mut encoded, value).map(|()| encoded);
        upsert(&mut self.extra, name.into(), value);
        self
    }

    /// Decodes the answers into `B` instead: a struct with one field per
    /// question, for instance, which reads each answer straight into its
    /// field.
    pub fn typed<B>(self) -> SystemOne<'a, S, T, B>
    where
        B: AnswerSet,
    {
        SystemOne {
            client: self.client,
            state: self.state,
            questions: self.questions,
            model: self.model,
            deadline: self.deadline,
            headers: self.headers,
            extra: self.extra,
            retry: self.retry,
            answers: PhantomData,
        }
    }
}

impl<S, T, A> SystemOne<'_, S, T, A>
where
    S: HttpService,
    T: Serialize + ?Sized,
    A: AnswerSet,
{
    /// Sends the request and decodes the answer, retrying a failure as the
    /// call's retry policy, or else the client's, says.
    ///
    /// The body is encoded once: every attempt sends the same bytes. When
    /// retrying stops, the error is the one the last attempt failed with.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest),
    ///   before anything is sent: a header that is not a valid header, a
    ///   deadline of zero, a `state` that is not a JSON string, object or
    ///   array, or a value that cannot be encoded as JSON.
    /// - [`ErrorKind::Api`](crate::ErrorKind::Api) for a status outside 2xx.
    /// - [`ErrorKind::Timeout`](crate::ErrorKind::Timeout) when the attempt
    ///   ran past its deadline.
    /// - [`ErrorKind::Connection`](crate::ErrorKind::Connection) when no
    ///   response could be read: the connection failed or broke.
    /// - [`ErrorKind::ResponseTooLarge`](crate::ErrorKind::ResponseTooLarge)
    ///   when a success response's body was larger than the client's limit.
    /// - [`ErrorKind::ResponseValidation`](crate::ErrorKind::ResponseValidation)
    ///   when the body does not decode into `A`.
    pub async fn send(self) -> Result<SystemOneResponse<A>, Error> {
        let shared = self.client.shared();
        let deadline = self.deadline.resolve(shared.config.timeout())?;
        let headers = self.headers.parse(true)?;
        let mut body = self.encode()?;

        let uri = shared.config.endpoints().system_one();
        let exchange = Exchange {
            method: &Method::POST,
            uri,
            base_headers: &shared.post_headers,
            call_headers: &headers,
            deadline,
            config: &shared.config,
        };
        let policy = self.retry.as_ref().unwrap_or(&shared.retry);
        // Every attempt after the first shares these bytes; the first clone
        // allocates the reference count they are shared through, so a call
        // that cannot retry hands its one attempt the body itself.
        let retain = policy.can_retry();
        let asked =
            de::AnswerContext::new(self.questions.len()).with_levels(self.questions.max_levels());
        retry::run(
            policy,
            &Method::POST,
            shared.config.endpoints().system_one_log(),
            move |retry| {
                let body = if retain { body.clone() } else { std::mem::take(&mut body) };
                async move {
                    let (status, headers, body) =
                        transport::attempt(&shared.service, exchange, retry, Some(body)).await?;
                    // Decoding is part of the attempt, so a retry predicate sees a
                    // response that did not decode, as the Python SDK's does.
                    de::decode_system_one_with(
                        body,
                        status,
                        headers,
                        asked,
                        Some((&Method::POST, uri)),
                    )
                }
            },
        )
        .await
    }

    /// The body: `{"state":<state>,"model":<model>,"questions":<questions>}` and the extra members,
    /// spliced out of finished JSON.
    fn encode(&self) -> Result<Bytes, Error> {
        for (name, value) in &self.extra {
            if let Err(error) = value {
                let part = format!("the extra member {}", text::quoted(name));
                return Err(encode_failure(&part, error));
            }
        }
        let extra = |wanted: &str| {
            self.extra.iter().find_map(|(name, value)| match value {
                Ok(bytes) if name == wanted => Some(bytes.as_slice()),
                _ => None,
            })
        };

        let mut state_is_json_content = true;
        let body = codec::encode_body(|buffer| {
            buffer.extend_from_slice(br#"{"state":"#);
            match extra(STATE) {
                Some(bytes) => buffer.extend_from_slice(bytes),
                None => {
                    let start = buffer.len();
                    codec::encode_into(buffer, self.state)?;
                    // The API takes text, an object or an array. The first
                    // byte of the encoding says which it is, so the check
                    // costs nothing whatever the state's size.
                    if !matches!(buffer.get(start), Some(b'"' | b'{' | b'[')) {
                        state_is_json_content = false;
                        return Ok(());
                    }
                }
            }
            buffer.extend_from_slice(br#","model":"#);
            match (extra(MODEL), &self.model) {
                (Some(bytes), _) => buffer.extend_from_slice(bytes),
                (None, Some(model)) => codec::write_json_string(buffer, model),
                (None, None) => buffer.extend_from_slice(&self.client.shared().model_json),
            }
            buffer.extend_from_slice(br#","questions":"#);
            buffer.extend_from_slice(extra(QUESTIONS).unwrap_or(self.questions.as_bytes()));
            for (name, value) in &self.extra {
                if let (Ok(bytes), false) = (value, [STATE, MODEL, QUESTIONS].contains(&&**name)) {
                    buffer.push(b',');
                    codec::write_json_string(buffer, name);
                    buffer.push(b':');
                    buffer.extend_from_slice(bytes);
                }
            }
            buffer.push(b'}');
            Ok(())
        });
        let body = body.map_err(|error| encode_failure("the state", &error))?;
        if !state_is_json_content {
            return Err(Error::invalid_request(
                "The state must be a JSON string, object or array; \
                 it encoded as a number, a boolean or null.",
            ));
        }
        Ok(body)
    }
}

/// The error for a part of the body that could not be encoded.
fn encode_failure(part: &str, error: &EncodeError) -> Error {
    // The encoder's message can be the caller's own `Serialize` error, of any
    // length and content.
    Error::invalid_request(format!(
        "The request body could not be encoded as JSON: {part}: {}",
        text::bounded(&error.message(), text::MAX_MESSAGE_CHARS)
    ))
}

impl<S, T, A> fmt::Debug for SystemOne<'_, S, T, A>
where
    T: ?Sized,
{
    /// The request's settings: no state, no header value and no body member,
    /// which are the caller's data. A retry policy is shown when the call has
    /// one of its own.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut shown = formatter.debug_struct("SystemOne");
        shown
            .field("questions", &self.questions.len())
            .field("model", &self.model)
            .field("deadline", &self.deadline)
            .field("headers", &self.headers)
            .field("extra_body", &self.extra.iter().map(|(name, _)| name).collect::<Vec<_>>());
        if let Some(retry) = &self.retry {
            shown.field("retry", retry);
        }
        shown.finish_non_exhaustive()
    }
}

#[cfg(all(test, feature = "hyper"))]
#[path = "request_tests.rs"]
mod tests;
