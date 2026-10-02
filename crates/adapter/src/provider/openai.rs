//! The OpenAI provider, for OpenAI's API and the services that speak it.
//!
//! Ported from `providers/openai.py` of system-one-adapter-python.
//!
//! Two vendor operations are spoken. The Responses API is OpenAI's own; Chat
//! Completions is what most OpenAI-compatible services offer. Which one a
//! provider uses is fixed when it is built: see [`OpenAiProviderBuilder::api`].
//!
//! There is no vendor SDK underneath: a request is the JSON this module
//! writes, sent through the shared HTTP module, and a reply is read from the
//! JSON the vendor returned.

use std::{borrow::Cow, fmt, time::Duration};

use ::http::{HeaderMap, Uri, header::AUTHORIZATION};
use bytes::Bytes;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use typesafe_sdk::HttpService;

use super::{
    AttemptTrace, BoxFuture, Message, NonAnswer, Provider, ProviderCall, ProviderResult, Role,
    Schema,
    http::{
        BaseUrl, Endpoint, Exchange, KeyHeader, Limits, Transport, env_var, key_header, non_answer,
        post, request_headers,
    },
};
use crate::error::Error;

/// The vendor's name in a non-answer.
const VENDOR: &str = "OpenAI";

/// The base URL when neither the builder nor the environment names one.
const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

/// The only host the Responses API is assumed on.
const OPENAI_HOST: &str = "api.openai.com";

/// The variable the key is read from when the builder was given none.
const KEY_VARIABLE: &str = "OPENAI_API_KEY";

/// The variable the base URL is read from when the builder was given none.
const BASE_URL_VARIABLE: &str = "OPENAI_BASE_URL";

/// The name the schema is sent under in the vendor's structured-output mode.
const SCHEMA_NAME: &str = "evaluation";

/// Which of OpenAI's two operations a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OpenAiApi {
    /// The Responses API, `POST /v1/responses`: OpenAI's own.
    Responses,
    /// Chat Completions, `POST /v1/chat/completions`: what an
    /// OpenAI-compatible service offers.
    ChatCompletions,
}

impl OpenAiApi {
    /// The name an attempt's trace records the api under.
    const fn name(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
        }
    }

    /// The operation's path under the base URL, and its fixed path as the
    /// vendor documents it, which is what a log names.
    const fn paths(self) -> (&'static str, &'static str) {
        match self {
            Self::Responses => ("/responses", "/v1/responses"),
            Self::ChatCompletions => ("/chat/completions", "/v1/chat/completions"),
        }
    }
}

/// The api a provider speaks when its builder names none: Responses when the
/// base URL's host is exactly `api.openai.com`, Chat Completions for every
/// other host. Letter case is not part of a host name, and upstream's HTTP
/// client lower-cases the host before it compares.
fn default_api(host: &str) -> OpenAiApi {
    if host.eq_ignore_ascii_case(OPENAI_HOST) {
        OpenAiApi::Responses
    } else {
        OpenAiApi::ChatCompletions
    }
}

// ------------------------------------------------------------ the provider

/// A model behind OpenAI's API, or behind a service that speaks it.
///
/// Built by [`OpenAiProvider::builder`]. Cloning it shares the connection
/// pool of its service. `Debug` prints the model, the base URL's host and the
/// api, never the key.
///
/// `S` is the service the requests go through:
/// [`Transport`](crate::Transport) unless the provider was built with
/// [`build_with_service`](OpenAiProviderBuilder::build_with_service).
#[derive(Clone)]
pub struct OpenAiProvider<S = Transport> {
    service: S,
    model: Box<str>,
    api: OpenAiApi,
    /// The base URL's host, kept for `Debug` only.
    host: Box<str>,
    endpoint: Endpoint,
    headers: HeaderMap,
    key: KeyHeader,
    limits: Limits,
}

impl OpenAiProvider {
    /// A builder for a provider that asks `model`.
    pub fn builder(model: impl Into<String>) -> OpenAiProviderBuilder {
        OpenAiProviderBuilder {
            model: model.into(),
            api_key: None,
            base_url: None,
            api: None,
            timeout: None,
            max_response_bytes: None,
            extra_roots: Vec::new(),
        }
    }
}

