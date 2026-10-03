//! The questions of a call, re-validated from the SDK's JSON form, and the
//! values the decoder reads from a model's reply.
//!
//! Ported from `_schema.py` of system-one-adapter-python.
//!
//! Upstream dumps every question to JSON and validates the result again with
//! pydantic in strict mode against closed models, then requires two criteria
//! of every choice and score (`_schema.py:54-66`). The SDK hands the prepared
//! questions over in one form only, the JSON text of
//! [`PreparedQuestions::as_json`], whatever built them (a builder, a raw
//! question or a derived question set), so this module reads that text and
//! applies the same rules: a known `type`, no member the type does not take,
//! content that is a string, an object or an array, and the shapes of the
//! three `criteria`.
//!
//! Content that is not a string is kept as its JSON text, unparsed, so its
//! members stay in the order the SDK wrote them; parsing it into a
//! `serde_json::Value` would sort them, because the workspace does not enable
//! `preserve_order`. White space between its tokens is removed, so the text is
//! as compact as upstream's `to_json`.

use std::{borrow::Cow, fmt};

use decision_model_sdk::PreparedQuestions;
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;

use crate::error::Error;

/// What a criterion or the instructions are written as in a prompt and a
/// schema when the question gives none (upstream's
/// `_serialize_instruction_value_for_prompt`).
pub(crate) const NO_INSTRUCTIONS: &str = "No additional instructions.";

/// Upstream's sentence for a choice or score with fewer than two criteria.
const TOO_FEW_CRITERIA: &str = "Score and choice questions require at least two criteria.";

/// The questions of one call, in the order the SDK sent them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct QuestionModel {
    pub(crate) questions: Vec<Question>,
}

/// One re-validated question.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Question {
    /// The name the answer comes back under.
    pub(crate) id: String,
    pub(crate) instructions: Option<Content>,
    pub(crate) kind: QuestionKind,
}

/// The three question types and their criteria.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum QuestionKind {
    /// A yes/no question. `None` when it has no `criteria` member (or a
    /// `null` one); a present object may still describe neither outcome.
    Noul { criteria: Option<NoulCriteria> },
    /// One of the labels, in their order, each with its description, `None`
    /// for a label left undescribed.
    Choice { criteria: Vec<(String, Option<Content>)> },
    /// One of the levels, from 0 up, each with its description.
    Score { criteria: Vec<Content> },
}

/// The descriptions of a noul's outcomes, upstream's `criteria` object with
/// its `true` and `false` members.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct NoulCriteria {
    pub(crate) yes: Option<Content>,
    pub(crate) no: Option<Content>,
}

/// Instructions or a criterion: text, or a JSON object or array kept as its
/// compact JSON text.
#[derive(Clone)]
pub(crate) enum Content {
    Text(String),
    Json(Box<RawValue>),
}

/// One decoded answer, as the decoder produces it and the conversion to the
/// SDK's answers consumes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DecodedAnswer {
    /// A noul in probabilities mode: the probability of yes, in `[0, 1]`.
    Probability(f64),
    /// A noul in discrete mode.
    Bool(bool),
    /// A choice in discrete mode: the index of the label in criteria order.
    Label(usize),
    /// A score in discrete mode: the level, below the number of levels.
    Level(usize),
    /// A choice or a score in probabilities mode: one probability per label
    /// or level, in criteria order, each in `[0, 1]`.
    Distribution(Vec<f64>),
}

/// The decoded answers of one reply, one per question in question order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DecodedAnswers {
    pub(crate) answers: Vec<DecodedAnswer>,
}

impl QuestionModel {
    /// The questions of `prepared`, re-validated.
    pub(crate) fn from_prepared(prepared: &PreparedQuestions) -> Result<Self, Error> {
        Self::from_json(prepared.as_json())
    }

    /// The questions of the JSON object `json`, the form of
    /// [`PreparedQuestions::as_json`], re-validated.
    ///
    /// Every question is checked for its shape first, in order, and the
    /// number of criteria after that, as upstream does: a malformed later
    /// question is reported before an earlier one with too few criteria.
    pub(crate) fn from_json(json: &str) -> Result<Self, Error> {
        let members = serde_json::from_str::<Members>(json)
            .map_err(|_| Error::invalid_request("The questions must be one JSON object."))?;
        if members.0.is_empty() {
            return Err(Error::invalid_request("At least one question is required."));
        }
        let questions = members
            .0
            .into_iter()
            .map(|(id, value)| parse_question(id, &value))
            .collect::<Result<Vec<_>, _>>()?;
        let too_few = |question: &Question| match &question.kind {
            QuestionKind::Choice { criteria } => criteria.len() < 2,
            QuestionKind::Score { criteria } => criteria.len() < 2,
            QuestionKind::Noul { .. } => false,
        };
        if questions.iter().any(too_few) {
            return Err(Error::invalid_request(TOO_FEW_CRITERIA));
        }
        Ok(Self { questions })
    }
}

