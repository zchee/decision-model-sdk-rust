//! The provider seam: the trait a model behind the adapter implements, the
//! call it receives and the result it returns, and the built-in providers.
//!
//! Ported from `providers/base.py` of system-one-adapter-python.

#[cfg(feature = "anthropic")]
pub(crate) mod anthropic;
pub(crate) mod factory;
#[cfg(feature = "gemini")]
pub(crate) mod gemini;
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
pub(crate) mod http;
#[cfg(feature = "openai")]
pub(crate) mod openai;

use std::{fmt, pin::Pin, str::FromStr};

use serde::{Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;

use crate::error::Error;

/// A future that is `Send` and boxed, so that [`Provider`] stays
/// dyn-compatible: one allocation per model call.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A model the adapter can ask: one request in, one reply's text and token
/// counts out.
///
/// The client owns everything around the call: the schema, the prompts, the
/// decoding of the reply, the corrective retries and the retry policy. A
/// provider performs exactly one model request per call of
/// [`request`](Self::request).
///
/// The trait is dyn-compatible, so a client holds every provider, a built-in
/// one or the caller's, as `Arc<dyn Provider>`; that is why
/// [`request`](Self::request) returns a [`BoxFuture`] rather than being an
/// `async fn`.
///
/// The result has two layers. The outer `Err` is a failure of the exchange
/// itself, an API error, a connection failure, a timeout or a body over the
/// size cap, which the retry policy may retry. The inner `Err` is a
/// [`NonAnswer`]: the vendor answered with a success status but declared the
/// reply unfinished or refused, which no retry of the same request fixes.
pub trait Provider: Send + Sync + fmt::Debug {
    /// The model this provider asks, as recorded in the trace and in the
    /// response.
    fn model_name(&self) -> &str;

    /// Sends one request built from `call` and returns the model's reply.
    ///
    /// A provider that talks to a vendor records the JSON it sent and the
    /// JSON it received through [`ProviderCall::trace`], so the attempt's
    /// trace holds both.
    ///
    /// The adapter prints what this returns as it is, in its error and in
    /// the trace: a [`NonAnswer`]'s message, and the `Display` of an error,
    /// including the message an [`ApiError`](decision_model_sdk::ApiError) reads
    /// out of a response body. The built-in providers search these texts for
    /// their key; a provider of your own searches them for its key or
    /// returns fixed texts.
    fn request<'a>(
        &'a self,
        call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, decision_model_sdk::Error>>;

    /// The name recorded as an attempt's `debug_info.provider`.
    ///
    /// The default is [`std::any::type_name`] of the implementing type, whose
    /// text the compiler does not promise to keep stable across versions; it
    /// is for diagnostics, not for matching. The built-in providers override
    /// it with their public path, such as
    /// `decision_model_adapter::OpenAiProvider`. A provider that wraps another
    /// may return a name it builds at run time.
    fn type_name(&self) -> &str {
        std::any::type_name::<Self>()
    }

    /// The URI that names this provider's endpoint in the retry policy's log
    /// line: the line the SDK writes before each retry when the SDK's
    /// `tracing` feature is on, which this crate's `tracing` feature turns
    /// on. The URI is only printed, never used to send a request.
    ///
    /// The type is `Uri` of the `http` crate, version 1. This crate does not
    /// re-export `http`, so a provider that overrides the method depends on
    /// `http` itself.
    ///
    /// It should hold the scheme, the host, the port when it is not the
    /// scheme's default, and the vendor's own path of the operation, such as
    /// `/v1/responses`. The path is printed as it is, so it must hold no
    /// credential in a path segment and no path prefix taken from a caller's
    /// base URL. A query and userinfo are not printed.
    ///
    /// The default is `None`, for a provider with no endpoint worth naming,
    /// such as a fake or an in-process model; the line then names the
    /// request as `POST /`. The built-in providers return their vendor
    /// endpoint. A provider that wraps another should return the inner
    /// provider's value; the default does not forward it.
    fn log_uri(&self) -> Option<&::http::Uri> {
        None
    }
}

/// What one model request is asked with.
///
/// The messages, the output schema and whether the vendor's structured-output
/// mode is used are fixed by the client; the trace is where the provider
/// writes what it sent and received.
#[non_exhaustive]
pub struct ProviderCall<'a> {
    messages: &'a [Message],
    schema: &'a Schema,
    structured: bool,
    trace: &'a mut AttemptTrace,
}

