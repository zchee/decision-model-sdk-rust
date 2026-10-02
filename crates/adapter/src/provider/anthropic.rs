//! The Anthropic provider, over Anthropic's Messages API.
//!
//! Ported from `providers/anthropic.py` of system-one-adapter-python. There
//! is no vendor SDK underneath: the provider writes the request body itself,
//! sends it through the shared HTTP module and reads the reply out of the
//! JSON it gets back.

use std::{fmt, sync::Arc, time::Duration};

use ::http::{HeaderMap, HeaderName, HeaderValue, Uri};
use bytes::Bytes;
use secrecy::SecretString;
use serde::{Deserialize, Serialize, Serializer};
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

/// The vendor's name, as a non-answer names it.
const VENDOR: &str = "Anthropic";

/// The name of the vendor API, as an attempt's trace records it.
const API: &str = "messages";

/// The path of the Messages operation, under the base URL and in a log.
const MESSAGES_PATH: &str = "/v1/messages";

/// Where requests go unless the builder or the environment says otherwise.
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The variable the key is read from when the builder was given none.
const API_KEY_VARIABLE: &str = "ANTHROPIC_API_KEY";

/// The variable the base URL is read from when the builder was given none.
const BASE_URL_VARIABLE: &str = "ANTHROPIC_BASE_URL";

/// The header the key is sent in.
const API_KEY_HEADER: HeaderName = HeaderName::from_static("x-api-key");

/// The header naming the version of the Messages API, and its value: the
/// one the vendor's own SDK sends.
const VERSION_HEADER: HeaderName = HeaderName::from_static("anthropic-version");
const VERSION: HeaderValue = HeaderValue::from_static("2023-06-01");

/// The most output tokens a reply may hold unless the builder says otherwise.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// A model behind Anthropic's Messages API.
///
/// With structured outputs the output schema travels in the request's
/// `output_config.format`, which constrains the reply to it; otherwise the
/// schema is already written into the system prompt and the request carries
/// none.
///
/// Cloning a provider shares its connection pool. `Debug` prints the model,
/// the host of the base URL and the API's name, never the key.
///
/// `S` is the service the requests are sent through: [`Transport`] for a
/// provider built with [`build`](AnthropicProviderBuilder::build), the
/// caller's own for one built with
/// [`build_with_service`](AnthropicProviderBuilder::build_with_service).
#[derive(Clone)]
pub struct AnthropicProvider<S = Transport> {
    /// Everything but the service, shared by every clone.
    settings: Arc<Settings>,
    service: S,
}

/// What a provider was built with, checked once.
struct Settings {
    model: Box<str>,
    max_tokens: u32,
    base_url: BaseUrl,
    endpoint: Endpoint,
    /// The headers of every request: the shared ones, the key and the API
    /// version.
    headers: HeaderMap,
    key: KeyHeader,
    limits: Limits,
}

impl AnthropicProvider {
    /// Starts a provider that asks `model`, such as `claude-haiku-4-5`.
    #[must_use]
    pub fn builder(model: impl Into<String>) -> AnthropicProviderBuilder {
        AnthropicProviderBuilder {
            model: model.into(),
            api_key: None,
            base_url: None,
            max_tokens: DEFAULT_MAX_TOKENS,
            timeout: None,
            max_response_bytes: None,
            root_certificates: Vec::new(),
        }
    }
}

// No bound on `S`: a caller's service need not implement `Debug`, and the
// provider must, to be a `Provider`.
impl<S> fmt::Debug for AnthropicProvider<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicProvider")
            .field("model", &self.settings.model)
            .field("host", &self.settings.base_url.host())
            .field("api", &API)
            .finish_non_exhaustive()
    }
}

