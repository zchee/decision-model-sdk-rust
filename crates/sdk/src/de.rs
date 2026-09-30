//! Reading an answer set in one pass.
//!
//! The decoder walks the wire format directly instead of building a value and
//! then interpreting it: an answer's kind is known from the member that names
//! it, so the visitor that reads it can be chosen before its contents are
//! parsed, and a score's integer level keys become integers without a string
//! ever existing.
//!
//! The field path an error reports is built as the walk descends, which is why
//! a failure deep in the answers can name the field it failed at rather than
//! the object that contained it.
//!
//! JSON objects are unordered, so an answer's `type` may also arrive after the
//! members it governs. Those members are then held as the raw text they
//! arrived as - a slice of the response body, not a copy - and parsed once the
//! type is known. Reading them eagerly instead would fail the whole response
//! whenever an answer of a future type happened to reuse a member name with a
//! different shape, which is exactly the answer this decoder promises to skip.
//!
//! [`AnswerSet`] is the seam between this walk and what the answers decode
//! into: [`Answers`] reads them into a lookup by name, and a question set
//! declared as a struct reads each answer straight into its field.

use std::{borrow::Cow, fmt, marker::PhantomData};

use bytes::Bytes;
use http::{HeaderMap, Method, StatusCode, Uri};
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};

use crate::{
    codec::{self, DecodeError},
    content::Content,
    error::{Error, ResponseValidationError, format_endpoint},
    name::Name,
    response::{
        Answer, Answers, ChoiceAnswer, NoulAnswer, ResponseMeta, ScoreAnswer, SystemOneResponse,
        Usage, push_by_level, sort_by_level,
    },
};

// ------------------------------------------------------------- the seam

/// What the decoder knows about the answers before it reads the first one.
///
/// Everything in it is a hint for sizing storage. An implementation may use it
/// or ignore it - a struct with one field per question has nothing to size -
/// and it never changes what is decoded or whether decoding succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct AnswerContext {
    // Both counts are `u32`, not `usize`, saturating: they are capacity hints,
    // and the context is carried twice in every call's future, where wider
    // fields push that future over tokio's debug-build box size.
    expected_answers: u32,
    /// The most levels any score question of the request has, or 0 when
    /// unknown: the capacity a score's level lists start at, since the codec
    /// gives no size hint for an object.
    levels: u32,
}

impl AnswerContext {
    /// A context for a request that asked `expected_answers` questions.
    pub(crate) fn new(expected_answers: usize) -> Self {
        Self { expected_answers: saturate(expected_answers), levels: 0 }
    }

    /// The same, for a request whose largest score question has `levels`
    /// levels, held to at most [`MAX_LEVEL_HINT`].
    pub(crate) fn with_levels(self, levels: usize) -> Self {
        Self { levels: saturate(levels.min(MAX_LEVEL_HINT)), ..self }
    }

    /// The level hint, as a capacity.
    fn levels(self) -> usize {
        self.levels as usize
    }

    /// How many questions the request asked, and so how many answers a
    /// complete response carries, capped at the number of answers the body is
    /// long enough to hold. Zero when unknown.
    ///
    /// It is a capacity hint: a response may carry fewer answers or more.
    #[must_use]
    pub fn expected_answers(&self) -> usize {
        self.expected_answers as usize
    }
}

/// The largest capacity a score's first level list starts at.
///
/// The hint is the largest score the request asked, but the server decides
/// how many answers come back and how many levels each carries, so an
/// unbounded hint lets a response multiply its size in memory. The hint
/// exists to save the one growth a list of 5 to 8 levels pays after starting
/// at 4, so 8 keeps all of that saving; a longer list grows from 8 as it would
/// without a hint.
const MAX_LEVEL_HINT: usize = 8;

/// The largest capacity a choice's probability list starts at from a
/// deserializer's size hint.
///
/// The answer types deserialize from any serde format, so that a caller can
/// store answers and read them back; a self-describing binary format such as
/// MessagePack or CBOR reports a map's declared length as its hint, and that
/// length is chosen by the input. Trusting it would let a few bytes request
/// an arbitrary allocation or overflow `Vec`'s capacity. A choice has a
/// handful of options, and a longer list grows as it would without a hint,
/// as serde's own collections do past their cap.
const MAX_OPTION_HINT: usize = 8;