impl<'a> ProviderCall<'a> {
    /// A call over these parts, for a caller that tests its own provider
    /// outside the client.
    #[must_use]
    pub fn new(
        messages: &'a [Message],
        schema: &'a Schema,
        structured: bool,
        trace: &'a mut AttemptTrace,
    ) -> Self {
        Self { messages, schema, structured, trace }
    }

    /// The conversation to send, in order: the system prompt, the document,
    /// and any corrective turns.
    #[must_use]
    pub fn messages(&self) -> &'a [Message] {
        self.messages
    }

    /// The JSON schema the reply must follow.
    #[must_use]
    pub fn schema(&self) -> &'a Schema {
        self.schema
    }

    /// Whether the vendor's structured-output mode carries the schema
    /// (`true`), or the schema is already written into the system prompt
    /// (`false`).
    #[must_use]
    pub fn structured(&self) -> bool {
        self.structured
    }

    /// The record of this attempt, where the provider writes the request and
    /// the response it exchanged.
    pub fn trace(&mut self) -> &mut AttemptTrace {
        self.trace
    }
}

impl fmt::Debug for ProviderCall<'_> {
    /// Counts and kinds only: the messages and the schema hold the caller's
    /// document and questions.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderCall")
            .field("messages", &self.messages.len())
            .field("schema", &ByteLen(self.schema.as_str().len()))
            .field("structured", &self.structured)
            .field("trace", &self.trace)
            .finish()
    }
}

/// One chat message, provider-neutral.
///
/// Serialized as `{"role":..,"content":..}`, the form an attempt's trace
/// records, and read back from it, so an attempt can be replayed from its
/// serialized trace.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    role: Role,
    content: String,
}

impl Message {
    /// A message from `role` with this text.
    #[must_use]
    pub fn new(role: Role, content: impl Into<String>) -> Self {
        Self { role, content: content.into() }
    }

    /// Who the message is from.
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// The text of the message.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }
}

impl fmt::Debug for Message {
    /// The role and the length of the text, never the text: it holds the
    /// caller's document or the model's reply.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Message")
            .field("role", &self.role)
            .field("content", &ByteLen(self.content.len()))
            .finish()
    }
}

/// Who a [`Message`] is from; serialized in lower case.
///
/// The enum is exhaustive on purpose: a new role is a protocol change that
/// every provider must map to its vendor's roles, so adding one is a major
/// release rather than a variant a provider could silently not handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// The instructions the client gives the model.
    System,
    /// The document, or a request to correct the previous reply.
    User,
    /// A reply of the model, echoed back before a correction.
    Assistant,
}

/// The model's reply text and the token counts the vendor reported.
///
/// A count the vendor did not report is `None`, never zero.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderResult {
    text: String,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl ProviderResult {
    /// A reply with this text and these token counts.
    #[must_use]
    pub fn new(text: String, input_tokens: Option<u64>, output_tokens: Option<u64>) -> Self {
        Self { text, input_tokens, output_tokens }
    }

    /// The reply's text, which the client decodes as the answers.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The input tokens the request was billed for.
    #[must_use]
    pub fn input_tokens(&self) -> Option<u64> {
        self.input_tokens
    }

    /// The output tokens the reply was billed for.
    #[must_use]
    pub fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }
}

impl fmt::Debug for ProviderResult {
    /// The token counts and the length of the text, never the text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderResult")
            .field("text", &ByteLen(self.text.len()))
            .field("input_tokens", &self.input_tokens)
            .field("output_tokens", &self.output_tokens)
            .finish()
    }
}

