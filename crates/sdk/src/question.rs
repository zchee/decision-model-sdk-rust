//! The questions a call asks, and the shapes the API accepts them in.
//!
//! A question is a noul (how true is this?), a choice (which of these?) or a
//! score (how much, on this scale?); a raw question carries a shape this
//! version of the SDK does not model, so a new question type on the server does
//! not need a new release here.
//!
//! A question set is validated and serialized once, and the bytes are reused
//! for every call that asks it. That is what keeps the per-call cost to
//! splicing one prepared fragment into the body instead of walking a structure
//! that has not changed since the last call.
//!
//! ```
//! use typesafe_sdk::question::{Choice, Noul, Questions, Score};
//!
//! let prepared = Questions::new()
//!     .noul("billing", Noul::new().instructions("Is this about billing?"))
//!     .choice(
//!         "tone",
//!         Choice::new(["calm", "angry"])
//!             .option("calm", "neutral or polite")
//!             .instructions("What is the tone?"),
//!     )
//!     .score("urgency", Score::new(["can wait", "this week", "today"]))
//!     .prepare()?;
//!
//! assert_eq!(prepared.len(), 3);
//! assert_eq!(prepared.names().collect::<Vec<_>>(), ["billing", "tone", "urgency"]);
//! # Ok::<(), typesafe_sdk::Error>(())
//! ```

use std::{borrow::Cow, fmt, sync::Arc};

use bytes::Bytes;
use serde::Serialize;

use crate::{
    client::Client,
    codec::{self, EncodeError, RawJson},
    content::Content,
    de::AnswerSet,
    error::Error,
    request::SystemOne,
    transport::HttpService,
};

/// A yes/no question: how true is a statement about the state?
///
/// Every member is optional. `yes` and `no` describe what counts as each
/// outcome; they are sent as the `criteria` object's `true` and `false`
/// members, and that object is left off entirely when neither is set.
///
/// ```
/// use typesafe_sdk::question::Noul;
///
/// let spam = Noul::new()
///     .instructions("Is this message spam?")
///     .yes("unsolicited advertising")
///     .no("a legitimate conversation");
/// # let _ = spam;
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Noul<'a> {
    instructions: Option<Content<'a>>,
    yes: Option<Content<'a>>,
    no: Option<Content<'a>>,
}

impl<'a> Noul<'a> {
    /// A noul with no instructions and no criteria.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The question or statement to evaluate, as text or a JSON object or
    /// array. Setting it again replaces it.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Content<'a>>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// What counts as a yes answer. Setting it again replaces it.
    #[must_use]
    pub fn yes(mut self, description: impl Into<Content<'a>>) -> Self {
        self.yes = Some(description.into());
        self
    }

    /// What counts as a no answer. Setting it again replaces it.
    #[must_use]
    pub fn no(mut self, description: impl Into<Content<'a>>) -> Self {
        self.no = Some(description.into());
        self
    }
}

/// A question that picks one of a set of named options.
///
/// An option without a description is interpreted by its name alone and is
/// sent as `null`. Options are sent in the order they were first given; naming
/// an option again replaces its description but keeps its position, which is
/// what the upstream SDK's dictionary does.
///
/// ```
/// use typesafe_sdk::question::Choice;
///
/// let tone = Choice::new(["calm", "angry"])
///     .option("calm", "neutral or polite")
///     .instructions("What is the tone?");
/// # let _ = tone;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<'a> {
    instructions: Option<Content<'a>>,
    options: Vec<(Cow<'a, str>, Option<Content<'a>>)>,
}

