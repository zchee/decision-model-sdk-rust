//! The owned text of a name a response carries: the model that answered, each
//! answer's question name, a choice's pick and its option names.
//!
//! These names are short, and a `String` costs one heap block per name.
//! [`Name`] keeps a name of up to 24 bytes (12 on a 32-bit target: the size
//! of a `String`) inside the value itself and allocates only for a longer one,
//! while every accessor still hands out `&str`.
//!
//! The representation is the `compact_str` crate's: it needs `unsafe`, which
//! this crate forbids, and this module is the only one that names it. Its
//! `serde` feature is not used: a name is decoded and serialized here, as a
//! plain JSON string.

use std::{borrow::Cow, fmt};

use compact_str::CompactString;
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Visitor},
};

/// A name owned by a response, stored inline when it is short.
///
/// It prints, compares and serializes exactly as a `String` does.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Name(CompactString);

impl Name {
    /// The name as text.
    pub(crate) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for Name {
    fn from(text: &str) -> Self {
        Self(CompactString::from(text))
    }
}

/// A long `String` hands over its heap buffer instead of being copied; a short
/// one is moved inline and its buffer freed.
impl From<String> for Name {
    fn from(text: String) -> Self {
        Self(CompactString::from(text))
    }
}

/// Text the decoder borrowed from the body is copied; text it had to unescape
/// into a `String` is taken over as that `String` is.
impl From<Cow<'_, str>> for Name {
    fn from(text: Cow<'_, str>) -> Self {
        match text {
            Cow::Borrowed(text) => Self::from(text),
            Cow::Owned(text) => Self::from(text),
        }
    }
}

/// Prints the text quoted and escaped, as `String`'s `Debug` does, so a
/// response's `Debug` output does not show how its names are stored.
impl fmt::Debug for Name {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), formatter)
    }
}

impl Serialize for Name {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Name {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_str(NameVisitor)
    }
}

/// Builds a [`Name`] straight from the text the codec hands over, borrowed or
/// unescaped, without an intermediate `String`.
struct NameVisitor;

impl Visitor<'_> for NameVisitor {
    type Value = Name;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Name, E> {
        Ok(Name::from(value))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Name, E> {
        Ok(Name::from(value))
    }
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
