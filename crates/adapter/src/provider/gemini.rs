//! The Gemini provider, over Gemini's API.
//!
//! Ported from `providers/gemini.py` of system-one-adapter-python. Upstream
//! calls the vendor's Python SDK; here the provider writes the Interactions
//! request itself, sends it through the shared HTTP module and reads the
//! reply by the rules that SDK applies.

use std::{fmt, time::Duration};

use ::http::{HeaderMap, HeaderName, Uri};
use bytes::Bytes;
use decision_model_sdk::HttpService;
use secrecy::SecretString;
use serde::Serialize;
use serde_json::Value;

use super::{
    BoxFuture, NonAnswer, Provider, ProviderCall, ProviderResult, Role, Schema,
    http::{
        self, BaseUrl, Endpoint, Exchange, KeyHeader, Limits, Transport, env_var, key_header,
        non_answer, request_headers,
    },
};
use crate::error::Error;

/// The vendor's name, as a non-answer names it.
const VENDOR: &str = "Gemini";

/// The vendor API a request goes to, as an attempt's trace records it.
const API: &str = "interactions";

/// The path of the Interactions operation: what is appended to the base URL,
/// and the fixed path the log URI names.
const INTERACTIONS_PATH: &str = "/v1beta/interactions";

/// Where requests go unless the builder names another base URL.
const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com";

/// The header the key is sent in.
const KEY_HEADER: HeaderName = HeaderName::from_static("x-goog-api-key");

/// The variables the key is read from when the builder has none, in the
/// order the vendor's SDK reads them.
const KEY_VARIABLES: [&str; 2] = ["GOOGLE_API_KEY", "GEMINI_API_KEY"];

/// The status of an interaction that finished with an answer.
const COMPLETED: &str = "completed";

/// A Gemini model, asked through the Interactions API.
///
/// Built by [`GeminiProvider::builder`]. Cloning it shares the connection
/// pool of its service. `S` is the HTTP service the requests are sent
/// through: [`Transport`] unless the provider was built with
/// [`GeminiProviderBuilder::build_with_service`].
///
/// `Debug` prints the model, the host of the base URL and the API, never the
/// key.
#[derive(Clone)]
pub struct GeminiProvider<S = Transport> {
    model: Box<str>,
    base_url: BaseUrl,
    endpoint: Endpoint,
    headers: HeaderMap,
    key: KeyHeader,
    limits: Limits,
    service: S,
}

impl GeminiProvider {
    /// Starts a provider that asks the model `model`, such as
    /// `gemini-3.5-flash-lite`.
    #[must_use]
    pub fn builder(model: impl Into<String>) -> GeminiProviderBuilder {
        GeminiProviderBuilder {
            model: model.into(),
            api_key: None,
            base_url: None,
            timeout: None,
            max_response_bytes: None,
            extra_roots: Vec::new(),
        }
    }
}

impl<S> fmt::Debug for GeminiProvider<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeminiProvider")
            .field("model", &self.model)
            .field("host", &self.base_url.host())
            .field("api", &API)
            .finish_non_exhaustive()
    }
}

/// Configures a [`GeminiProvider`].
///
/// `Debug` prints the model and which settings were given, never the key or
/// the base URL.
#[derive(Clone)]
pub struct GeminiProviderBuilder {
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
    timeout: Option<Duration>,
    max_response_bytes: Option<usize>,
    extra_roots: Vec<Vec<u8>>,
}