impl<'a> Choice<'a> {
    /// A choice between `options`, none of them described yet.
    ///
    /// No option count is enforced here: the API documents its limits as
    /// subject to change, so the server is the one to judge them.
    #[must_use]
    pub fn new<I>(options: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Cow<'a, str>>,
    {
        let options = options.into_iter();
        let mut choice =
            Self { instructions: None, options: Vec::with_capacity(options.size_hint().0) };
        for name in options {
            upsert(&mut choice.options, name.into(), None);
        }
        choice
    }

    /// Adds the option `name` with a description, or describes it if it is
    /// already there.
    #[must_use]
    pub fn option(
        mut self,
        name: impl Into<Cow<'a, str>>,
        description: impl Into<Content<'a>>,
    ) -> Self {
        upsert(&mut self.options, name.into(), Some(description.into()));
        self
    }

    /// What the model should decide when choosing. Setting it again replaces
    /// it.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Content<'a>>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

/// A question that rates the state on an ordered scale.
///
/// Each level is described by text or a JSON object or array, and its
/// position is its score, starting at zero. A score with no levels is rejected
/// by [`Questions::prepare`].
///
/// ```
/// use typesafe_sdk::question::Score;
///
/// let urgency = Score::new(["can wait", "this week", "today"]).instructions("How urgent is it?");
/// # let _ = urgency;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Score<'a> {
    instructions: Option<Content<'a>>,
    levels: Vec<Content<'a>>,
}

impl<'a> Score<'a> {
    /// A score over `levels`, lowest first.
    #[must_use]
    pub fn new<I>(levels: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Content<'a>>,
    {
        Self { instructions: None, levels: levels.into_iter().map(Into::into).collect() }
    }

    /// What the model should rate. Setting it again replaces it.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<Content<'a>>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
}

/// A question of a type, or with fields, that this version of the SDK does
/// not model.
///
/// It is a JSON object built field by field: `type` is set by
/// [`new`](Self::new), and every [`field`](Self::field) is encoded when it is
/// given and sent unread. Setting a field again replaces its value and keeps
/// its position, `type` included.
///
/// [`Questions::prepare`] applies the checks the API's own shape makes
/// possible without knowing the type: `type` is a nonempty string, a `choice`
/// or `score` has `criteria`, and a `score`'s `criteria` is not empty.
/// Everything else is left to the server.
///
/// ```
/// use typesafe_sdk::question::RawQuestion;
///
/// let spam = RawQuestion::new("noul").field("instructions", "Spam?").field("weight", 3);
/// # let _ = spam;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawQuestion<'a> {
    fields: Vec<(Cow<'a, str>, RawJson)>,
    /// The first field that could not be encoded. It is reported by
    /// [`Questions::prepare`] rather than here, so that a question can still
    /// be built as one chain of calls.
    failure: Option<(Cow<'a, str>, EncodeError)>,
}

impl<'a> RawQuestion<'a> {
    /// A question whose `type` is `kind`.
    #[must_use]
    pub fn new(kind: &str) -> Self {
        let kind = RawJson::from_value(kind).expect("invariant: a string always encodes as JSON");
        Self { fields: vec![(Cow::Borrowed("type"), kind)], failure: None }
    }

    /// Sets the field `name` to the JSON form of `value`.
    ///
    /// A value that cannot be encoded - a map whose keys are neither strings,
    /// booleans nor numbers, or a [`Serialize`] implementation that fails - is
    /// not stored, and [`Questions::prepare`] reports it.
    #[must_use]
    pub fn field(mut self, name: impl Into<Cow<'a, str>>, value: impl Serialize) -> Self {
        let name = name.into();
        match RawJson::from_value(&value) {
            Ok(raw) => upsert(&mut self.fields, name, raw),
            Err(error) => {
                if self.failure.is_none() {
                    self.failure = Some((name, error));
                }
            }
        }
        self
    }

    /// The encoded value of the field `name`, if it is set.
    fn get(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

/// Any one question, for code that builds a question set from data.
///
/// [`Questions`] has a method per kind; this is what
/// [`Questions::question`] takes, and every question type converts into it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Question<'a> {
    /// A yes/no question.
    Noul(Noul<'a>),
    /// A question that picks one option.
    Choice(Choice<'a>),
    /// A question that rates on a scale.
    Score(Score<'a>),
    /// A question this version of the SDK does not model.
    Raw(RawQuestion<'a>),
}

impl<'a> From<Noul<'a>> for Question<'a> {
    fn from(question: Noul<'a>) -> Self {
        Self::Noul(question)
    }
}

impl<'a> From<Choice<'a>> for Question<'a> {
    fn from(question: Choice<'a>) -> Self {
        Self::Choice(question)
    }
}

impl<'a> From<Score<'a>> for Question<'a> {
    fn from(question: Score<'a>) -> Self {
        Self::Score(question)
    }
}

impl<'a> From<RawQuestion<'a>> for Question<'a> {
    fn from(question: RawQuestion<'a>) -> Self {
        Self::Raw(question)
    }
}

/// The questions of one call, keyed by the names their answers come back
/// under.
///
/// The order questions are added in is the order they are sent in. Adding a
/// name that is already there replaces that question and keeps its position,
/// as the upstream SDK's dictionary does.
///
/// Nothing is checked until [`prepare`](Self::prepare), which validates and
/// serializes the whole set once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Questions<'a> {
    entries: Vec<(Cow<'a, str>, Question<'a>)>,
}

impl<'a> Questions<'a> {
    /// An empty question set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a question of any kind under `name`.
    #[must_use]
    pub fn question(
        mut self,
        name: impl Into<Cow<'a, str>>,
        question: impl Into<Question<'a>>,
    ) -> Self {
        upsert(&mut self.entries, name.into(), question.into());
        self
    }

    /// Adds a yes/no question under `name`.
    #[must_use]
    pub fn noul(self, name: impl Into<Cow<'a, str>>, question: Noul<'a>) -> Self {
        self.question(name, question)
    }

    /// Adds a choice question under `name`.
    #[must_use]
    pub fn choice(self, name: impl Into<Cow<'a, str>>, question: Choice<'a>) -> Self {
        self.question(name, question)
    }

    /// Adds a score question under `name`.
    #[must_use]
    pub fn score(self, name: impl Into<Cow<'a, str>>, question: Score<'a>) -> Self {
        self.question(name, question)
    }

    /// Adds a raw question under `name`.
    #[must_use]
    pub fn raw(self, name: impl Into<Cow<'a, str>>, question: RawQuestion<'a>) -> Self {
        self.question(name, question)
    }

    /// Validates the set and serializes it into the bytes every call will
    /// send.
    ///
    /// # Errors
    ///
    /// Returns an [`ErrorKind::InvalidRequest`](crate::ErrorKind::InvalidRequest)
    /// error, with the upstream SDK's message, when the set is empty, when a
    /// score has no levels, when a raw question's `type` is not a nonempty
    /// string, when a raw `choice` or `score` has no `criteria`, when a raw
    /// `score`'s `criteria` is empty, or when a raw question's field could not
    /// be encoded. The first failing question, in order, is the one reported.
    pub fn prepare(self) -> Result<PreparedQuestions, Error> {
        if self.entries.is_empty() {
            return Err(Error::invalid_request("At least one question is required."));
        }
        for (name, question) in &self.entries {
            validate(name, question)?;
        }

        // The codec reserves room for the worst-case escaping of every string
        // before it writes it, so the buffer is sized for that worst case up
        // front and never grows while it is written; it is cut down to the
        // bytes actually written once at the end.
        // Per question: the escaped key, the separator and colon, the question,
        // and the unescaped copy of the name that follows the JSON.
        let bound = 2 + self
            .entries
            .iter()
            .map(|(name, question)| string_bound(name.len()) + 2 + bound_of(question) + name.len())
            .sum::<usize>();
        let mut buf = Vec::with_capacity(bound);
        buf.push(b'{');
        for (index, (name, question)) in self.entries.iter().enumerate() {
            if index > 0 {
                buf.push(b',');
            }
            codec::write_json_string(&mut buf, name);
            buf.push(b':');
            write_question(&mut buf, question);
        }
        buf.push(b'}');
        let json_len = buf.len();

        // The names follow the JSON unescaped, so that they can be handed out
        // as `&str` without a separate allocation per name.
        let mut end = json_len;
        let name_ends = self
            .entries
            .iter()
            .map(|(name, _)| {
                end += name.len();
                end
            })
            .collect::<Arc<[usize]>>();
        for (name, _) in &self.entries {
            buf.extend_from_slice(name.as_bytes());
        }

        let max_levels = self
            .entries
            .iter()
            .map(|(_, question)| match question {
                Question::Score(score) => score.levels.len(),
                _ => 0,
            })
            .max()
            .unwrap_or(0);
        Ok(PreparedQuestions {
            buf: Bytes::from(buf.into_boxed_slice()),
            json_len,
            name_ends: NameEnds::Shared(name_ends),
            max_levels,
        })
    }
}

/// A validated question set, serialized once.
///
/// Cloning it copies a reference count, not the bytes, and it can be shared
/// between threads and reused by any number of calls.
#[derive(Clone)]
pub struct PreparedQuestions {
    /// The JSON object sent as `questions`, then every name, unescaped, back
    /// to back.
    buf: Bytes,
    /// Where the JSON object ends and the first name begins.
    json_len: usize,
    /// Where each name ends in `buf`; each name starts where the one before
    /// it ends.
    name_ends: NameEnds,
    /// The most levels any score question has, or 0 when not known (a set
    /// compiled into the program, or scores given as raw questions only). A
    /// sizing hint for decoding the answers, never sent.
    max_levels: usize,
}

/// Two sets are equal when their bytes are, however each was made; the
/// sizing hint is not part of the set.
impl PartialEq for PreparedQuestions {
    fn eq(&self, other: &Self) -> bool {
        self.buf == other.buf
            && self.json_len == other.json_len
            && self.name_ends == other.name_ends
    }
}

impl Eq for PreparedQuestions {}

/// The name ends of a prepared set: allocated once by [`Questions::prepare`],
/// or compiled into the program for a [`QuestionSet`].
#[derive(Clone)]
enum NameEnds {
    Shared(Arc<[usize]>),
    Static(&'static [usize]),
}

impl NameEnds {
    fn as_slice(&self) -> &[usize] {
        match self {
            Self::Shared(ends) => ends,
            Self::Static(ends) => ends,
        }
    }
}

/// Two sets are equal when their bytes are, however each was made.
impl PartialEq for NameEnds {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for NameEnds {}

impl PreparedQuestions {
    /// A set whose bytes were produced at compile time; what the code that
    /// `#[derive(QuestionSet)]` generates calls. No semver promise.
    ///
    /// `buf` holds the JSON object in its first `json_len` bytes and then
    /// every name, unescaped and back to back; `name_ends` holds where each
    /// name ends. Nothing is allocated, now or when the set is used. The
    /// layout is checked here, and since the generated code calls this in a
    /// `static`, a layout that does not hold stops the build rather than a
    /// running program.
    ///
    /// # Panics
    ///
    /// When `name_ends` is empty, when the JSON or a name would end past the
    /// end of `buf`, when a name would end before it starts, when the last
    /// name does not end where `buf` does, or when a boundary falls inside a
    /// UTF-8 character.
    #[doc(hidden)]
    #[must_use]
    pub const fn from_static(
        buf: &'static str,
        json_len: usize,
        name_ends: &'static [usize],
    ) -> Self {
        assert!(!name_ends.is_empty(), "a question set has at least one question");
        assert!(json_len <= buf.len(), "the JSON must end within the buffer");
        assert!(buf.is_char_boundary(json_len), "the JSON must end on a character boundary");
        let mut start = json_len;
        // `for` loops and iterators are not available in a `const fn`.
        let mut index = 0;
        while index < name_ends.len() {
            let end = name_ends[index];
            assert!(start <= end, "a name must not end before it starts");
            assert!(end <= buf.len(), "a name must end within the buffer");
            assert!(buf.is_char_boundary(end), "a name must end on a character boundary");
            start = end;
            index += 1;
        }
        assert!(start == buf.len(), "the last name must end where the buffer does");
        Self {
            buf: Bytes::from_static(buf.as_bytes()),
            json_len,
            name_ends: NameEnds::Static(name_ends),
            max_levels: 0,
        }
    }

    /// The number of questions in the set. It is never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.name_ends.as_slice().len()
    }

    /// Always `false`: an empty set is rejected by [`Questions::prepare`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name_ends.as_slice().is_empty()
    }

    /// The question names, in the order they are sent.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> + DoubleEndedIterator + '_ {
        let ends = self.name_ends.as_slice();
        (0..ends.len()).map(|index| {
            let start = index.checked_sub(1).map_or(self.json_len, |previous| ends[previous]);
            std::str::from_utf8(&self.buf[start..ends[index]])
                .expect("invariant: the names were copied from `str`s")
        })
    }

    /// The most levels any score question of the set has, or 0 when not
    /// known.
    pub(crate) fn max_levels(&self) -> usize {
        self.max_levels
    }

    /// The JSON object that goes after `"questions":` in a request body.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.json_len]
    }

