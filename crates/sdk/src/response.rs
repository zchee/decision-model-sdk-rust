//! What a call answers with.
//!
//! The response type is generic in its answers from the start, so the map of
//! answers a runtime question set produces and the struct a derived one
//! produces are the same type at different parameters rather than two types
//! that drift apart.
//!
//! The received bytes are kept beside the decoded answers. An answer of a kind
//! this version does not model is dropped by the decoder and is still there in
//! the raw body, so a caller is never left with no way to read what the server
//! actually said.
//!
//! Every container keeps the order that means something: the answers and a
//! choice's probabilities are in the order the server sent them, which is the
//! order the caller asked in, while a score's legend and probabilities are
//! sorted by level, so two responses that differ only in the order of a score's
//! keys compare equal and print the same.

use std::fmt;

use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeStruct},
};

use crate::{constants::request_id, content::Content, name::Name};

// ---------------------------------------------------------------- response

/// The answers to one System One call, with the model and token usage the
/// server reported and the HTTP response they came in.
///
/// `A` is what the answers decode into: [`Answers`], a lookup by question name,
/// unless the question set was declared as a struct, in which case each answer
/// lands in that struct's field of the same name.
///
/// Serializing a response writes `model`, `usage` and `answers` only; the HTTP
/// metadata in [`meta`](SystemOneResponse::meta) is runtime state, not part of
/// the API payload.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemOneResponse<A = Answers> {
    model: Name,
    usage: Usage,
    answers: A,
    meta: ResponseMeta,
}

impl<A> SystemOneResponse<A> {
    /// Assembles a decoded response.
    pub(crate) fn from_parts(model: Name, usage: Usage, answers: A, meta: ResponseMeta) -> Self {
        Self { model, usage, answers, meta }
    }

    /// The model that answered. It may differ from the alias the request
    /// named: asking for `jev-latest` is answered by a concrete model.
    #[must_use]
    pub fn model(&self) -> &str {
        self.model.as_str()
    }

    /// The tokens the call used.
    #[must_use]
    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// The answers, one per question.
    #[must_use]
    pub fn answers(&self) -> &A {
        &self.answers
    }

    /// The status, headers and raw body of the HTTP response.
    #[must_use]
    pub fn meta(&self) -> &ResponseMeta {
        &self.meta
    }

    /// Gives up everything but the answers.
    #[must_use]
    pub fn into_answers(self) -> A {
        self.answers
    }
}

impl<A> Serialize for SystemOneResponse<A>
where
    A: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("SystemOneResponse", 3)?;
        out.serialize_field("model", &self.model)?;
        out.serialize_field("usage", &self.usage)?;
        out.serialize_field("answers", &self.answers)?;
        out.end()
    }
}

/// The HTTP side of a successful response.
///
/// The body is the exact bytes the server sent, kept as a reference-counted
/// buffer rather than a copy. It is the way back to anything the decoder left
/// out, such as an answer of a type this version does not know.
///
/// `Debug` prints the header count and the body length, not their contents: a
/// response header may carry a cookie, and the body is the caller's to log.
#[derive(Clone, PartialEq)]
pub struct ResponseMeta {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl ResponseMeta {
    /// Keeps what a response arrived with.
    pub(crate) fn new(status: StatusCode, headers: HeaderMap, body: Bytes) -> Self {
        Self { status, headers, body }
    }

    /// Hands the parts back, for a failure that has to carry them.
    pub(crate) fn into_parts(self) -> (StatusCode, HeaderMap, Bytes) {
        (self.status, self.headers, self.body)
    }

    /// The HTTP status, which is always a success status here.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The response headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// The server's identifier for the request, from the
    /// `x-typesafe-request-id` header, when the server sent one that is valid
    /// text.
    #[must_use]
    pub fn request_id(&self) -> Option<&str> {
        request_id(&self.headers)
    }

    /// The body exactly as it was received.
    #[must_use]
    pub fn raw_body(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for ResponseMeta {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResponseMeta")
            .field("status", &self.status.as_u16())
            .field("request_id", &self.request_id())
            .field("headers", &format_args!("<{} headers>", self.headers.len()))
            .field("body", &format_args!("<{} bytes>", self.body.len()))
            .finish()
    }
}

/// Token counts for a call, when the server reported them.
///
/// Both counts are optional because the API may leave either out; an absent
/// count is `None`, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize)]
pub struct Usage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl Usage {
    /// Builds a usage record, for example to stand in for a real response in a
    /// test.
    #[must_use]
    pub fn new(input_tokens: Option<u64>, output_tokens: Option<u64>) -> Self {
        Self { input_tokens, output_tokens }
    }

