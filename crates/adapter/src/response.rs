//! What a call returns: the answers, the token usage and a trace of every
//! attempt.
//!
//! Ported from `_response.py` of system-one-adapter-python.
//!
//! Every type here serializes to upstream's member names, so a serialized
//! response has the shape of upstream's `model_dump()`. None of them
//! deserializes. `Debug` prints counts and kinds only: the trace holds the
//! caller's document, the schema and the model's text.

use std::{fmt, time::Duration};

use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeStruct, SerializeTuple},
};
use serde_json::value::RawValue;
use typesafe_sdk::Answers;

use crate::provider::{AttemptTrace, ByteLen, Message, ProviderResult, Schema, raw_json};

/// The answers to one call, with the model, the token usage and the trace of
/// the attempts that produced them.
///
/// `A` is what the answers are: [`Answers`], a lookup by question name, or
/// the struct of a question set declared with `#[derive(QuestionSet)]`.
///
/// The serialized shape is defined for `serde_json`: the schema and the
/// recorded request and response bodies of the trace are raw JSON, which only
/// `serde_json` embeds as JSON.
#[non_exhaustive]
#[derive(Clone)]
pub struct Response<A = Answers> {
    pub(crate) model: String,
    pub(crate) usage: Usage,
    pub(crate) answers: A,
    /// The number of questions answered, which `Debug` prints in place of the
    /// answers of any `A`.
    pub(crate) n_answers: usize,
    pub(crate) debug: Trace,
}

impl<A> Response<A> {
    /// The model that answered.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The tokens the call used and how many retries it took.
    #[must_use]
    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// The answers, one per question.
    #[must_use]
    pub fn answers(&self) -> &A {
        &self.answers
    }

    /// Gives up everything but the answers.
    #[must_use]
    pub fn into_answers(self) -> A {
        self.answers
    }

    /// The trace of the call: every attempt, the reasons for each retry, and
    /// what probability normalization changed. It holds the caller's document
    /// and the model's text.
    #[must_use]
    pub fn debug(&self) -> &Trace {
        &self.debug
    }
}

impl<A> Serialize for Response<A>
where
    A: Serialize,
{
    /// Upstream's members, in its order: `model`, `usage`, `answers`, `debug`.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("Response", 4)?;
        out.serialize_field("model", &self.model)?;
        out.serialize_field("usage", &self.usage)?;
        out.serialize_field("answers", &self.answers)?;
        out.serialize_field("debug", &self.debug)?;
        out.end()
    }
}

impl<A> fmt::Debug for Response<A> {
    /// The model, the usage, the number of answers and the trace's counts.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Response")
            .field("model", &self.model)
            .field("usage", &self.usage)
            .field("answers", &self.n_answers)
            .field("debug", &self.debug)
            .finish()
    }
}

/// The tokens a call used, and how many retries it took.
///
/// A token count the provider did not report is `None`, never zero. The
/// totals become `None` as soon as any attempt reported no count.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Usage {
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) input_tokens_total: Option<u64>,
    pub(crate) output_tokens_total: Option<u64>,
    pub(crate) n_retries: u32,
    pub(crate) n_retries_malformed_structure: u32,
    pub(crate) latency: Duration,
}

impl Usage {
    /// The input tokens of the last attempt.
    #[must_use]
    pub fn input_tokens(&self) -> Option<u64> {
        self.input_tokens
    }

    /// The output tokens of the last attempt.
    #[must_use]
    pub fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }

    /// The input tokens of every attempt that returned a reply, or `None`
    /// once any of them reported none.
    #[must_use]
    pub fn input_tokens_total(&self) -> Option<u64> {
        self.input_tokens_total
    }

    /// The output tokens of every attempt that returned a reply, or `None`
    /// once any of them reported none.
    #[must_use]
    pub fn output_tokens_total(&self) -> Option<u64> {
        self.output_tokens_total
    }

    /// The retries the retry policy made after a failed request.
    #[must_use]
    pub fn n_retries(&self) -> u32 {
        self.n_retries
    }

    /// The corrective retries made after a reply that did not match the
    /// output schema.
    #[must_use]
    pub fn n_retries_malformed_structure(&self) -> u32 {
        self.n_retries_malformed_structure
    }

    /// The time from the provider being ready until the answers of the last
    /// reply are converted, as upstream stops its clock; serialized as
    /// seconds.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.latency
    }
}