/// `count` as a `u32`, or `u32::MAX` when it does not fit.
fn saturate(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// A type the `answers` object of a response decodes into.
///
/// [`Answers`] implements it as a lookup by question name. A question set
/// declared as a struct implements it by reading each answer into the field of
/// the same name, which needs no map and no name string at all: the struct's
/// visitor matches the key and hands the value to [`NoulAnswer`],
/// [`ChoiceAnswer`] or [`ScoreAnswer`], whose `Deserialize` implementations
/// are the same single-pass readers [`Answers`] uses, fixed to one kind.
///
/// # Contract
///
/// Every implementation, written by hand or generated, keeps these rules;
/// [`Answers`] and the struct example below keep them, and the tests of this
/// module hold both to them.
///
/// * **Input.** The deserializer yields exactly one JSON object, keyed by
///   question name. Anything else (an array, a string, `null`) is an error at
///   `answers`.
/// * **Order.** The members of that object may arrive in any order, and inside
///   one answer `type` may arrive after the members it governs. What a
///   successful decode yields does not depend on either order, except which of
///   two answers with one name is kept: the first in wire order, as the
///   repeated-answer rule says. Which path a failure names can depend on
///   order, as the next two rules say.
/// * **Wrong kind.** An answer whose `type` is not the kind the field holds -
///   including an answer that is not an object at all, or has no `type` - is
///   an error at `answers.<field>.type` whenever `type` comes before the
///   members of the field's kind, which is the order the API writes. A typed
///   field knows its kind before `type` arrives and reads those members as
///   they come, so when a misshaped member of the field's kind precedes a
///   wrong `type`, the error is reported at that member,
///   `answers.<field>.<member>`.
/// * **Wrong shape.** A member of the right kind with the wrong shape is an
///   error at `answers.<field>.<member>` when the kind is known as the member
///   arrives: always for a typed field, and for [`Answers`] when `type` came
///   first. [`Answers`] holds a member that arrives before `type` as raw text
///   and checks it once the type is known, after the walk has left it, so it
///   reports that failure at `answers.<field>`.
/// * **Two types.** An answer that names `type` twice with two different
///   values is an error at `answers.<field>.type`, whichever members it
///   carries; naming the same type twice is accepted. (Upstream lets the last
///   `type` win; an answer that contradicts itself is refused here instead.)
/// * **Repeated answer.** When the object names one question twice, the first
///   answer is the one the set holds. A struct keeps its field's first answer
///   and skips a later answer of the same name unread, as it skips an extra
///   answer, so the later one's kind and shape do not matter. [`Answers`]
///   keeps every answer it reads, in wire order, and every lookup returns the
///   first of them; it reads a later answer like any other, so one of the
///   wrong shape is still an error there. A body both accept gives both the
///   same answer.
/// * **Missing answer.** A field with no answer is an error at
///   `answers.<field>`, where `<field>` is the question's wire name (what
///   `missing_field` receives), not the Rust field's identifier. A response
///   with no `answers` member at all is an error at `answers` for every set
///   that cannot be empty: the method is then called with an empty object,
///   and whatever it fails with is reported as the missing member. A set that
///   can be empty, as [`Answers`] can, decodes to its empty value.
/// * **Extra answers.** An answer the type has no field for is skipped unread,
///   whatever its kind or shape, and is never an error. It stays in the raw
///   body. [`Answers`] keeps every answer of a kind this version models and
///   skips the others the same way.
/// * **Allocation.** Nothing is allocated beyond the storage of the fields
///   themselves: keys are matched where they lie, never copied into a
///   `String`, and no intermediate map or value tree is built.
/// * **Context.** The [`AnswerContext`] is a sizing hint. An implementation may
///   use it or ignore it, and the result is the same either way.
///
/// A type that does not implement the trait is refused where a response of it
/// is asked for:
///
/// ```compile_fail,E0277
/// use typesafe_sdk::de::AnswerSet;
///
/// fn decode_into<A: AnswerSet>() {}
///
/// decode_into::<String>();
/// ```
///
/// An implementation for a struct of three answers looks like this. The key
/// is matched by a field identifier whose visitor only compares the text, so
/// a key written with escapes works and no key is copied:
///
/// ```
/// use std::fmt;
///
/// use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, Visitor};
/// use typesafe_sdk::{
///     de::{AnswerContext, AnswerSet},
///     response::{ChoiceAnswer, NoulAnswer, ScoreAnswer},
/// };
///
/// struct Ticket {
///     spam: NoulAnswer,
///     tone: ChoiceAnswer,
///     quality: ScoreAnswer,
/// }
///
/// enum Field {
///     Spam,
///     Tone,
///     Quality,
///     Other,
/// }
/// # impl<'de> Deserialize<'de> for Field {
/// #     fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
/// #         struct FieldVisitor;
/// #         impl Visitor<'_> for FieldVisitor {
/// #             type Value = Field;
/// #             fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
/// #                 formatter.write_str("a question name")
/// #             }
/// #             fn visit_str<E: de::Error>(self, value: &str) -> Result<Field, E> {
/// #                 Ok(match value {
/// #                     "spam" => Field::Spam,
/// #                     "tone" => Field::Tone,
/// #                     "quality" => Field::Quality,
/// #                     _ => Field::Other,
/// #                 })
/// #             }
/// #         }
/// #         deserializer.deserialize_str(FieldVisitor)
/// #     }
/// # }
///
/// impl AnswerSet for Ticket {
///     fn deserialize_answers<'de, D>(deserializer: D, _: AnswerContext) -> Result<Self, D::Error>
///     where
///         D: Deserializer<'de>,
///     {
///         struct TicketVisitor;
///
///         impl<'de> Visitor<'de> for TicketVisitor {
///             type Value = Ticket;
///
///             fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
///                 formatter.write_str("the answers of a Ticket")
///             }
///
///             fn visit_map<M>(self, mut map: M) -> Result<Ticket, M::Error>
///             where
///                 M: MapAccess<'de>,
///             {
///                 let (mut spam, mut tone, mut quality) = (None, None, None);
///                 while let Some(field) = map.next_key::<Field>()? {
///                     match field {
///                         Field::Spam if spam.is_none() => spam = Some(map.next_value()?),
///                         Field::Tone if tone.is_none() => tone = Some(map.next_value()?),
///                         Field::Quality if quality.is_none() => {
///                             quality = Some(map.next_value()?);
///                         }
///                         // An answer the struct has no field for, or a
///                         // later answer to a question already read: the
///                         // first answer of a name is the one kept.
///                         _ => {
///                             map.next_value::<IgnoredAny>()?;
///                         }
///                     }
///                 }
///                 Ok(Ticket {
///                     spam: spam.ok_or_else(|| de::Error::missing_field("spam"))?,
///                     tone: tone.ok_or_else(|| de::Error::missing_field("tone"))?,
///                     quality: quality.ok_or_else(|| de::Error::missing_field("quality"))?,
///                 })
///             }
///         }
///
///         deserializer.deserialize_map(TicketVisitor)
///     }
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot be decoded as the answers of a response",
    label = "not a set of answers",
    note = "use `Answers` to look answers up by question name, or declare a struct with one \
            field per question and `#[derive(QuestionSet)]` it (the `macros` feature, on by \
            default), which implements `AnswerSet`"
)]
pub trait AnswerSet: Sized {
    /// Reads the `answers` object of a response.
    ///
    /// When a response carries no `answers` member at all, this is called with
    /// a deserializer of an empty object. An implementation that holds
    /// required answers fails there in the usual way, and the decoder reports
    /// that failure as the missing `answers` member.
    ///
    /// # Errors
    ///
    /// Returns the deserializer's error when an answer is missing, is of the
    /// wrong kind, or does not have the shape its kind requires.
    fn deserialize_answers<'de, D>(
        deserializer: D,
        context: AnswerContext,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>;
}

