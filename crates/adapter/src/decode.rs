//! The strict decoder of a model's reply, driven by the questions asked.
//!
//! Ported from `_schema.py` of system-one-adapter-python.
//!
//! Upstream builds one pydantic model per request, closed and strict
//! (`extra="forbid", strict=True`, `_schema.py:28`), and validates the reply
//! with it. This module reads the same replies without building a type: the
//! questions say which members an object has and what each value may be.
//!
//! What a reply must be, as measured on pydantic:
//!
//! - one JSON object `{"answers": {<question id>: <answer>, ...}}` with every
//!   question id once and no other member, white space allowed around it and
//!   nothing after it;
//! - a probability is a JSON number in `[0, 1]`, written as an integer or a
//!   float; a discrete noul is `true` or `false`; a discrete score is an
//!   integer literal below the number of levels; a discrete choice is one of
//!   the labels; a choice or score in probabilities mode is an object with
//!   every label once, each a probability;
//! - no value is converted: `"0.5"`, `1` for a noul and `1.0` for a score are
//!   refused.
//!
//! A member written twice keeps its last value, and the earlier one is never
//! looked at, as pydantic does: every object is therefore read in two steps,
//! first the text of each member's value, then the check of the values that
//! remain. Reading the text first also keeps the stack flat: no step recurses
//! into a value, however deeply the reply nests it.
//!
//! A refused reply is described by [`Problem`]s, all of them, in a fixed
//! order: the reply, its `answers`, then each question in question order. No
//! problem holds text of the reply, only question ids, labels and JSON types,
//! because the problems become the correction prompt.

use std::{borrow::Cow, fmt};

use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::{
    model::{DecodedAnswer, DecodedAnswers, Question, QuestionKind, QuestionModel},
    options::AnswerMode,
    prompt::{Expected, JsonType, Location, Problem},
};

/// The one member of a reply.
const ANSWERS: &str = "answers";

/// The answers `reply` gives to `questions`, in question order, or every
/// problem that keeps it from being them.
///
/// `reply` is the JSON text itself: a Markdown fence around it is removed
/// before this is called. The error is never empty.
pub(crate) fn decode(
    questions: &QuestionModel,
    mode: AnswerMode,
    reply: &str,
) -> Result<DecodedAnswers, Vec<Problem>> {
    let mut json = serde_json::Deserializer::from_str(reply);
    let root = <&RawValue>::deserialize(&mut json).map_err(|_| vec![Problem::NotJson])?;
    json.end().map_err(|_| vec![Problem::TrailingText])?;

    let mut problems = Vec::new();
    match decode_reply(questions, mode, root, &mut problems) {
        Some(answers) if problems.is_empty() => Ok(DecodedAnswers { answers }),
        _ => {
            debug_assert!(!problems.is_empty(), "a refused reply has a problem");
            Err(problems)
        }
    }
}

/// The answers of the reply `root`, `None` when one is missing or refused;
/// every question is checked either way, so `problems` is complete.
fn decode_reply(
    questions: &QuestionModel,
    mode: AnswerMode,
    root: &RawValue,
    problems: &mut Vec<Problem>,
) -> Option<Vec<DecodedAnswer>> {
    let Some(answers) =
        members(root, &[Cow::Borrowed(ANSWERS)], Location::Reply, problems)?.pop().flatten()
    else {
        problems.push(Problem::Missing { at: Location::Reply, member: ANSWERS.to_owned() });
        return None;
    };

    let ids: Vec<Cow<'_, str>> =
        questions.questions.iter().map(|question| Cow::Borrowed(question.id.as_str())).collect();
    let values = members(answers, &ids, Location::Answers, problems)?;
    let decoded: Vec<Option<DecodedAnswer>> = questions
        .questions
        .iter()
        .zip(values)
        .map(|(question, value)| match value {
            Some(value) => decode_answer(question, mode, value, problems),
            None => {
                problems
                    .push(Problem::Missing { at: Location::Answers, member: question.id.clone() });
                None
            }
        })
        .collect();
    decoded.into_iter().collect()
}

/// The answer `value` gives to `question`.
fn decode_answer(
    question: &Question,
    mode: AnswerMode,
    value: &RawValue,
    problems: &mut Vec<Problem>,
) -> Option<DecodedAnswer> {
    let at = Location::Question(question.id.clone());
    match (&question.kind, mode) {
        (QuestionKind::Noul { .. }, AnswerMode::Probabilities) => {
            probability(value, at, problems).map(DecodedAnswer::Probability)
        }
        (QuestionKind::Noul { .. }, AnswerMode::Discrete) => {
            boolean(value, at, problems).map(DecodedAnswer::Bool)
        }
        (QuestionKind::Score { criteria }, AnswerMode::Discrete) => {
            level(value, criteria.len(), at, problems).map(DecodedAnswer::Level)
        }
        (QuestionKind::Choice { .. }, AnswerMode::Discrete) => {
            label(value, &outcome_names(question), at, problems).map(DecodedAnswer::Label)
        }
        (QuestionKind::Choice { .. } | QuestionKind::Score { .. }, AnswerMode::Probabilities) => {
            distribution(value, question, at, problems).map(DecodedAnswer::Distribution)
        }
    }
}

