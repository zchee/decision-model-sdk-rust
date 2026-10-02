#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(
    dead_code,
    reason = "frozen API items whose callers arrive with later lanes; removed when the last provider lane merges"
)]

mod client;
mod convert;
mod decode;
mod error;
mod metrics;
mod model;
mod options;
mod prompt;
mod provider;
mod response;
mod retry;
mod run;
mod schema;

#[cfg(feature = "internals")]
#[doc(hidden)]
pub mod __internals;

/// The TypeSafe SDK this crate is built on, re-exported whole.
///
/// The question and answer types below come from it. A caller that derives a
/// question set with `#[derive(QuestionSet)]` either depends on
/// `typesafe-sdk-rust` directly, with its `macros` feature, or enables this
/// crate's `macros` feature and points the derive here with
/// `#[question_set(crate = "system_one_adapter::typesafe_sdk")]`, because the
/// derive's expansion names `::typesafe_sdk` unless told otherwise.
pub use typesafe_sdk;

// The questions, the answers and the retry policy, as the SDK defines them.
pub use typesafe_sdk::{
    Answer, AnswerSet, Answers, Choice, ChoiceAnswer, Noul, NoulAnswer, PreparedQuestions,
    QuestionSet, Questions, RetryPolicy, Score, ScoreAnswer,
};

// How a call is configured, what it returns and how it fails.

// The seam a model is plugged into, and the messages and schema it receives.

// The HTTP transport the built-in providers send their requests through.

// The client and the requests it builds.

// The OpenAI provider, for OpenAI's API and the services that speak it.

// The Anthropic provider.

// The Gemini provider.
