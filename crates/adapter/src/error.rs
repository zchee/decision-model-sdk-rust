//! The adapter's error and its kinds.
//!
//! Ported from `_utils/error_handling.py` of system-one-adapter-python.
//!
//! [`Error`] is one pointer wide, as the SDK's is: the kind, the sentence, the
//! trace of the attempts and the cause live behind one box. `Display` and
//! `source()` depend on the kind, which is why the trait implementations are
//! written out here rather than derived.

use std::{error::Error as StdError, fmt};

use crate::{provider::NonAnswer, response::Trace};

/// What an [`Error`] was caused by, when the adapter could not attribute the
/// failure to itself.
pub(crate) type Cause = Box<dyn StdError + Send + Sync>;

/// Anything a call through the adapter can fail with.
///
/// Match on [`kind`](Error::kind) to tell the failures apart. `Display` is the
/// sentence to show a user; it holds no model output and no response text.
/// [`debug`](Error::debug) holds the trace of every attempt made before the
/// failure, which does hold the caller's document and the model's text.
pub struct Error(Box<Inner>);

/// The heap half of [`Error`].
struct Inner {
    kind: ErrorKind,
    /// The sentence of the kinds that carry no payload of their own; empty for
    /// [`ErrorKind::Provider`] and [`ErrorKind::NonAnswer`], which render from
    /// their payload.
    message: Box<str>,
    /// The attempts made before the failure; `None` when it happened before
    /// the first attempt.
    trace: Option<Trace>,
    /// The failure underneath, for the kinds whose payload is not one.
    source: Option<Cause>,
}

/// Which kind of failure an [`Error`] is.
///
/// New variants may be added, so a `match` over this enum needs a catch-all
/// arm.
#[derive(Debug)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The model request failed, and the retry policy gave up: the SDK's
    /// error of kind `Api`, `Connection`, `Timeout` or `ResponseTooLarge`.
    Provider(typesafe_sdk::Error),
    /// The vendor answered with a success status, but declared the reply
    /// unfinished or refused, or sent a body that is not its JSON.
    NonAnswer(NonAnswer),
    /// The model's reply still did not match the output schema after the
    /// last corrective retry.
    MalformedStructure,
    /// The call cannot be made as asked: no model, no provider, an unknown
    /// question type, a choice or score with fewer than two criteria, or a
    /// state that serializes to `null`.
    InvalidRequest,
    /// A provider could not be built: no key, a key that cannot be a header
    /// value, a base URL the adapter refuses, a zero limit or timeout, or the
    /// TLS roots.
    Config,
}

impl ErrorKind {
    /// A fixed name for the kind, for an attempt's `error_type` and the
    /// adapter's events: the SDK's kind for a provider failure, the adapter's
    /// own otherwise.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Provider(error) => match error.kind() {
                typesafe_sdk::ErrorKind::Api(_) => "Api",
                typesafe_sdk::ErrorKind::Connection => "Connection",
                typesafe_sdk::ErrorKind::Timeout { .. } => "Timeout",
                typesafe_sdk::ErrorKind::ResponseTooLarge { .. } => "ResponseTooLarge",
                typesafe_sdk::ErrorKind::ResponseValidation(_) => "ResponseValidation",
                typesafe_sdk::ErrorKind::InvalidRequest => "InvalidRequest",
                typesafe_sdk::ErrorKind::Config => "Config",
                _ => "Provider",
            },
            Self::NonAnswer(_) => "NonAnswer",
            Self::MalformedStructure => "MalformedStructure",
            Self::InvalidRequest => "InvalidRequest",
            Self::Config => "Config",
        }
    }
}

impl Error {
    /// Which kind of failure this is.
    #[must_use]
    pub fn kind(&self) -> &ErrorKind {
        &self.0.kind
    }

    /// The attempts made before the failure and the reasons each retry was
    /// made, or `None` when the call failed before its first attempt.
    ///
    /// Serialized, it has upstream's two members, `llm_attempts` and
    /// `retry_reasons`. It holds the caller's document and the model's text.
    #[must_use]
    pub fn debug(&self) -> Option<&Trace> {
        self.0.trace.as_ref()
    }

    /// The model request failed with the SDK's `error`, which becomes the
    /// `source()`.
    pub(crate) fn provider(error: typesafe_sdk::Error) -> Self {
        Self::new(ErrorKind::Provider(error), "")
    }

    /// The vendor's success response is not an answer.
    pub(crate) fn non_answer(non_answer: NonAnswer) -> Self {
        Self::new(ErrorKind::NonAnswer(non_answer), "")
    }

    /// The reply is still invalid after the last corrective retry; `message`
    /// is the validation text the correction prompt carried.
    pub(crate) fn malformed_structure(message: impl Into<Box<str>>) -> Self {
        Self::new(ErrorKind::MalformedStructure, message)
    }

    /// The call cannot be made as asked.
    pub(crate) fn invalid_request(message: impl Into<Box<str>>) -> Self {
        Self::new(ErrorKind::InvalidRequest, message)
    }

    /// A provider could not be built.
    pub(crate) fn config(message: impl Into<Box<str>>) -> Self {
        Self::new(ErrorKind::Config, message)
    }

    /// The same error with `source` as the failure underneath it: the decode
    /// failure of a malformed reply, or what a provider could not be built
    /// from. A provider failure's source is its SDK error and is not replaced.
    #[must_use]
    pub(crate) fn with_source(mut self, source: Cause) -> Self {
        self.0.source = Some(source);
        self
    }

    /// The same error with the trace of the attempts made before it.
    #[must_use]
    pub(crate) fn with_trace(mut self, trace: Trace) -> Self {
        self.0.trace = Some(trace);
        self
    }

    fn new(kind: ErrorKind, message: impl Into<Box<str>>) -> Self {
        Self(Box::new(Inner { kind, message: message.into(), trace: None, source: None }))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0.kind {
            ErrorKind::Provider(error) => error.fmt(formatter),
            ErrorKind::NonAnswer(non_answer) => non_answer.fmt(formatter),
            ErrorKind::MalformedStructure | ErrorKind::InvalidRequest | ErrorKind::Config => {
                formatter.write_str(&self.0.message)
            }
        }
    }
}

impl fmt::Debug for Error {
    /// The kind and the trace's counts, never the sentence of a malformed
    /// reply (it is a retry reason's message) nor anything the trace holds.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Error")
            .field("kind", &self.0.kind)
            .field("debug", &self.0.trace)
            .finish()
    }
}

impl StdError for Error {
    /// The SDK's error for a provider failure; the recorded cause otherwise,
    /// such as the decode failure of a malformed reply.
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match &self.0.kind {
            ErrorKind::Provider(error) => Some(error),
            _ => self.0.source.as_ref().map(|cause| &**cause as &(dyn StdError + 'static)),
        }
    }
}

#[cfg(test)]
#[path = "error_tests.rs"]
mod tests;
