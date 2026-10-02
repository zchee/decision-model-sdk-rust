# System One adapter for Rust

Ask TypeSafe System One questions of an OpenAI, Anthropic or Gemini model instead of the TypeSafe
API: the same prepared questions go to the model, and its reply comes back as the answers the
`typesafe-sdk-rust` crate returns, with probabilities, usage and a trace of every attempt. It is a
port of the Python package
[system-one-adapter-python](https://github.com/typesafe-ai/system-one-adapter-python) 0.2.1
(commit `e1d4cc9`), useful for comparing System One against a general-purpose model on cost, speed
and quality.

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
| `internals` | off | Exposes a hidden `system_one_adapter::__internals` module used by this repository's parity tests and fuzz targets. It carries **no semver promise**; do not depend on it. |

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
3. Prepare the questions with the SDK's builder, re-exported here:
   `Questions::new().noul("positive", Noul::new().instructions("The review is positive.")).prepare()`.
4. Call `client.system_one(&state, &questions)`, where `state` is any value that implements
   `serde::Serialize` (the document the questions are about), optionally override `provider`,
   `model`, `provider_instance` or `retry` for this call, then `.send().await`.
5. The result is a `Response` whose `answers()` are the SDK's `Answers`, or an `Error` (see
   [Errors](#errors)).

The first call for a provider name and model builds that provider and reads its key from the
environment (see [Providers](#providers)); later calls reuse it.

## Typed answers

`client.ask::<Q>(&state)` takes the questions from a type that implements the SDK's `QuestionSet`
and returns `Response<Q>`: the answers are decoded into the fields of `Q`.

A caller that derives question sets depends on `typesafe-sdk-rust` directly, with its `macros`
feature, and writes a plain `#[derive(QuestionSet)]` (recommended); or it enables this crate's
`macros` feature and points the derive at the re-exported SDK with
`#[question_set(crate = "system_one_adapter::typesafe_sdk")]`.
The attribute is needed in the second form because the derive's expansion names `::typesafe_sdk`
unless told otherwise, and a crate that depends on the adapter alone has no crate of that name.

## Client options

| Method of `ClientBuilder` | Default | Meaning |
| --- | --- | --- |
| `Client::builder(structured_outputs, llm_answer_mode)` | required | `StructuredOutputs::Native` or `Prompted`; `AnswerMode::Probabilities` or `Discrete`. |
| `provider(ProviderName)` | none | The built-in provider that a model name is given to. |
| `model(impl Into<String>)` | none | The model name. |
| `provider_instance(Arc<dyn Provider>)` | none | A provider the caller built or implemented. It is borrowed: the client never caches it. |
| `normalize_probabilities(bool)` | `false` | Whether a probability distribution of a choice or score question whose sum is off by more than 1e-6 is rescaled to sum to 1. The trace records how far each sum was off and, for a rescaled one, the original values. |
| `n_retry_malformed_structure(u32)` | `0` | How many corrective turns follow a reply that does not fit the answer schema. |
| `retry(RetryPolicy)` | `RetryPolicy::none()` | The SDK's retry policy for provider failures (see [Retries](#retries)). |

A call (`Request`, returned by `system_one` and `ask`) takes `provider`, `model`,
`provider_instance` and `retry` as overrides. The provider of a call is resolved in this order:
the call's `provider_instance`, else the call's `model`, else the client's `provider_instance`,
else the client's `model`. A model name needs a provider name, from the call or else from the
client. A call with no model, or with a model name and no provider name, fails with
`ErrorKind::InvalidRequest` before anything is sent.

A provider that the client builds from a name and a model is kept for the life of the client,
keyed by that pair, and shared by the client's clones. It is built on first use; concurrent first
calls build one instance, and a build that fails is not kept, so the next call tries again. The
providers are dropped with the last clone of the client. There is no `close`.

Inside `send`, in this order: the provider is resolved, and built if the client owns it; a state
that serializes to `null` is refused as `ErrorKind::InvalidRequest`; the questions are validated
again; the latency clock starts; the model is asked. A missing provider is therefore reported
before an invalid question.

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
| `api(OpenAiApi)` | yes | - | - | `OpenAiApi::Responses` when the base URL's host is exactly `api.openai.com`, else `OpenAiApi::ChatCompletions` |
| `max_tokens(u32)` | - | yes | - | 4096; 0 is refused |
| `timeout(Duration)` | yes | yes | yes | 600 s for each attempt; zero is refused |
| `max_response_bytes(usize)` | yes | yes | yes | 16 MiB, the SDK's value; 0 is refused |
| `add_root_certificate(der)` | yes | yes | yes | none; a certificate is added to the platform's roots and never replaces them |
| `build()` | yes | yes | yes | the default transport, `Transport` |
| `build_with_service(service)` | yes | yes | yes | the caller's HTTP service in place of `Transport` |

A refused value, a missing key, a key with a byte that is illegal in a header value and a base URL
that breaks the rules below are each `ErrorKind::Config`, reported when the provider is built and
never when a request is sent.

**Keys.** When `api_key` is not called, the key is read from the environment:

| Provider | Key variable | Base URL variable | Default base URL |
| --- | --- | --- | --- |
| OpenAI | `OPENAI_API_KEY` | `OPENAI_BASE_URL` | `https://api.openai.com/v1` |
| Anthropic | `ANTHROPIC_API_KEY` | `ANTHROPIC_BASE_URL` | `https://api.anthropic.com` |
| Gemini | `GOOGLE_API_KEY`, else `GEMINI_API_KEY` | none | `https://generativelanguage.googleapis.com` |

The environment is read once, when a provider is built: by `build()` or `build_with_service` for
a provider built explicitly, at the first call that uses it for a provider the client owns. A
variable changed later has no effect on a provider that exists. `GOOGLE_GEMINI_BASE_URL`, the proxy
variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`) and `SSL_CERT_FILE` are not read.

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

**The OpenAI API.** `OpenAiApi::Responses` is OpenAI's Responses API and
`OpenAiApi::ChatCompletions` its Chat Completions API. The default follows the host, as upstream's
does, so a service that speaks OpenAI's protocol on another host gets Chat Completions.
The OpenAI-compatible Chat Completions shape is not verified against a live endpoint: upstream recorded no exchange of it, and the code is ported from upstream's synthetic tests.

**Response size.** A response body is collected under `max_response_bytes`. A success body over
the cap is `ErrorKind::Provider` holding the SDK's `ResponseTooLarge` error with the limit; a
failure status with a body over the cap stays an API error, as in the SDK.

**The default transport.** `Transport` is an opaque HTTP service over a pooled hyper-util client:
TLS through rustls with the platform's verifier plus the added roots, HTTP/1.1 or HTTP/2 as the
server negotiates. There is no HTTP-version setting. No hyper type is public.

**A custom transport.** `build_with_service(service)` takes the caller's HTTP service, with the
bound the SDK's own `build_with_service` has. The adapter never formats a header value itself, but
a foreign service may put the request headers into its error. Under
`build_with_service`, when a call of the service fails, the adapter looks for the key in every
rendering of the error chain (`Display`, `Debug` and alternate `Debug` of every `source()` link)
and, on a hit, replaces the whole chain with a fixed text.

**A provider of your own.** `Provider` is a dyn-compatible trait with `model_name()`,
`request(call)` returning a boxed future, and a provided `type_name()`. `request` receives a
`ProviderCall` (the messages, the answer schema, whether structured output is asked for, and the
attempt's `AttemptTrace`) and returns the model's text and token counts as a `ProviderResult`, a
`NonAnswer` when the model declined or did not finish, or a `typesafe_sdk::Error` for a failure
the retry policy may retry. `record_request` and `record_response` on the `AttemptTrace` put the
wire JSON into the trace. `ProviderCall::new`, `AttemptTrace::default()`, `Schema::from_json` and
`Deserialize` for `Message` exist so that such a provider can be tested outside a client.

## Response and trace

`Response` is `Clone` and `Serialize` (not `Deserialize`), with upstream's member names:

- `model()`: the model that answered.
- `answers()` / `into_answers()`: the SDK's `Answers`, or the caller's question set after `ask`.
- `usage()`: `input_tokens()` and `output_tokens()` of the last attempt; `input_tokens_total()`
  and `output_tokens_total()` over every attempt, `None` once any attempt reported none;
  `n_retries()`; `n_retries_malformed_structure()`; `latency()`, a `Duration` serialized as
  seconds, measured from after the provider is resolved until the answers are converted.
- `debug()`: the `Trace`, serialized under the member name `debug`.

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
 "debug_info":{"model_name":..,"provider":..,"api":..,"finish_reason":..,"error":..,"error_type":..},
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
every message sent, the schema, each request body and each response body. Treat a serialized
response or trace as the data it was made from. `Debug` of `Error`, `Response`, `Trace`,
`Attempt`, `RetryReason`, `Message`, `ProviderResult`, `AttemptTrace` and `Schema` prints counts
and kinds only: the model name, the usage, the number of answers, attempts, messages and retry
reasons, retry categories, roles, `api`, `finish_reason`, `error_type` and byte lengths; never
message content, the schema, request or response text, a retry reason's message or an attempt's
`error` text. One exception: `ErrorKind::Provider` renders the SDK error by the SDK's own rules
(the status, the endpoint, the vendor's message escaped and cut at 200 characters, the body as a
byte count), and a vendor's error message may quote parts of the request.

## Errors

`Error` is `Send + Sync + 'static`. `kind()` returns the `ErrorKind`, `source()` the SDK error or
the decode failure behind it, and `debug()` the `Trace` of the attempts made, `None` for an error
raised before the first attempt.

| `ErrorKind` | When |
| --- | --- |
| `Provider(typesafe_sdk::Error)` | The request failed and the retry policy gave up: the SDK's `Api`, `Connection`, `Timeout` or `ResponseTooLarge` kind. |
| `NonAnswer(NonAnswer)` | A 2xx answer the provider declared unfinished or refused, or a 2xx body that is not the vendor's JSON. Never retried. |
| `MalformedStructure` | The model's output still does not fit the answer schema after the last corrective turn. |
| `InvalidRequest` | No model, no provider, an unknown question type, fewer than two criteria, or a state that serializes to `null`. Nothing was sent. |
| `Config` | A provider could not be built: no key, a key with a byte that is illegal in a header value, a base URL that breaks the rules, `max_tokens` of 0, `max_response_bytes` of 0, a zero `timeout`, or the TLS roots. |

`ErrorKind` is `#[non_exhaustive]`. The `Display` of an `Error` holds no model output and no
response text, with the exception named above for `ErrorKind::Provider`. A `NonAnswer` names the
vendor and the status, the stop reason or the word `refusal`, escaped and cut as the SDK cuts
server text; the refusal text and an error message from the body stay in the attempt's
`llm_response`. The text of `MalformedStructure`, which is also the retry reason and the message
of the corrective turn, names the expected question ids, the expected labels and the JSON type
found, never a string the model chose; it lists at most 8 problems and counts the rest.

## Retries

Two loops run inside one call.

**Provider failures** are retried by the SDK's `RetryPolicy`, the same type and the same rules as
in `typesafe-sdk-rust`: which errors are retried, the backoff, `Retry-After`, and a caller's
predicate. The default is `RetryPolicy::none()`, as upstream's default is no retry; pass
`RetryPolicy::default()` or a policy of your own to the builder or to one call. Each retry adds a
`RetryCategory::ProviderError` reason to the trace. A `NonAnswer` ends the call without a retry
and never reaches the policy's predicate.

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

With the `tracing` feature (on by default) the adapter emits one event per finished attempt at
`DEBUG` on the target `system_one_adapter`, with these fields only: the method, the log URI, the
status when a response arrived, the attempt number, the elapsed milliseconds, the input and output
token counts, and on failure the name of the error kind. Never a header, a body, a message, the
schema or an error's text.

The retry log line is the SDK's, not the adapter's: before each retry the SDK writes the method,
the log URI and the retry number at `INFO` on the target `typesafe_sdk`. That line exists only
when the SDK is built with its `tracing` feature, which this crate's `tracing` feature turns on;
a build with `default-features = false` and no `tracing` has neither the events nor the retry
line.

The log URI is the scheme, the host (and the port when it is not the scheme's default) and the
fixed path of the operation as the vendor documents it: `/v1/responses`, `/v1/chat/completions`,
`/v1/messages` or `/v1beta/interactions`. It never holds userinfo, a query, or the path prefix of
a caller's base URL.

## Testing

`cargo nextest run` and `cargo test`, without `--workspace`, run the workspace's default members,
this crate among them. Its tests replay recorded and scripted HTTP exchanges against a local
server and need neither a key nor the network; several test binaries need a feature, so
`cargo nextest run -p typesafe-sdk-rust-adapter --all-features` runs them all.

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

## License

This crate is licensed under the Apache License 2.0; see [`LICENSE`](LICENSE). It is a port of
system-one-adapter-python 0.2.1 (commit `e1d4cc9`), which is MIT-licensed; that license text is
reproduced, as the upstream repository ships it, in [`LICENSE-THIRD-PARTY`](LICENSE-THIRD-PARTY)
and applies to the material derived from it, including the recorded exchanges and expected
responses under `tests/fixtures/`.
