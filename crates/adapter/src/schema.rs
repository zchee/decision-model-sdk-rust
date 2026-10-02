//! The JSON schema of a question set, written in the questions' order.
//!
//! Ported from `_schema.py` of system-one-adapter-python.
//!
//! Upstream builds a pydantic model per call and asks it for its JSON schema;
//! the text of that schema is part of every prompted request, so this writer
//! reproduces pydantic's text byte for byte instead of building a model:
//!
//! - The members of a schema object are sorted by name, and so are the
//!   members of `$defs`, as strings: `ProbabilityMap10` comes before
//!   `ProbabilityMap2`.
//! - The members of `properties` and the elements of `required` keep the
//!   order of the questions, or of a question's labels.
//! - pydantic does not sort the members of an object whose own name is
//!   `properties` or `default`, so the schema of a question or a label with
//!   one of those two names keeps the order pydantic generated it in, `type`
//!   before `description`.
//! - The description of a probability map is a class docstring to pydantic,
//!   which cleans it as Python cleans any docstring; see [`clean_docstring`].
//! - `title` and the numeric bounds are left out, as upstream removes them.

use std::borrow::Cow;

use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};

use crate::{
    model::{Content, Question, QuestionKind, QuestionModel},
    options::AnswerMode,
    provider::Schema,
};

/// The name of the definition that holds one property per question.
const ANSWERS: &str = "TypeSafeAnswers";

/// The description of [`ANSWERS`], upstream's docstring of that model.
const ANSWERS_DESCRIPTION: &str = "Exactly one answer per property below. Use these property names verbatim and do not add, rename, or nest them under any other key.";

/// The schema a reply to `questions` must follow in `mode`.
pub(crate) fn write(questions: &QuestionModel, mode: AnswerMode) -> Schema {
    let mut maps = Vec::new();
    let mut answers = Vec::with_capacity(questions.questions.len());
    for (index, question) in questions.questions.iter().enumerate() {
        let is_map = mode == AnswerMode::Probabilities
            && !matches!(question.kind, QuestionKind::Noul { .. });
        let property = if is_map {
            // The index counts every question, not only the maps.
            let name = format!("ProbabilityMap{index}");
            maps.push((name.clone(), probability_map(question, mode)));
            Property::Reference(name)
        } else {
            Property::Value {
                kind: value_kind(question, mode),
                description: field_description(question, mode),
            }
        };
        answers.push((Cow::Borrowed(question.id.as_str()), property));
    }
    maps.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
    let root = Root {
        maps,
        answers: Object { description: Cow::Borrowed(ANSWERS_DESCRIPTION), properties: answers },
    };
    let json = serde_json::to_string(&root)
        .expect("invariant: a schema is maps with string names, strings and booleans");
    Schema::from_string(json).expect("invariant: the writer emits one JSON object")
}

/// The whole schema: the definitions, and a root whose one property, `answers`,
/// refers to [`ANSWERS`].
struct Root<'a> {
    /// The probability maps by definition name, sorted by that name.
    maps: Vec<(String, Object<'a>)>,
    answers: Object<'a>,
}

/// A closed object schema: every property is required and no other is
/// allowed.
struct Object<'a> {
    description: Cow<'a, str>,
    properties: Vec<(Cow<'a, str>, Property<'a>)>,
}

/// The schema of one property.
enum Property<'a> {
    /// A reference to the definition of that name, with no sibling keyword:
    /// a vendor may drop what stands next to a `$ref`.
    Reference(String),
    /// A value described in place.
    Value { kind: ValueKind<'a>, description: String },
}

/// The JSON type of a value answer.
enum ValueKind<'a> {
    Number,
    Boolean,
    Integer,
    /// One of these labels.
    Label(Vec<&'a str>),
}

