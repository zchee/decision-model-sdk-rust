//! The scripted provider the client tests share: it answers from a list of
//! steps and keeps every call it received, without any network.
//!
//! Upstream's `_ScriptedProvider` of `tests/test_client_with_fake_model.py`.

use std::{fmt, sync::Mutex};

use system_one_adapter::{
    AttemptTrace, BoxFuture, Message, NonAnswer, Provider, ProviderCall, ProviderResult,
    typesafe_sdk,
};

/// What one call of a provider ends with.
pub(crate) type Exchange = Result<Result<ProviderResult, NonAnswer>, typesafe_sdk::Error>;

/// One step of a script: called once per provider call it serves, with the
/// record of that attempt, so a step can record a request and a response as a
/// built-in provider does. A closure rather than a value, because the SDK's
/// error does not clone and the last step is served again and again.
pub(crate) type Step = Box<dyn Fn(&mut AttemptTrace) -> Exchange + Send + Sync>;

/// A step that replies with `text` and upstream's default token counts, 11 in
/// and 7 out.
pub(crate) fn reply(text: impl Into<String>) -> Step {
    let text = text.into();
    Box::new(move |_| Ok(Ok(ProviderResult::new(text.clone(), Some(11), Some(7)))))
}

/// What the provider was called with.
#[derive(Clone)]
pub(crate) struct Call {
    pub(crate) messages: Vec<Message>,
    pub(crate) structured: bool,
}

/// A provider that serves its steps in order; the last step repeats once the
/// script is exhausted, so a corrective retry always has a reply.
pub(crate) struct Scripted {
    /// What [`Provider::log_uri`] returns; `None` unless a test sets it.
    pub(crate) log_uri: Option<http::Uri>,
    steps: Vec<Step>,
    calls: Mutex<Vec<Call>>,
}

impl Scripted {
    /// A provider that serves `steps`, which must not be empty.
    pub(crate) fn new(steps: Vec<Step>) -> Self {
        assert!(!steps.is_empty(), "a script has at least one step");
        Self { log_uri: None, steps, calls: Mutex::new(Vec::new()) }
    }

    /// Every call received so far, in order.
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("not poisoned").clone()
    }
}

impl fmt::Debug for Scripted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Scripted").field("steps", &self.steps.len()).finish()
    }
}

impl Provider for Scripted {
    fn model_name(&self) -> &str {
        "fake-model"
    }

    fn request<'a>(&'a self, mut call: ProviderCall<'a>) -> BoxFuture<'a, Exchange> {
        let served = {
            let mut calls = self.calls.lock().expect("not poisoned");
            calls.push(Call { messages: call.messages().to_vec(), structured: call.structured() });
            calls.len() - 1
        };
        let step = &self.steps[served.min(self.steps.len() - 1)];
        let exchange = step(call.trace());
        Box::pin(async move { exchange })
    }

    fn log_uri(&self) -> Option<&http::Uri> {
        self.log_uri.as_ref()
    }
}
