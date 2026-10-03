//! `#[derive(QuestionSet)]` for the `decision-model-sdk` crate.
//!
//! Use it through the SDK, which re-exports it as `decision_model_sdk::QuestionSet`
//! behind its `macros` feature (on by default); its documentation there has a
//! compiled example. This package is released with the SDK and depended on at
//! exactly one version, so the two always agree on the code the derive
//! generates.

#![forbid(unsafe_code)]

mod expand;
mod json;
mod parse;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Declares a question set as a struct: one field per question, each holding
/// that question's answer.
///
/// The questions are validated and serialized while the program compiles,
/// into the same bytes `Questions::prepare` produces for them at run time.
/// `QuestionSet::prepared` hands out that set without allocating, and the
/// generated `AnswerSet` implementation reads each answer of a response
/// straight into its field.
///
/// # Attributes
///
/// Every field carries exactly one question attribute, and its type is the
/// answer type of that question:
///
/// ```text
/// #[noul(instructions = "...", yes = "...", no = "...")]     field: NoulAnswer
/// #[choice(instructions = "...", options("a" = "...", "b"))] field: ChoiceAnswer
/// #[score(instructions = "...", levels("low", "high"))]      field: ScoreAnswer
/// ```
///
/// - `#[noul]`: every key is optional, and `#[noul]` alone is a noul with
///   nothing set. `yes` and `no` describe what counts as each outcome.
/// - `#[choice]`: `options(...)` is required; it lists the options in order,
///   each an option name optionally followed by `= "description"`. An option
///   is listed once. `instructions` is optional.
/// - `#[score]`: `levels(...)` is required and lists at least one level
///   description, lowest first; a level's position is its score.
///   `instructions` is optional.
///
/// Each key is given at most once. Every value is a string literal: text is
/// the only content the derive takes. Object or array content is a feature of
/// the runtime `Questions` builder.
///
/// A question is asked under its field's name, `r#` removed. Another name,
/// such as one that is not a Rust identifier, is set with
/// `#[question(name = "...")]` on the field. No two fields answer to one name.
///
/// The field type is recognized by its last path segment, so `NoulAnswer` and
/// `decision_model_sdk::NoulAnswer` both work; a type alias does not, because a
/// derive sees the tokens of a type and not what they name. A field cannot be
/// an `Option`: the API answers every question it is asked, and a response
/// without one of the answers is refused as malformed.
///
/// The struct itself has named fields, at least one, and no generics. Its
/// name is not one of those the generated code gives to its own items:
/// `__QuestionSetField`, `__QuestionSetFieldVisitor`, `__QuestionSetVisitor`,
/// `__D`, `__M` and `__private` are refused.
///
/// # Crate path
///
/// The generated code reaches the SDK as `::decision_model_sdk`. Where it is known
/// by another path, such as a re-export, the container attribute
/// `#[question_set(crate = path::to::sdk)]` names that path. A dependency
/// renamed in `Cargo.toml`, as `sdk = { package = "decision-model-sdk", ... }`,
/// is one such case: the generated code still names `::decision_model_sdk`, so the
/// struct needs `#[question_set(crate = ::sdk)]`.
///
/// # Limits
///
/// None on the number of options or levels: the API documents its limits as
/// subject to change, so the server is the one to judge them.
#[proc_macro_derive(QuestionSet, attributes(noul, choice, score, question, question_set))]
pub fn derive_question_set(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::expand(&input).unwrap_or_else(syn::Error::into_compile_error).into()
}