impl Question {
    /// The outcomes of a choice or a score with their descriptions, in
    /// criteria order: a choice's labels, or a score's levels written as
    /// `0`, `1`, ... Empty for a noul.
    pub(crate) fn outcomes(&self) -> Vec<(Cow<'_, str>, Option<&Content>)> {
        match &self.kind {
            QuestionKind::Noul { .. } => Vec::new(),
            QuestionKind::Choice { criteria } => criteria
                .iter()
                .map(|(label, description)| (Cow::Borrowed(label.as_str()), description.as_ref()))
                .collect(),
            QuestionKind::Score { criteria } => criteria
                .iter()
                .enumerate()
                .map(|(level, description)| (Cow::Owned(level.to_string()), Some(description)))
                .collect(),
        }
    }
}

impl Content {
    /// The text a prompt or a schema description writes for this content:
    /// the text itself, or the compact JSON of an object or array.
    pub(crate) fn as_prompt_text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Json(json) => json.get(),
        }
    }

    /// The prompt text of optional content, [`NO_INSTRUCTIONS`] when absent.
    pub(crate) fn prompt_text(content: Option<&Self>) -> &str {
        content.map_or(NO_INSTRUCTIONS, Self::as_prompt_text)
    }
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text(left), Self::Text(right)) => left == right,
            (Self::Json(left), Self::Json(right)) => left.get() == right.get(),
            _ => false,
        }
    }
}

impl fmt::Debug for Content {
    /// The shape and the length, never the text.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => write!(formatter, "Text(<{} bytes>)", text.len()),
            Self::Json(json) => write!(formatter, "Json(<{} bytes>)", json.get().len()),
        }
    }
}

/// The members of a JSON object in document order, with a repeated name
/// keeping its first position and its last value, as a Python dictionary
/// built from the pairs does.
struct Members(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MembersVisitor;

        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Members;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Members, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut members: Vec<(String, Box<RawValue>)> =
                    Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((name, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    match members.iter_mut().find(|(known, _)| *known == name) {
                        Some((_, earlier)) => *earlier = value,
                        None => members.push((name, value)),
                    }
                }
                Ok(Members(members))
            }
        }

        deserializer.deserialize_map(MembersVisitor)
    }
}

/// The JSON type of a raw value, read from its first byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Null,
    String,
    Object,
    Array,
    Other,
}

fn shape(value: &RawValue) -> Shape {
    match value.get().as_bytes().first() {
        Some(b'n') => Shape::Null,
        Some(b'"') => Shape::String,
        Some(b'{') => Shape::Object,
        Some(b'[') => Shape::Array,
        _ => Shape::Other,
    }
}

/// One question object, checked against the closed model of its `type`.
fn parse_question(id: String, value: &RawValue) -> Result<Question, Error> {
    let members = members_of(value).ok_or_else(|| invalid(&id, "must be a JSON object"))?;
    let kind = members
        .iter()
        .find(|(name, _)| name == "type")
        .and_then(|(_, kind)| serde_json::from_str::<String>(kind.get()).ok())
        .filter(|kind| matches!(kind.as_str(), "noul" | "choice" | "score"))
        .ok_or_else(|| unknown_type(&id))?;
    let mut instructions = None;
    let mut criteria = None;
    for (name, member) in &members {
        match name.as_str() {
            "type" => {}
            "instructions" => {
                instructions = optional_content(member).ok_or_else(|| {
                    invalid(
                        &id,
                        "has instructions that are not a string, an object, an array or null",
                    )
                })?
            }
            "criteria" => criteria = Some(member),
            _ => {
                return Err(invalid(
                    &id,
                    &format!("has a member \"{name}\" that a {kind} question does not take"),
                ));
            }
        }
    }
    let kind = match kind.as_str() {
        "noul" => QuestionKind::Noul {
            criteria: criteria.map(|criteria| noul_criteria(&id, criteria)).transpose()?.flatten(),
        },
        "choice" => QuestionKind::Choice {
            criteria: choice_criteria(
                &id,
                criteria.ok_or_else(|| invalid(&id, "requires \"criteria\""))?,
            )?,
        },
        "score" => QuestionKind::Score {
            criteria: score_criteria(
                &id,
                criteria.ok_or_else(|| invalid(&id, "requires \"criteria\""))?,
            )?,
        },
        _ => return Err(unknown_type(&id)),
    };
    Ok(Question { id, instructions, kind })
}