impl<S> OpenAiProvider<S> {
    /// What every attempt of this provider is sent with.
    fn exchange(&self) -> Exchange<'_> {
        Exchange {
            vendor: VENDOR,
            endpoint: &self.endpoint,
            headers: &self.headers,
            key: &self.key,
            limits: self.limits,
        }
    }

    /// The JSON body of one request.
    fn body(&self, messages: &[Message], schema: &Schema, structured: bool) -> String {
        let json = match self.api {
            OpenAiApi::Responses => serde_json::to_string(&ResponsesBody::new(
                &self.model,
                messages,
                schema,
                structured,
            )),
            OpenAiApi::ChatCompletions => serde_json::to_string(&ChatBody {
                model: &self.model,
                messages,
                response_format: response_format(schema, structured),
            }),
        };
        json.expect("invariant: a request body of strings and raw JSON always serializes")
    }
}

impl<S> fmt::Debug for OpenAiProvider<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiProvider")
            .field("model", &self.model)
            .field("host", &self.host)
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

impl<S> Provider for OpenAiProvider<S>
where
    S: HttpService,
{
    fn model_name(&self) -> &str {
        &self.model
    }

    fn request<'a>(
        &'a self,
        mut call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async move {
            let body = self.body(call.messages(), call.schema(), call.structured());
            call.trace().record_request(&body, self.api.name());
            let reply = match post(&self.service, self.exchange(), Bytes::from(body)).await? {
                Ok(reply) => reply,
                Err(not_json) => return Ok(Err(not_json)),
            };
            let outcome = match self.api {
                OpenAiApi::Responses => read_responses(&reply, call.trace()),
                OpenAiApi::ChatCompletions => read_chat(&reply, call.trace()),
            };
            Ok(self.exchange().screened(outcome, call.trace()))
        })
    }

    fn type_name(&self) -> &str {
        "system_one_adapter::OpenAiProvider"
    }

    fn log_uri(&self) -> Option<&Uri> {
        Some(self.endpoint.log_uri())
    }
}

// ------------------------------------------------------------- the builder

/// Builds an [`OpenAiProvider`]; made by [`OpenAiProvider::builder`].
///
/// `Debug` prints neither the key nor the base URL.
#[must_use = "a builder does nothing until `build` or `build_with_service` is called"]
#[derive(Clone)]
pub struct OpenAiProviderBuilder {
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
    api: Option<OpenAiApi>,
    timeout: Option<Duration>,
    max_response_bytes: Option<usize>,
    extra_roots: Vec<Vec<u8>>,
}