    /// The JSON object as text.
    fn json(&self) -> &str {
        std::str::from_utf8(&self.buf[..self.json_len]).expect("invariant: the codec emits UTF-8")
    }
}

impl fmt::Debug for PreparedQuestions {
    /// Prints the JSON the set is sent as: questions are not secrets, and the
    /// wire form is the one thing worth seeing.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("PreparedQuestions").field("json", &self.json()).finish()
    }
}

/// A question set declared as a type: it knows the questions it asks, and its
/// answers decode into it.
///
/// `#[derive(QuestionSet)]` implements it for a struct with one field per
/// question, serializing the questions when the program is compiled; see the
/// derive for the attributes it reads. [`Client::ask`] sends one:
///
/// ```
/// # #[cfg(feature = "macros")]
/// # fn main() -> Result<(), typesafe_sdk::Error> {
/// use typesafe_sdk::{ChoiceAnswer, Choice, NoulAnswer, Noul, Questions, QuestionSet};
///
/// #[derive(QuestionSet)]
/// struct Ticket {
///     #[noul(instructions = "Is this about billing?")]
///     billing: NoulAnswer,
///     #[choice(options("calm", "angry"))]
///     tone: ChoiceAnswer,
/// }
///
/// // The same questions, built at run time, are the same bytes.
/// let built = Questions::new()
///     .noul("billing", Noul::new().instructions("Is this about billing?"))
///     .choice("tone", Choice::new(["calm", "angry"]))
///     .prepare()?;
/// assert_eq!(Ticket::prepared(), &built);
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "macros"))]
/// # fn main() {}
/// ```
///
/// Implementing it by hand takes a `'static` set and an [`AnswerSet`]
/// implementation that follows that trait's contract.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a question set",
    label = "no questions are declared for this type",
    note = "derive it: `#[derive(typesafe_sdk::QuestionSet)]` on a struct with one \
            `NoulAnswer`, `ChoiceAnswer` or `ScoreAnswer` field per question"
)]
pub trait QuestionSet: AnswerSet {
    /// The questions, validated and serialized once for the whole program.
    fn prepared() -> &'static PreparedQuestions;
}