impl<S> Provider for AnthropicProvider<S>
where
    S: HttpService,
{
    fn model_name(&self) -> &str {
        &self.settings.model
    }

    fn request<'a>(
        &'a self,
        mut call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async move {
            let settings = &*self.settings;
            let body = request_body(
                &settings.model,
                settings.max_tokens,
                call.messages(),
                call.structured().then_some(call.schema()),
            );
            // Recorded before it is sent, so a failed exchange still shows
            // what was asked.
            call.trace().record_request(&body, API);
            let exchange = Exchange {
                vendor: VENDOR,
                endpoint: &settings.endpoint,
                headers: &settings.headers,
                key: &settings.key,
                limits: settings.limits,
            };
            Ok(match post(&self.service, exchange, Bytes::from(body)).await? {
                Ok(reply) => exchange.screened(read_reply(&reply, call.trace()), call.trace()),
                Err(not_json) => Err(not_json),
            })
        })
    }

    fn type_name(&self) -> &str {
        "system_one_adapter::AnthropicProvider"
    }

    fn log_uri(&self) -> Option<&Uri> {
        Some(self.settings.endpoint.log_uri())
    }
}

/// Configures an [`AnthropicProvider`]; made by
/// [`AnthropicProvider::builder`].
///
/// Nothing is checked until [`build`](Self::build) or
/// [`build_with_service`](Self::build_with_service), which is also when the
/// environment is read. `Debug` prints neither the key nor the base URL.
#[derive(Clone)]
pub struct AnthropicProviderBuilder {
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
    max_tokens: u32,
    timeout: Option<Duration>,
    max_response_bytes: Option<usize>,
    root_certificates: Vec<Vec<u8>>,
}