    /// Billable input tokens.
    #[must_use]
    pub fn input_tokens(&self) -> Option<u64> {
        self.input_tokens
    }

    /// Output tokens.
    #[must_use]
    pub fn output_tokens(&self) -> Option<u64> {
        self.output_tokens
    }
}

// ----------------------------------------------------------------- answers

/// The answers of a response, looked up by question name.
///
/// The answers are kept in the order the server sent them, which is the order
/// the questions were asked in. A lookup scans them, which for the handful of
/// questions a call carries is faster than hashing the name. Should a document
/// name one question twice - JSON allows it, the API does not do it - both
/// answers are kept and a lookup finds the first.
///
/// The typed accessors ([`nouls`](Answers::nouls) and friends) filter as they
/// iterate; nothing is copied or cached.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answers {
    entries: Vec<(Name, Answer)>,
}

impl Answers {
    /// An empty set whose storage is sized for `capacity` answers.
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self { entries: Vec::with_capacity(capacity) }
    }

    /// Appends one answer.
    pub(crate) fn push(&mut self, name: Name, answer: Answer) {
        self.entries.push((name, answer));
    }

    /// How many answers the storage holds without growing.
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.entries.capacity()
    }

    /// How many answers there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The answer to the question called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Answer> {
        self.entries.iter().find(|(key, _)| key.as_str() == name).map(|(_, answer)| answer)
    }

    /// The answer to the yes/no question called `name`, if there is one and it
    /// is a yes/no answer.
    #[must_use]
    pub fn noul(&self, name: &str) -> Option<&NoulAnswer> {
        self.get(name).and_then(Answer::as_noul)
    }

    /// The answer to the choice question called `name`, if there is one and it
    /// is a choice answer.
    #[must_use]
    pub fn choice(&self, name: &str) -> Option<&ChoiceAnswer> {
        self.get(name).and_then(Answer::as_choice)
    }

    /// The answer to the score question called `name`, if there is one and it
    /// is a score answer.
    #[must_use]
    pub fn score(&self, name: &str) -> Option<&ScoreAnswer> {
        self.get(name).and_then(Answer::as_score)
    }

    /// Every answer with its question name, in the order received.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Answer)> + DoubleEndedIterator {
        self.entries.iter().map(|(name, answer)| (name.as_str(), answer))
    }

    /// The question names, in the order received.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// The yes/no answers, in the order received.
    pub fn nouls(&self) -> impl DoubleEndedIterator<Item = (&str, &NoulAnswer)> {
        self.iter().filter_map(|(name, answer)| answer.as_noul().map(|noul| (name, noul)))
    }

    /// The choice answers, in the order received.
    pub fn choices(&self) -> impl DoubleEndedIterator<Item = (&str, &ChoiceAnswer)> {
        self.iter().filter_map(|(name, answer)| answer.as_choice().map(|choice| (name, choice)))
    }

    /// The score answers, in the order received.
    pub fn scores(&self) -> impl DoubleEndedIterator<Item = (&str, &ScoreAnswer)> {
        self.iter().filter_map(|(name, answer)| answer.as_score().map(|score| (name, score)))
    }
}

impl<S> FromIterator<(S, Answer)> for Answers
where
    S: Into<String>,
{
    fn from_iter<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = (S, Answer)>,
    {
        Self {
            entries: iter
                .into_iter()
                .map(|(name, answer)| (Name::from(name.into()), answer))
                .collect(),
        }
    }
}

impl Serialize for Answers {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Pairs(&self.entries).serialize(serializer)
    }
}

/// One answer, of whichever kind its question was.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Answer {
    /// The answer to a yes/no question.
    Noul(NoulAnswer),
    /// The answer to a choice question.
    Choice(ChoiceAnswer),
    /// The answer to a score question.
    Score(ScoreAnswer),
}

impl Answer {
    /// The yes/no answer, when this is one.
    #[must_use]
    pub fn as_noul(&self) -> Option<&NoulAnswer> {
        match self {
            Self::Noul(answer) => Some(answer),
            Self::Choice(_) | Self::Score(_) => None,
        }
    }

    /// The choice answer, when this is one.
    #[must_use]
    pub fn as_choice(&self) -> Option<&ChoiceAnswer> {
        match self {
            Self::Choice(answer) => Some(answer),
            Self::Noul(_) | Self::Score(_) => None,
        }
    }