/// Asking a [`QuestionSet`].
impl<S> Client<S>
where
    S: HttpService,
{
    /// A System One request that asks the questions of `Q` about `state` and
    /// decodes the answers into a `Q`.
    ///
    /// It is [`system_one`](Client::system_one) with `Q`'s prepared questions,
    /// made [`typed`](SystemOne::typed) as `Q`: the same builder, configured
    /// and sent the same way.
    ///
    /// The state's type is not a type parameter of this method, so that
    /// `ask::<Ticket>(&state)` names only the question set. The price is that
    /// the state's type is opaque in the returned builder's type: a function
    /// that takes the builder takes it as a generic. Where the type must be
    /// named,
    /// `client.system_one(&state, Ticket::prepared()).typed::<Ticket>()` is
    /// the same request with the state's own type.
    ///
    /// ```
    /// # #[cfg(feature = "macros")]
    /// # fn main() -> Result<(), typesafe_sdk::Error> {
    /// use std::time::Duration;
    ///
    /// use typesafe_sdk::{Client, NoulAnswer, QuestionSet};
    ///
    /// #[derive(QuestionSet)]
    /// struct Spam {
    ///     #[noul(instructions = "Is this message spam?")]
    ///     spam: NoulAnswer,
    /// }
    ///
    /// let client = Client::builder().api_key("your-api-key").build()?;
    /// let request = client.ask::<Spam>("Buy now!").timeout(Duration::from_secs(2));
    /// // `request.send().await?` needs a Tokio runtime and returns a
    /// // `SystemOneResponse<Spam>`, whose `answers().spam` is the answer.
    /// drop(request);
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "macros"))]
    /// # fn main() {}
    /// ```
    pub fn ask<'a, Q>(
        &'a self,
        state: &'a (impl Serialize + ?Sized),
    ) -> SystemOne<'a, S, impl Serialize + ?Sized, Q>
    where
        Q: QuestionSet,
    {
        self.system_one(state, Q::prepared()).typed::<Q>()
    }
}

