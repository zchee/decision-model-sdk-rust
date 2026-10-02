//! The lifecycle of the providers a client builds and caches itself.
//!
//! Every built-in provider here reads its key and base URL from the
//! environment the test replaces, and the base URL names a server on the
//! loopback interface. A provider reads the environment once, when it is
//! built, so a test counts the builds of a client by pointing the base URL at
//! a fresh server after a call: a provider that is reused keeps sending to
//! the server it was built with, and a new build sends to the new one.
//!
//! Upstream runs each case for both vendors of its fixture, OpenAI and
//! Anthropic, and for its synchronous client too; the port has the
//! asynchronous half only.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use http::StatusCode;
use serde_json::{Value, json};
use system_one_adapter::{
    __internals::env::{self, Replaced},
    AnswerMode, Answers, AnthropicProvider, BoxFuture, Client, ClientBuilder, ErrorKind, NonAnswer,
    Noul, OpenAiProvider, PreparedQuestions, Provider, ProviderCall, ProviderName, ProviderResult,
    Questions, Response, StructuredOutputs,
};
use test_support::{Protocol, RecordedRequest, TestResponse, TestServer, json_response};
use tokio::sync::Barrier;

/// The model a client names unless a call names another.
const MODEL: &str = "test-model";

/// The reply that answers [`positive`] in discrete mode.
const ANSWER: &str = r#"{"answers":{"positive":true}}"#;

/// The state of upstream's lifecycle cases.
const STATE: &str = "Great book";

/// A built-in provider upstream's lifecycle fixture runs each case for.
#[derive(Clone, Copy)]
struct Vendor {
    name: ProviderName,
    /// The path a request of this vendor's provider arrives on.
    path: &'static str,
    /// The variable its key is read from.
    key_variable: &'static str,
    /// A provider built by the caller, with `key`, sending to `base_url`.
    injected: fn(base_url: &str, key: &str) -> Arc<dyn Provider>,
}

const OPENAI: Vendor = Vendor {
    name: ProviderName::OpenAi,
    path: "/v1/chat/completions",
    key_variable: "OPENAI_API_KEY",
    injected: |base_url, key| {
        Arc::new(
            OpenAiProvider::builder(MODEL)
                .api_key(key)
                .base_url(format!("{base_url}/v1"))
                .build()
                .expect("the injected provider builds"),
        )
    },
};

const ANTHROPIC: Vendor = Vendor {
    name: ProviderName::Anthropic,
    path: "/v1/messages",
    key_variable: "ANTHROPIC_API_KEY",
    injected: |base_url, key| {
        Arc::new(
            AnthropicProvider::builder(MODEL)
                .api_key(key)
                .base_url(base_url)
                .build()
                .expect("the injected provider builds"),
        )
    },
};

impl Vendor {
    /// The other vendor of the fixture.
    fn other(self) -> Self {
        if self.name == ProviderName::OpenAi { ANTHROPIC } else { OPENAI }
    }

    /// The key `request` carried, as this vendor's provider sends it.
    fn key_of(self, request: &RecordedRequest) -> String {
        if self.name == ProviderName::OpenAi {
            let value = request.header_values("authorization").concat();
            value.strip_prefix("Bearer ").expect("a bearer key").to_owned()
        } else {
            request.header_values("x-api-key").concat()
        }
    }
}

/// The one question of upstream's lifecycle cases.
fn positive() -> PreparedQuestions {
    Questions::new()
        .noul("positive", Noul::new().instructions("The review is positive."))
        .prepare()
        .expect("one noul is a valid question set")
}

/// A client of upstream's fixture: native structured output, discrete
/// answers, `vendor` and [`MODEL`].
fn client(vendor: Vendor) -> ClientBuilder {
    Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider(vendor.name)
        .model(MODEL)
}