impl OpenAiProviderBuilder {
    /// The key sent as `authorization: Bearer <key>`.
    ///
    /// Unset, the key is the `OPENAI_API_KEY` environment variable, read
    /// once when the provider is built.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(SecretString::from(key.into()));
        self
    }

    /// The API root, such as `https://api.openai.com/v1`: the operation's
    /// path (`/responses` or `/chat/completions`) is appended to it, so it
    /// ends with the version segment. A trailing slash is dropped and a path
    /// prefix is kept.
    ///
    /// It must be an absolute `http` or `https` URL without userinfo, query
    /// or fragment. An `http://` base URL sends the key unencrypted; use it
    /// only for a local proxy or a test server.
    ///
    /// Unset, it is the `OPENAI_BASE_URL` environment variable, read once
    /// when the provider is built, else `https://api.openai.com/v1`.
    pub fn base_url(mut self, url: impl AsRef<str>) -> Self {
        self.base_url = Some(url.as_ref().to_owned());
        self
    }

    /// Which operation the provider speaks.
    ///
    /// Unset, it is [`OpenAiApi::Responses`] when the base URL's host is
    /// exactly `api.openai.com`, and [`OpenAiApi::ChatCompletions`] for every
    /// other host.
    pub fn api(mut self, api: OpenAiApi) -> Self {
        self.api = Some(api);
        self
    }

    /// The deadline of each whole attempt: connecting, sending, and reading
    /// the response. The default is 600 seconds.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The largest response body read, in bytes. The default is 16 MiB.
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Trusts `der`, a DER-encoded certificate, in addition to the operating
    /// system's roots, never instead of them.
    ///
    /// The default transport only; see
    /// [`build_with_service`](Self::build_with_service).
    pub fn add_root_certificate(mut self, der: impl Into<Vec<u8>>) -> Self {
        self.extra_roots.push(der.into());
        self
    }

    /// Builds the provider over the default [`Transport`](crate::Transport).
    ///
    /// Nothing connects until the first request.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error
    /// when:
    ///
    /// - no key was given and `OPENAI_API_KEY` is unset or empty, or the key
    ///   is empty or holds a character that cannot be sent in a header;
    /// - the base URL is not an absolute `http` or `https` URL, or carries
    ///   userinfo, a query or a fragment;
    /// - the timeout is zero or the response limit is zero bytes;
    /// - a variable read from the environment is not valid UTF-8;
    /// - the certificate verifier cannot be built, for instance because an
    ///   added root is not a certificate.
    ///
    /// No error text repeats the key or the base URL.
    pub fn build(self) -> Result<OpenAiProvider, Error> {
        let (settled, extra_roots) = self.settle()?;
        Ok(settled.over(Transport::new(extra_roots)?))
    }

    /// Builds the provider over `service`, a transport of the caller's own:
    /// a proxy, a recorder, a `tower` stack.
    ///
    /// The service owns its connections; the provider still bounds each
    /// attempt by its own deadline and reads the response under its own
    /// limit. When the service fails, its error is searched for the key as
    /// written and as `Debug` writes it, and is replaced by a fixed text on
    /// a hit; a key the service transformed in another way is not found.
    ///
    /// # Errors
    ///
    /// As [`build`](Self::build), without the certificate verifier's case.
    /// A root added with
    /// [`add_root_certificate`](Self::add_root_certificate) is a
    /// [`ErrorKind::Config`](crate::ErrorKind::Config) error here: it
    /// configures the default transport, which this provider does not have.
    pub fn build_with_service<S>(self, service: S) -> Result<OpenAiProvider<S>, Error>
    where
        S: HttpService,
    {
        let (settled, extra_roots) = self.settle()?;
        if !extra_roots.is_empty() {
            return Err(Error::config(
                "add_root_certificate configures the default transport, \
                 which a provider built with build_with_service does not use.",
            ));
        }
        Ok(settled.over(service))
    }

    /// Reads the environment for what the builder was not given, and checks
    /// every setting. The added roots are handed back unread: only the
    /// default transport takes them.
    fn settle(self) -> Result<(Settled, Vec<Vec<u8>>), Error> {
        let limits = Limits::new(self.timeout, self.max_response_bytes)?;

        let key = match self.api_key {
            Some(key) => key,
            None => env_var(KEY_VARIABLE)?.map(SecretString::from).ok_or_else(|| {
                Error::config(
                    "The OpenAI API key is missing: pass api_key or set the OPENAI_API_KEY \
                     environment variable.",
                )
            })?,
        };
        let key = key_header(AUTHORIZATION, true, &key)?;

        let base_url = match self.base_url {
            Some(base_url) => Some(base_url),
            None => env_var(BASE_URL_VARIABLE)?,
        };
        let base_url = BaseUrl::parse(base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?;
        let api = self.api.unwrap_or_else(|| default_api(base_url.host()));
        let (path, log_path) = api.paths();

        let settled = Settled {
            model: self.model.into_boxed_str(),
            api,
            host: base_url.host().into(),
            endpoint: base_url.endpoint(path, log_path)?,
            headers: request_headers(&key),
            key,
            limits,
        };
        Ok((settled, self.extra_roots))
    }
}

impl fmt::Debug for OpenAiProviderBuilder {
    /// The key and the base URL as whether they were given: a base URL's
    /// path can hold a caller's secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiProviderBuilder")
            .field("model", &self.model)
            .field("api_key", &self.api_key.is_some())
            .field("base_url", &self.base_url.is_some())
            .field("api", &self.api)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("extra_roots", &self.extra_roots.len())
            .finish()
    }
}

/// A provider's checked settings, before it has a service.
struct Settled {
    model: Box<str>,
    api: OpenAiApi,
    host: Box<str>,
    endpoint: Endpoint,
    headers: HeaderMap,
    key: KeyHeader,
    limits: Limits,
}

impl Settled {
    /// The provider that sends these settings' requests through `service`.
    fn over<S>(self, service: S) -> OpenAiProvider<S> {
        OpenAiProvider {
            service,
            model: self.model,
            api: self.api,
            host: self.host,
            endpoint: self.endpoint,
            headers: self.headers,
            key: self.key,
            limits: self.limits,
        }
    }
}

// ------------------------------------------------------ the request bodies

/// The body of a Responses request.
#[derive(Serialize)]
struct ResponsesBody<'a> {
    model: &'a str,
    input: Vec<&'a Message>,
    /// The system messages, in the vendor's structured-output mode only.
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    text: TextConfig<'a>,
    /// Always `false`: the vendor keeps no copy of the exchange.
    store: bool,
}

