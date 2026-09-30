//! Async Rust SDK for the [TypeSafe AI](https://typesafe.ai) API.
//!
//! The crate is published as `typesafe-sdk-rust` because `typesafe-sdk` is
//! already taken on crates.io; the library it builds is `typesafe_sdk`, so
//! callers write `use typesafe_sdk::...`.
//!
//! # Asking questions
//!
//! A call asks a set of named questions about a state. The set is built once,
//! validated and serialized by [`Questions::prepare`], and the resulting
//! [`PreparedQuestions`] is reused by every call that asks it. A [`Client`]
//! sends it: [`Client::system_one`] makes a request, whose methods set the
//! model, the deadline, extra headers and extra body members, and
//! [`send`](SystemOne::send) sends it and decodes the answers.
//!
//! ```
//! use std::time::Duration;
//!
//! use typesafe_sdk::{Choice, Client, Noul, Questions, Score};
//!
//! let questions = Questions::new()
//!     .noul("billing", Noul::new().instructions("Is this about billing?"))
//!     .choice("tone", Choice::new(["calm", "angry"]).instructions("What is the tone?"))
//!     .score("urgency", Score::new(["can wait", "this week", "today"]))
//!     .prepare()?;
//! assert_eq!(questions.names().collect::<Vec<_>>(), ["billing", "tone", "urgency"]);
//!
//! // `Client::from_env()` reads the same settings from TYPESAFE_API_KEY and
//! // friends. Building connects to nothing.
//! let client = Client::builder().api_key("your-api-key").build()?;
//!
//! let state = "I was charged twice for one order.";
//! let request = client
//!     .system_one(state, &questions)
//!     .model("jev-latest")
//!     .timeout(Duration::from_secs(2))
//!     .header("x-team", "billing");
//!
//! // Sending needs a Tokio runtime; this example stops before it.
//! async fn ask<S: typesafe_sdk::HttpService>(request: typesafe_sdk::SystemOne<'_, S, str>)
//! -> Result<f64, typesafe_sdk::Error> {
//!     let response = request.send().await?;
//!     Ok(response.answers().noul("billing").map_or(0.0, |answer| answer.noul()))
//! }
//! drop(ask(request));
//! # Ok::<(), typesafe_sdk::Error>(())
//! ```
//!
//! # Runtime requirements
//!
//! Every network operation is `async` and expects a [Tokio] runtime whose
//! **time driver is enabled** (`#[tokio::main]`, or a `Builder` with
//! `enable_time()` / `enable_all()`). Per-attempt deadlines and HTTP/2
//! keep-alive both arm timers, and Tokio panics when a timer is created on a
//! runtime without that driver.
//!
//! # Safety
//!
//! The crate is `#![forbid(unsafe_code)]`. Dependencies that use `unsafe`
//! internally are confined to single modules so that swapping one out is a
//! local change.
//!
//! [Tokio]: https://docs.rs/tokio

#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod client;
mod codec;
mod config;
pub mod constants;
pub mod content;
pub mod de;
pub mod error;
pub mod models;
mod name;
pub mod question;
mod redact;
#[cfg(test)]
mod rendering_tests;
pub mod request;
pub mod response;
pub mod retry;
mod telemetry;
mod text;
pub mod transport;

#[cfg(feature = "internals")]
#[doc(hidden)]
pub mod __internals;

pub use crate::{
    client::{Client, ClientBuilder},
    codec::{DecodeError, DecodeErrorKind, EncodeError, RawJson},
    content::{Content, ContentError},
    de::{AnswerContext, AnswerSet},
    error::{ApiError, ApiErrorKind, Error, ErrorKind, ResponseValidationError},
    models::{ListModels, ListModelsResponse, ModelMetadata, Models},
    question::{Choice, Noul, PreparedQuestions, Question, Questions, RawQuestion, Score},
    request::SystemOne,
    response::{
        Answer, Answers, ChoiceAnswer, NoulAnswer, ResponseMeta, ScoreAnswer, SystemOneResponse,
        Usage,
    },
    retry::{RetryPolicy, StatusSet},
    transport::{Body, BoxError, HttpService},
};

#[cfg(feature = "hyper")]
#[cfg_attr(docsrs, doc(cfg(feature = "hyper")))]
pub use crate::transport::{HttpVersion, HyperResponseFuture, HyperTransport, ResponseBody};

pub use crate::question::QuestionSet;

/// Implements [`QuestionSet`](trait@QuestionSet) and [`AnswerSet`] for a struct with one field
/// per question.
///
/// Available with the `macros` feature, which is on by default.
///
/// ```
/// use typesafe_sdk::{ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer};
///
/// #[derive(QuestionSet)]
/// struct Ticket {
///     #[noul(instructions = "Is this about billing?", yes = "payments or invoices")]
///     billing: NoulAnswer,
///     #[choice(instructions = "What is the tone?", options("calm" = "neutral or polite", "angry"))]
///     tone: ChoiceAnswer,
///     #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
///     urgency: ScoreAnswer,
/// }
///
/// assert_eq!(Ticket::prepared().names().collect::<Vec<_>>(), ["billing", "tone", "urgency"]);
/// ```
#[cfg(feature = "macros")]
#[doc(inline)]
pub use typesafe_sdk_rust_macros::QuestionSet;

#[doc(hidden)]
pub mod __private;

/// Compiles every Rust block of `README.md` as a doctest, so the front page
/// cannot drift from the API. The typed-answers block needs the derive, hence
/// the feature.
#[cfg(all(doctest, feature = "macros", feature = "hyper"))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
