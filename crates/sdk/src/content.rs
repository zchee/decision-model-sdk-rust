//! The value the API calls "string, object or array" content.
//!
//! [`Content`] is what carries free-form JSON in both directions: a question's
//! instructions and criteria on the way out, a score legend's labels on the way
//! back. The API accepts exactly three JSON shapes there - a string, an object
//! or an array - so a number, a boolean or `null` is rejected where the value
//! is built rather than where the server answers.
//!
//! A borrowed string is held as a borrow. Anything else is held as the JSON
//! text it was encoded from, so a shape this version does not know still
//! round-trips unchanged.

use std::borrow::Cow;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

use crate::codec::{self, DecodeError, EncodeError, RawJson};

/// A value could not be used as [`Content`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ContentError {
    /// The value encoded to a number, a boolean or `null`. The API takes a
    /// string, an object or an array.
    #[error("content must be a JSON string, object or array")]
    Shape,
    /// The value could not be encoded as JSON at all.
    #[error(transparent)]
    Encode(#[from] EncodeError),
    /// A string arrived with escape sequences that do not decode.
    #[error(transparent)]
    Decode(#[from] DecodeError),
}

/// Text, or a JSON object or array.
///
/// The lifetime is the input the value borrows from: `Content<'static>` owns
/// everything it holds, which is what a decoded response produces, while a
/// request can be built from a `&str` the caller already has without copying
/// it.
///
/// Text is written out as a JSON string by any serializer. A JSON object or
/// array is spliced in byte for byte by the SDK and written out as data by
/// every other serializer, which is described on [`RawJson`].
///
/// Reading a `Content` back always takes the value as JSON text. A JSON codec
/// (sonic-rs, `serde_json`) hands over that text, so any `Content` round-trips
/// through one; a non-JSON serde format hands over a text value as a bare
/// string, which is then parsed as JSON, so plain text such as `low` fails to
/// read back there.
///
/// ```
/// use typesafe_sdk::Content;
///
/// let borrowed = Content::text("payments or invoices");
/// assert_eq!(borrowed.as_text(), Some("payments or invoices"));
///
/// let structured = Content::json(&[1, 2, 3])?;
/// assert_eq!(structured.as_json().map(|raw| raw.as_str()), Some("[1,2,3]"));
/// # Ok::<(), typesafe_sdk::ContentError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content<'a> {
    repr: Repr<'a>,
}

/// The two shapes a [`Content`] can take, kept private so that the variants
/// are not part of the public API.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Repr<'a> {
    /// A JSON string, held decoded: the escapes are gone and the quotes with
    /// them.
    Text(Cow<'a, str>),
    /// A JSON object or array, held as the text it arrived as.
    Json(RawJson),
}

impl<'a> Content<'a> {
    /// Wraps text, borrowing it when the caller owns it elsewhere.
    ///
    /// A caller who wants the JSON shape of a value calls [`Content::json`].
    #[must_use]
    pub fn text(text: impl Into<Cow<'a, str>>) -> Self {
        Self { repr: Repr::Text(text.into()) }
    }

    /// Encodes `value` and keeps the result.
    ///
    /// A value that encodes to a JSON string becomes text, so
    /// `Content::json(&"hello")` and `Content::text("hello")` are equal.
    ///
    /// # Errors
    ///
    /// Returns [`ContentError::Shape`] when the value encodes to a number, a
    /// boolean or `null`, and [`ContentError::Encode`] when it cannot be
    /// encoded at all.
    pub fn json<T>(value: &T) -> Result<Self, ContentError>
    where
        T: Serialize + ?Sized,
    {
        let mut buffer = Vec::new();
        codec::encode_into(&mut buffer, value)?;
        let text = String::from_utf8(buffer).expect("invariant: the codec emits UTF-8");
        Self::from_raw(Cow::Owned(text))
    }

    /// The text, when this is a string.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match &self.repr {
            Repr::Text(text) => Some(text),
            Repr::Json(_) => None,
        }
    }

    /// The raw JSON, when this is an object or an array.
    #[must_use]
    pub fn as_json(&self) -> Option<&RawJson> {
        match &self.repr {
            Repr::Text(_) => None,
            Repr::Json(raw) => Some(raw),
        }
    }

    /// Detaches the value from whatever it borrows, copying only if it has to.
    #[must_use]
    pub fn into_owned(self) -> Content<'static> {
        Content {
            repr: match self.repr {
                Repr::Text(text) => Repr::Text(Cow::Owned(text.into_owned())),
                Repr::Json(raw) => Repr::Json(raw),
            },
        }
    }

    /// Builds a value from the raw JSON text of one value.
    ///
    /// The text has already been validated by the codec, either by encoding it
    /// or by parsing it out of a document.
    fn from_raw(raw: Cow<'a, str>) -> Result<Self, ContentError> {
        match raw.as_bytes().first() {
            Some(b'"') => match raw {
                Cow::Borrowed(text) => Ok(Self::text(unquote(text)?)),
                Cow::Owned(text) => Ok(Self::text(unquote(&text)?.into_owned())),
            },
            Some(b'{' | b'[') => {
                Ok(Self { repr: Repr::Json(RawJson::from_text(raw.into_owned())) })
            }
            _ => Err(ContentError::Shape),
        }
    }
}

/// Builds the text shape, borrowing `text`; see [`Content::text`].
impl<'a> From<&'a str> for Content<'a> {
    fn from(text: &'a str) -> Self {
        Self::text(text)
    }
}

/// Builds the text shape, owning `text`; see [`Content::text`].
impl From<String> for Content<'_> {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

/// Builds the text shape, borrowing or owning as `text` does; see
/// [`Content::text`].
impl<'a> From<Cow<'a, str>> for Content<'a> {
    fn from(text: Cow<'a, str>) -> Self {
        Self::text(text)
    }
}

/// Turns the raw text of a JSON string, quotes and all, into its value.
///
/// Text without a backslash is the input minus its two quotes, so the common
/// case borrows; an escape sequence has to be decoded and therefore allocates.
fn unquote(raw: &str) -> Result<Cow<'_, str>, ContentError> {
    let inner = raw.get(1..raw.len().saturating_sub(1)).ok_or(ContentError::Shape)?;
    if inner.as_bytes().contains(&b'\\') {
        Ok(Cow::Owned(codec::decode::<String>(raw.as_bytes())?))
    } else {
        Ok(Cow::Borrowed(inner))
    }
}

impl Serialize for Content<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match &self.repr {
            Repr::Text(text) => serializer.serialize_str(text),
            Repr::Json(raw) => raw.serialize(serializer),
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for Content<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = codec::deserialize_raw(deserializer)?;
        Self::from_raw(raw).map_err(de::Error::custom)
    }
}

#[cfg(test)]
#[path = "content_tests.rs"]
mod tests;