impl AnswerSet for Answers {
    fn deserialize_answers<'de, D>(
        deserializer: D,
        context: AnswerContext,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(AnswersVisitor {
            capacity: context.expected_answers(),
            levels: context.levels(),
        })
    }
}

impl<'de> Deserialize<'de> for Answers {
    /// Reads answers with no expectation about their number. Answers of a type
    /// this version does not model are skipped, as they are in a response.
    ///
    /// Reloading is supported through a JSON codec (sonic-rs, `serde_json`);
    /// see [`ScoreAnswer`] for what a non-JSON serde format cannot read back.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::deserialize_answers(deserializer, AnswerContext::default())
    }
}

/// Reads the answers object into [`Answers`], in wire order.
struct AnswersVisitor {
    capacity: usize,
    levels: usize,
}

impl<'de> Visitor<'de> for AnswersVisitor {
    type Value = Answers;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of question name to answer")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Answers, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut answers = Answers::with_capacity(self.capacity);
        while let Some(name) = map.next_key_seed(TextSeed)? {
            // The name is copied only once the answer is known to be kept, so
            // an answer that is skipped costs no allocation for its name.
            let seed = AnswerSeed::<Option<Answer>> {
                name: &name,
                levels: self.levels,
                target: PhantomData,
            };
            if let Some(answer) = map.next_value_seed(seed)? {
                answers.push(Name::from(name), answer);
            }
        }
        Ok(answers)
    }
}

// ------------------------------------------------------------ one answer

/// The three answer kinds this version models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Noul,
    Choice,
    Score,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Noul => "noul",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }
}

/// What an answer's `type` member said.
enum Seen<'de> {
    Known(Kind),
    /// A type this version does not model, kept only to be named in a
    /// warning. It borrows from the body unless it was written with escapes.
    Unknown(Cow<'de, str>),
}

/// What one answer object decodes into, and how.
///
/// The runtime set reads any kind and skips unknown ones; the typed answers
/// each accept exactly one kind. Both share the walk in [`AnswerSeed`] and
/// differ only in what they build from what it collected, which is why the
/// errors a typed field reports have the same paths as the runtime set's.
trait Target: Sized {
    /// The kind the answer must be, or `None` to accept any.
    const EXPECTED: Option<Kind>;

    /// Builds the value once the whole answer object has been read.
    fn build<'de, E>(seen: Option<Seen<'de>>, members: Members<'de>, name: &str) -> Result<Self, E>
    where
        E: de::Error;
}

impl Target for Option<Answer> {
    const EXPECTED: Option<Kind> = None;

    fn build<'de, E>(seen: Option<Seen<'de>>, members: Members<'de>, name: &str) -> Result<Self, E>
    where
        E: de::Error,
    {
        match seen {
            Some(Seen::Known(Kind::Noul)) => members.noul().map(|answer| Some(answer.into())),
            Some(Seen::Known(Kind::Choice)) => members.choice().map(|answer| Some(answer.into())),
            Some(Seen::Known(Kind::Score)) => members.score().map(|answer| Some(answer.into())),
            Some(Seen::Unknown(kind)) => {
                // Both names are the server's text - the answer's key and its
                // `type` - so both are escaped and cut before they reach a log
                // line; the answer's members are not logged at all.
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    target: crate::telemetry::TARGET,
                    question = %crate::telemetry::ServerName(name),
                    answer_type = %crate::telemetry::ServerName(&kind),
                    "ignoring an answer of a type this version does not model; \
                     the raw body still carries it"
                );
                #[cfg(not(feature = "tracing"))]
                let _ = (name, kind);
                Ok(None)
            }
            None => Err(E::missing_field("type")),
        }
    }
}

impl Target for NoulAnswer {
    const EXPECTED: Option<Kind> = Some(Kind::Noul);

    fn build<'de, E>(seen: Option<Seen<'de>>, members: Members<'de>, _: &str) -> Result<Self, E>
    where
        E: de::Error,
    {
        match seen {
            Some(_) => members.noul(),
            None => Err(E::missing_field("type")),
        }
    }
}