impl AnthropicProviderBuilder {
    /// The API key, sent as the `x-api-key` header and printed nowhere.
    ///
    /// Without it the key is the `ANTHROPIC_API_KEY` environment variable.
    ///
    /// The host the key is sent to is still the one `ANTHROPIC_BASE_URL`
    /// names when [`base_url`](Self::base_url) is not called: call
    /// `base_url` when the key does not come from the same environment.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(SecretString::from(key.into()));
        self
    }

    /// The API root, to which `/v1/messages` is appended; a path prefix is
    /// kept and a trailing slash is dropped.
    ///
    /// It must be an absolute `http` or `https` URL without userinfo, query
    /// or fragment. An `http://` base URL sends the API key unencrypted; use
    /// it only for a local proxy or a test server.
    ///
    /// Without it the base URL is the `ANTHROPIC_BASE_URL` environment
    /// variable, and `https://api.anthropic.com` when that is unset too.
    #[must_use]
    pub fn base_url(mut self, url: impl AsRef<str>) -> Self {
        self.base_url = Some(url.as_ref().to_owned());
        self
    }

    /// The most output tokens a reply may hold; 4,096 unless set. The
    /// Messages API requires the bound in every request.
    ///
    /// A reply cut at the bound is not an answer, even when what arrived is
    /// valid JSON: raise the bound or ask fewer questions.
    #[must_use]
    pub fn max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// The deadline of each whole attempt, from waiting for a connection to
    /// the last byte of the response; 600 seconds unless set.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The largest response body read, in bytes; 16 MiB unless set. A larger
    /// body is not read past the limit.
    #[must_use]
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Trusts `der`, a DER-encoded certificate, in addition to the operating
    /// system's roots: for a corporate CA the system store lacks, or a test
    /// server's own certificate.
    ///
    /// The default transport only; see
    /// [`build_with_service`](Self::build_with_service).
    #[must_use]
    pub fn add_root_certificate(mut self, der: impl Into<Vec<u8>>) -> Self {
        self.root_certificates.push(der.into());
        self
    }

    /// Builds the provider over the default [`Transport`].
    ///
    /// Needs no runtime: nothing connects until the first request.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error when
    /// `max_tokens` is zero; when no API key was given and
    /// `ANTHROPIC_API_KEY` is unset or empty; when the key is empty or holds
    /// a character that cannot be sent in a header; when the base URL is not
    /// an absolute `http` or `https` URL without userinfo, query or fragment;
    /// when the timeout or the response limit is zero; when an environment
    /// variable that is read is not UTF-8; or when the certificate verifier
    /// cannot be built, for an added root that is not a certificate among
    /// other causes. No message repeats the key or the URL.
    pub fn build(mut self) -> Result<AnthropicProvider, Error> {
        let root_certificates = std::mem::take(&mut self.root_certificates);
        // The settings first: they are cheap to check, and the transport
        // loads the operating system's certificate roots.
        let settings = self.settings()?;
        let service = Transport::new(root_certificates)?;
        Ok(AnthropicProvider { settings: Arc::new(settings), service })
    }

    /// Builds the provider over `service`, a transport of the caller's own:
    /// a proxy, a recorder, a `tower` stack with its own middleware.
    ///
    /// The service owns its connections; the provider still bounds each
    /// attempt with its own deadline and reads the response under its own
    /// limit. When the service fails, its error is searched for the API key
    /// as written and as `Debug` writes it before it is kept, and dropped
    /// for a fixed text on a hit; a key the service transformed in another
    /// way, into base64 say, is not found.
    ///
    /// # Errors
    ///
    /// As [`build`](Self::build), except that no certificate verifier is
    /// built. A root added with
    /// [`add_root_certificate`](Self::add_root_certificate) is a config
    /// error: it configures the default transport, which this provider does
    /// not have, and is refused rather than ignored.
    pub fn build_with_service<S>(self, service: S) -> Result<AnthropicProvider<S>, Error>
    where
        S: HttpService,
    {
        if !self.root_certificates.is_empty() {
            return Err(Error::config(
                "add_root_certificate configures the default transport, \
                 which a provider built with build_with_service does not use.",
            ));
        }
        Ok(AnthropicProvider { settings: Arc::new(self.settings()?), service })
    }

    /// Checks everything but the transport, reading the environment for the
    /// key and the base URL the builder was not given.
    fn settings(self) -> Result<Settings, Error> {
        if self.max_tokens == 0 {
            return Err(Error::config("max_tokens must be greater than zero."));
        }
        let limits = Limits::new(self.timeout, self.max_response_bytes)?;

        let api_key = match self.api_key {
            Some(key) => key,
            None => env_var(API_KEY_VARIABLE)?.map(SecretString::from).ok_or_else(|| {
                Error::config(
                    "No Anthropic API key: pass one to api_key, \
                     or set the ANTHROPIC_API_KEY environment variable.",
                )
            })?,
        };
        let key = key_header(API_KEY_HEADER, false, &api_key)?;
        let mut headers = request_headers(&key);
        headers.insert(VERSION_HEADER, VERSION);

        let base_url = match self.base_url {
            Some(url) => BaseUrl::parse(&url)?,
            None => match env_var(BASE_URL_VARIABLE)? {
                Some(url) => BaseUrl::parse(&url)?,
                None => BaseUrl::parse(DEFAULT_BASE_URL)?,
            },
        };
        let endpoint = base_url.endpoint(MESSAGES_PATH, MESSAGES_PATH)?;

        Ok(Settings {
            model: self.model.into_boxed_str(),
            max_tokens: self.max_tokens,
            base_url,
            endpoint,
            headers,
            key,
            limits,
        })
    }
}

impl fmt::Debug for AnthropicProviderBuilder {
    /// Whether a key and a base URL were given, never their text: a base
    /// URL's path can hold a caller's secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnthropicProviderBuilder")
            .field("model", &self.model)
            .field("api_key", &self.api_key.is_some())
            .field("base_url", &self.base_url.is_some())
            .field("max_tokens", &self.max_tokens)
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("root_certificates", &self.root_certificates.len())
            .finish()
    }
}

// ------------------------------------------------------------ the request

/// The body of one Messages request (upstream's `_request_kwargs`).
#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    /// The system messages, joined by a blank line. Always sent, as upstream
    /// sends it, even when it is empty.
    system: String,
    messages: Conversation<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig<'a>>,
}

/// The messages that are not system messages, in order. The Messages API
/// takes the system prompt beside the conversation, not inside it.
struct Conversation<'a>(&'a [Message]);