impl Serialize for Usage {
    /// Upstream's members, in its order, with the latency in seconds.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("Usage", 7)?;
        out.serialize_field("input_tokens", &self.input_tokens)?;
        out.serialize_field("output_tokens", &self.output_tokens)?;
        out.serialize_field("input_tokens_total", &self.input_tokens_total)?;
        out.serialize_field("output_tokens_total", &self.output_tokens_total)?;
        out.serialize_field("n_retries", &self.n_retries)?;
        out.serialize_field("n_retries_malformed_structure", &self.n_retries_malformed_structure)?;
        out.serialize_field("latency", &self.latency.as_secs_f64())?;
        out.end()
    }
}

/// The trace of one call: every attempt, the reasons for each retry, and what
/// probability normalization found.
///
/// Named `Trace` rather than `Debug`, so it is not mistaken for
/// [`std::fmt::Debug`]; it is serialized under upstream's member name
/// `debug`. The trace of an [`Error`](crate::Error) has no probability part:
/// it serializes `llm_attempts` and `retry_reasons` only, and its probability
/// accessors return 0 and nothing.
///
/// The serialized shape is defined for `serde_json`: each attempt's schema,
/// request and response are raw JSON, which only `serde_json` embeds as JSON.
#[non_exhaustive]
#[derive(Clone)]
pub struct Trace {
    pub(crate) attempts: Vec<Attempt>,
    pub(crate) retry_reasons: Vec<RetryReason>,
    /// `None` for the trace of an error.
    pub(crate) probabilities: Option<ProbabilityDebug>,
}

impl Trace {
    /// Every provider call made, in order, the failed ones included
    /// (upstream's `llm_attempts`).
    #[must_use]
    pub fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }

    /// The reason for each retry, in order.
    #[must_use]
    pub fn retry_reasons(&self) -> &[RetryReason] {
        &self.retry_reasons
    }

    /// The largest distance of any question's probabilities from summing to
    /// 1, or 0 when no question has a distribution.
    #[must_use]
    pub fn max_error(&self) -> f64 {
        self.probabilities.as_ref().map_or(0.0, |probabilities| probabilities.max_error)
    }

    /// How many questions' probabilities missed summing to 1 by more than the
    /// tolerance of 1e-6.
    #[must_use]
    pub fn invalid_probs(&self) -> usize {
        self.probabilities.as_ref().map_or(0, |probabilities| probabilities.invalid_probs)
    }

    /// Each question whose probabilities missed summing to 1 by more than the
    /// tolerance, with that distance, in question order.
    #[must_use]
    pub fn probability_errors(&self) -> impl ExactSizeIterator<Item = (&str, f64)> {
        self.probabilities
            .as_ref()
            .map_or(&[][..], |probabilities| &probabilities.probability_errors)
            .iter()
            .map(|(question, error)| (question.as_str(), *error))
    }

    /// Each question whose probabilities normalization rescaled, with the
    /// probabilities the model gave before it, label by label, in question
    /// order. Empty unless normalization is on.
    #[must_use]
    pub fn original_probabilities(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, impl ExactSizeIterator<Item = (&str, f64)>)> {
        self.probabilities
            .as_ref()
            .map_or(&[][..], |probabilities| &probabilities.original_probabilities)
            .iter()
            .map(|(question, labels)| {
                (
                    question.as_str(),
                    labels.iter().map(|(label, probability)| (label.as_str(), *probability)),
                )
            })
    }
}

impl Serialize for Trace {
    /// Upstream's members, in its order: the probability members (absent from
    /// an error's trace; `original_probabilities` only when not empty), then
    /// `llm_attempts` and `retry_reasons`.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_map(None)?;
        if let Some(probabilities) = &self.probabilities {
            out.serialize_entry("max_error", &probabilities.max_error)?;
            out.serialize_entry("invalid_probs", &probabilities.invalid_probs)?;
            out.serialize_entry("probability_errors", &Pairs(&probabilities.probability_errors))?;
            if !probabilities.original_probabilities.is_empty() {
                out.serialize_entry(
                    "original_probabilities",
                    &Nested(&probabilities.original_probabilities),
                )?;
            }
        }
        out.serialize_entry("llm_attempts", &self.attempts)?;
        out.serialize_entry("retry_reasons", &self.retry_reasons)?;
        out.end()
    }
}

impl fmt::Debug for Trace {
    /// The number of attempts, the retry categories and the probability
    /// counts.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let categories =
            self.retry_reasons.iter().map(|reason| reason.category).collect::<Vec<_>>();
        formatter
            .debug_struct("Trace")
            .field("attempts", &self.attempts.len())
            .field("retry_reasons", &categories)
            .field("max_error", &self.max_error())
            .field("invalid_probs", &self.invalid_probs())
            .finish()
    }
}

