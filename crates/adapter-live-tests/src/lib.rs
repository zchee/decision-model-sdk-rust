//! What the adapter's tests against the live vendor APIs share.
//!
//! **These tests make real, billed calls** to OpenAI, Anthropic and Gemini.
//! A test runs only when BOTH `TYPESAFE_ADAPTER_LIVE_TESTS=1` and its
//! provider's key are in the environment: `OPENAI_API_KEY`,
//! `ANTHROPIC_API_KEY`, or `GOOGLE_API_KEY` (else `GEMINI_API_KEY`) for
//! Gemini. Then any command naming `-p typesafe-sdk-rust-adapter-live-tests`,
//! or a `--workspace` run, sends requests to that vendor on that key's
//! account. A key alone is not enough, so a contributor who has one exported
//! for other work is not billed by a stray `--workspace`. This crate is a
//! workspace member (so its code is linted and compiled) but not a default
//! member: a plain `cargo test` or `cargo nextest run` never builds or runs
//! it. Run it on purpose, with keys meant for it.
//!
//! The tests themselves are in `tests/live.rs`, ported from
//! `tests/test_client_with_live_apis.py` of system-one-adapter-python. Each
//! one gets its client from [`openai`], [`anthropic`] or [`gemini`], which
//! panics when the opt-in variable or the provider's key is missing, before
//! any client is built or any request is made, so such a run fails with a
//! message naming both variables instead of passing without having asked the
//! vendor anything.
//!
//! Each case sends exactly one request: no retry of a failed request and no
//! corrective retry of a malformed reply. After it, whether it succeeded or
//! not, [`Live::system_one`] writes one line to stderr,
//! `live-request <provider> <structured> <mode> attempts=<n>`, so the
//! requests a run sent can be counted from its output.

use std::{env, sync::Arc, time::Duration};

use system_one_adapter::{
    AnswerMode, AnthropicProvider, Client, Error, GeminiProvider, OpenAiProvider,
    PreparedQuestions, Provider, ProviderName, Response, RetryPolicy, StructuredOutputs,
};

/// The deadline of one live request: upstream's live tests accept a latency
/// below 120 seconds, and the SDK's live tests give their clients the same.
pub const LIVE_TIMEOUT: Duration = Duration::from_secs(120);

/// The variable that opts in to the live tests; it must be exactly `1`.
pub const LIVE_TESTS_ENV: &str = "TYPESAFE_ADAPTER_LIVE_TESTS";

/// The OpenAI model upstream's live tests ask.
pub const OPENAI_MODEL: &str = "gpt-4o-mini";

/// The Anthropic model upstream's live tests ask.
pub const ANTHROPIC_MODEL: &str = "claude-haiku-4-5";

/// The Gemini model upstream's live tests ask.
pub const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";

/// How the model is made to answer in the shape of the output schema, by
/// upstream's test ids.
///
/// The adapter's [`StructuredOutputs`] may grow, so a word for each of its
/// values would need a catch-all arm; these two are the cases upstream runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Structured {
    /// The schema written into the prompt: [`StructuredOutputs::Prompted`].
    Prompted,
    /// The vendor's own structured output: [`StructuredOutputs::Native`].
    Native,
}

impl Structured {
    /// Upstream's test id: `prompted` or `native`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prompted => "prompted",
            Self::Native => "native",
        }
    }

    const fn outputs(self) -> StructuredOutputs {
        match self {
            Self::Prompted => StructuredOutputs::Prompted,
            Self::Native => StructuredOutputs::Native,
        }
    }
}

/// What the model answers each question with, by upstream's test ids.
///
/// The adapter's [`AnswerMode`] may grow, for the reason [`Structured`]
/// gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A probability per outcome: [`AnswerMode::Probabilities`].
    Probabilities,
    /// One outcome per question: [`AnswerMode::Discrete`].
    Discrete,
}

impl Mode {
    /// Upstream's test id: `probabilities` or `discrete`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Probabilities => "probabilities",
            Self::Discrete => "discrete",
        }
    }

    const fn answer_mode(self) -> AnswerMode {
        match self {
            Self::Probabilities => AnswerMode::Probabilities,
            Self::Discrete => AnswerMode::Discrete,
        }
    }
}

/// A client for one live case: one vendor's model, asked in one structured
/// mode for one kind of answer, with one request per call.
///
/// `Debug` prints the client's options and its provider's type name, never
/// the key.
#[derive(Debug)]
pub struct Live {
    client: Client,
    provider: ProviderName,
    structured: Structured,
    mode: Mode,
}

impl Live {
    /// The provider this case asks.
    #[must_use]
    pub fn provider(&self) -> ProviderName {
        self.provider
    }

    /// The structured mode this case asks in.
    #[must_use]
    pub fn structured(&self) -> Structured {
        self.structured
    }

