//! Everything the code `#[derive(QuestionSet)]` generates refers to.
//!
//! The expansion lands in the caller's crate, where `serde`, `core`'s prelude
//! or even the name `Option` may be missing or mean something else: a module
//! can declare `#![no_implicit_prelude]`, or define a type named `Result`.
//! Every path in the generated code therefore starts at this module, through
//! the crate root the caller names (`::typesafe_sdk` unless
//! `#[question_set(crate = ...)]` says otherwise), and this module re-exports
//! each item under the name the expansion uses.
//!
//! It is hidden from the documentation and carries **no semver promise**: it
//! changes whenever the macro does, and the two are released together with an
//! exact version requirement between them.

pub use core::{
    fmt::{self, Formatter},
    option::Option::{self, None, Some},
    primitive::{str, u8},
    result::Result::{self, Err, Ok},
};

pub use serde::{
    Deserialize, Deserializer,
    de::{Error, IgnoredAny, MapAccess, Visitor},
};

pub use crate::{
    de::{AnswerContext, AnswerSet},
    question::{PreparedQuestions, QuestionSet},
    response::{ChoiceAnswer, NoulAnswer, ScoreAnswer},
};
