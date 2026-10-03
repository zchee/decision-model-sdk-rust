//! The client, its builder and the requests it sends.
//!
//! Ported from `_client.py` of system-one-adapter-python.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use decision_model_sdk::{
    AnswerContext, AnswerSet, Answers, PreparedQuestions, QuestionSet, RetryPolicy,
};
use serde::Serialize;
use tokio::sync::OnceCell;

use crate::{
    error::Error,
    model::QuestionModel,
    options::{AnswerMode, StructuredOutputs},
    prompt::{self, StateError},
    provider::{Provider, ProviderName, factory},
    response::{Response, Trace},
    run::{self, Evaluation},
};

/// Upstream's sentence for a call that names no model anywhere.
const MODEL_REQUIRED: &str = "An LLM model is required on the client or call.";

/// Upstream's sentence for a model name without a provider to ask it of.
const PROVIDER_REQUIRED: &str = "A provider is required: set provider='openai', 'anthropic', or 'gemini', or pass a provider instance as the model.";

/// Asks a model System One questions about a state.
///
/// A client holds the options of every call it makes and, optionally, the
/// model the calls go to. It is cheap to clone: the clones share one set of
/// options. Building it connects to nothing.
///
/// A call that names a built-in provider and a model is asked of a provider
/// the client builds from the two at the first such call, with the key and
/// the base URL the environment holds then, and keeps for every later call
/// that names the same pair. A build that fails is not kept: the next call
/// tries again. Clones share these providers; they are dropped with the last
/// clone.
///
/// So each distinct provider name and model is built once and kept for the
/// life of the client and its clones, and each provider it keeps has its own
/// connection pool and TLS configuration. Do not pass a model name an end
/// user chose without checking it against a list of the models you accept,
/// because every new name keeps one more provider; or build the provider
/// yourself and pass it with [`ClientBuilder::provider_instance`] or
/// [`Request::provider_instance`].
///
/// `Debug` prints the options, the provider name and the model, for a
/// provider instance only its type name, and the provider name and model of
/// each provider the client built; never a provider's fields.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Settings>,
    owned: Arc<Owned>,
}

/// The providers a client built itself, by provider name and model.
///
/// A cell is created empty on the first call that names its pair and filled
/// by the first build that succeeds; calls that arrive during a build wait
/// for it instead of building their own. A build that fails takes its cell
/// out of the map again, so a pair that never builds holds no entry.
#[derive(Default)]
struct Owned {
    cells: Mutex<Cells>,
}

/// A cell per provider name and model, empty until a build succeeds.
type Cells = HashMap<(ProviderName, String), Arc<Cell>>;

/// The provider of one provider name and model, once built.
type Cell = OnceCell<Arc<dyn Provider>>;

impl Owned {
    /// The map, whether or not a thread panicked while it held the lock: an
    /// entry is inserted in one step, so the map is never left half-written.
    fn cells(&self) -> MutexGuard<'_, Cells> {
        self.cells.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The provider for `provider` and `model`, built now unless an earlier
    /// call built it.
    async fn get(&self, provider: ProviderName, model: &str) -> Result<Arc<dyn Provider>, Error> {
        let cell = Arc::clone(self.cells().entry((provider, model.to_owned())).or_default());
        match cell.get_or_try_init(|| async { factory::build(provider, model) }).await {
            Ok(built) => Ok(Arc::clone(built)),
            Err(error) => {
                self.forget(&(provider, model.to_owned()), &cell);
                Err(error)
            }
        }
    }

    /// Takes `cell`, whose build just failed, out of the map, unless the map
    /// no longer holds it under `key` or a waiter on it has filled it since.
    ///
    /// Without both checks a call that put a new cell in after an earlier
    /// failure, or that filled this one, would lose the provider it built.
    fn forget(&self, key: &(ProviderName, String), cell: &Arc<Cell>) {
        let mut cells = self.cells();
        if cells.get(key).is_some_and(|held| Arc::ptr_eq(held, cell) && !held.initialized()) {
            cells.remove(key);
        }
    }