    /// The kind of answer this case asks for.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Asks `questions` about `state`, then writes the case's one stderr line
    /// with the number of attempts the response's or the error's trace holds
    /// (0 for an error without a trace), and returns the result unchanged.
    ///
    /// # Errors
    ///
    /// The adapter's error for the call, unchanged.
    pub async fn system_one(
        &self,
        state: &str,
        questions: &PreparedQuestions,
    ) -> Result<Response, Error> {
        let result = self.client.system_one(state, questions).send().await;
        let attempts = match &result {
            Ok(response) => response.debug().attempts().len(),
            Err(error) => error.debug().map_or(0, |trace| trace.attempts().len()),
        };
        eprintln!(
            "live-request {} {} {} attempts={attempts}",
            self.provider.as_str(),
            self.structured.as_str(),
            self.mode.as_str()
        );
        result
    }
}

/// A live case against OpenAI's [`OPENAI_MODEL`].
///
/// # Panics
///
/// When `TYPESAFE_ADAPTER_LIVE_TESTS` is not `1` or `OPENAI_API_KEY` is unset
/// or empty, checked before anything is built, with a message naming both
/// variables and never the key's value; and when the provider cannot be
/// built, with the adapter's configuration error.
#[must_use]
pub fn openai(structured: Structured, mode: Mode) -> Live {
    let key = opted_in_key(ProviderName::OpenAi, &["OPENAI_API_KEY"]);
    let provider = OpenAiProvider::builder(OPENAI_MODEL).api_key(key).timeout(LIVE_TIMEOUT).build();
    live(ProviderName::OpenAi, provider, structured, mode)
}

/// A live case against Anthropic's [`ANTHROPIC_MODEL`].
///
/// # Panics
///
/// As [`openai`] does, for `ANTHROPIC_API_KEY`.
#[must_use]
pub fn anthropic(structured: Structured, mode: Mode) -> Live {
    let key = opted_in_key(ProviderName::Anthropic, &["ANTHROPIC_API_KEY"]);
    let provider =
        AnthropicProvider::builder(ANTHROPIC_MODEL).api_key(key).timeout(LIVE_TIMEOUT).build();
    live(ProviderName::Anthropic, provider, structured, mode)
}

/// A live case against Gemini's [`GEMINI_MODEL`].
///
/// The key is `GOOGLE_API_KEY`, else `GEMINI_API_KEY`, in the order the
/// adapter itself reads them.
///
/// # Panics
///
/// As [`openai`] does, when neither key is set; the message names
/// `GOOGLE_API_KEY`.
#[must_use]
pub fn gemini(structured: Structured, mode: Mode) -> Live {
    let key = opted_in_key(ProviderName::Gemini, &["GOOGLE_API_KEY", "GEMINI_API_KEY"]);
    let provider = GeminiProvider::builder(GEMINI_MODEL).api_key(key).timeout(LIVE_TIMEOUT).build();
    live(ProviderName::Gemini, provider, structured, mode)
}

/// The key of the first of `variables` that is set and not empty, after
/// checking that the live tests are opted in to.
///
/// The check reads whether the variables are present, never their text; the
/// key is read only once both conditions hold, and it goes to the provider's
/// builder and nowhere else. Every panic names variables, never a value.
fn opted_in_key(provider: ProviderName, variables: &[&'static str]) -> String {
    let opted_in = env::var_os(LIVE_TESTS_ENV).is_some_and(|value| value == "1");
    let present = variables
        .iter()
        .copied()
        .find(|variable| env::var_os(variable).is_some_and(|value| !value.is_empty()));
    let named = variables[0];
    let Some(variable) = present.filter(|_| opted_in) else {
        let missing = match (opted_in, present.is_some()) {
            (false, false) => format!("{LIVE_TESTS_ENV}=1 and {named} are"),
            (false, true) => format!("{LIVE_TESTS_ENV}=1 is"),
            _ => format!("{named} is"),
        };
        panic!(
            "the {provider} live tests make billed calls and run only with both \
             {LIVE_TESTS_ENV}=1 and {named} set; {missing} missing, so they fail rather than skip"
        );
    };
    // The error of a key that is not Unicode holds the key's bytes, so it is
    // dropped unread.
    match env::var(variable) {
        Ok(key) => key,
        Err(_) => panic!("{variable} is set but is not Unicode, so it cannot be an API key"),
    }
}

/// The case's client: `provider`, one attempt per call, no corrective retry.
fn live<P: Provider + 'static>(
    name: ProviderName,
    provider: Result<P, Error>,
    structured: Structured,
    mode: Mode,
) -> Live {
    let provider = match provider {
        Ok(provider) => provider,
        Err(error) => panic!("the {name} provider could not be built: {error}"),
    };
    let client = Client::builder(structured.outputs(), mode.answer_mode())
        .provider_instance(Arc::new(provider))
        .retry(RetryPolicy::none())
        .n_retry_malformed_structure(0)
        .build();
    match client {
        Ok(client) => Live { client, provider: name, structured, mode },
        Err(error) => panic!("the {name} live client could not be built: {error}"),
    }
}