/// What probability normalization found over a response's questions
/// (upstream's `probability_debug_data`).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct ProbabilityDebug {
    pub(crate) max_error: f64,
    pub(crate) invalid_probs: usize,
    /// Question id and distance from 1, for the distances over the tolerance.
    pub(crate) probability_errors: Vec<(String, f64)>,
    /// Question id and the label-by-label probabilities before rescaling.
    pub(crate) original_probabilities: Vec<(String, Vec<(String, f64)>)>,
}

/// A list of pairs serialized as a JSON object.
struct Pairs<'a>(&'a [(String, f64)]);

impl Serialize for Pairs<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
    }
}

/// A list of pair lists serialized as a JSON object of JSON objects.
struct Nested<'a>(&'a [(String, Vec<(String, f64)>)]);

impl Serialize for Nested<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_map(self.0.iter().map(|(key, pairs)| (key, Pairs(pairs))))
    }
}

/// One provider call: what was sent, what came back, and how it ended.
#[non_exhaustive]
#[derive(Clone)]
pub struct Attempt {
    pub(crate) messages: Vec<Message>,
    pub(crate) schema: Schema,
    pub(crate) structured: bool,
    /// The recorded response body, else the reply's text and token counts;
    /// `None` (serialized `null`) for a failed attempt that recorded none.
    pub(crate) llm_response: Option<Box<RawValue>>,
    pub(crate) model_name: String,
    /// The provider's `type_name()`.
    pub(crate) provider: String,
    /// Present once the provider recorded its request.
    pub(crate) api: Option<&'static str>,
    /// `None` until the provider recorded a response; then the stop reason,
    /// which may itself be absent.
    pub(crate) finish_reason: Option<Option<String>>,
    /// The failure's text, for a failed attempt.
    pub(crate) error: Option<String>,
    /// The failure's kind name, for a failed attempt.
    pub(crate) error_type: Option<String>,
    /// The recorded request body.
    pub(crate) request: Option<Box<RawValue>>,
}

impl Attempt {
    /// An attempt about to be made, before anything was exchanged.
    pub(crate) fn new(
        messages: Vec<Message>,
        schema: Schema,
        structured: bool,
        model_name: String,
        provider: String,
    ) -> Self {
        Self {
            messages,
            schema,
            structured,
            llm_response: None,
            model_name,
            provider,
            api: None,
            finish_reason: None,
            error: None,
            error_type: None,
            request: None,
        }
    }

    /// Takes over what the provider recorded during the attempt.
    pub(crate) fn record(&mut self, trace: AttemptTrace) {
        let AttemptTrace { request, api, response, finish_reason } = trace;
        self.request = request;
        self.api = api;
        self.llm_response = response;
        self.finish_reason = finish_reason;
    }

    /// Records the provider's result as the response when the provider
    /// recorded no response body itself (upstream's fallback).
    pub(crate) fn record_result(&mut self, result: &ProviderResult) {
        if self.llm_response.is_some() {
            return;
        }
        /// Upstream's `asdict(result)`, members in its order; a `json!` map
        /// would sort them.
        #[derive(Serialize)]
        struct Fallback<'a> {
            text: &'a str,
            input_tokens: Option<u64>,
            output_tokens: Option<u64>,
        }
        let fallback = Fallback {
            text: result.text(),
            input_tokens: result.input_tokens(),
            output_tokens: result.output_tokens(),
        };
        let json =
            serde_json::to_string(&fallback).expect("invariant: a string and two counts serialize");
        self.llm_response = Some(raw_json(&json));
    }

    /// Marks the attempt failed, with the failure's text and kind name.
    pub(crate) fn record_error(&mut self, error: impl Into<String>, error_type: impl Into<String>) {
        self.error = Some(error.into());
        self.error_type = Some(error_type.into());
    }

    /// The messages sent.
    #[must_use]
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The output schema sent.
    #[must_use]
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Whether the vendor's structured-output mode carried the schema.
    #[must_use]
    pub fn structured(&self) -> bool {
        self.structured
    }

    /// The JSON body the provider sent, when it recorded one.
    #[must_use]
    pub fn request(&self) -> Option<&str> {
        self.request.as_deref().map(RawValue::get)
    }

    /// The `llm_response` member as JSON text: the body the provider
    /// received, else the reply's text and token counts; `None` for a failed
    /// attempt that recorded no response.
    #[must_use]
    pub fn response(&self) -> Option<&str> {
        self.llm_response.as_deref().map(RawValue::get)
    }

    /// The model the provider asked.
    #[must_use]
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// The provider's [`type_name`](crate::Provider::type_name): by default
    /// the compiler's name of its type, which is not promised stable; the
    /// built-in providers give their public path.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The vendor API the request went to, when the provider recorded its
    /// request.
    #[must_use]
    pub fn api(&self) -> Option<&str> {
        self.api
    }

    /// The vendor's stop reason, when the provider recorded a response that
    /// gave one.
    #[must_use]
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_ref().and_then(Option::as_deref)
    }

    /// The failure's text, for a failed attempt.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The failure's kind name, for a failed attempt.
    #[must_use]
    pub fn error_type(&self) -> Option<&str> {
        self.error_type.as_deref()
    }
}