impl Target for ChoiceAnswer {
    const EXPECTED: Option<Kind> = Some(Kind::Choice);

    fn build<'de, E>(seen: Option<Seen<'de>>, members: Members<'de>, _: &str) -> Result<Self, E>
    where
        E: de::Error,
    {
        match seen {
            Some(_) => members.choice(),
            None => Err(E::missing_field("type")),
        }
    }
}

impl Target for ScoreAnswer {
    const EXPECTED: Option<Kind> = Some(Kind::Score);

    fn build<'de, E>(seen: Option<Seen<'de>>, members: Members<'de>, _: &str) -> Result<Self, E>
    where
        E: de::Error,
    {
        match seen {
            Some(_) => members.score(),
            None => Err(E::missing_field("type")),
        }
    }
}

impl<'de> Deserialize<'de> for NoulAnswer {
    /// Reads a yes/no answer object. Its `type` must be `noul`.
    ///
    /// Reloading is supported through a JSON codec (sonic-rs, `serde_json`);
    /// see [`ScoreAnswer`] for what a non-JSON serde format cannot read back.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AnswerSeed::<Self>::DETACHED)
    }
}

impl<'de> Deserialize<'de> for ChoiceAnswer {
    /// Reads a choice answer object. Its `type` must be `choice`.
    ///
    /// Reloading is supported through a JSON codec (sonic-rs, `serde_json`);
    /// see [`ScoreAnswer`] for what a non-JSON serde format cannot read back.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AnswerSeed::<Self>::DETACHED)
    }
}

impl<'de> Deserialize<'de> for ScoreAnswer {
    /// Reads a score answer object. Its `type` must be `score`.
    ///
    /// Reloading is supported through a JSON codec (sonic-rs, `serde_json`);
    /// see [`ScoreAnswer`] for what a non-JSON serde format cannot read back.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(AnswerSeed::<Self>::DETACHED)
    }
}

impl<'de> Deserialize<'de> for Answer {
    /// Reads an answer of any kind this version models. An answer of another
    /// type is an error here: unlike a set of answers, a single answer has
    /// nothing to fall back to.
    ///
    /// Reloading is supported through a JSON codec (sonic-rs, `serde_json`);
    /// see [`ScoreAnswer`] for what a non-JSON serde format cannot read back.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer
            .deserialize_any(AnswerSeed::<Option<Answer>>::DETACHED)?
            .ok_or_else(|| de::Error::custom("an answer of a type this version does not model"))
    }
}

/// Reads one answer object into `T`.
///
/// It is both the seed handed to the map that holds the answer and the visitor
/// of the answer object itself.
struct AnswerSeed<'n, T> {
    /// The question name, for the warning an unknown type raises.
    name: &'n str,
    /// See [`AnswerContext`]'s field of the same name.
    levels: usize,
    target: PhantomData<T>,
}

impl<T> AnswerSeed<'static, T> {
    /// A seed for an answer read on its own, outside a response: no question
    /// name to warn with and no level hint.
    const DETACHED: Self = Self { name: "", levels: 0, target: PhantomData };
}

impl<'de, T> DeserializeSeed<'de> for AnswerSeed<'_, T>
where
    T: Target,
{
    type Value = T;

    fn deserialize<D>(self, deserializer: D) -> Result<T, D::Error>
    where
        D: Deserializer<'de>,
    {
        // `deserialize_any` rather than `deserialize_map`, so that a value
        // that is not an object at all reaches this visitor and can be
        // reported the way the API's own validation reports it: as an answer
        // without a type.
        deserializer.deserialize_any(self)
    }
}