/// Inserts `value` under `name`, or replaces the value already there without
/// moving it: the semantics of a Python `dict`, which is what the upstream SDK
/// holds questions, options and raw fields in.
///
/// The lookup is linear. These lists are the options, questions, members or
/// headers of one request, a handful to a few hundred entries, where a scan of
/// a `Vec` is cheaper than hashing and keeps the insertion order for free.
pub(crate) fn upsert<K: PartialEq, V>(entries: &mut Vec<(K, V)>, name: K, value: V) {
    match entries.iter_mut().find(|(key, _)| *key == name) {
        Some((_, slot)) => *slot = value,
        None => entries.push((name, value)),
    }
}

/// Applies the upstream SDK's checks (`_core/questions.py`) to one question.
fn validate(name: &str, question: &Question<'_>) -> Result<(), Error> {
    match question {
        Question::Noul(_) | Question::Choice(_) => Ok(()),
        Question::Score(score) if score.levels.is_empty() => Err(no_criteria(name)),
        Question::Score(_) => Ok(()),
        Question::Raw(raw) => {
            if let Some((field, error)) = &raw.failure {
                return Err(Error::invalid_request(format!(
                    "Question \"{name}\" field \"{field}\": {error}"
                )));
            }
            let Some(kind) = raw.get("type").and_then(string_value).filter(|kind| !kind.is_empty())
            else {
                return Err(Error::invalid_request(format!(
                    "Question \"{name}\" must be a question object or a dictionary with a nonempty string \"type\"."
                )));
            };
            if kind != "choice" && kind != "score" {
                return Ok(());
            }
            let Some(criteria) = raw.get("criteria") else {
                return Err(Error::invalid_request(format!(
                    "Question \"{name}\" requires \"criteria\"."
                )));
            };
            if kind == "score" && is_falsy(criteria) {
                return Err(no_criteria(name));
            }
            Ok(())
        }
    }
}