    /// The score answer, when this is one.
    #[must_use]
    pub fn as_score(&self) -> Option<&ScoreAnswer> {
        match self {
            Self::Score(answer) => Some(answer),
            Self::Noul(_) | Self::Choice(_) => None,
        }
    }
}

impl From<NoulAnswer> for Answer {
    fn from(answer: NoulAnswer) -> Self {
        Self::Noul(answer)
    }
}

impl From<ChoiceAnswer> for Answer {
    fn from(answer: ChoiceAnswer) -> Self {
        Self::Choice(answer)
    }
}

impl From<ScoreAnswer> for Answer {
    fn from(answer: ScoreAnswer) -> Self {
        Self::Score(answer)
    }
}

impl Serialize for Answer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Noul(answer) => answer.serialize(serializer),
            Self::Choice(answer) => answer.serialize(serializer),
            Self::Score(answer) => answer.serialize(serializer),
        }
    }
}

/// A yes/no answer.
///
/// See the [noul primitive](https://docs.typesafe.ai/primitives/noul).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoulAnswer {
    noul: f64,
}

impl NoulAnswer {
    /// Builds an answer, for example to stand in for a real response in a
    /// test.
    #[must_use]
    pub fn new(noul: f64) -> Self {
        Self { noul }
    }

    /// The probability, from 0 to 1, that the answer is yes or the statement
    /// is true. Near 0.5 means the model is unsure.
    #[must_use]
    pub fn noul(&self) -> f64 {
        self.noul
    }
}

impl Serialize for NoulAnswer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("NoulAnswer", 2)?;
        out.serialize_field("type", "noul")?;
        out.serialize_field("noul", &self.noul)?;
        out.end()
    }
}

/// The option a choice question picked, with how likely each option was.
///
/// See the [choice primitive](https://docs.typesafe.ai/primitives/choice).
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceAnswer {
    choice: Name,
    confidence: f64,
    probabilities: Vec<(Name, f64)>,
}

impl ChoiceAnswer {
    /// Builds an answer, for example to stand in for a real response in a
    /// test. The probabilities keep the order they are given in.
    #[must_use]
    pub fn new<C, I, S>(choice: C, confidence: f64, probabilities: I) -> Self
    where
        C: Into<String>,
        I: IntoIterator<Item = (S, f64)>,
        S: Into<String>,
    {
        Self::from_parts(
            Name::from(choice.into()),
            confidence,
            probabilities
                .into_iter()
                .map(|(name, probability)| (Name::from(name.into()), probability))
                .collect(),
        )
    }

    /// Assembles a decoded answer.
    pub(crate) fn from_parts(
        choice: Name,
        confidence: f64,
        probabilities: Vec<(Name, f64)>,
    ) -> Self {
        Self { choice, confidence, probabilities }
    }

    /// The option with the highest probability.
    #[must_use]
    pub fn choice(&self) -> &str {
        self.choice.as_str()
    }

    /// How sure the model is of the pick, from 0 to 1.
    #[must_use]
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Every option with its probability, in the order received.
    pub fn probabilities(
        &self,
    ) -> impl ExactSizeIterator<Item = (&str, f64)> + DoubleEndedIterator {
        self.probabilities.iter().map(|(name, probability)| (name.as_str(), *probability))
    }

    /// The probability of the option called `name`.
    #[must_use]
    pub fn probability(&self, name: &str) -> Option<f64> {
        self.probabilities
            .iter()
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, probability)| *probability)
    }
}

impl Serialize for ChoiceAnswer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("ChoiceAnswer", 4)?;
        out.serialize_field("type", "choice")?;
        out.serialize_field("choice", &self.choice)?;
        out.serialize_field("confidence", &self.confidence)?;
        out.serialize_field("probabilities", &Pairs(&self.probabilities))?;
        out.end()
    }
}

/// A rating on the levels a score question defined, with the level
/// descriptions it was rated against and how likely each level was.
///
/// See the [score primitive](https://docs.typesafe.ai/primitives/score).
///
/// # Storing a score
///
/// A score serializes with any serde format, but reading one back is
/// supported through a JSON codec (sonic-rs, `serde_json`) only. Through a
/// non-JSON serde format a score whose legend holds text fails to read back:
/// each description is a [`Content`], which is read as JSON text in every
/// format, and a plain description such as `low` is not JSON text.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreAnswer {
    score: f64,
    confidence: f64,
    legend: Vec<(u32, Content<'static>)>,
    probabilities: Vec<(u32, f64)>,
}