impl<'de, T> Visitor<'de> for AnswerSeed<'_, T>
where
    T: Target,
{
    type Value = T;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an answer object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<T, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut seen: Option<Seen<'de>> = None;
        let mut members = Members { levels: self.levels, ..Members::default() };

        while let Some(index) = map.next_key_seed(Member::NAMES)? {
            let member = match index {
                Some(0) => {
                    let seed = KindSeed { expected: T::EXPECTED, previous: seen.as_ref() };
                    seen = Some(map.next_value_seed(seed)?);
                    continue;
                }
                Some(at) => Member::DATA.get(at - 1).copied(),
                None => None,
            };
            let Some(member) = member else {
                map.next_value::<IgnoredAny>()?;
                continue;
            };
            // How a data member is read depends on what is known of the kind
            // at the moment it arrives. A typed answer knows its kind from the
            // start; a runtime one learns it from `type`, and until then holds
            // the member's raw text.
            let known = match &seen {
                Some(Seen::Known(kind)) => Some(*kind),
                Some(Seen::Unknown(_)) => None,
                None => T::EXPECTED,
            };
            match known {
                Some(kind) if member.belongs_to(kind) => members.read(member, kind, &mut map)?,
                None if seen.is_none() => members.hold(member, map.next_value_seed(RawSeed)?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        T::build(seen, members, self.name)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_unit<E: de::Error>(self) -> Result<T, E> {
        Err(E::missing_field("type"))
    }

    fn visit_seq<S>(self, _: S) -> Result<T, S::Error>
    where
        S: SeqAccess<'de>,
    {
        Err(de::Error::missing_field("type"))
    }
}

/// The data members of an answer object this version reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    Noul,
    Choice,
    Confidence,
    Score,
    Legend,
    Probabilities,
}

impl Member {
    /// The member names this version reads: `type` first, then the data
    /// members in the order of [`DATA`](Member::DATA).
    const NAMES: KeyIn =
        KeyIn(&["type", "noul", "choice", "confidence", "score", "legend", "probabilities"]);
    const DATA: [Self; 6] = [
        Self::Noul,
        Self::Choice,
        Self::Confidence,
        Self::Score,
        Self::Legend,
        Self::Probabilities,
    ];

    fn belongs_to(self, kind: Kind) -> bool {
        match self {
            Self::Noul => kind == Kind::Noul,
            Self::Choice => kind == Kind::Choice,
            Self::Confidence | Self::Probabilities => kind != Kind::Noul,
            Self::Score | Self::Legend => kind == Kind::Score,
        }
    }
}

/// Reads an answer's `type`, refusing any other kind when one is expected, and
/// any other type than the one the answer already named.
struct KindSeed<'s, 'de> {
    expected: Option<Kind>,
    /// What an earlier `type` member of the same answer said, if one did.
    previous: Option<&'s Seen<'de>>,
}

impl<'de> DeserializeSeed<'de> for KindSeed<'_, 'de> {
    type Value = Seen<'de>;

    fn deserialize<D>(self, deserializer: D) -> Result<Seen<'de>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl<'de> KindSeed<'_, 'de> {
    /// Classifies `text`, copying it only for a type this version does not
    /// know, where it is kept to be named in a warning.
    ///
    /// A second `type` that says something else is refused here, while the
    /// walk is on the member, so the error names `type`. Without this the last
    /// one would win, as it does upstream, and an answer that contradicts
    /// itself would be read as whichever kind it named last.
    fn classify<E>(self, text: &str, keep: impl FnOnce() -> Cow<'de, str>) -> Result<Seen<'de>, E>
    where
        E: de::Error,
    {
        let seen = match text {
            "noul" => Seen::Known(Kind::Noul),
            "choice" => Seen::Known(Kind::Choice),
            "score" => Seen::Known(Kind::Score),
            _ => Seen::Unknown(keep()),
        };
        match (self.expected, &seen) {
            (Some(expected), Seen::Known(kind)) if *kind != expected => {
                return Err(wrong_kind(expected));
            }
            (Some(expected), Seen::Unknown(_)) => return Err(wrong_kind(expected)),
            _ => {}
        }
        match self.previous {
            Some(previous) if !previous.is_same(&seen) => Err(mixed_types()),
            _ => Ok(seen),
        }
    }
}

/// The error for an answer of another kind than the one a field holds.
fn wrong_kind<E: de::Error>(expected: Kind) -> E {
    E::custom(format_args!("expected an answer of type `{}`", expected.name()))
}

impl Seen<'_> {
    /// Whether two `type` members name the same type.
    fn is_same(&self, other: &Seen<'_>) -> bool {
        match (self, other) {
            (Seen::Known(left), Seen::Known(right)) => left == right,
            (Seen::Unknown(left), Seen::Unknown(right)) => left == right,
            _ => false,
        }
    }
}

impl<'de> Visitor<'de> for KindSeed<'_, 'de> {
    type Value = Seen<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an answer type name")
    }

    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Seen<'de>, E> {
        self.classify(value, || Cow::Borrowed(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Seen<'de>, E> {
        self.classify(value, || Cow::Owned(value.to_owned()))
    }
}

// ---------------------------------------------------- collected members

/// The data members of one answer object, in whatever state they arrived.
#[derive(Default)]
struct Members<'de> {
    noul: Slot<'de, f64>,
    choice: Slot<'de, Name>,
    confidence: Slot<'de, f64>,
    score: Slot<'de, f64>,
    legend: Slot<'de, Vec<(u32, Content<'static>)>>,
    probabilities: Probabilities<'de>,
    /// The capacity a score's first level list starts at.
    levels: usize,
}

/// One data member: absent, read, or held as raw text until the answer's type
/// is known.
#[derive(Default)]
enum Slot<'de, T> {
    #[default]
    Missing,
    Read(T),
    Raw(Cow<'de, str>),
}

/// `probabilities` is keyed by option name for a choice and by level for a
/// score, so which reading it gets depends on the kind.
#[derive(Default)]
enum Probabilities<'de> {
    #[default]
    Missing,
    Named(Vec<(Name, f64)>),
    Levels(Vec<(u32, f64)>),
    Raw(Cow<'de, str>),
}

impl<'de> Members<'de> {
    /// Reads `member` in place, as a member of an answer of `kind`.
    fn read<M>(&mut self, member: Member, kind: Kind, map: &mut M) -> Result<(), M::Error>
    where
        M: MapAccess<'de>,
    {
        match member {
            Member::Noul => self.noul = Slot::Read(map.next_value()?),
            Member::Choice => self.choice = Slot::Read(map.next_value()?),
            Member::Confidence => self.confidence = Slot::Read(map.next_value()?),
            Member::Score => self.score = Slot::Read(map.next_value()?),
            // A score's legend and probabilities have one entry per level, so
            // whichever of the two arrives second is sized from the first. The
            // first is sized from the request's level hint, never from
            // `map.size_hint()`: that counts the answer object's remaining
            // members, not levels, and a binary format reports whatever
            // length its input declares, so a few bytes could ask for
            // gigabytes.
            Member::Legend => {
                let capacity = match &self.probabilities {
                    Probabilities::Levels(levels) => levels.len(),
                    _ => self.levels,
                };
                self.legend = Slot::Read(map.next_value_seed(LegendSeed { capacity })?);
            }
            Member::Probabilities if kind == Kind::Score => {
                let capacity = match &self.legend {
                    Slot::Read(legend) => legend.len(),
                    _ => self.levels,
                };
                self.probabilities =
                    Probabilities::Levels(map.next_value_seed(LevelsSeed { capacity })?);
            }
            Member::Probabilities => {
                self.probabilities = Probabilities::Named(map.next_value_seed(NamedSeed)?);
            }
        }
        Ok(())
    }

    /// Keeps the raw text of `member` for when the type is known.
    fn hold(&mut self, member: Member, raw: Cow<'de, str>) {
        match member {
            Member::Noul => self.noul = Slot::Raw(raw),
            Member::Choice => self.choice = Slot::Raw(raw),
            Member::Confidence => self.confidence = Slot::Raw(raw),
            Member::Score => self.score = Slot::Raw(raw),
            Member::Legend => self.legend = Slot::Raw(raw),
            Member::Probabilities => self.probabilities = Probabilities::Raw(raw),
        }
    }

    // The members are checked in the order the API schema declares them, so
    // that an answer missing several reports the one the API's own validation
    // would report first.

    fn noul<E: de::Error>(self) -> Result<NoulAnswer, E> {
        Ok(NoulAnswer::new(self.noul.resolve::<f64, E>("noul")?))
    }

    fn choice<E: de::Error>(self) -> Result<ChoiceAnswer, E> {
        let choice = self.choice.resolve::<Name, E>("choice")?;
        let confidence = self.confidence.resolve::<f64, E>("confidence")?;
        let probabilities = match self.probabilities {
            Probabilities::Named(named) => named,
            Probabilities::Raw(raw) => decode_held::<NamedProbabilities, E>(&raw)?.0,
            Probabilities::Missing => return Err(E::missing_field("probabilities")),
            Probabilities::Levels(_) => return Err(mixed_types()),
        };
        Ok(ChoiceAnswer::from_parts(choice, confidence, probabilities))
    }

    fn score<E: de::Error>(self) -> Result<ScoreAnswer, E> {
        let score = self.score.resolve::<f64, E>("score")?;
        let confidence = self.confidence.resolve::<f64, E>("confidence")?;
        let legend = self.legend.resolve::<Legend, E>("legend")?;
        let probabilities = match self.probabilities {
            Probabilities::Levels(levels) => levels,
            Probabilities::Raw(raw) => decode_held::<LevelProbabilities, E>(&raw)?.0,
            Probabilities::Missing => return Err(E::missing_field("probabilities")),
            Probabilities::Named(_) => return Err(mixed_types()),
        };
        Ok(ScoreAnswer::from_sorted(score, confidence, legend, probabilities))
    }
}

/// The error for an answer that names two different types.
///
/// [`KindSeed`] raises it at the second `type`. The two arms of the builders
/// above that raise it too - probabilities read under one kind and built as
/// another - cannot be reached past that check; they are there because the
/// match over what was collected has to cover every state.
fn mixed_types<E: de::Error>() -> E {
    E::custom("the answer names two different types")
}

impl<T> Slot<'_, T> {
    /// The member's value, parsing held text as `W`.
    fn resolve<W, E>(self, member: &'static str) -> Result<T, E>
    where
        W: DeserializeOwned + Into<T>,
        E: de::Error,
    {
        match self {
            Self::Read(value) => Ok(value),
            Self::Raw(raw) => decode_held::<W, E>(&raw).map(Into::into),
            Self::Missing => Err(E::missing_field(member)),
        }
    }
}

/// Parses a member held as raw text.
///
/// The text is a complete JSON value the codec already accepted once, so the
/// only way this fails is a value of the wrong shape. The failure is reported
/// at the answer rather than at the member, because the walk has left the
/// member by the time the type that says what shape it needs is known.
fn decode_held<W, E>(raw: &str) -> Result<W, E>
where
    W: DeserializeOwned,
    E: de::Error,
{
    codec::decode(raw.as_bytes()).map_err(E::custom)
}

/// Captures a member's value as the raw JSON text it arrived as.
struct RawSeed;

impl<'de> DeserializeSeed<'de> for RawSeed {
    type Value = Cow<'de, str>;

    fn deserialize<D>(self, deserializer: D) -> Result<Cow<'de, str>, D::Error>
    where
        D: Deserializer<'de>,
    {
        codec::deserialize_raw(deserializer)
    }
}

// ------------------------------------------------------------ containers

/// Matches an object key against a fixed list of names and yields the index
/// of the one it is, without keeping the key's text: a key written with
/// escapes costs nothing, and one that matches nothing is `None`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeyIn(pub(crate) &'static [&'static str]);

impl<'de> DeserializeSeed<'de> for KeyIn {
    type Value = Option<usize>;

    fn deserialize<D>(self, deserializer: D) -> Result<Option<usize>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for KeyIn {
    type Value = Option<usize>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Option<usize>, E> {
        Ok(self.0.iter().position(|name| *name == value))
    }
}

/// Reads a JSON string, borrowing it from the body when it has no escapes.
struct TextSeed;

impl<'de> DeserializeSeed<'de> for TextSeed {
    type Value = Cow<'de, str>;

    fn deserialize<D>(self, deserializer: D) -> Result<Cow<'de, str>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for TextSeed {
    type Value = Cow<'de, str>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Cow<'de, str>, E> {
        Ok(Cow::Borrowed(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Cow<'de, str>, E> {
        Ok(Cow::Owned(value.to_owned()))
    }
}

/// A score level, read straight out of the text of an object key.
///
/// The key is parsed where it lies, so no string is built for it. A codec
/// that hands object keys over as numbers reaches `visit_u64` instead.
struct LevelSeed;

impl<'de> DeserializeSeed<'de> for LevelSeed {
    type Value = u32;

    fn deserialize<D>(self, deserializer: D) -> Result<u32, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(self)
    }
}

impl Visitor<'_> for LevelSeed {
    type Value = u32;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a score level")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<u32, E> {
        // The message does not quote the key. The key still reaches the error
        // as the last name of its field path, which the codec renders with
        // control and format characters escaped and its length capped.
        value.parse().map_err(|_| E::custom("a score level is a non-negative integer"))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<u32, E> {
        u32::try_from(value).map_err(|_| E::custom("a score level is a non-negative integer"))
    }
}

/// A choice's probabilities, keyed by option name, in wire order.
struct NamedSeed;

impl<'de> DeserializeSeed<'de> for NamedSeed {
    type Value = Vec<(Name, f64)>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for NamedSeed {
    type Value = Vec<(Name, f64)>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of option name to probability")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        // The JSON codec gives no hint; a binary format gives the length its
        // input declares, which is only trusted up to a few entries.
        let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0).min(MAX_OPTION_HINT));
        while let Some(name) = map.next_key_seed(TextSeed)? {
            let probability = map.next_value()?;
            entries.push((Name::from(name), probability));
        }
        Ok(entries)
    }
}