fn no_criteria(name: &str) -> Error {
    Error::invalid_request(format!(
        "Score question \"{name}\" has no criteria; at least one score is required."
    ))
}

/// The value of an encoded JSON string, or `None` when the fragment is not a
/// string.
///
/// A fragment without a backslash is borrowed. One with an escape is decoded,
/// so that a `type` spelled `"score"` by a spliced [`RawJson`] is still
/// recognized as `score`, as it would be once the server has parsed it.
fn string_value(fragment: &str) -> Option<Cow<'_, str>> {
    let text = fragment.trim_ascii();
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    if inner.contains('\\') {
        codec::decode::<String>(text.as_bytes()).ok().map(Cow::Owned)
    } else {
        Some(Cow::Borrowed(inner))
    }
}

/// Whether an encoded JSON value is one Python treats as false: `null`,
/// `false`, a zero, `""`, `[]` or `{}`.
///
/// Upstream rejects a raw score whose `criteria` is any of these (`if not
/// criteria`). The check reads the first and last bytes, and for a number its
/// mantissa digits; it never parses the value.
fn is_falsy(fragment: &str) -> bool {
    let text = fragment.trim_ascii().as_bytes();
    match text {
        b"null" | b"false" | b"\"\"" => true,
        [b'[', inner @ .., b']'] | [b'{', inner @ .., b'}'] => inner.trim_ascii().is_empty(),
        [b'-' | b'0'..=b'9', ..] => text
            .iter()
            .take_while(|byte| !matches!(byte, b'e' | b'E'))
            .all(|byte| matches!(byte, b'-' | b'0' | b'.')),
        _ => false,
    }
}