impl GeminiProviderBuilder {
    /// The API key, sent in the `x-goog-api-key` header.
    ///
    /// Without it the key is read when the provider is built, from the
    /// environment variable `GOOGLE_API_KEY`, or from `GEMINI_API_KEY` when
    /// that one is unset or empty.
    #[must_use]
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(SecretString::from(key.into()));
        self
    }

    /// The base URL the path `/v1beta/interactions` is appended to; the
    /// default is `https://generativelanguage.googleapis.com`. No
    /// environment variable is read for it.
    ///
    /// It must be absolute, use `http` or `https`, and carry no credentials,
    /// query or fragment. Over `http` the key travels unencrypted, so that
    /// form is for a local proxy or test server only.
    #[must_use]
    pub fn base_url(mut self, url: impl AsRef<str>) -> Self {
        self.base_url = Some(url.as_ref().to_owned());
        self
    }

    /// How long one attempt may take, from waiting for a connection to the
    /// last byte of the response; the default is 600 seconds. Zero is
    /// refused when the provider is built.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// The largest response body read, in bytes; the default is 16 MiB. Zero
    /// is refused when the provider is built.
    #[must_use]
    pub fn max_response_bytes(mut self, limit: usize) -> Self {
        self.max_response_bytes = Some(limit);
        self
    }

    /// Trusts the DER-encoded certificate `der` in addition to the operating
    /// system's roots, or to the certificates `SSL_CERT_FILE` and
    /// `SSL_CERT_DIR` name where those replace them (see *Certificate
    /// variables* in the README), which it never replaces.
    ///
    /// The default transport only:
    /// [`build_with_service`](Self::build_with_service) refuses a builder
    /// that was given one.
    #[must_use]
    pub fn add_root_certificate(mut self, der: impl Into<Vec<u8>>) -> Self {
        self.extra_roots.push(der.into());
        self
    }

    /// Builds the provider over the default [`Transport`].
    ///
    /// Nothing connects until the first request, which must run on a Tokio
    /// runtime with its time driver enabled.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error for
    /// a setting [`build_with_service`](Self::build_with_service) refuses,
    /// other than an added root, which this method uses, and when the
    /// certificate verifier cannot be built: an added root is
    /// not a certificate, or the operating system's roots cannot be loaded.
    /// The settings are checked first, so a missing key is reported as such
    /// and not as a failure to load the roots.
    pub fn build(mut self) -> Result<GeminiProvider, Error> {
        let extra_roots = std::mem::take(&mut self.extra_roots);
        self.provider(|| Transport::new(extra_roots))
    }

    /// Builds the provider over `service`, the caller's own HTTP service,
    /// instead of the default transport. The service owns its TLS, so a
    /// root added with [`add_root_certificate`](Self::add_root_certificate)
    /// is refused rather than ignored.
    ///
    /// When a call of `service` fails, its error is kept as the cause unless
    /// it spells the key, as written or as `Debug` writes it; then the cause
    /// is replaced by a fixed text. A key the service transformed in another
    /// way is not found.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::Config`](crate::ErrorKind::Config) error
    /// when:
    ///
    /// - no key was given and neither `GOOGLE_API_KEY` nor `GEMINI_API_KEY`
    ///   holds one, or such a variable is not valid UTF-8;
    /// - the key is empty or holds a character that cannot be sent in an
    ///   HTTP header;
    /// - the base URL is not absolute, does not use `http` or `https`, or
    ///   carries credentials, a query or a fragment;
    /// - the timeout or the response limit is zero;
    /// - a root certificate was added.
    ///
    /// No error text repeats the key or the base URL.
    pub fn build_with_service<S>(self, service: S) -> Result<GeminiProvider<S>, Error>
    where
        S: HttpService,
    {
        if !self.extra_roots.is_empty() {
            return Err(Error::config(
                "add_root_certificate configures the default transport, \
                 which a provider built with build_with_service does not use.",
            ));
        }
        self.provider(|| Ok(service))
    }

    /// Checks every setting, reading the environment for the key the
    /// builder was not given, and only then calls `service` for the
    /// transport.
    fn provider<S, F>(self, service: F) -> Result<GeminiProvider<S>, Error>
    where
        F: FnOnce() -> Result<S, Error>,
    {
        let key = match self.api_key {
            Some(key) => key,
            None => key_from_environment()?,
        };
        let key = key_header(KEY_HEADER, false, &key)?;
        let base_url = BaseUrl::parse(self.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL))?;
        let endpoint = base_url.endpoint(INTERACTIONS_PATH, INTERACTIONS_PATH)?;
        let limits = Limits::new(self.timeout, self.max_response_bytes)?;
        Ok(GeminiProvider {
            model: self.model.into_boxed_str(),
            base_url,
            endpoint,
            headers: request_headers(&key),
            key,
            limits,
            service: service()?,
        })
    }
}

impl fmt::Debug for GeminiProviderBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeminiProviderBuilder")
            .field("model", &self.model)
            .field("api_key_set", &self.api_key.is_some())
            .field("base_url_set", &self.base_url.is_some())
            .field("timeout", &self.timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("extra_roots", &self.extra_roots.len())
            .finish()
    }
}

/// The key of the first of [`KEY_VARIABLES`] that is set and not empty.
fn key_from_environment() -> Result<SecretString, Error> {
    for name in KEY_VARIABLES {
        if let Some(key) = env_var(name)? {
            return Ok(SecretString::from(key));
        }
    }
    Err(Error::config(
        "No Gemini API key: pass one to api_key, or set GOOGLE_API_KEY or GEMINI_API_KEY.",
    ))
}

impl<S> Provider for GeminiProvider<S>
where
    S: HttpService,
{
    fn model_name(&self) -> &str {
        &self.model
    }

    fn request<'a>(
        &'a self,
        mut call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, decision_model_sdk::Error>> {
        Box::pin(async move {
            let body = request_body(&self.model, &call);
            call.trace().record_request(&body, API);
            let exchange = Exchange {
                vendor: VENDOR,
                endpoint: &self.endpoint,
                headers: &self.headers,
                key: &self.key,
                limits: self.limits,
            };
            let json = match http::post(&self.service, exchange, Bytes::from(body)).await? {
                Ok(json) => json,
                Err(not_json) => return Ok(Err(not_json)),
            };
            // `post` has checked that the body is one JSON value. Should it
            // still not read as one here, it has no status and is refused
            // below like any body that is not the vendor's reply.
            let response = serde_json::from_str::<Value>(&json).unwrap_or(Value::Null);
            let status = response.get("status").and_then(Value::as_str);
            call.trace().record_response(&json, status);
            Ok(exchange.screened(result(&response, status), call.trace()))
        })
    }

    fn type_name(&self) -> &str {
        "decision_model_adapter::GeminiProvider"
    }

    fn log_uri(&self) -> Option<&Uri> {
        Some(self.endpoint.log_uri())
    }
}