/// The labels of a choice, or the levels of a score written `0`, `1`, ..., in
/// criteria order: the member names of its probability map.
fn outcome_names(question: &Question) -> Vec<Cow<'_, str>> {
    question.outcomes().into_iter().map(|(name, _)| name).collect()
}

/// A probability map: one probability per outcome of `question`, put in
/// criteria order whatever order the reply wrote them in.
fn distribution(
    value: &RawValue,
    question: &Question,
    at: Location,
    problems: &mut Vec<Problem>,
) -> Option<Vec<f64>> {
    let names = outcome_names(question);
    let values = members(value, &names, at, problems)?;
    let probabilities: Vec<Option<f64>> = names
        .iter()
        .zip(values)
        .map(|(name, value)| match value {
            Some(value) => {
                let at = Location::Label {
                    question: question.id.clone(),
                    label: name.clone().into_owned(),
                };
                probability(value, at, problems)
            }
            None => {
                problems.push(Problem::Missing {
                    at: Location::Question(question.id.clone()),
                    member: name.clone().into_owned(),
                });
                None
            }
        })
        .collect();
    probabilities.into_iter().collect()
}

/// A JSON number in `[0, 1]`.
fn probability(value: &RawValue, at: Location, problems: &mut Vec<Problem>) -> Option<f64> {
    let found = json_type(value);
    if found != JsonType::Number {
        problems.push(Problem::WrongType { at, expected: Expected::Probability, found });
        return None;
    }
    let text = value.get();
    let mut json = serde_json::Deserializer::from_str(text);
    // The comparison is false for NaN, and a number too large for a float is
    // an error of the parser.
    match json.deserialize_f64(NumberVisitor) {
        Ok(number) if (0.0..=1.0).contains(&number) => {
            // `serde_json` reads the integer text `-0` as the float -0.0.
            // Python reads it as the integer 0, which is the float 0.0; adding
            // 0.0 turns -0.0 into 0.0 and changes no other number.
            Some(if text.contains(['.', 'e', 'E']) { number } else { number + 0.0 })
        }
        _ => {
            problems.push(Problem::NotAllowed { at, expected: Expected::Probability });
            None
        }
    }
}

/// `true` or `false`.
fn boolean(value: &RawValue, at: Location, problems: &mut Vec<Problem>) -> Option<bool> {
    match value.get() {
        "true" => Some(true),
        "false" => Some(false),
        _ => {
            problems.push(Problem::WrongType {
                at,
                expected: Expected::Boolean,
                found: json_type(value),
            });
            None
        }
    }
}

/// An integer literal from 0 to `criteria - 1`.
fn level(
    value: &RawValue,
    criteria: usize,
    at: Location,
    problems: &mut Vec<Problem>,
) -> Option<usize> {
    let expected = Expected::Score { criteria };
    let found = json_type(value);
    if found != JsonType::Number {
        problems.push(Problem::WrongType { at, expected, found });
        return None;
    }
    let text = value.get();
    // A level is an integer literal: a text with a fraction or an exponent
    // is refused whatever number it denotes (`1.0`, `1e0`, `-0.0`), as
    // pydantic refuses it. The typed `deserialize_i64` reads the rest, never
    // the self-describing entry point: with `serde_json`'s
    // `arbitrary_precision` feature, which any crate of a build can turn on,
    // that one hands a number over as a map.
    let level = if text.contains(['.', 'e', 'E']) {
        None
    } else {
        serde_json::Deserializer::from_str(text)
            .deserialize_i64(LevelVisitor { criteria })
            .ok()
            .flatten()
    };
    if level.is_none() {
        problems.push(Problem::NotAllowed { at, expected });
    }
    level
}

/// One of the labels `names`, as its index.
fn label(
    value: &RawValue,
    names: &[Cow<'_, str>],
    at: Location,
    problems: &mut Vec<Problem>,
) -> Option<usize> {
    let expected = Expected::Label { labels: owned(names) };
    let found = json_type(value);
    if found != JsonType::String {
        problems.push(Problem::WrongType { at, expected, found });
        return None;
    }
    // An error here is a string no JSON reader accepts, such as half of a
    // surrogate pair: not a label either.
    match Name(names).deserialize(&mut serde_json::Deserializer::from_str(value.get())) {
        Ok(Some(index)) => Some(index),
        Ok(None) | Err(_) => {
            problems.push(Problem::NotAllowed { at, expected });
            None
        }
    }
}

/// The value of each of `names` in the object `value`, `None` for a name the
/// object lacks; the last value of a name written twice.
///
/// Reports a `value` that is not an object and a member that is none of
/// `names`, the latter once however many there are. It returns `None` only
/// when `value` is not an object that can be read.
fn members<'de>(
    value: &'de RawValue,
    names: &[Cow<'_, str>],
    at: Location,
    problems: &mut Vec<Problem>,
) -> Option<Vec<Option<&'de RawValue>>> {
    let found = json_type(value);
    if found != JsonType::Object {
        problems.push(Problem::WrongType {
            at,
            expected: Expected::Object { members: owned(names) },
            found,
        });
        return None;
    }
    // The text is already known to be JSON. What can still fail is a member
    // name no JSON reader accepts, such as half of a surrogate pair.
    let Ok(object) =
        Members(names).deserialize(&mut serde_json::Deserializer::from_str(value.get()))
    else {
        problems.push(Problem::NotJson);
        return None;
    };
    if object.unexpected {
        problems.push(Problem::Unexpected { at, members: owned(names) });
    }
    Some(object.values)
}