/// An upper bound on the bytes [`write_question`] needs, counting the room
/// the codec reserves before each string it writes (`6 * len + 35`).
fn bound_of(question: &Question<'_>) -> usize {
    // Quotes, colon and comma around a member, and its longest fixed name.
    const MEMBER: usize = 18;
    let string = string_bound;
    let content = |content: &Content<'_>| {
        MEMBER
            + match content.as_text() {
                Some(text) => string(text.len()),
                None => content.as_json().map_or(0, |raw| raw.as_str().len()),
            }
    };
    let optional = |value: &Option<Content<'_>>| value.as_ref().map_or(0, content);
    let fixed = 64; // braces, the type member and the criteria member
    fixed
        + match question {
            Question::Noul(noul) => {
                optional(&noul.instructions) + optional(&noul.yes) + optional(&noul.no)
            }
            Question::Choice(choice) => {
                optional(&choice.instructions)
                    + choice
                        .options
                        .iter()
                        .map(|(name, description)| {
                            string(name.len()) + description.as_ref().map_or(4 + MEMBER, content)
                        })
                        .sum::<usize>()
            }
            Question::Score(score) => {
                optional(&score.instructions) + score.levels.iter().map(content).sum::<usize>()
            }
            Question::Raw(raw) => raw
                .fields
                .iter()
                .map(|(name, value)| string(name.len()) + value.as_str().len() + MEMBER)
                .sum(),
        }
}

/// The room the codec reserves before writing a string of `len` bytes: its
/// worst-case escaping (`\u00XX`, six bytes per input byte) plus a margin.
fn string_bound(len: usize) -> usize {
    6 * len + 35
}

/// Writes one question object.
///
/// Members are written in the order of the upstream wire models: `type`,
/// `instructions`, `criteria`. Optional members that are unset are left out
/// rather than written as `null`, as upstream does.
fn write_question(buf: &mut Vec<u8>, question: &Question<'_>) {
    match question {
        Question::Noul(noul) => {
            buf.extend_from_slice(br#"{"type":"noul""#);
            write_instructions(buf, noul.instructions.as_ref());
            if noul.yes.is_some() || noul.no.is_some() {
                buf.extend_from_slice(br#","criteria":{"#);
                let mut first = true;
                for (key, value) in
                    [(&br#""true":"#[..], &noul.yes), (&br#""false":"#[..], &noul.no)]
                {
                    if let Some(value) = value {
                        if !first {
                            buf.push(b',');
                        }
                        first = false;
                        buf.extend_from_slice(key);
                        write_content(buf, value);
                    }
                }
                buf.push(b'}');
            }
            buf.push(b'}');
        }
        Question::Choice(choice) => {
            buf.extend_from_slice(br#"{"type":"choice""#);
            write_instructions(buf, choice.instructions.as_ref());
            buf.extend_from_slice(br#","criteria":{"#);
            for (index, (name, description)) in choice.options.iter().enumerate() {
                if index > 0 {
                    buf.push(b',');
                }
                codec::write_json_string(buf, name);
                buf.push(b':');
                match description {
                    Some(description) => write_content(buf, description),
                    None => buf.extend_from_slice(b"null"),
                }
            }
            buf.extend_from_slice(b"}}");
        }
        Question::Score(score) => {
            buf.extend_from_slice(br#"{"type":"score""#);
            write_instructions(buf, score.instructions.as_ref());
            buf.extend_from_slice(br#","criteria":["#);
            for (index, level) in score.levels.iter().enumerate() {
                if index > 0 {
                    buf.push(b',');
                }
                write_content(buf, level);
            }
            buf.extend_from_slice(b"]}");
        }
        Question::Raw(raw) => {
            buf.push(b'{');
            for (index, (name, value)) in raw.fields.iter().enumerate() {
                if index > 0 {
                    buf.push(b',');
                }
                codec::write_json_string(buf, name);
                buf.push(b':');
                buf.extend_from_slice(value.as_str().as_bytes());
            }
            buf.push(b'}');
        }
    }
}

fn write_instructions(buf: &mut Vec<u8>, instructions: Option<&Content<'_>>) {
    if let Some(instructions) = instructions {
        buf.extend_from_slice(br#","instructions":"#);
        write_content(buf, instructions);
    }
}

/// Writes text as a JSON string and raw JSON as the text it holds.
///
/// Raw JSON is copied rather than serialized: it is already one valid JSON
/// value, and copying it skips the codec entirely.
fn write_content(buf: &mut Vec<u8>, content: &Content<'_>) {
    match content.as_text() {
        Some(text) => codec::write_json_string(buf, text),
        None => {
            let raw = content.as_json().expect("invariant: content that is not text is raw JSON");
            buf.extend_from_slice(raw.as_str().as_bytes());
        }
    }
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