impl Serialize for Attempt {
    /// Upstream's members, in its order: `messages`,
    /// `model_request_parameters`, `llm_response`, `debug_info`, and `request`
    /// when one was recorded.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_map(None)?;
        out.serialize_entry("messages", &self.messages)?;
        out.serialize_entry("model_request_parameters", &RequestParameters(self))?;
        out.serialize_entry("llm_response", &self.llm_response)?;
        out.serialize_entry("debug_info", &DebugInfo(self))?;
        if let Some(request) = &self.request {
            out.serialize_entry("request", request)?;
        }
        out.end()
    }
}

impl fmt::Debug for Attempt {
    /// Counts and kinds: the roles, the lengths of the schema and the bodies,
    /// the model, the provider, the api, the stop reason and the failure's
    /// kind name, never the failure's text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let roles = self.messages.iter().map(Message::role).collect::<Vec<_>>();
        formatter
            .debug_struct("Attempt")
            .field("messages", &roles)
            .field("schema", &ByteLen(self.schema.as_str().len()))
            .field("structured", &self.structured)
            .field("model_name", &self.model_name)
            .field("provider", &self.provider)
            .field("api", &self.api)
            .field("finish_reason", &self.finish_reason)
            .field("error_type", &self.error_type)
            .field("request", &self.request.as_ref().map(|json| ByteLen(json.get().len())))
            .field(
                "llm_response",
                &self.llm_response.as_ref().map(|json| ByteLen(json.get().len())),
            )
            .finish()
    }
}

/// An attempt's `model_request_parameters` member.
struct RequestParameters<'a>(&'a Attempt);

impl Serialize for RequestParameters<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_map(Some(2))?;
        out.serialize_entry("schema", &self.0.schema)?;
        out.serialize_entry("structured", &self.0.structured)?;
        out.end()
    }
}

/// An attempt's `debug_info` member: `model_name` and `provider` always, `api`
/// once a request was recorded, `finish_reason` once a response was, `error`
/// and `error_type` on failure.
struct DebugInfo<'a>(&'a Attempt);

impl Serialize for DebugInfo<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let attempt = self.0;
        let mut out = serializer.serialize_map(None)?;
        out.serialize_entry("model_name", &attempt.model_name)?;
        out.serialize_entry("provider", &attempt.provider)?;
        if let Some(api) = attempt.api {
            out.serialize_entry("api", api)?;
        }
        if let Some(finish_reason) = &attempt.finish_reason {
            out.serialize_entry("finish_reason", finish_reason)?;
        }
        if let Some(error) = &attempt.error {
            out.serialize_entry("error", error)?;
        }
        if let Some(error_type) = &attempt.error_type {
            out.serialize_entry("error_type", error_type)?;
        }
        out.end()
    }
}

/// Why one retry was made.
///
/// Serialized as the pair `[category, message]` upstream emits.
#[non_exhaustive]
#[derive(Clone)]
pub struct RetryReason {
    pub(crate) category: RetryCategory,
    pub(crate) message: String,
}

impl RetryReason {
    /// A retry for `category`, described by `message`.
    pub(crate) fn new(category: RetryCategory, message: impl Into<String>) -> Self {
        Self { category, message: message.into() }
    }

    /// Which mechanism retried.
    #[must_use]
    pub fn category(&self) -> RetryCategory {
        self.category
    }

    /// The cause: the failed request's error, or the validation text the
    /// correction prompt carried.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Serialize for RetryReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_tuple(2)?;
        out.serialize_element(&self.category)?;
        out.serialize_element(&self.message)?;
        out.end()
    }
}

impl fmt::Debug for RetryReason {
    /// The category and the length of the message, never the message.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetryReason")
            .field("category", &self.category)
            .field("message", &ByteLen(self.message.len()))
            .finish()
    }
}

/// The mechanism that made a retry; serialized as upstream's
/// `provider_error` and `malformed_structure`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryCategory {
    /// The retry policy retried a failed request.
    ProviderError,
    /// A corrective retry followed a reply that did not match the output
    /// schema.
    MalformedStructure,
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