/// The answer of either vendor, with 11 input and 7 output tokens, by the
/// path the request arrived on.
fn reply(request: &RecordedRequest) -> TestResponse {
    let body = match request.uri.path() {
        "/v1/chat/completions" => json!({
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": ANSWER}}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
        }),
        "/v1/messages" => json!({
            "id": "message-test",
            "type": "message",
            "role": "assistant",
            "model": MODEL,
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": ANSWER}],
            "usage": {"input_tokens": 11, "output_tokens": 7},
        }),
        _ => return json_response(StatusCode::NOT_FOUND, r#"{"error":"unknown path"}"#),
    };
    json_response(StatusCode::OK, body.to_string())
}

/// A server that answers every request at once.
async fn server() -> TestServer {
    TestServer::start(Protocol::Http1, |request| {
        let response = reply(&request);
        async move { response }
    })
    .await
    .expect("the server starts")
}

/// Points the base URL of both vendors at `server`.
fn point(environment: &Replaced, server: &TestServer) {
    environment.set("OPENAI_BASE_URL", format!("{}/v1", server.base_url()));
    environment.set("ANTHROPIC_BASE_URL", server.base_url());
}

/// Sets the key of both vendors.
fn set_keys(environment: &Replaced, key: &str) {
    environment.set(OPENAI.key_variable, key);
    environment.set(ANTHROPIC.key_variable, key);
}

/// The model a recorded request asked.
fn model_of(request: &RecordedRequest) -> String {
    let body: Value = serde_json::from_slice(&request.body).expect("the request body is JSON");
    body["model"].as_str().expect("the request names its model").to_owned()
}

/// The path and the model of every request `server` served, oldest first.
fn served(server: &TestServer) -> Vec<(String, String)> {
    server
        .requests()
        .iter()
        .map(|request| (request.uri.path().to_owned(), model_of(request)))
        .collect()
}

fn asked(vendor: Vendor, model: &str) -> (String, String) {
    (vendor.path.to_owned(), model.to_owned())
}

/// Checks the answer upstream's fixture expects.
fn assert_answered(response: &Response<Answers>) {
    let positive = response.answers().noul("positive").expect("the noul is answered");
    assert_eq!(positive.noul(), 1.0);
}

/// What `Debug` of `client` prints for the providers it built.
fn owned_providers(client: &Client) -> String {
    let shown = format!("{client:?}");
    let (_, owned) = shown.split_once("owned_providers: ").expect("Debug prints the cache");
    owned.trim_end_matches(" }").to_owned()
}

async fn reuses_owned_provider(vendor: Vendor) {
    let environment = env::replace();
    set_keys(&environment, "env-key");
    let first = server().await;
    let later = server().await;
    point(&environment, &first);
    let questions = positive();
    let client = client(vendor).build().expect("the client builds");
    assert_eq!(owned_providers(&client), "[]", "nothing is built before the first call");

    for call in 0..3 {
        let response = client.system_one(STATE, &questions).send().await.expect("it answers");
        assert_answered(&response);
        if call == 0 {
            point(&environment, &later);
        }
    }

    assert_eq!(served(&first), vec![asked(vendor, MODEL); 3], "one provider served every call");
    assert_eq!(later.request_count(), 0, "no provider was built after the first call");
    assert_eq!(owned_providers(&client), format!(r#"["{}/{MODEL}"]"#, vendor.name));
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_reuses_owned_provider_and_closes_sdk_on_context_exit
async fn lifecycle_reuses_owned_provider_openai() {
    reuses_owned_provider(OPENAI).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_reuses_owned_provider_and_closes_sdk_on_context_exit
async fn lifecycle_reuses_owned_provider_anthropic() {
    reuses_owned_provider(ANTHROPIC).await;
}

async fn cache_key_is_resolved_pair_per_client(vendor: Vendor) {
    let environment = env::replace();
    set_keys(&environment, "env-key");
    let servers = [server().await, server().await, server().await, server().await];
    let questions = positive();
    let client = client(vendor).build().expect("the client builds");

    point(&environment, &servers[0]);
    client.system_one(STATE, &questions).send().await.expect("the client's pair answers");
    point(&environment, &servers[1]);
    client
        .system_one(STATE, &questions)
        .model(MODEL)
        .provider(vendor.name)
        .send()
        .await
        .expect("the same pair, named on the call, answers");
    client
        .system_one(STATE, &questions)
        .model("another-model")
        .send()
        .await
        .expect("another model answers");
    point(&environment, &servers[2]);
    // The call's provider name with the client's model: the call's name
    // wins over the client's.
    client
        .system_one(STATE, &questions)
        .provider(vendor.other().name)
        .send()
        .await
        .expect("the other vendor answers");

    assert_eq!(served(&servers[0]), vec![asked(vendor, MODEL); 2]);
    assert_eq!(served(&servers[1]), vec![asked(vendor, "another-model")]);
    assert_eq!(served(&servers[2]), vec![asked(vendor.other(), MODEL)]);
    let mut keys = [
        format!("{}/{MODEL}", vendor.name),
        format!("{}/another-model", vendor.name),
        format!("{}/{MODEL}", vendor.other().name),
    ];
    keys.sort();
    assert_eq!(owned_providers(&client), format!("{keys:?}"));

    point(&environment, &servers[3]);
    let second = self::client(vendor).build().expect("the second client builds");
    second.system_one(STATE, &questions).send().await.expect("the second client answers");
    client.system_one(STATE, &questions).send().await.expect("the first client still answers");

    assert_eq!(served(&servers[3]), vec![asked(vendor, MODEL)], "each client builds its own");
    assert_eq!(servers[0].request_count(), 3, "the first client kept its provider");
    assert_eq!(owned_providers(&second), format!(r#"["{}/{MODEL}"]"#, vendor.name));
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_cache_uses_resolved_provider_and_model_and_is_per_client
async fn lifecycle_cache_key_is_resolved_pair_per_client_openai() {
    cache_key_is_resolved_pair_per_client(OPENAI).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_cache_uses_resolved_provider_and_model_and_is_per_client
async fn lifecycle_cache_key_is_resolved_pair_per_client_anthropic() {
    cache_key_is_resolved_pair_per_client(ANTHROPIC).await;
}

/// Where a caller hands over its own provider: as the client's default, or
/// on each call.
#[derive(Clone, Copy, PartialEq)]
enum Injected {
    OnTheClient,
    PerCall,
}

async fn injected_provider_is_borrowed(vendor: Vendor, injected_where: Injected) {
    let environment = env::replace();
    set_keys(&environment, "env-key");
    let own = server().await;
    let owned = server().await;
    point(&environment, &owned);
    let injected = (vendor.injected)(own.base_url(), "injected-key");
    let questions = positive();
    let client = match injected_where {
        Injected::OnTheClient => Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
            .provider(vendor.name)
            .provider_instance(Arc::clone(&injected)),
        Injected::PerCall => client(vendor),
    }
    .build()
    .expect("the client builds");
    let ask_injected = || {
        let request = client.system_one(STATE, &questions);
        match injected_where {
            Injected::OnTheClient => request,
            Injected::PerCall => request.provider_instance(Arc::clone(&injected)),
        }
    };

    assert_answered(&ask_injected().send().await.expect("the injected provider answers"));
    client
        .system_one(STATE, &questions)
        .model("owned-model")
        .send()
        .await
        .expect("an owned provider answers");
    assert_answered(&ask_injected().send().await.expect("the injected provider answers again"));

    assert_eq!(served(&own), vec![asked(vendor, MODEL); 2]);
    assert!(own.requests().iter().all(|request| vendor.key_of(request) == "injected-key"));
    assert_eq!(served(&owned), vec![asked(vendor, "owned-model")]);
    assert_eq!(owned_providers(&client), format!(r#"["{}/owned-model"]"#, vendor.name));
    let held_by_the_client = usize::from(injected_where == Injected::OnTheClient);
    assert_eq!(Arc::strong_count(&injected), 1 + held_by_the_client, "never cached");

    drop(client);
    assert_eq!(Arc::strong_count(&injected), 1, "the caller holds the last reference");
    let after = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider_instance(Arc::clone(&injected))
        .build()
        .expect("the client builds");
    after
        .system_one(STATE, &questions)
        .send()
        .await
        .expect("the injected provider outlives the client");
    assert_eq!(own.request_count(), 3);
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_injected_provider_is_borrowed
async fn lifecycle_injected_on_the_client_is_borrowed_openai() {
    injected_provider_is_borrowed(OPENAI, Injected::OnTheClient).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_injected_provider_is_borrowed
async fn lifecycle_injected_per_call_is_borrowed_openai() {
    injected_provider_is_borrowed(OPENAI, Injected::PerCall).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_injected_provider_is_borrowed
async fn lifecycle_injected_on_the_client_is_borrowed_anthropic() {
    injected_provider_is_borrowed(ANTHROPIC, Injected::OnTheClient).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_injected_provider_is_borrowed
async fn lifecycle_injected_per_call_is_borrowed_anthropic() {
    injected_provider_is_borrowed(ANTHROPIC, Injected::PerCall).await;
}

/// A provider of the caller's own, with nothing to close, that counts its
/// calls.
#[derive(Debug, Default)]
struct Counting {
    calls: AtomicUsize,
}

impl Provider for Counting {
    fn model_name(&self) -> &str {
        "counting-model"
    }

    fn request<'a>(
        &'a self,
        _: ProviderCall<'a>,
    ) -> BoxFuture<
        'a,
        Result<Result<ProviderResult, NonAnswer>, system_one_adapter::typesafe_sdk::Error>,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(Ok(ProviderResult::new(ANSWER.to_owned(), Some(11), Some(7)))) })
    }
}

async fn custom_provider_is_supported(vendor: Vendor) {
    let environment = env::replace();
    set_keys(&environment, "env-key");
    let unused = server().await;
    point(&environment, &unused);
    let custom = Arc::new(Counting::default());
    let client = Client::builder(StructuredOutputs::Native, AnswerMode::Discrete)
        .provider(vendor.name)
        .provider_instance(Arc::clone(&custom) as Arc<dyn Provider>)
        .build()
        .expect("the client builds");

    let response =
        client.system_one(STATE, &positive()).send().await.expect("the custom provider answers");

    assert_answered(&response);
    assert_eq!(response.model(), "counting-model");
    assert_eq!(custom.calls.load(Ordering::SeqCst), 1);
    assert_eq!(unused.request_count(), 0);
    assert_eq!(owned_providers(&client), "[]", "no provider was built");
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_custom_provider_without_close_remains_supported
async fn lifecycle_custom_provider_is_supported_openai() {
    custom_provider_is_supported(OPENAI).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_custom_provider_without_close_remains_supported
async fn lifecycle_custom_provider_is_supported_anthropic() {
    custom_provider_is_supported(ANTHROPIC).await;
}

async fn failed_build_is_not_cached(vendor: Vendor) {
    let environment = env::replace();
    let first = server().await;
    let later = server().await;
    point(&environment, &first);
    let questions = positive();
    let client = client(vendor).build().expect("the client builds");

    let error = client
        .system_one(STATE, &questions)
        .send()
        .await
        .expect_err("no key is set, so the provider cannot be built");
    assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
    assert!(error.to_string().contains(vendor.key_variable), "{error}");
    assert!(error.debug().is_none(), "no attempt was made");
    assert_eq!(owned_providers(&client), "[]", "the failed build is not kept");

    set_keys(&environment, "env-key");
    client.system_one(STATE, &questions).send().await.expect("the second build succeeds");
    point(&environment, &later);
    client.system_one(STATE, &questions).send().await.expect("the built provider answers");

    assert_eq!(served(&first), vec![asked(vendor, MODEL); 2], "built once, then kept");
    assert_eq!(later.request_count(), 0);
    assert_eq!(owned_providers(&client), format!(r#"["{}/{MODEL}"]"#, vendor.name));
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_failed_construction_is_not_cached
async fn lifecycle_failed_build_is_not_cached_openai() {
    failed_build_is_not_cached(OPENAI).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_failed_construction_is_not_cached
async fn lifecycle_failed_build_is_not_cached_anthropic() {
    failed_build_is_not_cached(ANTHROPIC).await;
}

async fn environment_is_read_on_first_use(vendor: Vendor) {
    let environment = env::replace();
    let first = server().await;
    let second = server().await;
    let questions = positive();
    // Built before the environment holds anything: the client reads none.
    let client = client(vendor).build().expect("the client builds");
    environment.set(vendor.key_variable, "first-test-key");
    point(&environment, &first);

    client.system_one(STATE, &questions).send().await.expect("the first call answers");
    environment.set(vendor.key_variable, "second-test-key");
    point(&environment, &second);
    client.system_one(STATE, &questions).send().await.expect("the second call answers");

    assert_eq!(first.request_count(), 2);
    assert!(first.requests().iter().all(|request| vendor.key_of(request) == "first-test-key"));
    assert_eq!(second.request_count(), 0, "the environment is not read again");

    let fresh = self::client(vendor).build().expect("a fresh client builds");
    fresh.system_one(STATE, &questions).send().await.expect("the fresh client answers");

    let requests = second.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(vendor.key_of(&requests[0]), "second-test-key");
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_environment_is_captured_on_first_use
async fn lifecycle_environment_is_read_on_first_use_openai() {
    environment_is_read_on_first_use(OPENAI).await;
}

#[tokio::test]
// Upstream: tests/test_provider_lifecycle.py::test_environment_is_captured_on_first_use
async fn lifecycle_environment_is_read_on_first_use_anthropic() {
    environment_is_read_on_first_use(ANTHROPIC).await;
}

/// How many calls the concurrent case starts at once.
const CONCURRENT: usize = 8;

/// A server that holds each request until [`CONCURRENT`] of them are in
/// flight, so that each needs a connection of its own. It gives up after ten
/// seconds with a 503, which fails the call instead of hanging the test.
async fn holding_server() -> TestServer {
    let barrier = Arc::new(Barrier::new(CONCURRENT));
    TestServer::start(Protocol::Http1, move |request| {
        let barrier = Arc::clone(&barrier);
        async move {
            match tokio::time::timeout(Duration::from_secs(10), barrier.wait()).await {
                Ok(_) => reply(&request),
                Err(_) => json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    r#"{"error":"fewer calls arrived than were started"}"#,
                ),
            }
        }
    })
    .await
    .expect("the server starts")
}

/// [`CONCURRENT`] calls of `client` at once, each on its own task, each about
/// its own document; their responses, in document order.
async fn at_once(client: &Client, questions: &Arc<PreparedQuestions>) -> Vec<Response<Answers>> {
    let tasks: Vec<_> = (0..CONCURRENT)
        .map(|index| {
            let client = client.clone();
            let questions = Arc::clone(questions);
            tokio::spawn(async move {
                let state = format!("document-{index}");
                client.system_one(&state, &questions).send().await
            })
        })
        .collect();
    let mut responses = Vec::with_capacity(CONCURRENT);
    for task in tasks {
        responses.push(task.await.expect("the task finishes").expect("the call answers"));
    }
    responses
}

async fn concurrent_first_use_builds_one(vendor: Vendor) {
    let environment = env::replace();
    set_keys(&environment, "env-key");
    let held = holding_server().await;
    let later = server().await;
    point(&environment, &held);
    let questions = Arc::new(positive());
    let client = client(vendor).build().expect("the client builds");

    let responses = at_once(&client, &questions).await;

    for (index, response) in responses.iter().enumerate() {
        assert_answered(response);
        let attempts = response.debug().attempts();
        assert_eq!(attempts.len(), 1);
        let document = format!("document-{index}");
        assert!(attempts[0].messages()[1].content().contains(&document), "{index}");
        assert!(attempts[0].request().expect("recorded").contains(&document), "{index}");
        assert_eq!(response.usage().input_tokens_total(), Some(11));
        assert_eq!(response.usage().n_retries(), 0);
    }
    assert_eq!(owned_providers(&client), format!(r#"["{}/{MODEL}"]"#, vendor.name));
    assert_eq!(held.accepted_connections(), CONCURRENT as u64, "one connection per held call");

    // One provider holds every connection of the first calls in its pool, so
    // as many calls again need no new one. Had each first call built its own
    // provider, at most one of their pools would be left to reuse.
    point(&environment, &later);
    tokio::time::sleep(Duration::from_millis(200)).await;
    at_once(&client, &questions).await;

    assert_eq!(held.request_count(), 2 * CONCURRENT);
    assert_eq!(held.accepted_connections(), CONCURRENT as u64, "the pool was reused");
    assert_eq!(later.request_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
// Upstream: tests/test_provider_lifecycle.py::test_concurrent_first_use_reuses_pool_and_isolates_traces
async fn lifecycle_concurrent_first_use_builds_one_openai() {
    concurrent_first_use_builds_one(OPENAI).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
// Upstream: tests/test_provider_lifecycle.py::test_concurrent_first_use_reuses_pool_and_isolates_traces
async fn lifecycle_concurrent_first_use_builds_one_anthropic() {
    concurrent_first_use_builds_one(ANTHROPIC).await;
}
