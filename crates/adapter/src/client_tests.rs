//! Unit tests of `client.rs`: the defaults, the resolution order, what
//! `Debug` prints and the typed decoding. What a call does over a provider is
//! tested in `tests/client.rs`.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use typesafe_sdk::{Answers, RetryPolicy};

use super::{Client, MODEL_REQUIRED, PROVIDER_REQUIRED, Target, typed};
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
use super::{Owned, ProviderName};
use crate::{
    convert::convert,
    error::ErrorKind,
    model::{DecodedAnswer, DecodedAnswers, QuestionModel},
    options::{AnswerMode, StructuredOutputs},
    provider::{BoxFuture, NonAnswer, Provider, ProviderCall, ProviderResult},
};

/// A provider that always gives the same reply; its derived `Debug` prints
/// its one field.
#[derive(Debug)]
struct Fixed {
    model: &'static str,
}

impl Fixed {
    fn named(model: &'static str) -> Arc<dyn Provider> {
        Arc::new(Self { model })
    }
}

impl Provider for Fixed {
    fn model_name(&self) -> &str {
        self.model
    }

    fn request<'a>(
        &'a self,
        _: ProviderCall<'a>,
    ) -> BoxFuture<'a, Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>> {
        Box::pin(async {
            Ok(Ok(ProviderResult::new(r#"{"answers":{"answer":0.75}}"#.to_owned(), None, None)))
        })
    }
}

fn builder() -> super::ClientBuilder {
    Client::builder(StructuredOutputs::Native, AnswerMode::Probabilities)
}

fn target(model: Option<&str>, instance: Option<Arc<dyn Provider>>) -> Target {
    Target { provider: None, model: model.map(str::to_owned), instance }
}

#[test]
fn a_builder_leaves_everything_but_its_two_arguments_at_the_defaults() {
    let builder = Client::builder(StructuredOutputs::Prompted, AnswerMode::Discrete);

    let settings = &builder.settings;
    assert_eq!(settings.structured_outputs, StructuredOutputs::Prompted);
    assert_eq!(settings.llm_answer_mode, AnswerMode::Discrete);
    assert!(settings.target.provider.is_none());
    assert!(settings.target.model.is_none());
    assert!(settings.target.instance.is_none());
    assert!(!settings.normalize_probabilities);
    assert_eq!(settings.n_retry_malformed_structure, 0);
    // `RetryPolicy` does not compare; its `Debug` prints every setting.
    assert_eq!(format!("{:?}", settings.retry), format!("{:?}", RetryPolicy::none()));
    assert_ne!(format!("{:?}", settings.retry), format!("{:?}", RetryPolicy::default()));
}

#[test]
fn the_setters_reach_the_client() {
    let client = builder()
        .model("a-model")
        .provider_instance(Fixed::named("an-instance"))
        .normalize_probabilities(true)
        .n_retry_malformed_structure(3)
        .retry(RetryPolicy::default())
        .build()
        .expect("every combination builds");

    let settings = &*client.inner;
    assert_eq!(settings.target.model.as_deref(), Some("a-model"));
    assert_eq!(
        settings.target.instance.as_ref().map(|instance| instance.model_name()),
        Some("an-instance")
    );
    assert!(settings.normalize_probabilities);
    assert_eq!(settings.n_retry_malformed_structure, 3);
    assert_eq!(format!("{:?}", settings.retry), format!("{:?}", RetryPolicy::default()));
}

#[tokio::test]
async fn the_instance_of_the_call_is_asked_before_anything_else() {
    let client = builder()
        .model("client-model")
        .provider_instance(Fixed::named("client-instance"))
        .build()
        .expect("builds");

    let resolved = client
        .resolve(&target(Some("call-model"), Some(Fixed::named("call-instance"))))
        .await
        .expect("an instance needs nothing else");

    assert_eq!(resolved.model_name(), "call-instance");
}

#[tokio::test]
async fn the_instance_of_the_client_is_asked_when_the_call_names_nothing() {
    let client = builder()
        .model("client-model")
        .provider_instance(Fixed::named("client-instance"))
        .build()
        .expect("builds");

    let resolved = client.resolve(&Target::default()).await.expect("the client holds an instance");

    assert_eq!(resolved.model_name(), "client-instance");
}

#[tokio::test]
async fn a_model_name_on_the_call_is_resolved_before_the_instance_of_the_client() {
    let client =
        builder().provider_instance(Fixed::named("client-instance")).build().expect("builds");

    let error = client
        .resolve(&target(Some("call-model"), None))
        .await
        .expect_err("a model name without a provider cannot be asked");

    assert!(matches!(error.kind(), ErrorKind::InvalidRequest));
    assert_eq!(error.to_string(), PROVIDER_REQUIRED);
}

#[tokio::test]
async fn a_call_without_any_model_is_refused_with_upstreams_sentence() {
    let client = builder().build().expect("builds");

    let error = client.resolve(&Target::default()).await.expect_err("no model anywhere");

    assert!(matches!(error.kind(), ErrorKind::InvalidRequest));
    assert_eq!(error.to_string(), MODEL_REQUIRED);
    assert_eq!(error.to_string(), "An LLM model is required on the client or call.");
    assert!(error.debug().is_none(), "no attempt was made");
}

#[tokio::test]
async fn a_model_name_of_the_client_without_a_provider_is_refused_with_upstreams_sentence() {
    let client = builder().model("gpt-4o-mini").build().expect("builds");

    let error = client.resolve(&Target::default()).await.expect_err("no provider anywhere");

    assert!(matches!(error.kind(), ErrorKind::InvalidRequest));
    assert_eq!(
        error.to_string(),
        "A provider is required: set provider='openai', 'anthropic', or 'gemini', or pass a \
         provider instance as the model."
    );
}

/// A provider name alone on the call changes nothing while an instance is
/// there to ask.
#[cfg(feature = "openai")]
#[tokio::test]
async fn a_provider_name_alone_does_not_displace_an_instance() {
    use crate::provider::ProviderName;

    let client =
        builder().provider_instance(Fixed::named("client-instance")).build().expect("builds");
    let call = Target { provider: Some(ProviderName::OpenAi), model: None, instance: None };

    let resolved = client.resolve(&call).await.expect("the instance is asked");

    assert_eq!(resolved.model_name(), "client-instance");
}

#[test]
fn debug_of_a_client_names_the_type_of_its_instance_and_none_of_its_fields() {
    let builder = builder().model("a-model").provider_instance(Fixed::named("an-instance"));
    let client = builder.clone().build().expect("builds");

    let of_the_client = format!("{client:?}");
    let of_the_builder = format!("{builder:?}");

    assert!(of_the_client.starts_with("Client {"), "{of_the_client}");
    assert!(of_the_client.contains("structured_outputs: Native"), "{of_the_client}");
    assert!(of_the_client.contains(r#"model: Some("a-model")"#), "{of_the_client}");
    assert!(of_the_client.contains(std::any::type_name::<Fixed>()), "{of_the_client}");
    assert!(!of_the_client.contains("an-instance"), "{of_the_client}");
    assert!(of_the_client.ends_with("owned_providers: [] }"), "{of_the_client}");
    // The builder prints the instance through the instance's own `Debug`.
    assert!(of_the_builder.starts_with("ClientBuilder {"), "{of_the_builder}");
    assert!(of_the_builder.contains(r#"Fixed { model: "an-instance" }"#), "{of_the_builder}");
}

#[test]
fn debug_of_a_request_prints_the_overrides_and_never_the_state() {
    let client = builder().build().expect("builds");
    let questions = typesafe_sdk::Questions::new()
        .noul("answer", typesafe_sdk::Noul::new().instructions("question-text"))
        .prepare()
        .expect("one noul prepares");

    let request = client
        .system_one("the caller's document", &questions)
        .model("call-model")
        .retry(RetryPolicy::default());
    let shown = format!("{request:?}");

    assert!(shown.starts_with("Request {"), "{shown}");
    assert!(shown.contains(r#"model: Some("call-model")"#), "{shown}");
    assert!(shown.contains("retry: Some("), "{shown}");
    assert!(!shown.contains("document"), "{shown}");
    assert!(!shown.contains("question-text"), "{shown}");
}

/// A state that takes `delay` to serialize, as a large or computed one can.
struct SlowState {
    delay: Duration,
}

impl serde::Serialize for SlowState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        std::thread::sleep(self.delay);
        serializer.serialize_str("A slow document.")
    }
}

/// The latency starts after the steps before the first model request:
/// resolving the provider (building it, for a provider the client owns),
/// writing the state and validating the questions take no part of it.
#[tokio::test]
async fn the_latency_starts_after_the_state_is_written() {
    let delay = Duration::from_millis(500);
    let client = builder().provider_instance(Fixed::named("an-instance")).build().expect("builds");
    let questions = typesafe_sdk::Questions::new()
        .noul("answer", typesafe_sdk::Noul::new())
        .prepare()
        .expect("one noul prepares");

    let started = Instant::now();
    let response =
        client.system_one(&SlowState { delay }, &questions).send().await.expect("it answers");

    assert!(started.elapsed() >= delay, "the state was written once");
    assert!(response.usage().latency() < delay, "{:?}", response.usage().latency());
}

/// A build that fails leaves no cell behind: `keys` cannot show one, since it
/// lists filled cells only, so the map itself is counted.
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[tokio::test]
async fn a_failed_build_leaves_no_cell_in_the_map() {
    // With the `internals` feature another test's variables could be in
    // force in this process; an empty replacement of this test's own rules
    // that out. Without the feature a unit-test build finds no variable, so
    // no provider finds its key.
    #[cfg(feature = "internals")]
    let _environment = crate::__internals::env::replace();
    let owned = Owned::default();

    for model in ["model-one", "model-two", "model-one"] {
        let error = owned.get(ProviderName::all()[0], model).await.expect_err("no key");
        assert!(matches!(error.kind(), ErrorKind::Config), "{error:?}");
        assert_eq!(owned.cells().len(), 0, "after {model}");
    }
}

/// The failed build's cell is taken out only while the map still holds that
/// very cell, still empty: a cell another call put in after the failure, or
/// one a waiter has filled since, stays with its provider.
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[test]
fn a_failed_build_takes_out_only_its_own_empty_cell() {
    let key = (ProviderName::all()[0], "a-model".to_owned());
    let failed = Arc::new(super::Cell::new());
    let owned = Owned::default();

    // Another call's cell under the same key, whether its build is still
    // running or done.
    let running = Arc::new(super::Cell::new());
    let built = Arc::new(super::Cell::new_with(Some(Fixed::named("built"))));
    for other in [running, built] {
        owned.cells().insert(key.clone(), Arc::clone(&other));
        owned.forget(&key, &failed);
        let held = owned.cells().get(&key).map(Arc::clone).expect("the other cell stays");
        assert!(Arc::ptr_eq(&held, &other));
    }

    // The failed cell itself, filled by a waiter after the failure.
    let filled = Arc::new(super::Cell::new_with(Some(Fixed::named("filled"))));
    owned.cells().insert(key.clone(), Arc::clone(&filled));
    owned.forget(&key, &filled);
    assert_eq!(owned.keys(), std::slice::from_ref(&key), "a filled cell stays");

    // The failed cell itself, still empty: the one case that is removed.
    owned.cells().insert(key.clone(), Arc::clone(&failed));
    owned.forget(&key, &failed);
    assert_eq!(owned.cells().len(), 0);
}

/// The future of `send` is `Send` for a state that is `Sync`, so a caller can
/// spawn the call.
#[test]
fn the_future_of_send_is_send() {
    fn assert_send<T: Send>(_: &T) {}
    let client = builder().provider_instance(Fixed::named("an-instance")).build().expect("builds");
    let questions = typesafe_sdk::Questions::new()
        .noul("answer", typesafe_sdk::Noul::new())
        .prepare()
        .expect("one noul prepares");

    let untyped = client.system_one("state", &questions).send();

    assert_send(&untyped);
}

/// The typed decoding keeps a criterion that is a JSON object as that JSON,
/// under either JSON backend of the SDK.
#[test]
fn typed_decoding_keeps_text_and_json_criteria_of_a_score() {
    let questions = QuestionModel::from_json(
        r#"{"stars":{"type":"score","criteria":["Bad.",{"label":"Good.","days":[1,7]}]},
            "genre":{"type":"choice","criteria":{"fiction":"A story.","nonfiction":null}},
            "positive":{"type":"noul"}}"#,
    )
    .expect("the questions are valid");
    let decoded = DecodedAnswers {
        answers: vec![
            DecodedAnswer::Distribution(vec![0.25, 0.75]),
            DecodedAnswer::Distribution(vec![0.9, 0.1]),
            DecodedAnswer::Probability(0.8),
        ],
    };
    let answers = convert(&questions, decoded, false).expect("the values fit").answers;

    let read_back: Answers = typed(answers.clone()).expect("answers decode as answers");

    let legend = |answers: &Answers| -> Vec<(u32, Option<String>, Option<String>)> {
        answers
            .score("stars")
            .expect("a score")
            .legend()
            .map(|(level, description)| {
                (
                    level,
                    description.as_text().map(str::to_owned),
                    description.as_json().map(|raw| raw.as_str().to_owned()),
                )
            })
            .collect()
    };
    assert_eq!(
        legend(&read_back),
        [
            (0, Some("Bad.".to_owned()), None),
            (1, None, Some(r#"{"label":"Good.","days":[1,7]}"#.to_owned())),
        ]
    );
    assert_eq!(legend(&read_back), legend(&answers));
    assert_eq!(
        serde_json::to_string(&read_back).expect("serializes"),
        serde_json::to_string(&answers).expect("serializes")
    );
    assert_eq!(read_back.names().collect::<Vec<_>>(), ["stars", "genre", "positive"]);
    assert_eq!(read_back.score("stars").expect("a score").score(), 0.75);
    assert_eq!(read_back.choice("genre").expect("a choice").choice(), "fiction");
    assert_eq!(read_back.noul("positive").expect("a noul").noul(), 0.8);
}