/// A score's probabilities, keyed by level, sorted by level.
struct LevelsSeed {
    capacity: usize,
}

impl<'de> DeserializeSeed<'de> for LevelsSeed {
    type Value = Vec<(u32, f64)>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for LevelsSeed {
    type Value = Vec<(u32, f64)>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of score level to probability")
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        by_level(map, self.capacity, |map| map.next_value())
    }
}

/// A score's legend, keyed by level, sorted by level.
struct LegendSeed {
    capacity: usize,
}

impl<'de> DeserializeSeed<'de> for LegendSeed {
    type Value = Vec<(u32, Content<'static>)>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for LegendSeed {
    type Value = Vec<(u32, Content<'static>)>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of score level to description")
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        // The description borrows the body while it is read and is copied
        // once, here, because a response outlives nothing it could borrow.
        by_level(map, self.capacity, |map| {
            map.next_value::<Content<'de>>().map(Content::into_owned)
        })
    }
}

/// Reads an object keyed by score level into its entries, sorted by level,
/// each value read by `value`.
///
/// `capacity` is reserved only once a first entry exists, so an empty `{}`
/// allocates nothing whatever the hint. The first key is read ahead of the
/// loop rather than tested for inside it, which keeps the loop itself as it
/// is without a hint.
fn by_level<'de, M, V>(
    mut map: M,
    capacity: usize,
    mut value: impl FnMut(&mut M) -> Result<V, M::Error>,
) -> Result<Vec<(u32, V)>, M::Error>
where
    M: MapAccess<'de>,
{
    let Some(mut level) = map.next_key_seed(LevelSeed)? else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::with_capacity(capacity);
    let mut in_order = true;
    loop {
        let read = value(&mut map)?;
        push_by_level(&mut entries, &mut in_order, level, read);
        match map.next_key_seed(LevelSeed)? {
            Some(next) => level = next,
            None => break,
        }
    }
    if !in_order {
        sort_by_level(&mut entries);
    }
    Ok(entries)
}