    /// The provider name and model of every provider built so far, in a
    /// stable order.
    fn keys(&self) -> Vec<(ProviderName, String)> {
        let mut keys: Vec<_> = self
            .cells()
            .iter()
            .filter(|(_, cell)| cell.initialized())
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by(|(left, left_model), (right, right_model)| {
            (left.as_str(), left_model).cmp(&(right.as_str(), right_model))
        });
        keys
    }
}

/// The options of a client, and the model it asks by default.
#[derive(Clone)]
struct Settings {
    structured_outputs: StructuredOutputs,
    llm_answer_mode: AnswerMode,
    normalize_probabilities: bool,
    n_retry_malformed_structure: u32,
    retry: RetryPolicy,
    target: Target,
}

/// Which model a client or a call names: a built-in provider and a model
/// name, or a provider instance.
#[derive(Clone, Default)]
struct Target {
    provider: Option<ProviderName>,
    model: Option<String>,
    instance: Option<Arc<dyn Provider>>,
}

impl Client {
    /// A builder for a client that asks the model through its native
    /// structured-output mode or a prompted JSON reply, for probabilities or
    /// for one value per question.
    ///
    /// Both arguments are required, as upstream's two keyword arguments
    /// without a default are. Everything else starts as upstream's default: no
    /// provider, no model, no probability normalization, no corrective retry,
    /// and [`RetryPolicy::none`].
    #[must_use]
    pub fn builder(
        structured_outputs: StructuredOutputs,
        llm_answer_mode: AnswerMode,
    ) -> ClientBuilder {
        ClientBuilder {
            settings: Settings {
                structured_outputs,
                llm_answer_mode,
                normalize_probabilities: false,
                n_retry_malformed_structure: 0,
                retry: RetryPolicy::none(),
                target: Target::default(),
            },
        }
    }

    /// A request that asks `questions` about `state` and returns the answers
    /// by question name.
    ///
    /// `state` is any value that serializes to JSON other than `null`: text,
    /// a map, a list, a struct. Nothing is sent until
    /// [`send`](Request::send) is awaited.
    pub fn system_one<'a, T: Serialize + ?Sized>(
        &'a self,
        state: &'a T,
        questions: &'a PreparedQuestions,
    ) -> Request<'a, T> {
        Request {
            client: self,
            state,
            questions,
            target: Target::default(),
            retry: None,
            finish: Ok,
        }
    }

    /// A request that asks the questions of `Q` about `state` and decodes the
    /// answers into a `Q`.
    ///
    /// The state's type is not a type parameter of this method, so that
    /// `ask::<Ticket>(&state)` names only the question set; the price is that
    /// the state's type cannot be named in the returned request.
    pub fn ask<'a, Q: QuestionSet>(
        &'a self,
        state: &'a (impl Serialize + ?Sized),
    ) -> Request<'a, impl Serialize + ?Sized, Q> {
        Request {
            client: self,
            state,
            questions: Q::prepared(),
            target: Target::default(),
            retry: None,
            finish: typed::<Q>,
        }
    }

    /// The provider a call goes to (upstream's `_resolve_provider`): the
    /// call's instance, else the call's model name, else the client's
    /// instance, else the client's model name. A model name needs a provider
    /// name, from the call or else from the client; the pair is asked of the
    /// provider the client built for it, built now if this is its first call.
    async fn resolve(&self, call: &Target) -> Result<Arc<dyn Provider>, Error> {
        let client = &self.inner.target;
        if let Some(instance) = &call.instance {
            return Ok(Arc::clone(instance));
        }
        let model = match &call.model {
            Some(model) => model,
            None => {
                if let Some(instance) = &client.instance {
                    return Ok(Arc::clone(instance));
                }
                client.model.as_ref().ok_or_else(|| Error::invalid_request(MODEL_REQUIRED))?
            }
        };
        let provider = call
            .provider
            .or(client.provider)
            .ok_or_else(|| Error::invalid_request(PROVIDER_REQUIRED))?;
        self.owned.get(provider, model).await
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let instance = self.inner.target.instance.as_deref().map(Provider::type_name);
        let owned = OwnedKeys(self.owned.keys());
        self.inner.fmt_as("Client", &instance, Some(&owned), formatter)
    }
}

/// The provider name and model of each provider a client built, as `Debug`
/// prints them: `[openai/gpt-4o-mini, ...]`.
struct OwnedKeys(Vec<(ProviderName, String)>);