/// The kind of the value that answers `question` in place, for every
/// question that is not a probability map.
fn value_kind(question: &Question, mode: AnswerMode) -> ValueKind<'_> {
    match (&question.kind, mode) {
        (_, AnswerMode::Probabilities) => ValueKind::Number,
        (QuestionKind::Noul { .. }, AnswerMode::Discrete) => ValueKind::Boolean,
        (QuestionKind::Score { .. }, AnswerMode::Discrete) => ValueKind::Integer,
        (QuestionKind::Choice { criteria }, AnswerMode::Discrete) => {
            ValueKind::Label(criteria.iter().map(|(label, _)| label.as_str()).collect())
        }
    }
}

/// The definition of the probability map of a choice or a score: one number
/// per label or level, each described by its criterion.
fn probability_map(question: &Question, mode: AnswerMode) -> Object<'_> {
    let properties = question
        .outcomes()
        .into_iter()
        .map(|(label, criterion)| {
            let description = Content::prompt_text(criterion).to_owned();
            (label, Property::Value { kind: ValueKind::Number, description })
        })
        .collect();
    Object {
        description: Cow::Owned(clean_docstring(&question_description(question, mode))),
        properties,
    }
}

/// Upstream's `_build_llm_output_question_description`: the instructions, and
/// in probabilities mode what the numbers mean.
fn question_description(question: &Question, mode: AnswerMode) -> String {
    let instructions = Content::prompt_text(question.instructions.as_ref());
    let meaning = match (mode, &question.kind) {
        (AnswerMode::Discrete, _) => return instructions.to_owned(),
        (AnswerMode::Probabilities, QuestionKind::Noul { .. }) => {
            "Probability that the answer is yes or the assertion is true. 0 means no or false, 0.5 means uncertain, and 1 means yes or true."
        }
        (AnswerMode::Probabilities, QuestionKind::Score { .. }) => {
            "Each property maps a rubric level to the probability that the document matches it."
        }
        (AnswerMode::Probabilities, QuestionKind::Choice { .. }) => {
            "Each property maps an option to the probability that it is the best answer."
        }
    };
    format!("{meaning}\nQuestion: {instructions}")
}

/// Upstream's `_build_llm_output_field_description`, for an answer described
/// in place: the question's description, then its criteria.
///
/// A choice or a score reaches this in discrete mode only; in probabilities
/// mode its answer is a reference without a description.
fn field_description(question: &Question, mode: AnswerMode) -> String {
    let mut description = question_description(question, mode);
    let heading = match &question.kind {
        QuestionKind::Noul { criteria: None } => return description,
        QuestionKind::Noul { criteria: Some(criteria) } => {
            description.push_str("\nTrue criteria: ");
            description.push_str(Content::prompt_text(criteria.yes.as_ref()));
            description.push_str("\nFalse criteria: ");
            description.push_str(Content::prompt_text(criteria.no.as_ref()));
            return description;
        }
        QuestionKind::Score { .. } => "\nScore levels, answer with the integer:",
        QuestionKind::Choice { .. } => "\nChoice labels, answer with one label:",
    };
    description.push_str(heading);
    for (label, criterion) in question.outcomes() {
        description.push('\n');
        description.push_str(&label);
        description.push_str(" = ");
        description.push_str(Content::prompt_text(criterion));
    }
    description
}

/// What Python's `inspect.cleandoc` makes of the description of a
/// probability map, which pydantic reads as the docstring of the map's model.
///
/// Every tab becomes spaces up to the next multiple of eight columns, the
/// column counted in characters from the last line feed or carriage return
/// (`str.expandtabs`), and the empty lines at the end are dropped. The rest of
/// `cleandoc` changes nothing here: it strips the spaces that begin the first
/// line and the indentation shared by the lines after it, and a description
/// begins with a fixed sentence followed by a line that starts `Question:`.
fn clean_docstring(text: &str) -> String {
    const TAB_STOP: usize = 8;
    let mut cleaned = String::with_capacity(text.len());
    let mut column = 0;
    for character in text.chars() {
        match character {
            '\t' => {
                cleaned.extend(std::iter::repeat_n(' ', TAB_STOP - column % TAB_STOP));
                column = 0;
            }
            '\n' | '\r' => {
                cleaned.push(character);
                column = 0;
            }
            _ => {
                cleaned.push(character);
                column += 1;
            }
        }
    }
    cleaned.truncate(cleaned.trim_end_matches('\n').len());
    cleaned
}