fn owned(names: &[Cow<'_, str>]) -> Vec<String> {
    names.iter().map(|name| name.clone().into_owned()).collect()
}

/// The JSON type of `value`, read from its first byte. The text of a value
/// `serde_json` captured starts at the value, with no white space before it.
fn json_type(value: &RawValue) -> JsonType {
    match value.get().as_bytes().first() {
        Some(b'{') => JsonType::Object,
        Some(b'[') => JsonType::Array,
        Some(b'"') => JsonType::String,
        Some(b't' | b'f') => JsonType::Boolean,
        Some(b'n') => JsonType::Null,
        _ => JsonType::Number,
    }
}

/// Reads an object whose members are named by the questions: the text of
/// each named member's value, unparsed, so that a value written again
/// replaces the earlier one before either is checked.
struct Members<'a, 'n>(&'a [Cow<'n, str>]);

/// What [`Members`] read.
struct Object<'de> {
    /// One entry per name, in the order of the names.
    values: Vec<Option<&'de RawValue>>,
    /// Whether the object holds a member that is none of the names. The
    /// member's own name is not kept: the reply chose it.
    unexpected: bool,
}

impl<'de> DeserializeSeed<'de> for Members<'_, '_> {
    type Value = Object<'de>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Members<'_, '_> {
    type Value = Object<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = Object { values: vec![None; self.0.len()], unexpected: false };
        while let Some(name) = map.next_key_seed(Name(self.0))? {
            match name {
                Some(index) => object.values[index] = Some(map.next_value()?),
                None => {
                    map.next_value::<IgnoredAny>()?;
                    object.unexpected = true;
                }
            }
        }
        Ok(object)
    }
}

/// Reads a JSON string as the index of the name it equals, `None` when it is
/// none of them. The string itself is not kept.
struct Name<'a, 'n>(&'a [Cow<'n, str>]);

impl<'de> DeserializeSeed<'de> for Name<'_, '_> {
    type Value = Option<usize>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for Name<'_, '_> {
    type Value = Option<usize>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON string")
    }

    fn visit_str<E>(self, text: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.0.iter().position(|name| name == text))
    }
}

/// Reads a JSON number as a float, whichever way it was written. It has no
/// other method, so nothing but a number becomes one.
struct NumberVisitor;

impl Visitor<'_> for NumberVisitor {
    type Value = f64;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON number")
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "only 0 and 1 are in range, and no rounding moves a larger integer below 1"
    )]
    fn visit_u64<E>(self, number: u64) -> Result<f64, E>
    where
        E: de::Error,
    {
        Ok(number as f64)
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "only 0 is in range, and no rounding moves a negative integer up to 0"
    )]
    fn visit_i64<E>(self, number: i64) -> Result<f64, E>
    where
        E: de::Error,
    {
        Ok(number as f64)
    }

    fn visit_f64<E>(self, number: f64) -> Result<f64, E>
    where
        E: de::Error,
    {
        Ok(number)
    }
}

/// Reads an integer literal as a level below `criteria`, `None` for any other
/// number.
struct LevelVisitor {
    criteria: usize,
}

impl LevelVisitor {
    fn in_range(&self, level: Option<usize>) -> Option<usize> {
        level.filter(|&level| level < self.criteria)
    }
}

impl Visitor<'_> for LevelVisitor {
    type Value = Option<usize>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON integer")
    }

    fn visit_u64<E>(self, number: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.in_range(usize::try_from(number).ok()))
    }

    fn visit_i64<E>(self, number: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.in_range(usize::try_from(number).ok()))
    }

    /// Only integer texts reach this visitor, and `serde_json` hands two
    /// kinds of them over as a float: `-0`, as -0.0, which is level 0, and an
    /// integer too large for 64 bits, which is no level.
    fn visit_f64<E>(self, number: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(self.in_range((number == 0.0 && number.is_sign_negative()).then_some(0)))
    }
}

#[cfg(test)]
#[path = "decode_tests.rs"]
mod tests;