/// A noul's `criteria`: `null`, or an object with no members but `true` and
/// `false`, each `null` or content.
fn noul_criteria(id: &str, value: &RawValue) -> Result<Option<NoulCriteria>, Error> {
    if shape(value) == Shape::Null {
        return Ok(None);
    }
    let members = members_of(value)
        .ok_or_else(|| invalid(id, "has noul criteria that are not an object or null"))?;
    let mut criteria = NoulCriteria::default();
    for (name, member) in &members {
        let outcome = match name.as_str() {
            "true" => &mut criteria.yes,
            "false" => &mut criteria.no,
            _ => {
                return Err(invalid(
                    id,
                    &format!(
                        "has a noul criterion \"{name}\"; only \"true\" and \"false\" are allowed"
                    ),
                ));
            }
        };
        *outcome = optional_content(member).ok_or_else(|| {
            invalid(id, "has a noul criterion that is not a string, an object, an array or null")
        })?;
    }
    Ok(Some(criteria))
}

/// A choice's `criteria`: an object from label to `null` or content.
fn choice_criteria(id: &str, value: &RawValue) -> Result<Vec<(String, Option<Content>)>, Error> {
    let members = members_of(value)
        .ok_or_else(|| invalid(id, "has choice criteria that are not an object"))?;
    members
        .into_iter()
        .map(|(label, member)| {
            let description = optional_content(&member).ok_or_else(|| {
                invalid(
                    id,
                    "has a choice criterion that is not a string, an object, an array or null",
                )
            })?;
            Ok((label, description))
        })
        .collect()
}

/// A score's `criteria`: an array of content, `null` not allowed.
fn score_criteria(id: &str, value: &RawValue) -> Result<Vec<Content>, Error> {
    let levels = (shape(value) == Shape::Array)
        .then(|| serde_json::from_str::<Vec<Box<RawValue>>>(value.get()).ok())
        .flatten()
        .ok_or_else(|| invalid(id, "has score criteria that are not an array"))?;
    levels
        .iter()
        .map(|level| {
            optional_content(level).flatten().ok_or_else(|| {
                invalid(id, "has a score criterion that is not a string, an object or an array")
            })
        })
        .collect()
}

/// The members of `value` when it is an object.
fn members_of(value: &RawValue) -> Option<Vec<(String, Box<RawValue>)>> {
    (shape(value) == Shape::Object)
        .then(|| serde_json::from_str::<Members>(value.get()).ok())
        .flatten()
        .map(|members| members.0)
}

/// `Some(None)` for `null`, `Some(Some(content))` for a string, an object or
/// an array, and `None` for anything else.
fn optional_content(value: &RawValue) -> Option<Option<Content>> {
    match shape(value) {
        Shape::Null => Some(None),
        Shape::String => {
            serde_json::from_str::<String>(value.get()).ok().map(|text| Some(Content::Text(text)))
        }
        Shape::Object | Shape::Array => Some(Some(Content::Json(compact(value)))),
        Shape::Other => None,
    }
}

/// `value` with the white space between its tokens removed; the text inside
/// its strings, member order and number text are kept.
fn compact(value: &RawValue) -> Box<RawValue> {
    let text = value.get();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for character in text.chars() {
        if in_string {
            out.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
        } else if character == '"' {
            in_string = true;
            out.push(character);
        } else if !matches!(character, ' ' | '\t' | '\n' | '\r') {
            out.push(character);
        }
    }
    RawValue::from_string(out)
        .expect("invariant: removing white space between the tokens of valid JSON keeps it valid")
}

fn invalid(id: &str, problem: &str) -> Error {
    Error::invalid_request(format!("Question \"{id}\" {problem}."))
}

fn unknown_type(id: &str) -> Error {
    invalid(id, "must have a \"type\" of \"noul\", \"choice\" or \"score\"")
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