impl<'a> ResponsesBody<'a> {
    /// In the vendor's structured-output mode the system messages move out
    /// of `input` into `instructions`. In prompted mode they stay: the
    /// vendor's JSON mode requires the word JSON inside `input`, and
    /// `instructions` does not satisfy that check.
    fn new(model: &'a str, messages: &'a [Message], schema: &'a Schema, structured: bool) -> Self {
        let (input, instructions, format) = if structured {
            let system = messages
                .iter()
                .filter(|message| message.role() == Role::System)
                .map(Message::content)
                .collect::<Vec<_>>()
                .join("\n\n");
            let input = messages.iter().filter(|message| message.role() != Role::System).collect();
            let format = TextFormat::JsonSchema { name: SCHEMA_NAME, schema, strict: true };
            (input, Some(system), format)
        } else {
            (messages.iter().collect(), None, TextFormat::JsonObject)
        };
        Self { model, input, instructions, text: TextConfig { format }, store: false }
    }
}

/// The `text` member of a Responses request.
#[derive(Serialize)]
struct TextConfig<'a> {
    format: TextFormat<'a>,
}

/// The output format of a Responses request.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TextFormat<'a> {
    /// The vendor's structured-output mode: the reply follows `schema`.
    JsonSchema { name: &'static str, schema: &'a Schema, strict: bool },
    /// Prompted mode: any JSON object; the schema is in the system prompt.
    JsonObject,
}

/// The body of a Chat Completions request.
#[derive(Serialize)]
struct ChatBody<'a> {
    model: &'a str,
    messages: &'a [Message],
    /// Written as JSON `null` in prompted mode, as upstream sends it.
    response_format: Option<ResponseFormat<'a>>,
}

/// The `response_format` member of a Chat Completions request in the
/// vendor's structured-output mode.
#[derive(Serialize)]
struct ResponseFormat<'a> {
    r#type: &'static str,
    json_schema: NamedSchema<'a>,
}

/// The schema as Chat Completions wraps it.
#[derive(Serialize)]
struct NamedSchema<'a> {
    name: &'static str,
    schema: &'a Schema,
    strict: bool,
}

/// What a Chat Completions request sends as `response_format`: the wrapped
/// schema in the vendor's structured-output mode, nothing in prompted mode.
fn response_format(schema: &Schema, structured: bool) -> Option<ResponseFormat<'_>> {
    structured.then_some(ResponseFormat {
        r#type: "json_schema",
        json_schema: NamedSchema { name: SCHEMA_NAME, schema, strict: true },
    })
}

// -------------------------------------------------------------- the replies

/// The non-answer for a success body that is JSON but not the vendor's
/// reply: a member is missing or has another type. Nothing of the body is
/// quoted.
fn not_a_reply() -> NonAnswer {
    non_answer(VENDOR, "a body that is not the vendor's reply")
}

/// `json`, one JSON value, read as the reply `T`, or `None` when it is not
/// one.
///
/// Only a JSON object is a reply: a derived `Deserialize` also reads an
/// array as a struct, member by position, and so would take `[]` for a
/// reply with every member absent.
fn reply<'a, T: Deserialize<'a>>(json: &'a str) -> Option<T> {
    json.trim_start().starts_with('{').then(|| serde_json::from_str(json).ok()).flatten()
}

/// What is read of a Responses reply.
#[derive(Deserialize)]
struct ResponsesReply<'a> {
    #[serde(default, borrow)]
    status: Option<Cow<'a, str>>,
    #[serde(default, borrow)]
    incomplete_details: Option<IncompleteDetails<'a>>,
    #[serde(default, borrow)]
    output: Option<Vec<OutputItem<'a>>>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
}

/// Why a Responses reply is incomplete.
#[derive(Deserialize)]
struct IncompleteDetails<'a> {
    #[serde(default, borrow)]
    reason: Option<Cow<'a, str>>,
}

/// One item of a Responses reply's `output`: a message, or something else
/// such as a reasoning item, which carries no answer.
#[derive(Deserialize)]
struct OutputItem<'a> {
    #[serde(borrow)]
    r#type: Cow<'a, str>,
    #[serde(default, borrow)]
    content: Option<Vec<OutputPart<'a>>>,
}