impl Serialize for Conversation<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // A `Message` serializes as the `{"role":..,"content":..}` object
        // the API takes, so the turns are written straight from the slice.
        serializer.collect_seq(self.0.iter().filter(|message| message.role() != Role::System))
    }
}

/// `output_config`: how the reply is constrained.
#[derive(Serialize)]
struct OutputConfig<'a> {
    format: OutputFormat<'a>,
}

/// `output_config.format`: the JSON schema the reply must follow.
#[derive(Serialize)]
struct OutputFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    schema: &'a Schema,
}

/// The JSON body asking `model` with `messages`; `schema` is the output
/// schema when the vendor's structured-output mode carries it.
fn request_body(
    model: &str,
    max_tokens: u32,
    messages: &[Message],
    schema: Option<&Schema>,
) -> String {
    let system = messages
        .iter()
        .filter(|message| message.role() == Role::System)
        .map(Message::content)
        .collect::<Vec<_>>()
        .join("\n\n");
    let request = MessagesRequest {
        model,
        max_tokens,
        system,
        messages: Conversation(messages),
        output_config: schema
            .map(|schema| OutputConfig { format: OutputFormat { kind: "json_schema", schema } }),
    };
    serde_json::to_string(&request)
        .expect("invariant: strings, a number and a JSON schema always serialize as JSON")
}

// -------------------------------------------------------------- the reply

/// What is read out of a Messages reply. Members it does not name are
/// skipped.
#[derive(Deserialize)]
struct MessagesReply {
    /// Absent and `null` both mean the vendor gave no stop reason.
    #[serde(default)]
    stop_reason: Option<String>,
    content: Vec<Block>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// One block of a reply's `content`, told apart by its `type` member.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Block {
    /// Text of the reply.
    Text { text: String },
    /// A block that is not text, thinking say: no part of the answer, so
    /// nothing of it is read.
    #[serde(other)]
    Other,
}

/// The token counts of a reply; one the vendor left out is `None`.
#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

/// Reads the reply out of `json`, the body of a success response
/// (upstream's `_result`), after recording it in `trace` with its stop
/// reason, so that a reply which is then refused is still in the trace.
///
/// The text is the `text` blocks joined. A reply cut at the output token
/// limit, or stopped for any reason but `end_turn`, `stop_sequence` or none,
/// is a non-answer naming the stop reason; so is a body that is JSON but not
/// a Messages reply. No non-answer quotes the body.
///
/// Only a JSON object is a reply: a derived `Deserialize` also reads an
/// array as a struct, member by position, and so would take `[null,[]]` for
/// a reply with no stop reason and no text.
fn read_reply(json: &str, trace: &mut AttemptTrace) -> Result<ProviderResult, NonAnswer> {
    let object = json.trim_start().starts_with('{');
    // serde's own message can quote the body, so it is dropped for a fixed
    // text.
    let Some(reply) = object.then(|| serde_json::from_str::<MessagesReply>(json).ok()).flatten()
    else {
        trace.record_response(json, None);
        return Err(non_answer(VENDOR, "a body that is not the vendor's reply"));
    };
    trace.record_response(json, reply.stop_reason.as_deref());

    match reply.stop_reason.as_deref() {
        None | Some("end_turn" | "stop_sequence") => {}
        // Even valid JSON must not hide a reply that was cut short.
        Some("max_tokens") => {
            return Err(non_answer(
                VENDOR,
                "stop reason max_tokens, the reply was cut at the output token limit; \
                 raise max_tokens on the provider or ask fewer questions",
            ));
        }
        Some(other) => return Err(non_answer(VENDOR, &format!("stop reason {other}"))),
    }

    let text = reply
        .content
        .iter()
        .filter_map(|block| match block {
            Block::Text { text } => Some(text.as_str()),
            Block::Other => None,
        })
        .collect::<String>();
    let (input_tokens, output_tokens) =
        reply.usage.map_or((None, None), |usage| (usage.input_tokens, usage.output_tokens));
    Ok(ProviderResult::new(text, input_tokens, output_tokens))
}

#[cfg(test)]
#[path = "anthropic_tests.rs"]
mod tests;