/// The body of an Interactions request.
#[derive(Serialize)]
struct InteractionRequest<'a> {
    model: &'a str,
    input: Vec<Step<'a>>,
    /// Always `false`: the vendor keeps no copy of the interaction.
    store: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
}

/// One turn of the conversation: `user_input` or `model_output`.
#[derive(Serialize)]
struct Step<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    content: [TextContent<'a>; 1],
}

/// A turn's text.
#[derive(Serialize)]
struct TextContent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

/// The vendor's structured-output setting: JSON text that follows `schema`.
#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    mime_type: &'static str,
    schema: &'a Schema,
}

/// The JSON body that asks `model` with `call`.
///
/// The system messages are joined into `system_instruction`, which is left
/// out when they are empty; every other message is a step, in order. The
/// schema is sent only in structured mode: in prompted mode the system
/// prompt already holds it.
fn request_body(model: &str, call: &ProviderCall<'_>) -> String {
    let mut system: Vec<&str> = Vec::new();
    let mut input = Vec::with_capacity(call.messages().len());
    for message in call.messages() {
        let kind = match message.role() {
            Role::System => {
                system.push(message.content());
                continue;
            }
            Role::User => "user_input",
            Role::Assistant => "model_output",
        };
        input.push(Step { kind, content: [TextContent { kind: "text", text: message.content() }] });
    }
    let system = system.join("\n\n");
    let request = InteractionRequest {
        model,
        input,
        store: false,
        system_instruction: (!system.is_empty()).then_some(system),
        response_format: call.structured().then(|| ResponseFormat {
            kind: "text",
            mime_type: "application/json",
            schema: call.schema(),
        }),
    };
    serde_json::to_string(&request)
        .expect("invariant: a request of strings and a JSON schema always serializes")
}

/// What a success response holds: the reply's text and token counts, or the
/// reason it is not an answer.
///
/// A body without a `status` that is a string is not the vendor's reply. An
/// interaction whose status is not `completed`, or that reports no usage or
/// lacks one of the two token counts, is not an answer. The reason names the
/// status and the missing member only; nothing of the body is quoted, its
/// own error text least of all.
fn result(response: &Value, status: Option<&str>) -> Result<ProviderResult, NonAnswer> {
    let Some(status) = status else {
        return Err(non_answer(VENDOR, "a body that is not the vendor's reply"));
    };
    if status != COMPLETED {
        return Err(non_answer(VENDOR, &format!("status {status}")));
    }
    let count = |member: &str| {
        response
            .get("usage")
            .and_then(|usage| usage.get(member))
            .and_then(Value::as_u64)
            .ok_or_else(|| non_answer(VENDOR, &format!("status {COMPLETED} without {member}")))
    };
    let input_tokens = count("total_input_tokens")?;
    let output_tokens = count("total_output_tokens")?;
    Ok(ProviderResult::new(output_text(response), Some(input_tokens), Some(output_tokens)))
}

/// The text of the model's last output, by the rule of the vendor's Python
/// SDK (`Interaction.output_text`, google-genai 2.24.0).
///
/// The steps are walked from the last one backwards. A `user_input` step
/// ends the walk. A step of another kind than `model_output`, or a
/// `model_output` step whose `content` is not a list, is skipped until text
/// has been found and ends the walk after that. Inside a step the content
/// is walked backwards as well: every `text` item is collected, a `text`
/// that is not a string as the empty string, and the first item of another
/// kind after a collected one ends the walk. The collected parts are joined
/// in the order the response holds them; without any, the text is empty.
fn output_text(response: &Value) -> String {
    let steps = response.get("steps").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut parts: Vec<&str> = Vec::new();
    'steps: for step in steps.iter().rev() {
        let content = match kind(step) {
            Some("user_input") => break,
            Some("model_output") => step.get("content").and_then(Value::as_array),
            _ => None,
        };
        let Some(content) = content else {
            if parts.is_empty() {
                continue;
            }
            break;
        };
        for item in content.iter().rev() {
            if kind(item) == Some("text") {
                parts.push(item.get("text").and_then(Value::as_str).unwrap_or(""));
            } else if !parts.is_empty() {
                break 'steps;
            }
        }
    }
    parts.iter().rev().copied().collect()
}

/// The `type` member of a step or of a content item, when it is a string.
fn kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