/// A success status whose reply is not an answer: the vendor declared it
/// unfinished or refused, or the body is not the vendor's JSON.
///
/// The message is printed by `Display` and `Debug` and becomes the text of
/// the adapter's error. A built-in provider's message names the vendor and
/// the stop reason or status, escaped and cut, and never carries the model's
/// text or an error message from the body; those stay in the attempt's
/// recorded response. When the response repeats the API key, a built-in
/// provider's message is the fixed sentence `<Vendor> did not answer: the
/// reason is not shown, because showing it could reveal the API key.`
///
/// The adapter does not search the non-answer a provider of your own
/// returns, so [`NonAnswer::new`] takes a fixed text, such as the vendor's
/// name and a status, never text from a response, which can hold the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonAnswer {
    message: String,
}

impl NonAnswer {
    /// A non-answer described by `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for NonAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// The JSON schema a reply must follow, kept as its JSON text.
///
/// Serializing it with `serde_json` embeds the JSON unchanged. Two schemas
/// are equal when their texts are.
#[derive(Clone)]
pub struct Schema {
    json: Box<RawValue>,
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Schema {}

impl Schema {
    /// The schema written as `json`, which must be one JSON object.
    ///
    /// White space around the object is dropped; the object's own text is
    /// kept as it is.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
    /// error when `json` is not valid JSON or is not an object.
    pub fn from_json(json: &str) -> Result<Schema, Error> {
        Self::from_string(json.to_owned())
    }

    /// The schema as JSON text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.json.get()
    }

    /// [`from_json`](Self::from_json) without copying `json`, for the schema
    /// the client writes itself.
    pub(crate) fn from_string(json: String) -> Result<Self, Error> {
        let json = RawValue::from_string(json).map_err(|error| {
            Error::invalid_request(format!("The schema is not valid JSON: {error}."))
        })?;
        if !json.get().starts_with('{') {
            return Err(Error::invalid_request("The schema must be a JSON object."));
        }
        Ok(Self { json })
    }
}

impl Serialize for Schema {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.json.serialize(serializer)
    }
}

impl fmt::Debug for Schema {
    /// The length of the schema, never its text: it names the caller's
    /// questions and labels.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Schema").field("json", &ByteLen(self.as_str().len())).finish()
    }
}

/// What a provider exchanged with its vendor during one attempt.
///
/// A provider records the JSON body it sent with
/// [`record_request`](Self::record_request) before sending it, and the JSON
/// body it received with [`record_response`](Self::record_response) before
/// reading the reply out of it, so a reply that is then refused is still in
/// the trace. A provider that records nothing leaves both out; the client
/// then records the reply's text and token counts in the response's place.
#[derive(Default)]
pub struct AttemptTrace {
    /// The request body, when one was recorded.
    pub(crate) request: Option<Box<RawValue>>,
    /// The vendor API the request went to, recorded with the request.
    pub(crate) api: Option<&'static str>,
    /// The response body, when one was recorded.
    pub(crate) response: Option<Box<RawValue>>,
    /// `None` until a response is recorded; then the vendor's stop reason,
    /// which may itself be absent.
    pub(crate) finish_reason: Option<Option<String>>,
}

impl AttemptTrace {
    /// Records the JSON body of the request and the vendor API it is sent
    /// to (`responses`, `chat_completions`, `messages`, `interactions`).
    ///
    /// Text that is not one JSON value is kept as a JSON string holding it,
    /// so the serialized trace stays valid JSON.
    pub fn record_request(&mut self, json: &str, api: &'static str) {
        self.request = Some(raw_json(json));
        self.api = Some(api);
    }

    /// Records the JSON body of the response and the vendor's stop reason,
    /// when it gave one.
    ///
    /// Text that is not one JSON value is kept as a JSON string holding it.
    pub fn record_response(&mut self, json: &str, finish_reason: Option<&str>) {
        self.response = Some(raw_json(json));
        self.finish_reason = Some(finish_reason.map(str::to_owned));
    }

    /// The JSON body of the request, when one was recorded.
    #[must_use]
    pub fn request(&self) -> Option<&str> {
        self.request.as_deref().map(RawValue::get)
    }

    /// The vendor API the request went to, when a request was recorded.
    #[must_use]
    pub fn api(&self) -> Option<&str> {
        self.api
    }

    /// The JSON body of the response, when one was recorded.
    #[must_use]
    pub fn response(&self) -> Option<&str> {
        self.response.as_deref().map(RawValue::get)
    }