/// The owned forms of the three containers, for a member that was held as raw
/// text and is parsed on its own.
struct Legend(Vec<(u32, Content<'static>)>);
struct NamedProbabilities(Vec<(Name, f64)>);
struct LevelProbabilities(Vec<(u32, f64)>);

impl From<Legend> for Vec<(u32, Content<'static>)> {
    fn from(legend: Legend) -> Self {
        legend.0
    }
}

impl<'de> Deserialize<'de> for Legend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        LegendSeed { capacity: 0 }.deserialize(deserializer).map(Self)
    }
}

impl<'de> Deserialize<'de> for NamedProbabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        NamedSeed.deserialize(deserializer).map(Self)
    }
}

impl<'de> Deserialize<'de> for LevelProbabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        LevelsSeed { capacity: 0 }.deserialize(deserializer).map(Self)
    }
}

// -------------------------------------------------------------- response

impl<'de> Deserialize<'de> for Usage {
    /// Reads the token counts from an object. A missing or `null` count is
    /// `None`; members this version does not know are ignored.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(UsageVisitor)
    }
}

/// Reads `usage` as an object only. serde's derived reader would also take a
/// JSON array positionally, which the API's schema does not allow.
struct UsageVisitor;

impl<'de> Visitor<'de> for UsageVisitor {
    type Value = Usage;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of token counts")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Usage, M::Error>
    where
        M: MapAccess<'de>,
    {
        let (mut input_tokens, mut output_tokens) = (None, None);
        while let Some(index) = map.next_key_seed(KeyIn(&["input_tokens", "output_tokens"]))? {
            match index {
                Some(0) => input_tokens = map.next_value()?,
                Some(1) => output_tokens = map.next_value()?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Usage::new(input_tokens, output_tokens))
    }
}

/// The top level of a System One response.
struct Envelope<A> {
    model: Name,
    usage: Usage,
    answers: A,
}

/// Reads a System One response, handing the answer set what the decoder knows
/// about the answers before it reads them.
///
/// A seed rather than a `Deserialize` implementation, because the context is
/// per call - the number of questions this request asked - and
/// `Deserialize` has nowhere to receive it.
struct EnvelopeSeed<A> {
    context: AnswerContext,
    answers: PhantomData<fn() -> A>,
}

// Written out rather than derived: a derive would ask for `A: Clone` and
// `A: Copy`, and the seed holds no `A` to copy.
impl<A> Clone for EnvelopeSeed<A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for EnvelopeSeed<A> {}

impl<'de, A> DeserializeSeed<'de> for EnvelopeSeed<A>
where
    A: AnswerSet,
{
    type Value = Envelope<A>;

    fn deserialize<D>(self, deserializer: D) -> Result<Envelope<A>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(self)
    }
}