impl ScoreAnswer {
    /// Builds an answer, for example to stand in for a real response in a
    /// test. The legend and the probabilities are sorted by level.
    #[must_use]
    pub fn new<L, P>(score: f64, confidence: f64, legend: L, probabilities: P) -> Self
    where
        L: IntoIterator<Item = (u32, Content<'static>)>,
        P: IntoIterator<Item = (u32, f64)>,
    {
        let mut sorted_legend: Vec<_> = legend.into_iter().collect();
        sort_by_level(&mut sorted_legend);
        let mut sorted_probabilities: Vec<_> = probabilities.into_iter().collect();
        sort_by_level(&mut sorted_probabilities);
        Self::from_sorted(score, confidence, sorted_legend, sorted_probabilities)
    }

    /// Assembles a decoded answer from maps the decoder built with
    /// [`push_by_level`] and [`sort_by_level`].
    pub(crate) fn from_sorted(
        score: f64,
        confidence: f64,
        legend: Vec<(u32, Content<'static>)>,
        probabilities: Vec<(u32, f64)>,
    ) -> Self {
        debug_assert!(legend.is_sorted_by_key(|(level, _)| *level), "the legend is sorted");
        debug_assert!(
            probabilities.is_sorted_by_key(|(level, _)| *level),
            "the probabilities are sorted"
        );
        Self { score, confidence, legend, probabilities }
    }

    /// The expected score: the probability-weighted average of the levels. It
    /// may fall between two levels.
    #[must_use]
    pub fn score(&self) -> f64 {
        self.score
    }

    /// How sure the model is of the score, from 0 to 1.
    #[must_use]
    pub fn confidence(&self) -> f64 {
        self.confidence
    }

    /// Every level with the description it was rated against, lowest level
    /// first.
    pub fn legend(
        &self,
    ) -> impl ExactSizeIterator<Item = (u32, &Content<'static>)> + DoubleEndedIterator {
        self.legend.iter().map(|(level, description)| (*level, description))
    }

    /// The description of `level`.
    #[must_use]
    pub fn description(&self, level: u32) -> Option<&Content<'static>> {
        self.legend.binary_search_by_key(&level, |(key, _)| *key).ok().map(|at| &self.legend[at].1)
    }

    /// Every level with its probability, lowest level first.
    pub fn probabilities(&self) -> impl ExactSizeIterator<Item = (u32, f64)> + DoubleEndedIterator {
        self.probabilities.iter().copied()
    }

    /// The probability of `level`.
    #[must_use]
    pub fn probability(&self, level: u32) -> Option<f64> {
        self.probabilities
            .binary_search_by_key(&level, |(key, _)| *key)
            .ok()
            .map(|at| self.probabilities[at].1)
    }
}

impl Serialize for ScoreAnswer {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_struct("ScoreAnswer", 5)?;
        out.serialize_field("type", "score")?;
        out.serialize_field("score", &self.score)?;
        out.serialize_field("confidence", &self.confidence)?;
        out.serialize_field("legend", &Pairs(&self.legend))?;
        out.serialize_field("probabilities", &Pairs(&self.probabilities))?;
        out.end()
    }
}

/// Writes a list of pairs as a JSON object. An integer key is written as the
/// text of the integer, which is how JSON spells a score level.
struct Pairs<'a, K, V>(&'a [(K, V)]);

impl<K, V> Serialize for Pairs<'_, K, V>
where
    K: Serialize,
    V: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut out = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            out.serialize_entry(key, value)?;
        }
        out.end()
    }
}

/// Appends `value` and clears `in_order` when its level sorts before the
/// level of the entry ahead of it; a list whose `in_order` was cleared is
/// passed to [`sort_by_level`] once it is complete.
///
/// The number of levels and their order are chosen by whoever wrote the
/// response, so the list is not kept sorted while it is built: moving the
/// tail on every insert costs O(n^2) for levels that arrive in descending
/// order, where one sort at the end costs O(n log n). Levels that arrive in
/// order, as the API writes them, cost one comparison each and no sort.
pub(crate) fn push_by_level<T>(
    entries: &mut Vec<(u32, T)>,
    in_order: &mut bool,
    level: u32,
    value: T,
) {
    *in_order &= entries.last().is_none_or(|(last, _)| *last <= level);
    entries.push((level, value));
}

/// Sorts `entries` by level. The sort is stable, so a level named twice keeps
/// both entries in the order they arrived.
pub(crate) fn sort_by_level<T>(entries: &mut [(u32, T)]) {
    entries.sort_by_key(|(level, _)| *level);
}

#[cfg(test)]
#[path = "response_tests.rs"]
mod tests;