impl fmt::Debug for OwnedKeys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_list()
            .entries(self.0.iter().map(|(provider, model)| format!("{provider}/{model}")))
            .finish()
    }
}

impl Settings {
    /// The options, the provider name and the model, `instance` in the
    /// place of the provider instance, and `owned` when there is a cache of
    /// built providers to print.
    fn fmt_as(
        &self,
        name: &str,
        instance: &dyn fmt::Debug,
        owned: Option<&dyn fmt::Debug>,
        formatter: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let mut debug = formatter.debug_struct(name);
        debug
            .field("structured_outputs", &self.structured_outputs)
            .field("llm_answer_mode", &self.llm_answer_mode)
            .field("normalize_probabilities", &self.normalize_probabilities)
            .field("n_retry_malformed_structure", &self.n_retry_malformed_structure)
            .field("retry", &self.retry)
            .field("provider", &self.target.provider)
            .field("model", &self.target.model)
            .field("provider_instance", instance);
        if let Some(owned) = owned {
            debug.field("owned_providers", owned);
        }
        debug.finish()
    }
}

/// Configures a [`Client`]; start it with [`Client::builder`].
///
/// `Debug` prints the options, the provider name and the model, and a
/// provider instance through its own `Debug`.
#[derive(Clone)]
pub struct ClientBuilder {
    settings: Settings,
}

impl ClientBuilder {
    /// The built-in provider a model name is asked of. Default: none.
    #[must_use]
    pub fn provider(mut self, provider: ProviderName) -> Self {
        self.settings.target.provider = Some(provider);
        self
    }

    /// The name of the model to ask, which needs a
    /// [`provider`](Self::provider). Default: none. The client builds and
    /// keeps a provider for each name it is given: see [`Client`] before
    /// passing a name an end user chose.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.settings.target.model = Some(model.into());
        self
    }

    /// A provider the caller built, a built-in one or its own, asked in the
    /// place of a provider name and a model. The client shares it and never
    /// builds or caches anything for it.
    #[must_use]
    pub fn provider_instance(mut self, provider: Arc<dyn Provider>) -> Self {
        self.settings.target.instance = Some(provider);
        self
    }

    /// Whether the probabilities of a choice or a score that miss summing to
    /// 1 by more than the tolerance are rescaled. Default: `false`.
    #[must_use]
    pub fn normalize_probabilities(mut self, normalize: bool) -> Self {
        self.settings.normalize_probabilities = normalize;
        self
    }

    /// The most corrective retries after a reply that does not match the
    /// output schema: each sends the model its reply and what was wrong with
    /// it. Default: 0.
    #[must_use]
    pub fn n_retry_malformed_structure(mut self, retries: u32) -> Self {
        self.settings.n_retry_malformed_structure = retries;
        self
    }

    /// The policy for retrying a failed model request. Default:
    /// [`RetryPolicy::none`], one attempt.
    #[must_use]
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.settings.retry = retry;
        self
    }

    /// Builds the client. It connects to nothing and reads no environment
    /// variable.
    ///
    /// # Errors
    ///
    /// Every combination of the options above builds; a missing model or
    /// provider is reported by the call that needs it, because a call may
    /// name its own.
    pub fn build(self) -> Result<Client, Error> {
        Ok(Client { inner: Arc::new(self.settings), owned: Arc::default() })
    }
}

impl fmt::Debug for ClientBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.settings.fmt_as("ClientBuilder", &self.settings.target.instance, None, formatter)
    }
}

/// One call, not yet sent: the state, the questions and what this call
/// overrides of its client.
///
/// `A` is what the answers come back as: [`Answers`] from
/// [`Client::system_one`], the question set's own type from [`Client::ask`].
///
/// `Debug` prints the overrides, never the state.
#[must_use = "a request does nothing until `send` is awaited"]
pub struct Request<'a, T: ?Sized, A = Answers> {
    client: &'a Client,
    state: &'a T,
    questions: &'a PreparedQuestions,
    target: Target,
    retry: Option<RetryPolicy>,
    /// Turns the answers of the reply into `A`.
    finish: fn(Answers) -> Result<A, Error>,
}