impl<'de, A> Visitor<'de> for EnvelopeSeed<A>
where
    A: AnswerSet,
{
    type Value = Envelope<A>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a System One response")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Envelope<A>, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut model = None;
        let mut usage = None;
        let mut answers = None;

        while let Some(index) = map.next_key_seed(KeyIn(&["model", "usage", "answers"]))? {
            match index {
                Some(0) => model = Some(map.next_value::<Name>()?),
                Some(1) => usage = Some(map.next_value::<Usage>()?),
                Some(2) => {
                    answers = Some(map.next_value_seed(AnswerSetSeed::<A> {
                        context: self.context,
                        answers: PhantomData,
                    })?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        let model = model.ok_or_else(|| de::Error::missing_field("model"))?;
        let usage = usage.ok_or_else(|| de::Error::missing_field("usage"))?;
        let answers = match answers {
            Some(answers) => answers,
            // The API always sends `answers`. Without it, the answer set
            // decides whether "no answers" is a value it can hold: `Answers`
            // is then empty, as the Python SDK's default makes it. A set that
            // requires answers fails, and it fails at `answers` - whatever
            // the set would have named, the member that is not there is the
            // one to report, and a set of any shape reports the same path.
            None => A::deserialize_answers(
                de::value::MapDeserializer::<_, M::Error>::new(std::iter::empty::<(&str, &str)>()),
                self.context,
            )
            .map_err(|_| de::Error::missing_field("answers"))?,
        };
        Ok(Envelope { model, usage, answers })
    }
}

/// Hands the `answers` member to the answer set's own reader.
struct AnswerSetSeed<A> {
    context: AnswerContext,
    answers: PhantomData<A>,
}

impl<'de, A> DeserializeSeed<'de> for AnswerSetSeed<A>
where
    A: AnswerSet,
{
    type Value = A;

    fn deserialize<D>(self, deserializer: D) -> Result<A, D::Error>
    where
        D: Deserializer<'de>,
    {
        A::deserialize_answers(deserializer, self.context)
    }
}

/// The fewest bytes one answer that an answer set keeps can take in a body:
/// `"":{"type":"noul","noul":0}`, an empty name and the shortest answer of the
/// shortest kind, without even the comma that separates it from the next.
///
/// A body of `n` bytes therefore holds at most `n / MIN_KEPT_ANSWER_BYTES`
/// answers, which is what bounds the storage sized from a question count.
const MIN_KEPT_ANSWER_BYTES: usize = r#""":{"type":"noul","noul":0}"#.len();

/// Decodes the body of a successful System One response.
///
/// `asked` carries what the request knows about its answers: the number of
/// questions, which sizes the answer storage once instead of growing it, and
/// the largest score's level count. The count is capped by how many answers
/// the body can hold, so no count - however large - reserves storage for
/// answers that cannot be there. `endpoint` names the request in the error,
/// and is formatted only when there is one.
///
/// # Errors
///
/// Returns [`ErrorKind::ResponseValidation`](crate::ErrorKind::ResponseValidation)
/// carrying the status, the headers, the whole body and the decode failure,
/// whose path names the field that did not fit.
pub(crate) fn decode_system_one_with<A>(
    body: Bytes,
    status: StatusCode,
    headers: HeaderMap,
    asked: AnswerContext,
    endpoint: Option<(&Method, &Uri)>,
) -> Result<SystemOneResponse<A>, Error>
where
    A: AnswerSet,
{
    let expected = asked.expected_answers().min(body.len() / MIN_KEPT_ANSWER_BYTES);
    // The level hint was bounded where it entered; only the count is capped
    // here.
    let context = AnswerContext { expected_answers: saturate(expected), ..asked };
    let meta = ResponseMeta::new(status, headers, body);
    let decoded =
        codec::decode_seed(meta.raw_body(), EnvelopeSeed::<A> { context, answers: PhantomData });
    match decoded {
        Ok(Envelope { model, usage, answers }) => {
            Ok(SystemOneResponse::from_parts(model, usage, answers, meta))
        }
        Err(source) => Err(invalid_response(meta, endpoint, source)),
    }
}

/// The error for a success response whose body did not decode.
pub(crate) fn invalid_response(
    meta: ResponseMeta,
    endpoint: Option<(&Method, &Uri)>,
    source: DecodeError,
) -> Error {
    let (status, headers, body) = meta.into_parts();
    let endpoint = endpoint.map(|(method, uri)| format_endpoint(method, uri).into_boxed_str());
    ResponseValidationError::new(status, body, headers, endpoint, source).into()
}

#[cfg(test)]
#[path = "de_tests.rs"]
mod tests;
