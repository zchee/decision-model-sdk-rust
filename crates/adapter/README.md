# System One adapter for Rust

Ask TypeSafe System One questions of an OpenAI, Anthropic or Gemini model instead of the TypeSafe
API: the same prepared questions go to the model, and its reply comes back as the answers the
`typesafe-sdk-rust` crate returns, with probabilities, usage and a trace of every attempt. It is a
port of the Python package
[system-one-adapter-python](https://github.com/typesafe-ai/system-one-adapter-python) 0.2.1
(commit `e1d4cc9`), useful for comparing System One against a general-purpose model on cost, speed
and quality. The places where it behaves differently on purpose are listed under
[Deviations from the Python adapter](#deviations-from-the-python-adapter).

The package is published as **`typesafe-sdk-rust-adapter`**, next to the SDK it builds on. The
library it builds is **`system_one_adapter`**, the Python package's name, so code writes
`use system_one_adapter::...`.

## Install

```toml
[dependencies]
typesafe-sdk-rust-adapter = "0.1.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The minimum supported Rust version is **1.98**, and the crate uses edition 2024. It depends on
`typesafe-sdk-rust` 0.2.1 or a later 0.2 release, without that crate's default features: the
adapter needs neither the SDK's transport nor its derive.

| Feature | Default | What it does |
| --- | --- | --- |
| `openai` | on | `OpenAiProvider`, `ProviderName::OpenAi` and the default transport (hyper-util over hyper-rustls, trust anchors from the operating system). |
| `anthropic` | on | `AnthropicProvider`, `ProviderName::Anthropic` and the default transport. |
| `gemini` | on | `GeminiProvider`, `ProviderName::Gemini` and the default transport. |
| `tracing` | on | The adapter's own events, and the SDK's retry line: the feature also turns on the SDK's `tracing` feature (see [Logging](#logging)). Without it, every event is compiled out. |
| `macros` | off | Turns on the SDK's `macros` feature, so the re-exported `QuestionSet` is also the derive (see [Typed answers](#typed-answers)). It re-exports nothing by itself. |
| `internals` | off | Exposes a hidden `system_one_adapter::__internals` module used by this repository's tests and fuzz targets. It carries **no semver promise**; do not depend on it. With it, a provider reads no process environment, so its key and base URL must be given to its builder. |

Without any provider feature hyper and rustls are not compiled, `ProviderName` has no variant, and
a client still takes a provider the caller implements (see [Providers](#providers)).

## Runtime requirement

The crate is `async` only and runs on the caller's [Tokio](https://tokio.rs) runtime, which must
have its **time driver enabled**: every attempt has a deadline and retries sleep between attempts.
`#[tokio::main]` and `Builder::enable_all()` / `enable_time()` enable it. There is no blocking
client.

## Quickstart

1. Build a client with `Client::builder(structured_outputs, llm_answer_mode)`. Both arguments are
   required, as they are upstream. `StructuredOutputs::Native` asks the vendor for its structured
   output mode with the answer schema; `StructuredOutputs::Prompted` writes the schema into the
   prompt instead. `AnswerMode::Probabilities` asks the model for a probability per question or
   per label; `AnswerMode::Discrete` asks for the answer itself.
2. Name the model: `.provider(ProviderName::OpenAi).model("gpt-4o-mini")` on the builder, or the
   same two methods on each call. `build()` connects to nothing and reads no environment
   variable.
3. Prepare the questions with the SDK's builder, re-exported here: start from `Questions::new()`,
   add each question by name, for example
   `.noul("positive", Noul::new().instructions("The review is positive."))`, and end with
   `.prepare()`.
4. Call `client.system_one(&state, &questions)`, where `state` is any value that implements
   `serde::Serialize` (the document the questions are about), optionally override `provider`,
   `model`, `provider_instance` or `retry` for this call, then `.send().await`.
5. The result is a `Response` whose `answers()` are the SDK's `Answers`, or an `Error` (see
   [Errors](#errors)).

The first call for a provider name and model builds that provider and reads its key from the
environment (see [Providers](#providers)); later calls reuse it.

```rust,no_run
use system_one_adapter::{AnswerMode, Client, Noul, ProviderName, Questions, StructuredOutputs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Connects to nothing; OPENAI_API_KEY is read at the first call.
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Probabilities)
        .provider(ProviderName::OpenAi)
        .model("gpt-4o-mini")
        .build()?;

    let questions = Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .prepare()?;

    let review = "A delightful novel; I read it twice.";
    let response = client.system_one(review, &questions).send().await?;

    if let Some(positive) = response.answers().noul("positive") {
        println!("positive: {}", positive.noul());
    }
    let usage = response.usage();
    println!("{:?} input tokens in {:?}", usage.input_tokens_total(), usage.latency());
    Ok(())
}
```

## Typed answers

`client.ask::<Q>(&state)` takes the questions from a type that implements the SDK's `QuestionSet`
and returns `Response<Q>`: the answers are decoded into the fields of `Q`. Answers that do not fit
the fields fail the call with `ErrorKind::MalformedStructure`.

A caller that derives question sets depends on `typesafe-sdk-rust` directly, with its `macros`
feature, and writes a plain `#[derive(QuestionSet)]` (recommended); or it enables this crate's
`macros` feature and points the derive at the re-exported SDK with
`#[question_set(crate = system_one_adapter::typesafe_sdk)]`, a path and not a string.
The attribute is needed in the second form because the derive's expansion names `::typesafe_sdk`
unless told otherwise, and a crate that depends on the adapter alone has no crate of that name.
The example below is the second form, with this crate's `macros` feature on:

```rust,no_run
# #[cfg(feature = "macros")]
# mod example {
use system_one_adapter::{ChoiceAnswer, Client, Error, NoulAnswer, QuestionSet, ScoreAnswer};

#[derive(Debug, QuestionSet)]
#[question_set(crate = system_one_adapter::typesafe_sdk)]
struct Review {
    #[noul(instructions = "The review is positive.")]
    positive: NoulAnswer,
    #[score(instructions = "How good the book is.", levels("Bad.", "Good."))]
    stars: ScoreAnswer,
    #[choice(instructions = "The genre.", options("fiction" = "A story.", "nonfiction" = "Facts."))]
    genre: ChoiceAnswer,
}

async fn classify(client: &Client, review: &str) -> Result<(), Error> {
    let response = client.ask::<Review>(review).send().await?;
    let review = response.answers();
    println!("positive {}", review.positive.noul());
    println!("stars {}, genre {}", review.stars.score(), review.genre.choice());
    Ok(())
}
# }
# fn main() {}
```

## Client options

| Method of `ClientBuilder` | Default | Meaning |
| --- | --- | --- |
| `Client::builder(structured_outputs, llm_answer_mode)` | required | `StructuredOutputs::Native` or `Prompted`; `AnswerMode::Probabilities` or `Discrete`. |
| `provider(ProviderName)` | none | The built-in provider that a model name is given to. |
| `model(impl Into<String>)` | none | The model name. |
| `provider_instance(Arc<dyn Provider>)` | none | A provider the caller built or implemented. It is borrowed: the client never caches it. |
| `normalize_probabilities(bool)` | `false` | Whether a probability distribution of a choice or score question whose sum is off by more than 1e-6 is rescaled to sum to 1. The trace records the largest error of any distribution's sum, each error beyond 1e-6 and, for a rescaled distribution, the original values. |
| `n_retry_malformed_structure(u32)` | `0` | How many corrective turns follow a reply that does not fit the answer schema. |
| `retry(RetryPolicy)` | `RetryPolicy::none()` | The SDK's retry policy for provider failures (see [Retries](#retries)). |

`StructuredOutputs`, `AnswerMode`, `ProviderName`, `OpenAiApi` and `ErrorKind` are
`#[non_exhaustive]`: a `match` over one of them needs a catch-all arm.

A call (`Request`, returned by `system_one` and `ask`) takes `provider`, `model`,
`provider_instance` and `retry` as overrides. The provider of a call is resolved in this order:
the call's `provider_instance`, else the call's `model`, else the client's `provider_instance`,
else the client's `model`. A model name is asked of the call's provider name, else the client's.
A call with no model, or with a model name and no provider name, fails with
`ErrorKind::InvalidRequest` before anything is sent. A provider whose cargo feature is off has no
`ProviderName` variant, so no call can name it.

A provider that the client builds from a name and a model is kept for the life of the client and
shared by the client's clones. It is keyed by the resolved pair of provider name and model, per
client: two calls that resolve to the same pair share one provider, wherever each named it, and
another client built with the same options builds its own. It is built on first use, with the key
and base URL the environment holds then; concurrent first calls wait for one build, and a build
that fails is not kept, so the next call that resolves to the pair builds again. The providers are
dropped with the last clone of the client. There is no `close`.

Inside `send`, in this order: the provider is resolved, and built if the client owns it and has
not built it yet; the state is written as JSON, and a state that serializes to `null` or does not
serialize at all is refused as `ErrorKind::InvalidRequest`; the questions are checked again for
the adapter; the latency clock starts; the model is asked. A missing provider, or one that cannot
be built, is therefore reported before an invalid state or question.

## Providers

`OpenAiProvider`, `AnthropicProvider` and `GeminiProvider` are built by `ProviderName` and a model
name through the client, or explicitly through their builders (`OpenAiProvider::builder(model)`
and so on) and handed to `provider_instance`. A provider is `Clone`; its clones share one
connection pool. `ProviderName` parses from and prints as `openai`, `anthropic` and `gemini`.

| Builder method | OpenAI | Anthropic | Gemini | Default |
| --- | --- | --- | --- | --- |
| `builder(model)` | yes | yes | yes | required |
| `api_key(impl Into<String>)` | yes | yes | yes | the environment, below |
| `base_url(impl AsRef<str>)` | yes | yes | yes | the environment, else the vendor host, below |
| `api(OpenAiApi)` | yes | - | - | `OpenAiApi::Responses` when the base URL's host is `api.openai.com` (letter case ignored), else `OpenAiApi::ChatCompletions` |
| `max_tokens(u32)` | - | yes | - | 4096; 0 is refused |
| `timeout(Duration)` | yes | yes | yes | 600 s for each whole attempt; zero is refused |
| `max_response_bytes(usize)` | yes | yes | yes | 16 MiB, the SDK's value; 0 is refused |
| `add_root_certificate(der)` | yes | yes | yes | none; a certificate is added to the platform's roots and never replaces them; the default transport only |
| `build()` | yes | yes | yes | the default transport, `Transport` |
| `build_with_service(service)` | yes | yes | yes | the caller's HTTP service in place of `Transport` |

A refused value, a missing key, a key with a byte that is illegal in a header value, a base URL
that breaks the rules below and an environment variable that is not UTF-8 are each
`ErrorKind::Config`, reported when the provider is built and never when a request is sent. An
added root certificate configures the default transport only: `build_with_service` refuses one as
`ErrorKind::Config` for OpenAI and Anthropic, and ignores it for Gemini.

**Keys.** When `api_key` is not called, the key is read from the environment:

| Provider | Key variable | Base URL variable | Default base URL |
| --- | --- | --- | --- |
| OpenAI | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `https://api.openai.com/v1` |
| Anthropic | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` |
| Gemini | `GOOGLE_API_KEY`, else `GEMINI_API_KEY` | none | `https://generativelanguage.googleapis.com` |

The environment is read once, when a provider is built: by `build()` or `build_with_service` for
a provider built explicitly, at the first call that uses it for a provider the client owns. A
variable changed later has no effect on a provider that exists, and an empty variable counts as
unset. `GOOGLE_GEMINI_BASE_URL`, the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
`NO_PROXY`) and `SSL_CERT_FILE` are not read. With the `internals` feature no variable is read.

The key is sent as `authorization: Bearer` (OpenAI), `x-api-key` with
`anthropic-version: 2023-06-01` (Anthropic) or `x-goog-api-key` (Gemini). The header value is
built once, when the provider is built, and marked sensitive; `Debug` of a provider prints the
model, the base URL's host and the API, never the key. Every request carries
`user-agent: typesafe-sdk-rust-adapter/` followed by the crate's version.

**Base URL.** The rules are the SDK's: an absolute `http` or `https` URL with a host; userinfo, a
query or a fragment is refused, and no error text repeats the URL. `http://` is accepted, and then
the key travels in clear text, so use it only for a local proxy or a test server. When `base_url`
is not called, `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL` decide which host receives the key: check
them in any environment that also holds a key. Redirects are not followed: a 3xx answer is an API
error after exactly one request, so a key is never sent to a host a response named.

The operation's path is appended to the base URL: `/responses` or `/chat/completions` for OpenAI,
so an OpenAI base URL ends with the version segment as `https://api.openai.com/v1` does;
`/v1/messages` for Anthropic; `/v1beta/interactions` for Gemini. A path prefix is kept and a
trailing slash is dropped.

**The OpenAI API.** `OpenAiApi::Responses` is OpenAI's Responses API and
`OpenAiApi::ChatCompletions` its Chat Completions API; an attempt's trace records them as
`responses` and `chat_completions`. The default follows the host, as upstream's does, so a
service that speaks OpenAI's protocol on another host gets Chat Completions.
The OpenAI-compatible Chat Completions shape is not verified against a live endpoint:
upstream recorded no exchange of it, and the code is ported from upstream's synthetic tests.

**Response size.** A response body is read under `max_response_bytes` and never past it; a
declared length over the cap is refused before a byte is read. A success body over the cap is
`ErrorKind::Provider` holding the SDK's `ResponseTooLarge` error with the limit. A failure status
with a body over the cap is an API error with its status and its headers; its body is not kept,
and its message, printed after the status, is
`The response body was larger than the limit and is not shown.`

**The default transport.** `Transport` is an opaque HTTP service over a pooled hyper-util client:
TLS through rustls with the platform's verifier plus the added roots, HTTP/2 or HTTP/1.1 as the
server chooses through ALPN on `https`, HTTP/1.1 on `http`. There is no HTTP-version setting. No
hyper type is public. Nothing connects until the first request.

**A custom transport.** `build_with_service(service)` takes the caller's HTTP service, with the
bound the SDK's own `build_with_service` has. The provider still bounds each attempt by its own
deadline and reads the response under its own cap. The adapter never formats a header value
itself, but a service may put the request headers into its error: see the next paragraph.

**The key in what comes back.** A server can send the key back, and a service can put it into its
error. So the adapter searches for the key, with every service, the default `Transport` included,
before it keeps or shows such text:

- When the service fails: every link of the error chain, up to 32, in `Display`, `Debug` and
  alternate `Debug`, and the message built from the chain. On a hit, or for a longer chain, the
  error is a connection error with no source and the text
  `Connection error: the transport's error is not shown, because showing it could reveal the API key or its chain of causes was too long to search.`
- A response with a status outside 2xx: the body as it arrived, each header name and value, and
  then the error built from them. On a hit the API error keeps the status and nothing else: it has
  no headers, so no request id and no `Retry-After` (a retry then waits the policy's own backoff),
  `ApiError::is_authentication()` is true only for status 401, and `ApiError::body()` returns a
  JSON object the adapter wrote, not the server's bytes, whose message, printed after the status,
  is `The response's body and headers are not shown, because showing them could reveal the API key.`
  Without a hit the body is kept as it arrived, and the vendor's message read from it may quote
  other parts of the request.
- A 2xx response, only when the provider reads a non-answer from it: the body as it arrived, the
  stop reason, and the non-answer's text. On a hit a built-in provider's non-answer reads
  `<Vendor> did not answer: the reason is not shown, because showing it could reveal the API key.`
  with `OpenAI`, `Anthropic` or `Gemini` for `<Vendor>`, its kind stays `ErrorKind::NonAnswer`,
  and the attempt's `finish_reason` is recorded as `null`. The trace keeps the body as received.
  A 2xx response that yields answers is not searched.

The search finds the key as written, as `Debug` of a string writes it and as a JSON string holds
it (with a few spellings derived from these). It does not find a key transformed in another way:
split over two links of an error chain, in hex or base64, or percent-encoded. A key cut to a
prefix, or spelled with a JSON `\u` escape for a character that `serde_json` writes as it is, is
not found in the bytes of a body; it is found only where the adapter or the SDK reads it, whole,
out of the body into a text (an error message, a stop reason, a non-answer's reason), and such a
text is cut at 200 characters. So such a key can be shown: in a member of a failure body that no
message is read from, or as the part before the cut. The search is over the whole body, so a
very short key hides text often: with the key `test`, any failure response whose body or headers
hold that word loses its message, its headers and its request id; over the 26 non-answer reasons
of this crate's test cases, the key `a` hid 26, `x` 12, `sk` 2, and `ab`, `abc` or `key` none.
Real bodies are longer, so a short placeholder key against a local server hides more there.

**A provider of your own.** `Provider` is a dyn-compatible trait with `model_name()`,
`request(call)` returning a boxed future, and two provided methods, `type_name()` and
`log_uri()`. `request` receives a `ProviderCall` (the messages, the answer schema, whether
structured output is asked for, and the attempt's `AttemptTrace`) and returns the model's text and
token counts as a `ProviderResult`, a `NonAnswer` when the model declined or did not finish, or a
`typesafe_sdk::Error` for a failure the retry policy may retry. `record_request` and
`record_response` on the `AttemptTrace` put the wire JSON into the trace, and its getters
`request()`, `api()`, `response()` and `finish_reason()` read it back. `ProviderCall::new`,
`AttemptTrace::default()`, `Schema::from_json` and `Deserialize` for `Message` exist so that such a
provider can be tested outside a client.

The adapter does not search what a provider of your own returns: give `NonAnswer::new` a fixed
text, such as the vendor's name and a status, never text from a response, which can hold the key.

`log_uri()` names the provider's endpoint in the SDK's retry line (see [Logging](#logging)); the
client passes it to the retry policy and to nothing else. The default is `None`, and the line
then names the request as `POST /`; the built-in providers return their log URI. An override
returns a `Uri` of the `http` crate, version 1, which this crate does not re-export, so its
crate depends on `http = "1"` itself. The line prints the scheme, the host, the port and the path
of that URI, never its query or userinfo: put no credential into a path segment.

```rust,no_run
use std::sync::Arc;

use system_one_adapter::{
    AnswerMode, BoxFuture, Client, NonAnswer, Provider, ProviderCall, ProviderResult,
    StructuredOutputs, typesafe_sdk,
};

/// A model that runs in this process.
#[derive(Debug)]
struct LocalModel;

impl Provider for LocalModel {
    fn model_name(&self) -> &str {
        "local-model"
    }

    fn request<'a>(
        &'a self,
        call: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async move {
            let prompt: Vec<&str> = call.messages().iter().map(|message| message.content()).collect();
            match generate(&prompt, call.schema().as_str()).await {
                Some(text) => Ok(Ok(ProviderResult::new(text, None, None))),
                // A fixed text, never the model's output.
                None => Ok(Err(NonAnswer::new("The local model stopped before it finished."))),
            }
        })
    }
}

# async fn generate(_prompt: &[&str], _schema: &str) -> Option<String> { None }
fn client() -> Result<Client, system_one_adapter::Error> {
    Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete)
        .provider_instance(Arc::new(LocalModel))
        .build()
}
# fn main() {}
```

## Response and trace

`Response` is `Clone` and `Serialize` (not `Deserialize`), with upstream's member names:

- `model()`: the model that answered.
- `answers()` / `into_answers()`: the SDK's `Answers`, or the caller's question set after `ask`.
- `usage()`: `input_tokens()` and `output_tokens()` of the last attempt; `input_tokens_total()`
  and `output_tokens_total()` over every attempt that returned a reply, `None` once any of them
  reported none;
  `n_retries()`, the provider retries of every turn; `n_retries_malformed_structure()`;
  `latency()`, a `Duration` serialized as seconds, measured from after the provider is resolved,
  the state written and the questions checked until the reply's answers are converted (before
  `ask` decodes them into its type).
- `debug()`: the `Trace`, serialized under the member name `debug`.

A choice's confidence is computed as upstream computes it, and is not clamped: labels of equal
probability can give a negative value of rounding size, for example `-3.469446951953614e-17` for
five labels at 0.04 each. A score's confidence is never below 0.

`Trace` holds `attempts()` (serialized as `llm_attempts`), `retry_reasons()`, and the probability
record of the reply: `max_error()`, `invalid_probs()`, `probability_errors()` and
`original_probabilities()`, the last serialized only when not empty. The trace of an `Error`
serializes `llm_attempts` and `retry_reasons` only, and its four probability accessors return 0
and nothing.

An `Attempt` is one request to the model: `messages()`, `schema()`, `structured()`, `request()`
(the JSON the provider sent), `response()` (the `llm_response` member as JSON text),
`model_name()`, `provider()`, `api()`, `finish_reason()`, `error()` and `error_type()`. It
serializes with these members, in this order:

```text
{"messages":[{"role":..,"content":..},..],
 "model_request_parameters":{"schema":<the schema object>,"structured":<bool>},
 "llm_response":<wire body, fallback object or null>,
 "debug_info":{"model_name":..,"provider":..,"api":..,"finish_reason":..,
               "error":..,"error_type":..},
 "request":<the request body>}
```

`debug_info.api` and `request` are present only when the provider recorded a request,
`finish_reason` only when it recorded a response (then possibly `null`), `error` and `error_type`
only on a failed attempt. `llm_response` is the wire JSON the provider recorded; when it recorded
none and the attempt returned a result, the object of that result's `text`, `input_tokens` and
`output_tokens`; `null` on a failed attempt that recorded none. `debug_info.provider` is
`Provider::type_name()`: the public path of a built-in provider, and by default the compiler's
name of the type, which is not promised to be stable. The serialized shape of `Response` and
`Trace` is defined for `serde_json`, whose raw values embed the recorded JSON unchanged.

A `RetryReason` has a `category()` (`RetryCategory::ProviderError` or `MalformedStructure`) and a
`message()`, and serializes as the pair upstream emits.

**What the trace holds, and what `Debug` prints.**
`Response::debug()`, `Error::debug()` and their serialized form hold the caller's document and the model's text:
every message sent, the schema, each request body and each response body as it was received,
with whatever else the vendor put into it. Treat a serialized response or trace as the data it was
made from. `Debug` of `Error`, `Response`, `Trace`,
`Attempt`, `RetryReason`, `Message`, `ProviderResult`, `AttemptTrace` and `Schema` prints counts
and kinds only: the model name, the usage, the number of answers, attempts, messages and retry
reasons, retry categories, roles, `api`, `finish_reason`, `error_type` and byte lengths; never
message content, the schema, request or response text, a retry reason's message or an attempt's
`error` text. One exception: `ErrorKind::Provider` renders the SDK error by the SDK's own rules
(the status, the endpoint, the vendor's message escaped and cut at 200 characters, the body as a
byte count), and a vendor's error message may quote parts of the request; a message that would
show the key is replaced (see "The key in what comes back" under [Providers](#providers)).

## Errors

`Error` is `Send + Sync + 'static`. `kind()` returns the `ErrorKind`, `source()` the SDK error or
the decode failure behind it, and `debug()` the `Trace` of the attempts made, `None` for an error
raised before the first attempt.

| `ErrorKind` | When |
| --- | --- |
| `Provider(typesafe_sdk::Error)` | The request failed and the retry policy gave up: the SDK's `Api`, `Connection`, `Timeout` or `ResponseTooLarge` kind. |
| `NonAnswer(NonAnswer)` | A 2xx answer the provider declared unfinished or refused, or a 2xx body that is not the vendor's JSON. Never retried. |
| `MalformedStructure` | The model's output still does not fit the answer schema after the last corrective turn, or, after `ask`, the answers do not fit the question set's type. |
| `InvalidRequest` | No model, no provider, no question, an unknown question type, fewer than two criteria, or a state that serializes to `null` or does not serialize to JSON. Nothing was sent. |
| `Config` | A provider could not be built: no key, a key with a byte that is illegal in a header value, a base URL that breaks the rules, an environment variable that is not UTF-8, `max_tokens` of 0, `max_response_bytes` of 0, a zero `timeout`, a root certificate given to `build_with_service`, or the TLS roots. |

`ErrorKind` is `#[non_exhaustive]`. The `Display` of an `Error` holds no model output and no
response text, with the exception named above for `ErrorKind::Provider`. A built-in provider's
`NonAnswer` names the vendor and the status, the stop reason or the word `refusal`, escaped and cut
as the SDK cuts server text, or reads the fixed text of the key search; the refusal text and an
error message from the body stay in the attempt's `llm_response`. The text of
`MalformedStructure`, which is also the retry reason and the message
of the corrective turn, names the expected question ids, the expected labels and the JSON type
found, never a string the model chose; it lists at most 8 problems and counts the rest.

## Retries

Two loops run inside one call.

**Provider failures** are retried by the SDK's `RetryPolicy`, the same type and the same rules as
in `typesafe-sdk-rust`: which errors are retried, the backoff, `Retry-After`, and a caller's
predicate. The default is `RetryPolicy::none()`, as upstream's default is no retry; pass
`RetryPolicy::default()` or a policy of your own to the builder or to one call. Each retry adds a
`RetryCategory::ProviderError` reason to the trace, whose message is the failed attempt's error
text. A `NonAnswer` ends the call without a retry and never reaches the policy's predicate.

The policy's budget (30 s by default) is checked only before a retry, and each attempt has its own
deadline (`timeout`, 600 s by default): a policy with retries makes no second attempt after an
attempt that ran longer than the budget, and a first attempt is never cut short by the budget.
This is upstream's behaviour too. To get retries after slow attempts, raise the policy's budget
or lower the provider's `timeout`.

**Malformed structure.** When the model's reply does not fit the answer schema, and
`n_retry_malformed_structure` allows it, the adapter sends a corrective turn: the model's reply as
it came, then a message that says what did not fit. Each such turn adds a
`RetryCategory::MalformedStructure` reason, and is an attempt of its own in the trace, with its
own run of the retry policy.

## Logging

With the `tracing` feature (on by default) the adapter emits two kinds of events, both at `DEBUG`
on the target `system_one_adapter`:

- one per HTTP exchange of a built-in provider: the method, the log URI, the status when a
  response arrived, the name of the error's kind when the exchange failed (`Api`, `Connection`,
  `Timeout` or `ResponseTooLarge`), and the elapsed milliseconds;
- one per finished attempt, from the client: the attempt's number within the call, the elapsed
  milliseconds, the input and output token counts of a reply, and on failure the name of the
  error's kind (`NonAnswer` among them). It has no method, URI or status. A provider of your own
  gets this event and not the first.

Neither holds a header, a body, a message, the schema or an error's text. Below the default
transport, crates such as `hyper_util` and `h2` emit events of their own under their own targets;
the key's header value is marked sensitive, so they print `Sensitive` in its place. rustls logs
through the `log` crate, not `tracing`.

The retry log line is the SDK's, not the adapter's: before each retry the SDK writes the method,
the log URI that `Provider::log_uri()` returns (`POST /` for `None`) and the retry number at
`INFO` on the target `typesafe_sdk`. That line exists only when the SDK is built with its
`tracing` feature, which this crate's `tracing` feature turns on; a build with
`default-features = false` and no `tracing` has neither the events nor the retry line.

The log URI is the scheme, the host (and the port when it is not the scheme's default) and the
fixed path of the operation as the vendor documents it: `/v1/responses`, `/v1/chat/completions`,
`/v1/messages` or `/v1beta/interactions`. It never holds userinfo, a query, or the path prefix of
a caller's base URL.

## Testing

`cargo nextest run` and `cargo test`, without `--workspace`, run the workspace's default members,
this crate among them. Its tests replay recorded and scripted HTTP exchanges against a local
server and need neither a key nor the network. Several test binaries need a feature:
`providers_openai`, `providers_anthropic`, `providers_gemini`, `cassettes`, `lifecycle`,
`parity_schema` and `parity_metrics` need `internals` (with the default providers), under which
the library reads no process environment, so no test can pick up a key of the machine it runs on;
`typed` needs `macros`. `cargo nextest run -p typesafe-sdk-rust-adapter --all-features` runs them
all, and `--features internals` all but `typed`. `docs/adapter-port-test-matrix.md` maps every
test of the Python adapter to the Rust tests that cover it, the deviation that explains why none
does, or the reason it was left out, and `docs/uncovered-lines.md` records this crate's measured
line coverage and why each uncovered line is not reached.

The files under `tests/fixtures/` are upstream's recorded exchanges and expected responses. A new
or re-recorded fixture must pass `python3 .github/scripts/no-placeholders.py` and
`python3 .github/scripts/text-hygiene.py` like any tracked file: the first reads what follows a
`#` on a line as a comment, and the prompted recordings hold `#/$defs/...` references.

`crates/adapter-live-tests` holds the tests against the live vendor APIs. It is a workspace member
but not a default member. **Its tests make real, billed calls** to OpenAI, Anthropic and Gemini
when `TYPESAFE_ADAPTER_LIVE_TESTS=1` and the provider's key are both set and a command reaches
them. Without the variable or without the key a test fails, never skips, before any request is
made, so a key exported for other work bills nobody. Run them only on purpose:

```sh
TYPESAFE_ADAPTER_LIVE_TESTS=1 OPENAI_API_KEY=... ANTHROPIC_API_KEY=... GEMINI_API_KEY=... cargo nextest run -p typesafe-sdk-rust-adapter-live-tests --no-fail-fast --retries 0
```

`--no-fail-fast` because nextest otherwise stops at the first failure and the remaining cases are
not run; `--retries 0` because a case that is run again is billed again.

The crate runs upstream's two live tests for the three providers, the two structured modes and the
two answer modes: 24 cases, each one request with no retry and no corrective turn, under a deadline
of 120 s. The models are upstream's: `gpt-4o-mini`, `claude-haiku-4-5` and
`gemini-3.5-flash-lite`. Each case reads its key in the test, gives it to the provider's builder
through `api_key`, and hands the provider to the client through `provider_instance`; a
`--workspace` build turns on `internals` for the adapter, which then reads no process
environment. Each case writes one line to stderr, `live-request <provider> <structured> <mode>
attempts=<n>`, so the requests a run sent can be counted from its output.

## Deviations from the Python adapter

Each row says what system-one-adapter-python 0.2.1 does, with the upstream line, and what this
crate does instead. Upstream lines are cited as `path:line` at commit `e1d4cc9`; a path that
starts with `_` or `providers/` is relative to `src/system_one_adapter/`. A path that starts with
`openai/`, `anthropic/`, `google/` or `httpx2/` is a file of that vendor package as upstream's
`uv.lock` installs it. A path that starts with `tests/` is relative to the root of upstream's
repository.

| Deviation | Python adapter | This crate |
| --- | --- | --- |
| Synchronous client | `SystemOneAdapterClient` beside `AsyncSystemOneAdapterClient`, and a `SyncProvider` protocol (`_client.py:459,554`, `providers/base.py:62`). | Async only, on Tokio, as the SDK. This row covers the synchronous half of every upstream test that runs against both clients. |
| Closing the client | `close()`, `aclose()`, both context managers, and the error `The adapter client is closed.` after closing (`_client.py:412-413,505,541,600,642`). | Dropping the last clone of the client drops the providers it owns; there is no closed state. Upstream's six close tests have no counterpart (`test_async_cleanup_propagates_cancellation_after_remaining_cleanup`, `test_cancelling_close_waiter_does_not_interrupt_cleanup`, `test_cleanup_continues_after_failure`, `test_close_before_first_use_does_not_construct_providers`, `test_concurrent_close_waits_for_same_cleanup`, `test_exceptional_exit_closes_owned_sdks`), nor has the close half of `test_reuses_owned_provider_and_closes_sdk_on_context_exit`. |
| Provider names | A string, with a pip-extra error for a provider that is not installed and an `Unknown provider` error (`providers/__init__.py:32-39,83`). | `ProviderName`, an enum whose variants are feature-gated: a disabled provider is absent at compile time, so there is no pip-extra error; an unknown name is the `FromStr` error of `ProviderName`; the client never receives a name outside the enum. |
| Response type | `SystemOneResponse` subclasses the SDK's response (`_response.py:25`). | `Response<A = Answers>` is the adapter's own type, without the SDK response's API-call members; typed answers come through `AnswerSet`. |
| Errors | The SDK's exceptions, and a pydantic `ValidationError` for an invalid question (`tests/test_schema.py:36-41`); the trace is an attribute set on the exception. | The adapter's `Error` wraps `typesafe_sdk::Error` and adds the kinds `InvalidRequest`, `Config`, `NonAnswer` and `MalformedStructure`; an invalid question is `InvalidRequest`; the trace is the typed accessor `Error::debug()`. |
| Correction-prompt and validation wording | The correction prompt quotes pydantic's validation text, which quotes model output, in a user message outside `<document>` (`_client.py:110-115`). | The adapter's own wording: it names the expected ids, the expected labels and the JSON type found, never a string the model chose; it lists at most 8 problems and counts the rest, each item escaped and cut at 200 characters plus U+2026. |
| Latency type | `Usage.latency` is a float of seconds (`_response.py:22`). | A `Duration`, serialized as seconds. |
| Provider builder options | `base_url` on OpenAI only (`providers/openai.py:100`), and no other transport option. | `base_url` on all three providers; `timeout`, `max_response_bytes`, `add_root_certificate` and `build_with_service` on every provider; no HTTP-version setting. Under `build_with_service` the adapter checks every rendering of the service's error chain for the key and replaces the chain with a fixed text on a hit. |
| Timeouts | The vendor SDKs' timeouts. OpenAI and Anthropic use `Timeout(timeout=600, connect=5.0)`: 600 s for each read, write and pool wait (`openai/_constants.py:7`, `anthropic/_constants.py:7`). Gemini sets none by default (`google/genai/_gaos/google_genai.py:204`). A timeout becomes `TypeSafeAPITimeoutError` built from `httpx2.Timeout(None)`, without a duration (`_utils/error_handling.py:65-66`). | One total deadline per attempt for all three providers, 600 s by default, and the timeout error names the deadline that expired. The 600 s figure is upstream's; what differs is total against per-phase, that Gemini is bounded too, and the error text. |
| Dropped vendor-SDK settings and headers | Through the vendor SDKs: `OPENAI_ORG_ID` and `OPENAI_PROJECT_ID` (`openai/_client.py:279,283`), `OPENAI_CUSTOM_HEADERS` (`openai/_client.py:315`), `ANTHROPIC_CUSTOM_HEADERS` (`anthropic/_client.py:242`), `ANTHROPIC_AUTH_TOKEN` (`anthropic/_client.py:223`) and profile federation (`anthropic/_client.py:99-100`), Gemini's Vertex mode (`google/genai/_api_client.py:641`), the Anthropic SDK's refusal of a large non-streaming `max_tokens` (`anthropic/_base_client.py:766-767`), and the `x-stainless-*` and `x-goog-api-client` headers (`google/genai/_api_client.py:164`). | None of these: the variables are not read and the headers are not sent. |
| Attempt trace | `debug_info.provider` is a Python class, `error_type` an exception class, and `llm_response` the vendor SDK's dump of its response object (`_client.py:236-237,243-244`). | `provider` is `Provider::type_name()`: by default the compiler's type name (not promised stable), overridden by the built-in providers with their public path. `error_type` is the SDK's kind name for a provider failure (`Api`, `Connection`, `Timeout`, `ResponseTooLarge`) and the adapter's kind name otherwise. `llm_response` is the wire JSON body, with upstream's fallback object when no response was recorded. Against upstream's expected responses, numbers compare by value and these members exist on one side only: OpenAI `created_at` and `completed_at` (floats in the dump, integers on the wire); Gemini `id` and `output_text` (dump only); Gemini `usage.model_invocation_token_counts`, `usage.non_grounding_model_invocation_token_counts` and `usage.raw_prompt_token` (wire only). |
| Where retry reasons are recorded | In tenacity's `before_sleep` hook (`_utils/error_handling.py:75-82`). | Around the attempt closure that the SDK's retry runner calls; the recorded reasons are identical. |
| Environment in tests | Tests set the process environment with `monkeypatch` (`tests/test_provider_lifecycle.py:281`). | Tests inject a lookup function; the process environment is never mutated. |
| Non-answers and the retry predicate | A non-answer is raised inside the retry loop (`providers/anthropic.py:57-63`). | It travels outside the retry loop, so a caller's `RetryPolicy::predicate` never sees it; a predicate that retried non-answers upstream has no effect here. |
| Non-finite numbers in the state | `to_json` writes the bare words `NaN`, `Infinity` and `-Infinity` (`_client.py:91`). | `serde_json` writes `null` for each. A state held as a `serde_json::Value` also differs from Python for three inputs: `-0` is written `-0.0`; an integer beyond u64 becomes a float; `1e400` is a parse error in the default build and is written `1e+400` under serde_json's `arbitrary_precision` feature, where Python writes `Infinity`. A typed Rust state is unaffected. |
| No translation of provider-SDK exceptions | `translating` and `translate_error` map vendor-SDK exceptions; an unknown exception becomes a bare `TypeSafeError` (`providers/base.py:28-41`, `_utils/error_handling.py:72`). | There is no vendor SDK: the HTTP module produces the SDK error directly. `test_translating_context_manager_reraises_translated_error` and `test_unknown_and_sdk_errors_pass_through` have no counterpart. |
| Proxy and CA variables | httpx2 with `trust_env=True` (`httpx2/_client.py:192`) reads `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (`httpx2/_utils.py:52`) and `SSL_CERT_FILE` (`httpx2/_config.py:35`). | No proxy variable and no CA variable is read; the trust anchors are the platform's roots plus `add_root_certificate`. |
| Redirects | The OpenAI and Anthropic SDK clients follow redirects (`openai/_base_client.py:876,1471`, `anthropic/_base_client.py:906,1584`). | A 3xx answer is an API error after exactly one request. |
| Connect timeout | OpenAI and Anthropic have a separate 5 s connect timeout (`openai/_constants.py:7`, `anthropic/_constants.py:7`). | No separate connect timeout; connecting counts against the attempt deadline of the row `Timeouts`. |
| `GOOGLE_GEMINI_BASE_URL` | `google-genai` reads it (`google/genai/_base_url.py:50`). | Not read; Gemini's base URL is set only through the builder. |
| User agent | Each vendor SDK sends its own `User-Agent` (`openai/_base_client.py:711`); upstream's recorded exchanges all carry one. | `typesafe-sdk-rust-adapter/` followed by the crate's version; no vendor SDK's value. |
| `Response` is `Serialize` only | A response round-trips through `model_dump_json` and `model_validate_json` (`tests/test_client_with_fake_model.py:146-150`). | `Response` serializes to upstream's shape but does not deserialize. The round-trip half of `test_sdk_questions_and_response_serialization` has no counterpart. |
| Non-answer wording | The messages name Python classes (`Increase max_tokens on AnthropicProvider or AsyncAnthropicProvider`, `providers/anthropic.py:57-61`) and print a Python list of Gemini errors (`providers/gemini.py:79-81`); a refusal text and an error message from the body are printed (`providers/openai.py:85`, `providers/gemini.py:81`). | The adapter's own wording, with no Python class or list syntax: the vendor and the status, the stop reason or the word `refusal`. The refusal text and the body's error message stay in the attempt's `llm_response`. |
| Number text of non-string content | Instructions and criteria that are not strings are written with pydantic's `to_json` (`_schema.py:250-255`). | Their compact JSON text as the SDK prepared it, kept unparsed in member order; a number keeps the text the SDK's serializer wrote, which is not always pydantic's text for the same number. The number text of a `serde_json::Value` state differs from Python too, for `-0`, an integer beyond u64 and `1e400`: see the row `Non-finite numbers in the state`. |
| Response-size cap | The vendor SDK clients are built with no size limit (`providers/openai.py:121`). | 16 MiB by default, as the SDK; a success body over the cap is `ErrorKind::Provider` carrying the SDK's `ResponseTooLarge` with the limit; a cap of 0 is a `Config` error. |
| A `null` state | Refuses `state is None` with a `ValueError` before serializing (`_client.py:431-432`). | Any state that serializes to `null` (`None`, `()`, a unit struct) is `InvalidRequest`. |
| Duplicate members | Pydantic's parser keeps the last value of a member written twice in the reply and ignores an earlier value that does not fit, but it still parses that earlier value as JSON: a lone surrogate escape, or nesting of 200 levels or more, inside an earlier duplicate makes it refuse the whole reply (no upstream line). | The last value is used too, and an earlier duplicate's value is skipped without being parsed, so such a reply is accepted. A lone surrogate escape in the last duplicate, in a member name or in a chosen label is refused by both. |

## License

This crate is licensed under the Apache License 2.0; see `LICENSE`. It is a port of
system-one-adapter-python 0.2.1 (commit `e1d4cc9`), which is MIT-licensed; that license text is
reproduced, as the upstream repository ships it, in `LICENSE-THIRD-PARTY`
and applies to the material derived from it, including the recorded exchanges and expected
responses under `tests/fixtures/`.
