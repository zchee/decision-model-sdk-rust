//! The selected JSON engine and its raw-value protocol.
//!
//! Both implementations provide serde serializers and deserializers, string
//! decoding, appending writes, error positions and syntax classification. The
//! deserializer exposes `from_str`, `from_slice` and `end`; its concrete reader
//! type is inferred at the constructor. Only this module names sonic-rs types.
//! The private splice token must remain paired with the selected engine.

#[cfg(feature = "sonic")]
mod selected {
    #[cfg(test)]
    pub(crate) use sonic_rs::Serializer;
    pub(crate) use sonic_rs::{Deserializer, Error, from_str, to_writer};

    /// The struct and field name that splice validated raw JSON unchanged.
    pub(crate) const SPLICE_TOKEN: &str = "$sonic_rs::LazyValue";

    /// The parser's reported line and byte column.
    pub(crate) fn error_position(error: &Error) -> (usize, usize) {
        (error.line(), error.column())
    }

    /// Whether parsing failed on syntax or an incomplete document.
    pub(crate) fn is_syntax(error: &Error) -> bool {
        matches!(
            error.classify(),
            sonic_rs::error::Category::Syntax | sonic_rs::error::Category::Eof
        )
    }
}

#[cfg(not(feature = "sonic"))]
mod selected {
    #[cfg(test)]
    pub(crate) use serde_json::Serializer;
    pub(crate) use serde_json::{Deserializer, Error, from_str, to_writer};

    /// The struct and field name that splice validated raw JSON unchanged.
    pub(crate) const SPLICE_TOKEN: &str = "$serde_json::private::RawValue";

    /// The parser's reported line and byte column.
    pub(crate) fn error_position(error: &Error) -> (usize, usize) {
        (error.line(), error.column())
    }

    /// Whether parsing failed on syntax or an incomplete document.
    pub(crate) fn is_syntax(error: &Error) -> bool {
        matches!(
            error.classify(),
            serde_json::error::Category::Syntax | serde_json::error::Category::Eof
        )
    }
}

#[cfg(test)]
pub(crate) use selected::Serializer;
pub(crate) use selected::{
    Deserializer, Error, SPLICE_TOKEN, error_position, from_str, is_syntax, to_writer,
};