/// One part of a message item: `output_text` with its text, a `refusal`, or
/// another kind.
#[derive(Deserialize)]
struct OutputPart<'a> {
    #[serde(borrow)]
    r#type: Cow<'a, str>,
    #[serde(default, borrow)]
    text: Option<Cow<'a, str>>,
}

/// The token counts of a Responses reply.
#[derive(Deserialize)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

/// Records the Responses reply `json` in `trace` and reads the answer out
/// of it.
///
/// The stop reason recorded is the reply's `status`. A status other than
/// `completed` is a non-answer naming the status and, when the vendor gave
/// one, the reason the reply is incomplete; a refusal part anywhere in the
/// output is a non-answer naming the word `refusal`. Neither holds the
/// vendor's error message or the refusal's text, which stay in the trace.
/// Otherwise the text is every `output_text` part of every message item, in
/// order.
fn read_responses(json: &str, trace: &mut AttemptTrace) -> Result<ProviderResult, NonAnswer> {
    let Some(reply) = reply::<ResponsesReply<'_>>(json) else {
        trace.record_response(json, None);
        return Err(not_a_reply());
    };
    trace.record_response(json, reply.status.as_deref());

    match reply.status.as_deref() {
        Some("completed") => {}
        Some(status) => {
            let reason = reply.incomplete_details.and_then(|details| details.reason);
            return Err(non_answer(
                VENDOR,
                &match reason {
                    Some(reason) => format!("response status {status}, reason {reason}"),
                    None => format!("response status {status}"),
                },
            ));
        }
        None => return Err(non_answer(VENDOR, "a response without a status")),
    }

    let mut text = String::new();
    let parts = reply
        .output
        .iter()
        .flatten()
        .filter(|item| item.r#type == "message")
        .flat_map(|item| item.content.iter().flatten());
    for part in parts {
        match (&*part.r#type, &part.text) {
            ("refusal", _) => return Err(non_answer(VENDOR, "refusal")),
            ("output_text", Some(part)) => text.push_str(part),
            _ => {}
        }
    }
    let (input_tokens, output_tokens) =
        reply.usage.map_or((None, None), |usage| (usage.input_tokens, usage.output_tokens));
    Ok(ProviderResult::new(text, input_tokens, output_tokens))
}

/// What is read of a Chat Completions reply.
#[derive(Deserialize)]
struct ChatReply<'a> {
    #[serde(default, borrow)]
    choices: Option<Vec<ChatChoice<'a>>>,
    #[serde(default)]
    usage: Option<ChatUsage>,
}

/// One choice of a Chat Completions reply; only the first is read.
#[derive(Deserialize)]
struct ChatChoice<'a> {
    #[serde(default, borrow)]
    finish_reason: Option<Cow<'a, str>>,
    #[serde(borrow)]
    message: ChatMessage<'a>,
}

/// The message of a choice.
#[derive(Deserialize)]
struct ChatMessage<'a> {
    #[serde(default, borrow)]
    content: Option<Cow<'a, str>>,
}

/// The token counts of a Chat Completions reply.
#[derive(Deserialize)]
struct ChatUsage {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
}

/// Records the Chat Completions reply `json` in `trace` and reads the answer
/// out of it.
///
/// The stop reason recorded is the first choice's `finish_reason`. One other
/// than `stop` or none is a non-answer naming it. A `content` of JSON `null`
/// is the empty text, which the client then refuses as any other reply that
/// is not the answers.
fn read_chat(json: &str, trace: &mut AttemptTrace) -> Result<ProviderResult, NonAnswer> {
    let first = reply::<ChatReply<'_>>(json)
        .and_then(|reply| Some((reply.choices?.into_iter().next()?, reply.usage)));
    let Some((choice, usage)) = first else {
        trace.record_response(json, None);
        return Err(not_a_reply());
    };
    trace.record_response(json, choice.finish_reason.as_deref());

    if let Some(reason) = choice.finish_reason.as_deref()
        && reason != "stop"
    {
        return Err(non_answer(VENDOR, &format!("finish reason {reason}")));
    }
    let (input_tokens, output_tokens) =
        usage.map_or((None, None), |usage| (usage.prompt_tokens, usage.completion_tokens));
    Ok(ProviderResult::new(
        choice.message.content.map(Cow::into_owned).unwrap_or_default(),
        input_tokens,
        output_tokens,
    ))
}

#[cfg(test)]
#[path = "openai_tests.rs"]
mod tests;