/// Whether pydantic sorts the members of the schema object named `name`: it
/// leaves the object under a `properties` or a `default` key as generated.
fn is_sorted(name: &str) -> bool {
    !matches!(name, "properties" | "default")
}

impl Serialize for Root<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("$defs", &Definitions(self))?;
        map.serialize_entry("additionalProperties", &false)?;
        map.serialize_entry("properties", &RootProperties)?;
        map.serialize_entry("required", &["answers"])?;
        map.serialize_entry("type", "object")?;
        map.end()
    }
}

/// The `$defs` of a schema: the probability maps, then [`ANSWERS`], whose
/// name sorts after every `ProbabilityMap`.
struct Definitions<'a>(&'a Root<'a>);

impl Serialize for Definitions<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Root { maps, answers } = self.0;
        let mut map = serializer.serialize_map(Some(maps.len() + 1))?;
        for (name, object) in maps {
            map.serialize_entry(name, object)?;
        }
        map.serialize_entry(ANSWERS, answers)?;
        map.end()
    }
}

impl Serialize for Object<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(5))?;
        map.serialize_entry("additionalProperties", &false)?;
        map.serialize_entry("description", &self.description)?;
        map.serialize_entry("properties", &Properties(&self.properties))?;
        map.serialize_entry("required", &Required(&self.properties))?;
        map.serialize_entry("type", "object")?;
        map.end()
    }
}

/// The `properties` of an object, in their own order.
struct Properties<'a>(&'a [(Cow<'a, str>, Property<'a>)]);

impl Serialize for Properties<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (name, property) in self.0 {
            map.serialize_entry(name, &Named { property, sorted: is_sorted(name) })?;
        }
        map.end()
    }
}

/// The `required` of an object: the names of its properties, in their order.
struct Required<'a>(&'a [(Cow<'a, str>, Property<'a>)]);

impl Serialize for Required<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut names = serializer.serialize_seq(Some(self.0.len()))?;
        for (name, _) in self.0 {
            names.serialize_element(name)?;
        }
        names.end()
    }
}

/// A property and whether its members are written sorted, which the name it
/// is written under decides.
struct Named<'a> {
    property: &'a Property<'a>,
    sorted: bool,
}

impl Serialize for Named<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (kind, description) = match self.property {
            Property::Reference(name) => return Reference(name).serialize(serializer),
            Property::Value { kind, description } => (kind, description),
        };
        let (labels, json_type) = match kind {
            ValueKind::Number => (None, "number"),
            ValueKind::Boolean => (None, "boolean"),
            ValueKind::Integer => (None, "integer"),
            ValueKind::Label(labels) => (Some(labels), "string"),
        };
        let mut map = serializer.serialize_map(Some(2 + usize::from(labels.is_some())))?;
        if self.sorted {
            map.serialize_entry("description", description)?;
        }
        if let Some(labels) = labels {
            map.serialize_entry("enum", labels)?;
        }
        map.serialize_entry("type", json_type)?;
        if !self.sorted {
            map.serialize_entry("description", description)?;
        }
        map.end()
    }
}

/// `{"$ref":"#/$defs/<name>"}`.
struct Reference<'a>(&'a str);

impl Serialize for Reference<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("$ref", &format_args!("#/$defs/{}", self.0))?;
        map.end()
    }
}

/// The `properties` of the root: `answers`, a reference to [`ANSWERS`].
struct RootProperties;

impl Serialize for RootProperties {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("answers", &Reference(ANSWERS))?;
        map.end()
    }
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
