# TypeSafe AI Rust SDK

An async Rust client for the [TypeSafe AI](https://typesafe.ai) API: the System One endpoint
(`POST /v1/systemone`), which answers named questions about a state, and the model listing
(`GET /v1/models`). It is a port of the official Python SDK,
[typesafe-sdk-python](https://github.com/typesafe-ai/typesafe-sdk-python) 0.7.1 (commit
`0ffd094`); the places where it behaves differently on purpose are listed under
[Deviations from the Python SDK](#deviations-from-the-python-sdk).

The package is published as **`typesafe-sdk-rust`**, because the name `typesafe-sdk` is already
taken on crates.io. The library it builds is **`typesafe_sdk`**, so code writes
`use typesafe_sdk::...`.

## Install

```toml
[dependencies]
typesafe-sdk-rust = "0.2.0"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The minimum supported Rust version is **1.98**, and the crate uses edition 2024.

| Feature | Default | What it does |
| --- | --- | --- |
| `hyper` | on | The built-in transport (hyper over rustls, OS trust store): `Client::builder()`, `build()`, `from_env()`, `HttpVersion`, `add_root_certificate`, `http_version`, `connect_timeout`. Without it, start from `ClientBuilder::new()` and `build_with_service`; hyper and rustls are not compiled. |
| `macros` | on | `#[derive(QuestionSet)]`: questions declared as a struct and serialized at compile time, answers decoded straight into its fields. Pulls in the `typesafe-sdk-rust-macros` crate at the exact version this SDK release pins (the two crates are versioned independently). |
| `tracing` | on | Log events through the [`tracing`](https://docs.rs/tracing) crate (see [Logging](#logging)). Without it, every event is compiled out. |
| `sonic` | off | JSON through sonic-rs (SIMD, carries `unsafe`) instead of serde_json; faster on large bodies (ledger S7); drops the sign of a literal `-0.0`. |
| `internals` | off | Exposes a hidden `typesafe_sdk::__internals` module used by this repository's allocation tests and benchmarks. It carries **no semver promise**; do not depend on it. |

serde_json is compiled with `float_roundtrip` and `raw_value`, which Cargo also turns on for the
application's own serde_json. `arbitrary_precision` from elsewhere in the graph is supported;
under it, a `RawJson` read by the application's own serde_json keeps the number as serde_json
scanned it (`0.50`; `1E2` becomes `1e+2`) instead of the canonical `100.0`. A `RawJson` written
through another serializer rounds integers beyond `u64`/`i64` to `f64`, as serde_json does
without `arbitrary_precision`. Cargo features are additive: enabling `sonic` anywhere in the
graph selects sonic-rs for the SDK.

## Runtime requirement

Every network operation is `async` and runs on the caller's [Tokio](https://tokio.rs) runtime,
which must have its **time driver enabled**: each attempt has a deadline, retries sleep between
attempts, and HTTP/2 keep-alive pings run on a timer. `#[tokio::main]` and
`Builder::enable_all()` / `enable_time()` enable it; Tokio panics when a timer is created on a
runtime without it.

## Quickstart

A call asks a set of named questions about a state. The set is validated and serialized once by
`Questions::prepare`, and the resulting `PreparedQuestions` is reused by every call that asks it.

```rust,no_run
use typesafe_sdk::{Choice, Client, Noul, Questions, Score};

#[tokio::main]
async fn main() -> Result<(), typesafe_sdk::Error> {
    // Reads TYPESAFE_API_KEY (and optionally TYPESAFE_BASE_URL, TYPESAFE_DEFAULT_MODEL).
    let client = Client::from_env()?;

    let questions = Questions::new()
        .noul("billing", Noul::new().instructions("Is this about billing?"))
        .choice("tone", Choice::new(["calm", "angry"]).instructions("What is the tone?"))
        .score("urgency", Score::new(["can wait", "this week", "today"]))
        .prepare()?;

    let state = "I was charged twice for one order.";
    let response = client.system_one(state, &questions).model("jev-latest").send().await?;

    let answers = response.answers();
    if let Some(billing) = answers.noul("billing") {
        println!("billing: {}", billing.noul());
    }
    if let Some(tone) = answers.choice("tone") {
        println!("tone: {} ({})", tone.choice(), tone.confidence());
    }
    if let Some(urgency) = answers.score("urgency") {
        println!("urgency: {} ({})", urgency.score(), urgency.confidence());
    }
    println!("request id: {:?}", response.meta().request_id());
    Ok(())
}
```

`state` is anything that implements `serde::Serialize` and encodes as a JSON string, object or
array; it is encoded straight into the request body when the request is sent. A request is
configured with `.model()`, `.timeout()` / `.no_timeout()`, `.header()`, `.retry()` and
`.extra_body(name, &value)` (a top-level body member the SDK does not model; a later member of
the same name replaces an earlier one, and one named `state`, `model` or `questions` replaces
the built-in member). Nothing is checked or encoded before `.send()`.

A question that the builders do not model is written with `RawQuestion`, which sends its
fields unread and applies only the checks the Python SDK applies to a raw dictionary (`type` is
a non-empty string, a `choice` or `score` has `criteria`, a `score`'s `criteria` is not empty):

```rust
use typesafe_sdk::{Questions, RawQuestion};

fn main() -> Result<(), typesafe_sdk::Error> {
    let questions = Questions::new()
        .raw("spam", RawQuestion::new("noul").field("instructions", "Is this spam?"))
        .prepare()?;
    assert_eq!(questions.names().collect::<Vec<_>>(), ["spam"]);
    Ok(())
}
```

The answers of a response are looked up by question name (`answers().noul(name)`,
`.choice(name)`, `.score(name)`, `.get(name)`), iterated in wire order (`iter()`), or filtered
by kind (`nouls()`, `choices()`, `scores()`, which copy nothing). An answer of a type this
version does not know is skipped with a `WARN` event; the received bytes stay available through
`response.meta().raw_body()`, together with the status and the headers.

Responses and answers implement `Serialize`, and the answer types implement `Deserialize`, so
they can be stored and read back. They serialize with any serde format, but reading them back is
supported through JSON codecs (sonic-rs, `serde_json`): through a non-JSON serde format, a
score whose legend holds text fails, because a level's description is read back as JSON text.

## Typed answers

With the `macros` feature, a struct with one field per question is both the question set and
the answer type. The questions JSON is generated at compile time, and the response decodes
straight into the fields, without a map in between.

```rust,no_run
use typesafe_sdk::{ChoiceAnswer, Client, NoulAnswer, QuestionSet, ScoreAnswer};

#[derive(QuestionSet)]
struct Ticket {
    #[noul(instructions = "Is this about billing?", yes = "payments or invoices")]
    billing: NoulAnswer,
    #[choice(instructions = "What is the tone?", options("calm" = "neutral or polite", "angry"))]
    tone: ChoiceAnswer,
    #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
    urgency: ScoreAnswer,
}

#[tokio::main]
async fn main() -> Result<(), typesafe_sdk::Error> {
    let client = Client::from_env()?;
    let response = client.ask::<Ticket>("I was charged twice for one order.").send().await?;
    let ticket = response.answers();
    println!("billing: {}", ticket.billing.noul());
    println!("tone: {}", ticket.tone.choice());
    println!("urgency: {}", ticket.urgency.score());
    Ok(())
}
```

`client.ask::<Ticket>(&state)` is `client.system_one(&state, Ticket::prepared()).typed::<Ticket>()`;
the second spelling is the one to use where the builder's type has to be named. The derive
accepts text content only; object or array content stays a feature of the runtime `Questions`
builder. It refuses at compile time a duplicate wire name, option or level, a score without
levels, a field whose type does not match its question kind, and an `Option<...Answer>` field.
`#[question(name = "...")]` sends a key that is not a Rust identifier, and
`#[question_set(crate = ...)]` points the generated code at a renamed dependency.

## Models

```rust,no_run
use typesafe_sdk::{Client, Error};

async fn print_models(client: &Client) -> Result<(), Error> {
    let listed = client.models().list().send().await?;
    for model in listed.models() {
        println!("{} ({}): {}", model.name(), model.release_date(), model.description());
    }
    Ok(())
}
```

## Errors

Every fallible operation returns `typesafe_sdk::Error`, one pointer wide. `Error::kind()` says
what failed:

| `ErrorKind` | Meaning |
| --- | --- |
| `Config` | The client could not be built: no API key, a key that is blank or holds whitespace, a control or a non-ASCII character once trimmed, a base URL that is not an absolute `http`/`https` URL without userinfo, query or fragment, a zero deadline or size limit, an environment variable that is not UTF-8, a default header that cannot be sent. |
| `InvalidRequest` | The request was never sent: a header that is not a valid header, a zero deadline, a `state` that encodes as a number, a boolean or `null`, a value that cannot be encoded as JSON, an invalid question set. |
| `Api(ApiError)` | The server answered with a status outside 2xx. |
| `Connection` | No HTTP response was read: the connection failed, was refused, or broke. `source()` leads to the transport's error, or to a redacted copy of its chain when that chain printed a credential of the request. |
| `Timeout { timeout }` | An attempt ran past its deadline. |
| `ResponseValidation(ResponseValidationError)` | A 2xx body did not decode into the expected response; `field_path()` names the offending field (`answers.spam.noul`), and the body is kept. |
| `ResponseTooLarge { limit }` | A 2xx body was larger than the client's limit (16 MiB unless set) and was not read past it. |

The enum is `#[non_exhaustive]`, so a `match` needs a catch-all arm.

`ApiError` carries `status()`, `kind()` (an `ApiErrorKind` by status: 400 `BadRequest`, 401
`Authentication`, 403 `PermissionDenied`, 404 `NotFound`, 422 `UnprocessableEntity`, 429
`RateLimit`, 500 and above `InternalServer`, anything else `Other`), `headers()`, `endpoint()`,
`message()`, `error_type()`, `body()`, `body_text()`, `body_json::<T>()`, `request_id()` (from
`x-typesafe-request-id`) and `retry_after()` (from `retry-after-ms`, else `Retry-After` as
seconds or an HTTP date, truncated to whole milliseconds), and `is_authentication()`: true
for 401 and for any status whose `error_type()` is `authentication_error`, which is how the
live API answers a request without a key (**403**, not the documented 401).

An error's `Display` is the sentence to show a user; for an API error it reads
`POST https://api.typesafe.ai/v1/systemone: 429 Too many requests (request_id=req_123)`.
`Error::summary()` instead returns one fixed sentence per kind, never server or transport text.

A response body that is **not valid UTF-8** is not JSON (RFC 8259 requires UTF-8), and every body
is checked before the parser reads it. With a 2xx status it is a `ResponseValidation` error: its
`decode_error()` is a `Syntax` error at the first bad byte (line and column, the column counted in
bytes), its `field_path()` is empty, and like any body that does not decode it is not retried for
its status. With any other status the caller still gets the `ApiError`, with its status, headers,
`retry_after()` and every byte of the body in `body()`; its `message()` is the whole body as
lossy text (each bad sequence becomes U+FFFD), escaped and cut at 200 characters like any body
that is not JSON, and `error_type()` is `None`.

```rust,no_run
use typesafe_sdk::{ApiErrorKind, Client, ErrorKind, PreparedQuestions};

async fn ask(client: &Client, questions: &PreparedQuestions) {
    match client.system_one("hello", questions).send().await {
        Ok(response) => println!("{} answers", response.answers().len()),
        Err(error) => match error.kind() {
            ErrorKind::Api(api) if api.kind() == ApiErrorKind::RateLimit => {
                eprintln!("rate limited, retry after {:?}: {error}", api.retry_after());
            }
            ErrorKind::ResponseValidation(invalid) => {
                eprintln!("unexpected response at {}: {error}", invalid.field_path());
            }
            _ => eprintln!("{error}"),
        },
    }
}
```

## Retries

A `RetryPolicy` decides which failed attempts are repeated. The defaults are the Python SDK's:
2 retries; a backoff starting at 500 ms, doubling up to 5 s, with up to a quarter of each delay
randomly taken off; the statuses 408, 429 and 500-599; connection failures and timeouts
retried; `retry-after-ms` / `Retry-After` obeyed, however long they ask for; and a **budget**
of 30 s for the whole call.

```rust,no_run
use std::time::Duration;

use typesafe_sdk::{Client, Error, PreparedQuestions, RetryPolicy, StatusSet};

async fn ask_twice(questions: &PreparedQuestions) -> Result<(), Error> {
    let client = Client::builder()
        .retry(
            RetryPolicy::default()
                .max_retries(3)
                .http_statuses([429, 502, 503, 504].into_iter().collect::<StatusSet>())
                .timeout(Duration::from_secs(10))?,
        )
        .build()?;

    // Retried as the client's policy says.
    client.system_one("hello", questions).send().await?;
    // One attempt only; the client and its other calls keep their policy.
    client.system_one("hello", questions).retry(RetryPolicy::none()).send().await?;
    Ok(())
}
```

- A policy given to a call with `.retry()` replaces the client's for that call only.
  `RetryPolicy::new().max_retries(0)` is the same policy as `RetryPolicy::none()`.
- **Budget rule**: before each retry, if the time the call has already taken plus the next delay
  reaches the budget, retrying stops and the call fails with the last attempt's error, unchanged.
  The budget is checked only before a retry and never cuts an attempt short, so a call lasts at
  most the budget plus one per-attempt deadline; with `RetryPolicy::none()` it lasts at most one
  per-attempt deadline. Dropping a call's future cancels the attempt in flight; the SDK spawns
  no task of its own, so nothing is sent or retried after the drop.
  `RetryPolicy::no_timeout()` removes the budget. The budget is separate from the per-attempt
  deadline set with the client's or the call's `timeout`. Without a budget a server's
  `Retry-After` is obeyed however long it is; keep a budget, or turn `respect_retry_after` off,
  when the server is not trusted.
- `predicate(|error| ...)` adds failures of the caller's choosing; it sees every failure,
  including a response that did not decode or was over the size limit.
- **A 2xx status in the status set retries nothing**: a success response whose body does not
  decode is a `ResponseValidation` error and is never retried for its status, since the same body
  would come back and every retry is a billed call. A `predicate` that accepts the error is the
  way to retry it.
- The body is encoded once; every attempt sends the same bytes. Retries carry
  `X-TypeSafe-Retry-Count`. Dropping the call's future cancels a pending retry.

## Configuration

`Client::from_env()` is `Client::builder().build()`. Every builder setting is optional; the
first three fall back to the environment, then to a default:

| Setting | Environment variable | Default |
| --- | --- | --- |
| `api_key` | `TYPESAFE_API_KEY` | none: building fails |
| `base_url` | `TYPESAFE_BASE_URL` | `https://api.typesafe.ai` |
| `default_model` | `TYPESAFE_DEFAULT_MODEL` | `jev-latest` |
| `timeout` / `no_timeout` (per attempt) | - | 10 s |
| `max_response_bytes` | - | 16 MiB |
| `default_header(name, value)` | - | none |
| `retry` | - | `RetryPolicy::default()` |
| `user_agent_product("my-app/1.2.0")` | - | none: `User-Agent` names the SDK alone |
| `send_runtime_header` | - | `true` |
| `log_endpoint_host(false)` | - | `true` |
| `add_root_certificate(der)`, `http_version`, `connect_timeout` (`hyper` feature) | - | none, see below |

These three are the only environment variables the SDK reads, and only for a setting the caller
left unset: an explicit value always wins. A value from the environment is trimmed (with
Python's `str.strip()` rules), a blank one counts as unset, and one that is not UTF-8 is a
`Config` error naming the variable. An explicit key or default model that is blank is refused
instead of sent. Trailing slashes come off the base URL, and a path prefix is kept
(`https://example.test/prefix///` sends to `https://example.test/prefix/v1/systemone`). The
per-attempt deadline covers transport readiness, connect, send and body read, including the
caller's own pool wait and connect with a custom transport. A multi-megabyte `state` on a slow
link can exceed 10 s and be retried, so raise the deadline for large states.

The API key is sent as `Authorization: Bearer <key>` and never printed: the `Debug` output of
the builder, the client and a request show neither the key nor any header value. Headers are
merged as client defaults < per-call headers < the SDK's own (`Authorization`, `Accept`,
`User-Agent`, `X-TypeSafe-SDK`, `X-TypeSafe-Runtime`, and `Content-Type` on a request with a
body), and a caller's `X-TypeSafe-Retry-Count` and framing or connection headers are dropped
(see [Security notes](#security-notes)). A base URL's path appears in `Debug`
and in error messages, so do not put a credential there.

Every request names the SDK in `User-Agent` and `X-TypeSafe-SDK` (`typesafe-sdk-rust/<version>`)
and its platform in `X-TypeSafe-Runtime` (`rust (<os>; <arch>)`). Two settings, and nothing
else, change that. `user_agent_product("my-app/1.2.0")` puts the application's product in front
of the SDK's in `User-Agent` (`my-app/1.2.0 typesafe-sdk-rust/<version>`); `X-TypeSafe-SDK` still
names the SDK alone. The product must be `name/version`, both parts RFC 9110 tokens, at most 64
bytes; anything else is a `Config` error from `build()`. `send_runtime_header(false)` leaves
`X-TypeSafe-Runtime` out of every request.

## Custom transport

Start a custom-transport client with `ClientBuilder::new()`; `ClientBuilder::default()` gives
the same value. Neither needs the `hyper` feature.

`ClientBuilder::build_with_service(service)` sends every request through any
[`tower_service::Service`](https://docs.rs/tower-service) that takes an
`http::Request<typesafe_sdk::Body>` and answers with an `http::Response` of any
[`http_body::Body`](https://docs.rs/http-body): a proxy, a recorder, a middleware stack, or, as
below, an in-memory answer for a test. The service owns its connections and their timeouts; the
SDK still applies its own per-attempt deadline and response size limit. The settings only the
default transport has (`add_root_certificate`, `http_version`, `connect_timeout`, available
with the `hyper` feature) are a `Config` error with `build_with_service`, never silently ignored.

```rust
use std::{
    convert::Infallible,
    future::{Ready, ready},
    task::{Context, Poll},
};

use http::{Request, Response};
use typesafe_sdk::{Body, ClientBuilder, Noul, Questions};

/// Answers every request with the same JSON body.
#[derive(Clone)]
struct Canned(&'static str);

impl tower_service::Service<Request<Body>> for Canned {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Request<Body>) -> Self::Future {
        ready(Ok(Response::new(Body::from(bytes::Bytes::from_static(self.0.as_bytes())))))
    }
}

#[tokio::main]
async fn main() -> Result<(), typesafe_sdk::Error> {
    let answer = r#"{"model":"jev-latest","usage":{},"answers":{"spam":{"type":"noul","noul":0.98}}}"#;
    let client = ClientBuilder::new().api_key("test-key").build_with_service(Canned(answer))?;
    let questions = Questions::new().noul("spam", Noul::new().instructions("Spam?")).prepare()?;
    let response = client.system_one("Buy now!", &questions).send().await?;
    assert_eq!(response.answers().noul("spam").map(|answer| answer.noul()), Some(0.98));
    Ok(())
}
```

When a custom service fails, its error's text becomes the `Connection` error's message (escaped
and cut at 200 characters): a service that prints a request header into its error puts that
header's value into the message.

## Connections and concurrency

A `Client` is cheap to clone (one reference count) and every clone shares one connection pool;
a clone per task is the intended use. With the `hyper` feature (on by default), the built-in
transport is hyper over rustls, with the
operating system's trust store through `rustls-platform-verifier` (plus any
`add_root_certificate` roots), `TCP_NODELAY`, idle connections kept 90 s and HTTP/2 keep-alive
pings every 30 s.

- For an `https` base URL the default is `HttpVersion::Http2Only`: all requests of a client share
  one multiplexed HTTP/2 connection. While a client has no connection - before its first request,
  or after the server closed an idle connection and the client has seen the close - one request
  opens it and the others started at the same time wait for it, so a burst opens one connection
  (64 concurrent cold calls open exactly 1 connection in the test suite). One case is not
  covered: when a connection ends while requests are in flight, hyper-util replaces it on its
  own, and a burst at that moment can still open a second connection, which is closed at once.
  `HttpVersion::Auto` lets ALPN choose HTTP/1.1 or HTTP/2, for a proxy that speaks HTTP/1.1 only;
  a cold client under `Auto` may open one connection per request started at the same time. An
  `http` base URL uses `Auto` unless told otherwise.
- When that one connect fails - TCP, TLS or the HTTP/2 handshake - every request that waited for
  it fails at once, without a connect of its own. Its error is an `ErrorKind::Connection` whose
  message quotes the failure (`Connection error: the connect this request waited for failed:`,
  then the failure's own messages). A downcast of its sources finds no hyper-util error: that
  error cannot be copied, so only the request that opened carries it. The kinds can differ: a
  request that opened and hit the `connect_timeout` reports `ErrorKind::Timeout`, and the requests
  that waited report `Connection`.
- When the request that opened is dropped before a connection exists - its deadline passes, or its
  task is dropped - the requests waiting for it fail at once as well, with an
  `ErrorKind::Connection` that says so (`the connect this request waited for was given up`), as
  hyper-util fails the requests waiting for a connect it no longer makes. The default retry policy
  retries them after its backoff, and the retry wave opens one connection again when its retries
  start within one attempt's deadline of each other, as the default backoff and the default 10 s
  deadline guarantee; with a deadline shorter than the backoff's spread (about 250 ms) it opens as
  many as hyper-util alone would. A waiting request that took over instead would start a connect
  at an endpoint that has just failed to answer one, once per waiting call.
- `client.warm_up().await` lists the models once and drops the answer. It checks the API key and
  leaves an open connection in the pool, so call it **before a fan-out**: the first requests
  then pay no TCP or TLS handshake, and a bad key fails once instead of once per request.
- `examples/concurrency.rs` shows a bounded fan-out over several states.

## Logging

With the `tracing` feature (on by default) the SDK emits [`tracing`](https://docs.rs/tracing)
events and never installs a subscriber: where the events go is the application's choice. Every
event has the target `typesafe_sdk`, so one filter directive selects them all, for example with
`tracing-subscriber`'s `EnvFilter`:

```text
RUST_LOG=typesafe_sdk=info
```

| Level | What is logged |
| --- | --- |
| `INFO` | One line per attempt: `GET https://api.typesafe.ai/v1/models <- 200 in 12ms (request req_1)` for a response of any status, or `... <- timeout` (a fixed word per failure kind, never the error's text) for an attempt that got none; `POST ... retry 1` before a retry. |
| `WARN` | An answer of a type this version does not model was skipped (question name and type only, each escaped and cut at 128). |
| `DEBUG` | Each request as it leaves and each response as it arrives: method, endpoint, retry count, status, request id, elapsed time, the headers with secrets redacted, and the **body length**. |
| `TRACE` | The request and response **bodies**, with control and format characters escaped, uncut. |

`ClientBuilder::log_endpoint_host(false)` makes events print only the fixed API path,
`/v1/systemone` or `/v1/models`, never the scheme, host or base URL's path prefix; the default
is `true`, which keeps the full URL. Errors (`Display`, `ApiError::endpoint()`,
`ResponseValidationError`) still name the scheme and host, so a caller logging `%error` puts
the host back. Request and response bodies still print at `TRACE` under the target
`typesafe_sdk` whatever this flag; cap that target to keep them out. No credential from
userinfo, query or fragment can reach the default endpoint line, because those URL components
are refused at build.

A header value is printed as `***` when its name is `authorization`, `proxy-authorization`,
`x-api-key`, `api-key`, `cookie` or `set-cookie`, when its name contains `token` or `secret`, or
when the value is flagged sensitive (the SDK flags `Authorization`). A `state` may carry personal
data, so bodies appear only at `TRACE`.

## Security notes

- **The API key** is held as a `secrecy::SecretString` until it becomes the `Authorization`
  header value, which is flagged sensitive; no `Debug` or `Display` of this crate prints it, and
  no error message repeats it. The key is copied once into the `Authorization` header value,
  which is not zeroed and lives as long as the client and every request built from it;
  `HeaderValue` is `Bytes`-backed and shared by reference count, so it cannot be zeroed on drop.
  Leading and trailing whitespace is stripped from it, as the Python
  SDK strips it; a key that is then empty, or holds whitespace, a control or a non-ASCII
  character, is refused when the client is built. When a transport fails with an error that
  prints the request's credentials, their values are replaced by `***` first, as the Python SDK
  does since 0.7.1: the key and every secret or sensitive header value, in the forms the
  documentation of `ClientBuilder::build_with_service` lists. An `http://` base URL sends the
  key unencrypted, so use one only for a local proxy or a test server.
- **Server text is escaped and cut.** Every message read from a response body (whichever member
  it came from, or the body itself when no member holds one) has its control characters and
  text-hiding format characters written as Rust escapes (`\n`, `\u{1b}`, `\u{202e}`) and is cut
  at **200** characters plus U+2026. The request id, the error type, and the question name and
  answer type of the skipped-answer `WARN` are shown escaped and cut at **128**; a field path's
  names at 128 each and the whole path at **320**; a connection error's chain (at most 8 links) at
  200. The raw data stays reachable through `body()`, `body_text()`, `body_json()`,
  `request_id()` and `error_type()`.
- **An API error can echo your `state`.** A 422 may repeat part of the request in its message,
  so the `Display` of an API error can carry up to 200 escaped characters of `state`. A caller
  whose `state` is sensitive should log `error.kind()` or the status rather than the whole error.
  `Debug` of an `ApiError` shows the headers as a count and the body as a length.
- **`{:?}` of an `Error` and chain-walking reporters print the transport's full text.** `source()`
  keeps the transport's error whole on purpose, and `Debug` prints it, as do reporters such as
  `anyhow` and `eyre` that walk the chain; that text can be long (an HTTP/2 GOAWAY carries up to
  16 KiB of debug data) and is not text this SDK wrote. Log the `Display` form when that matters.
- **Framing and connection headers belong to the transport.** `content-length`,
  `transfer-encoding`, `connection`, `keep-alive`, `proxy-connection`, `te`, `trailer` and
  `upgrade` set as a client default or on a call are dropped without an error, on every protocol,
  as the SDK's own headers are: a caller's `content-length` that disagrees with the body would
  fail an HTTP/2 stream and leave an HTTP/1.1 call waiting until its deadline. `host` is sent as
  given, on every protocol; over HTTP/2 the request's `:authority` still comes from the base URL.
  Over HTTP/2 a `host` that differs from the base URL's authority is outside RFC 9113 (section
  8.3.1), and a conforming server may refuse the request as malformed. A caller that needs another
  `Host` routes by the base URL instead, or speaks HTTP/1.1: `HttpVersion::Auto` does on an `http`
  base URL, and on an `https` one only when the server picks HTTP/1.1 through ALPN; the default
  transport cannot insist on HTTP/1.1 over TLS.
- **Responses are bounded.** The default JSON parser is serde_json; the optional `sonic`
  feature selects sonic-rs, which carries `unsafe` internally. With either backend a body is
  read under a 16 MiB cap, and a JSON document nested deeper than 16 levels is refused before
  it reaches the parser.

## Performance notes

The numbers below are measured, not estimated; the method, the machines and every run are in
[`docs/perf/ledger.md`](docs/perf/ledger.md). Allocation counts are dhat block counts on the
second identical call, 64-bit targets.

- **Allocations, both backends.** Encoding a request body is **1** allocation (2 when the body is kept for a
  retry). Decoding the three-answer fixture into `Answers` is **7** blocks (578 bytes), 6 into a
  derived struct. A whole call's own allocations, beyond what the transport allocates, are **12**
  blocks (11 with `max_retries(0)`).
- **Encode scratch.** The body is encoded into a per-thread scratch buffer whose capacity is kept
  between calls. A scratch that grew past **8 MiB** is dropped after its call. With `sonic`,
  the six-fold string reservation makes a string of about 1.33 MiB or more pay a first call's
  allocations on every call. With serde_json, retained capacity is the encoded body rounded up
  by the `Vec`'s growth rather than a six-fold reservation. Both backends use the same decay
  and 8x shrink rule; the measured mixed-size fixture shrinks at small call 6 with sonic-rs
  and call 32 with serde_json.
- **Debug builds and the default transport.** Tokio boxes a future larger than 2,048 bytes when
  it is spawned or blocked on in a debug build (16,384 in release). A System One call over the
  default transport is a 2,360-byte future, so a debug build that spawns calls pays one more
  allocation per call; a release build does not, and neither does a call over a custom transport
  (2,040 bytes).
- **Release profile.** A library cannot set the profile it is built with, so these belong in the
  application's `Cargo.toml`. They are the usual settings for a smaller, faster binary; this
  repository's numbers were taken with Cargo's default profiles, so their effect on this SDK is
  not measured here.

  ```toml
  [profile.release]
  lto = "fat"
  codegen-units = 1
  ```
- **`-C target-cpu` on x86_64.** With the `sonic` feature, [sonic-rs](https://docs.rs/sonic-rs)
  selects its SIMD code at compile time. Without a `target-cpu`, an x86_64 build gets the SSE2 baseline only.
  An application that knows its hardware can build with, for example,
  `RUSTFLAGS="-C target-cpu=x86-64-v3"` (AVX2) or `-C target-cpu=native`; the binary then does not
  run on CPUs without those features. Every number in the ledger was taken **without** such a
  flag, which is what a default build gets.

## Testing

`cargo nextest run` and `cargo test`, without `--workspace` or `-p`, run the default members: the
SDK, `crates/macros` and `crates/test-support`. Their tests run against local servers and need
neither a key nor the network.

`crates/live-tests` holds the tests against the live API. It is a workspace member, so `clippy
--workspace` compiles it, but not a default member. **Its tests make real, billed calls** on the
key's account when both `TYPESAFE_LIVE_TESTS=1` and `TYPESAFE_API_KEY` are set and a command
reaches them: `cargo test --workspace`, `cargo nextest run --workspace`, or anything naming
`-p typesafe-sdk-rust-live-tests`. Without either variable they fail, never skip, before any
request is made, so a key exported for other work does not make `--workspace` bill anyone; it
makes those tests fail instead. Run them only on purpose:

```sh
TYPESAFE_LIVE_TESTS=1 TYPESAFE_API_KEY=... cargo nextest run -p typesafe-sdk-rust-live-tests
```

- **Fuzzing.** `fuzz/` holds libFuzzer targets for the response decoders and the `Retry-After`
  parser, in a workspace of its own that needs the nightly toolchain and `cargo-fuzz`; see
  `fuzz/README.md`.
- **Coverage.** CI holds the crate at 85% line coverage; `docs/uncovered-lines.md` records the
  measured total and why each uncovered line is not reached.
- **The port.** `docs/port-test-matrix.md` maps every test of the Python SDK to the Rust tests
  that cover it, the deviation that explains why none does, or the reason it was left out;
  `python3 .github/scripts/port-test-matrix.py` checks that every Rust test and deviation it
  names exists (CI runs it; `--upstream <checkout>` also checks it against a checkout of the
  Python SDK).

## Deviations from the Python SDK

| Python SDK | This crate | Why |
| --- | --- | --- |
| Synchronous `TypeSafeClient` | No blocking client; async only | Scope: one client, on Tokio. Upstream runs most client tests against both its clients; this crate ports the async half. |
| Timeout per httpx phase; `httpx.Timeout` objects | One total deadline per attempt (default 10 s), an optional `connect_timeout`, and `no_timeout()` | One timer per attempt. |
| `http_client.timeout` takes precedence | A custom transport owns its own timeouts; the SDK deadline still wraps each attempt | The transport is the caller's service, configured by the caller. |
| `http_client=` or `transport=`, mutually exclusive | One builder with two terminal methods: `build()` needs the `hyper` feature (on by default) and gives the default transport, `build_with_service(s)` a custom one; `add_root_certificate`, `http_version` and `connect_timeout` are a `Config` error with a custom one | A client needs a key and a base URL whatever sends the bytes; a setting that cannot apply is refused, never ignored. |
| `.nouls` / `.choices` / `.scores` as cached dict copies | Iterators that filter without copying | Nothing to cache and nothing to leave out of serialization. |
| Covariant `Mapping` question inputs | The `Questions` builder | Generics take any string type and any iterator of options or levels. |
| `close()`, context managers, closing a supplied client | `Drop`; a supplied service is owned by value and dropped with the last clone of the client | Ownership replaces lifecycle calls. |
| Responses and errors are picklable and copyable | Responses are `Clone` and `Serialize`; errors are `Send + Sync + 'static` but not `Clone` | There are no process pools to cross. |
| `request_id` raises when absent | `request_id()` returns `Option<&str>` | Absence is not an error. |
| `raw_http_response` | `meta()`: the status, the headers and `raw_body()` | The same recovery path for data this version does not model, such as a skipped answer. |
| Frozen pydantic models | Private fields with getters | Immutable by construction. |
| Unknown fields rejected on typed questions; `RetryPolicy` field types checked at run time | Not representable: builders, `u32`, `Duration`; the jitter range and its finiteness are still checked | The type system does the check. |
| `str` subclasses and abstract `Mapping` / `Sequence` inputs | `impl Serialize`, `impl AsRef<str>`, `impl Into<Cow<str>>` and iterators | Generics. |
| Non-finite floats are written as `NaN` and `Infinity` | Written as `null`, as `serde_json` writes them; a `state` that is itself a non-finite float is refused as an `InvalidRequest` | `NaN` and `Infinity` are not JSON. |
| `TYPESAFE_LOG_LEVEL` sets the logger level | Not read | A library must not configure the application's subscriber; filter the `typesafe_sdk` target instead. |
| DEBUG logs full bodies | `DEBUG` logs the body length; `TRACE` logs the body | A `state` may carry personal data. |
| `X-TypeSafe-SDK: typesafe-sdk/<version>` | `typesafe-sdk-rust/<version>`, and `X-TypeSafe-Runtime: rust (<os>; <arch>)` | A port must not be counted as the official SDK. |
| `User-Agent` names the SDK alone; `X-TypeSafe-Runtime` is always sent | `user_agent_product("name/version")` puts the application's product in front of the SDK's in `User-Agent`, checked when the client is built; `send_runtime_header(false)` leaves `X-TypeSafe-Runtime` out. The defaults are unchanged, and a caller's header of either name is still dropped | An application built on the SDK must be able to name itself, and must be able not to disclose its operating system and architecture to the vendor. |
| `RetryPolicy.exceptions` | Dropped; `predicate` kept | There are no exception classes; a predicate sees the `Error`. |
| Raw dict questions | The `RawQuestion` builder, with the same three checks | Forward compatibility with question types this version does not model. |
| `response_model=` | `SystemOneResponse<A>` with `.typed::<A>()`, or `#[derive(QuestionSet)]` and `ask::<T>()` | Static typing. |
| No response size limit | A 16 MiB cap, configurable with `max_response_bytes` | Bounds the memory a broken or hostile endpoint can make one call hold. |
| No error kind for an oversized response | `ErrorKind::ResponseTooLarge { limit }` for a 2xx body over the cap; a failure status over the cap stays an `ApiError` with its status, headers and `Retry-After` and an empty body | A retry predicate must be able to see that retrying cannot help. |
| Any nesting depth is parsed | A response nested deeper than 16 levels is a response-validation error; an error body nested deeper than 16 is not parsed and becomes the raw-text message | The JSON parser either has no recursion limit and aborts on deep input (sonic-rs) or stops at 128 levels (serde_json); the depth is checked on the raw bytes first. |
| ALPN chooses between HTTP/2 and HTTP/1.1 | `Http2Only` for `https` base URLs, `Auto` as the option for HTTP/1.1-only proxies; `http` base URLs use `Auto` | A cold 64-way fan-out opened 64 connections under `Auto` and 1 under HTTP/2 only. |
| Server messages are used verbatim and uncut | Every message read from a response body is escaped and cut at 200 characters plus U+2026; the request id is shown escaped and cut at 128; the raw data stays in `body()`, `body_text()`, `body_json()`, `request_id()`, `error_type()` | A server-controlled body of up to 16 MiB, with real newlines, terminal escapes or bidi overrides, must not become a log line. A long validation message is cut in `message()`; the whole text is in `body_text()`. |
| `Retry-After` kept as float milliseconds | A `Duration` truncated to whole milliseconds (`125.7` becomes 125 ms) | The precision the header carries. |
| An explicit empty `default_model` is sent | An explicit default model that is empty or whitespace-only is a `Config` error, even when the environment holds a usable value; a padded non-blank model is kept byte for byte | A critical setting fails when the client is built, not later as a 401 or 403. |
| The base URL is not checked until the first request | Checked when the client is built: absolute `http`/`https`, a non-empty host, no userinfo, query or fragment; no message repeats the URL | Fail fast, and a URL that did not parse cannot be trusted to have had its userinfo found. |
| Caller headers are sent as the caller set them, `Content-Length`, `Transfer-Encoding`, `Connection` and the other framing and connection headers included | `Content-Length`, `Transfer-Encoding`, `Connection`, `Keep-Alive`, `Proxy-Connection`, `TE`, `Trailer` and `Upgrade` are dropped from client defaults and per-call headers on every protocol; `Host` is sent as given | They belong to the transport: HTTP/2 forbids the connection-specific ones, and a `Content-Length` that disagrees with the body fails an HTTP/2 stream and hangs an HTTP/1.1 call until its deadline. |
| Log redaction by header name only | A value flagged sensitive is redacted as well; the name rules are the Python SDK's | A tightening. |
| A typed noul can send `null` outcomes and empty `criteria` | An undescribed outcome and empty `criteria` are left out; `RawQuestion` can still send all three shapes | The same meaning to the API. |
| A body that is not an object fails at path `''` | The root is named `.` | The codec's name for the root. |
| Token counts are `int`; a negative count is accepted | `u64`; a negative count fails at `usage.input_tokens` | A count cannot be negative. |
| A standalone answer model defaults `type` | Deserializing a lone answer requires `type`; inside a response both SDKs require it | Response decoding is at parity. |
| A `SystemOneResponse` can be built without its HTTP response | No public constructor: a response always has its `meta()`; the answer types and `Usage` do have constructors | There is nothing to raise on access. |
| Several errors in one answer: the first in schema order | A type error in a member is reported where it occurs in wire order; missing members are reported in schema order | One pass over the bytes, with no second walk to reorder errors. |
| Score level keys go through `int()` | Level keys are parsed as `u32`: `-1`, `4294967296`, `" 1"`, `"1.0"` and `"1_0"` fail the whole response at `answers.<name>.legend.<key>`; `+1`, `01` and `00` are accepted as upstream accepts them | A level is a small non-negative index, and the API writes plain decimal keys. The body of a response lost this way is still in the error's `body()`. |
| `Usage`, a model card or the model list as a JSON array is rejected | Rejected as well, by hand-written readers | Parity on purpose, listed because serde's derive would accept an array positionally. |
| Duplicate keys: the last one wins | A repeated answer name: the first answer is the one held (`Answers` keeps every entry in wire order and every lookup returns the first; a derived struct keeps the first). Repeated option and level keys keep both entries; lookups find the first. A repeated member inside one answer keeps the last, as Python does | Refusing duplicates would need either a quadratic scan, which a hostile server could exploit, or an allocation per response; the API does not send duplicates. |
| An answer that names `type` twice: the last wins | Two different values are a response-validation error at `answers.<name>.type`; the same value twice is accepted | A response that contradicts itself is malformed. |
| A misshaped member is reported at `answers.<name>.<member>` in any member order | With `type` first, the order the API writes, every path matches. `Answers` holds a member that arrives before `type` as raw text and checks it later, so that failure is reported at `answers.<name>`; a derived field reads its members as they arrive, so a misshaped member ahead of a wrong `type` is reported at the member | One pass, without buffering parsed values. |
| Server-chosen keys appear verbatim in field paths | The key is still shown, but control characters and text-hiding format characters are escaped, each name is cut at 128 characters and the whole path at 320, marked with U+2026 | A server-chosen key must not break a log line, recolour a terminal or grow a message without bound. |
| `Connection error: {error}` carries the transport's whole text | The chain (at most 8 links) is escaped and cut at 200 characters plus U+2026 after the prefix; values of the request's credential headers are replaced by `***` first, as the Python SDK does since 0.7.1; `source()` keeps the whole chain unless a link printed a credential, when it is a redacted copy of up to 32 links | GOAWAY debug data (up to 16 KiB), certificate subjects and a custom transport's text are not text the SDK wrote. |
| Redacted exception copies keep their type, `__notes__`, `__context__` and shared or cyclic causes | When a credential of the request occurs in any link's `Display`, `{:?}` or `{:#?}`, `source()` becomes a copy of up to 32 links of a private type carrying the redacted texts. The copy cannot be downcast, whatever the transport, so a short secret header value (`x-csrf-token: 1`) can turn a clean `Connection refused (os error 61)` chain into a copy. A credential is matched as it is, in the five escaped forms Rust's formatting, `http`, `bytes` and JSON write, and in each of those escaped once more by `{:?}`, which is how a derived `Debug` prints a `String` field holding one. The message after the fixed `Connection error: ` prefix is redacted after escaping as well, so a credential the escaping forms by chance becomes `***` while the original chain is kept. A chain kept as the original is scanned once, at failure time; a link that prints changing interior state can still show a credential later. Rust errors have no notes, no second link and no cycles | A Rust error cannot be rebuilt from its message as `type(error)(message)` does. |
| The API key is any `str` | `api_key(impl Into<String>)`, wrapped into a secret at once | No pre-1.0 type appears in a public signature. |
| The response body type is httpx's | The default transport's body is this crate's `ResponseBody` | No `hyper` type in a public signature, so a hyper upgrade is not a breaking change here. |
| A 2xx status in the retryable set retries a response that did not validate | A 2xx in `StatusSet` retries nothing: `ResponseValidation` is never retried for its status; a `predicate` can ask for it | A schema mismatch is not transient, and every retry is a billed model call. |
| `RetryPolicy` fields can be read back | Setters named like the fields and no getters; `backoff_jitter` and `timeout` are the fallible setters (`Config` errors with the Python SDK's messages); `StatusSet` is a `Copy` bitset with a `const DEFAULT` | Nothing needs a policy read back; getters can be added later without breaking anyone. |
| The state is any value and the model is named at the call | `client.ask::<Q>(&state)` returns a builder whose state type is opaque; `system_one(&state, Q::prepared()).typed::<Q>()` is the nameable form | Naming the state type would force `ask::<Ticket, _>` at every call. |
| Questions are checked only at run time; a repeated dict key keeps the last | The derive refuses duplicate wire names, options and level keys at compile time; everything else mirrors `Questions::prepare()` (empty options and blank names are accepted); `Option<...Answer>` fields are a compile error | A well-formedness rule of a literal written in source, not an API limit; no documented prose limit is compiled in. |
| Any class name can hold questions | A derived struct named `__QuestionSetField`, `__QuestionSetFieldVisitor`, `__QuestionSetVisitor`, `__D`, `__M` or `__private` is refused with one error | The expansion declares those names beside the struct. |

## What is not measured

A connect timeout mapping to `ErrorKind::Timeout`, the HTTP/2 keep-alive ping (30 s), the idle
pool timeout (90 s) and `TCP_NODELAY` are configured but not observed by any test, and Android is
compiled by no CI job. Neither are the derive's compile-error texts under any rustc other than
the pinned 1.98.1, the real distribution of the retry jitter (only an injected draw is tested),
instruction counts on arm64, nor future sizes on targets other than macOS and Linux.

## License

This crate is licensed under the Apache License 2.0; see [`LICENSE`](LICENSE). It is a port of
typesafe-sdk-python, which is MIT-licensed; that license text is reproduced, as the upstream
repository ships it, in [`LICENSE-THIRD-PARTY`](LICENSE-THIRD-PARTY) and applies to the material
derived from it.