    /// The vendor's stop reason, when a recorded response gave one.
    #[must_use]
    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_ref().and_then(Option::as_deref)
    }
}

impl fmt::Debug for AttemptTrace {
    /// The api, the stop reason escaped and cut at 200 characters, and the
    /// lengths of the bodies, never the bodies.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let finish_reason = self
            .finish_reason
            .as_ref()
            .map(|reason| reason.as_deref().map(crate::response::StopReason));
        formatter
            .debug_struct("AttemptTrace")
            .field("api", &self.api)
            .field("request", &self.request.as_ref().map(|json| ByteLen(json.get().len())))
            .field("response", &self.response.as_ref().map(|json| ByteLen(json.get().len())))
            .field("finish_reason", &finish_reason)
            .finish()
    }
}

/// The built-in providers, by the names upstream selects them with.
///
/// A variant exists only when its cargo feature is on, so a client never
/// holds the name of a provider that is not compiled in. `Display` and
/// [`FromStr`] use `openai`, `anthropic` and `gemini`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderName {
    /// OpenAI, through the Responses API on `api.openai.com` and Chat
    /// Completions elsewhere.
    #[cfg(feature = "openai")]
    OpenAi,
    /// Anthropic, through the Messages API.
    #[cfg(feature = "anthropic")]
    Anthropic,
    /// Google Gemini, through the Interactions API.
    #[cfg(feature = "gemini")]
    Gemini,
}

impl ProviderName {
    /// Every provider compiled in, in the order upstream lists them.
    #[must_use]
    pub const fn all() -> &'static [ProviderName] {
        &[
            #[cfg(feature = "openai")]
            Self::OpenAi,
            #[cfg(feature = "anthropic")]
            Self::Anthropic,
            #[cfg(feature = "gemini")]
            Self::Gemini,
        ]
    }

    /// The name upstream selects the provider with: `openai`, `anthropic` or
    /// `gemini`; what `Display` prints and [`FromStr`] reads.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            #[cfg(feature = "openai")]
            Self::OpenAi => "openai",
            #[cfg(feature = "anthropic")]
            Self::Anthropic => "anthropic",
            #[cfg(feature = "gemini")]
            Self::Gemini => "gemini",
        }
    }
}

impl fmt::Display for ProviderName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProviderName {
    type Err = ParseProviderNameError;

    /// Reads `openai`, `anthropic` or `gemini`, exactly, when that provider is
    /// compiled in.
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::all()
            .iter()
            .copied()
            .find(|provider| provider.as_str() == name)
            .ok_or(ParseProviderNameError)
    }
}

/// A provider name that is not one of the providers compiled in.
///
/// The message lists the providers compiled in and does not repeat the name
/// it was given.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("Unknown provider. {}", compiled_in())]
pub struct ParseProviderNameError;

/// The sentence naming the providers compiled in, in upstream's wording.
fn compiled_in() -> String {
    let quoted =
        ProviderName::all().iter().map(|provider| format!("'{provider}'")).collect::<Vec<_>>();
    let listed = match quoted.as_slice() {
        [] => {
            return "No provider is compiled in; a provider whose feature is off is not available."
                .to_owned();
        }
        [one] => one.clone(),
        [first, second] => format!("{first} or {second}"),
        [init @ .., last] => format!("{}, or {last}", init.join(", ")),
    };
    format!("Use {listed}; a provider whose feature is off is not among them.")
}

/// A length printed as `<n bytes>`, for `Debug` output that must not print
/// the text it measures.
pub(crate) struct ByteLen(pub(crate) usize);

impl fmt::Debug for ByteLen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "<{} bytes>", self.0)
    }
}

/// `text` as a raw JSON value: unchanged when it is one JSON value, else a
/// JSON string holding it, so that a trace always serializes as valid JSON.
pub(crate) fn raw_json(text: &str) -> Box<RawValue> {
    RawValue::from_string(text.to_owned()).unwrap_or_else(|_| {
        let quoted =
            serde_json::to_string(text).expect("invariant: a string always serializes as JSON");
        RawValue::from_string(quoted).expect("invariant: a serialized string is one JSON value")
    })
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