impl<T: ?Sized, A> Request<'_, T, A> {
    /// The built-in provider this call's model name is asked of, in the place
    /// of the client's.
    pub fn provider(mut self, provider: ProviderName) -> Self {
        self.target.provider = Some(provider);
        self
    }

    /// The name of the model this call asks, in the place of the client's
    /// model or provider instance. The client builds and keeps a provider for
    /// each name it is given: see [`Client`] before passing a name an end
    /// user chose.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.target.model = Some(model.into());
        self
    }

    /// A provider the caller built that this call asks, in the place of
    /// anything the client or the call names.
    pub fn provider_instance(mut self, provider: Arc<dyn Provider>) -> Self {
        self.target.instance = Some(provider);
        self
    }

    /// The retry policy of this call, in the place of the client's.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }
}

impl<T: Serialize + ?Sized, A> Request<'_, T, A> {
    /// Asks the model and returns its answers, with the token usage and the
    /// trace of every attempt.
    ///
    /// The steps before the first model request run in a fixed order: the
    /// provider is resolved, the state is written, the questions are
    /// validated. So a missing provider is reported before an invalid
    /// question.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
    /// error when neither the call nor the client names a model, when a model
    /// name has no provider, when the state serializes to `null` or does not
    /// serialize to JSON, or when a question is not one the adapter can ask;
    /// an [`ErrorKind::Config`](crate::ErrorKind::Config) error when a
    /// provider cannot be built; an
    /// [`ErrorKind::Provider`](crate::ErrorKind::Provider) error when the
    /// model request fails and the retry policy gives up; an
    /// [`ErrorKind::NonAnswer`](crate::ErrorKind::NonAnswer) error when the
    /// vendor declares its reply unfinished or refused; and an
    /// [`ErrorKind::MalformedStructure`](crate::ErrorKind::MalformedStructure)
    /// error when the reply still does not match the output schema after the
    /// last corrective retry. The last three carry the trace of the attempts
    /// in [`Error::debug`].
    pub async fn send(self) -> Result<Response<A>, Error>
    where
        A: AnswerSet,
    {
        let settings = &*self.client.inner;
        let provider = self.client.resolve(&self.target).await?;
        let user_message = prompt::user_message(self.state).map_err(|error| match error {
            StateError::Null => {
                Error::invalid_request("The state must not serialize to JSON `null`.")
            }
            StateError::Json(error) => {
                Error::invalid_request("The state does not serialize to JSON.")
                    .with_source(Box::new(error))
            }
        })?;
        let questions = QuestionModel::from_prepared(self.questions)?;
        let started = Instant::now();

        let Response { model, usage, answers, n_answers, debug } = run::evaluate(Evaluation {
            provider: &*provider,
            questions: &questions,
            structured_outputs: settings.structured_outputs,
            answer_mode: settings.llm_answer_mode,
            normalize_probabilities: settings.normalize_probabilities,
            n_retry_malformed_structure: settings.n_retry_malformed_structure,
            retry: self.retry.as_ref().unwrap_or(&settings.retry),
            user_message,
            started,
        })
        .await?;
        match (self.finish)(answers) {
            Ok(answers) => Ok(Response { model, usage, answers, n_answers, debug }),
            Err(error) => Err(error.with_trace(Trace {
                attempts: debug.attempts,
                retry_reasons: debug.retry_reasons,
                probabilities: None,
            })),
        }
    }
}

impl<T: ?Sized, A> fmt::Debug for Request<'_, T, A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("provider", &self.target.provider)
            .field("model", &self.target.model)
            .field("provider_instance", &self.target.instance)
            .field("retry", &self.retry)
            .finish()
    }
}

/// The answers as the question set `A` holds them.
///
/// The SDK decodes a question set from a deserializer and from nothing else,
/// so the answers are written as the JSON object a System One response
/// carries and read back through `A`'s own decoder.
fn typed<A: AnswerSet>(answers: Answers) -> Result<A, Error> {
    let mismatch = |error: serde_json::Error| {
        Error::malformed_structure("The answers do not fit the type of the question set.")
            .with_source(Box::new(error))
    };
    let json = serde_json::to_string(&answers).map_err(mismatch)?;
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    A::deserialize_answers(&mut deserializer, AnswerContext::default()).map_err(mismatch)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
