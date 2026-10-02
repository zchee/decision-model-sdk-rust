//! The selected JSON engine and its raw-value protocol.
//!
//! Both implementations provide serde serializers and deserializers, string
//! decoding, appending writes, error positions and syntax classification. The
//! deserializer exposes `from_str`, `from_slice` and `end`; its concrete reader
//! type is inferred at the constructor. Only this module names sonic-rs types.
//! The private splice token must remain paired with the selected engine.

#[cfg(feature = "sonic")]
mod selected {
    use serde::Serialize;
    #[cfg(test)]
    pub(crate) use sonic_rs::Serializer;
    pub(crate) use sonic_rs::{Deserializer, Error, from_str};

    use super::raw_value::Bridge;

    /// The struct and field name that splice validated raw JSON unchanged.
    pub(crate) const SPLICE_TOKEN: &str = "$sonic_rs::LazyValue";

    /// Appends the JSON form of `value` to `buf`.
    ///
    /// Every `serde_json::value::RawValue` in `value` is written as its text,
    /// byte for byte, as serde_json writes it; see [`Bridge`].
    pub(crate) fn to_writer<T>(buf: &mut Vec<u8>, value: &T) -> Result<(), Error>
    where
        T: Serialize + ?Sized,
    {
        let mut serializer = sonic_rs::Serializer::new(buf);
        value.serialize(Bridge(&mut serializer))
    }

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

/// serde_json's raw-value protocol, carried over into sonic-rs's.
///
/// `serde_json::value::RawValue` writes itself as a one-field struct whose
/// name and field name are both serde_json's private token, and only
/// serde_json's own serializer knows that token: sonic-rs writes such a struct
/// as an ordinary object, the token as its key and the raw text as a string.
/// [`Bridge`] passes every call through to the serializer it wraps and renames
/// that one struct and its field to [`SPLICE_TOKEN`], which sonic-rs writes as
/// the text unchanged. Neither serde nor a crate in the dependency tree offers
/// a serializer adapter that renames a struct, so this one is written out.
///
/// sonic-rs only splices when the struct name and the field name are both its
/// token, so the two are renamed together. A map key is passed on unwrapped:
/// both engines refuse a struct as a key.
#[cfg(feature = "sonic")]
mod raw_value {
    use std::fmt::Display;

    use serde::ser::{
        Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
        SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
    };

    use super::selected::SPLICE_TOKEN;

    /// The struct and field name `serde_json::value::RawValue` serializes
    /// through. serde_json does not export it; the tests that compare the two
    /// engines on a `RawValue` fail if a serde_json release changes it.
    const SERDE_JSON_TOKEN: &str = "$serde_json::private::RawValue";

    /// A serializer that forwards to `S`, with serde_json's raw-value struct
    /// renamed to sonic-rs's.
    pub(super) struct Bridge<S>(pub(super) S);

    /// A value whose own serialization goes through a [`Bridge`], so that a
    /// `RawValue` nested at any depth is renamed as well.
    struct Wrap<'a, T: ?Sized>(&'a T);

    impl<T> Serialize for Wrap<'_, T>
    where
        T: Serialize + ?Sized,
    {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            self.0.serialize(Bridge(serializer))
        }
    }

    /// A compound of the wrapped serializer whose elements go through a
    /// [`Bridge`].
    pub(super) struct Compound<C>(C);

    /// A struct of the wrapped serializer; `raw` when it is serde_json's
    /// raw-value struct, renamed.
    pub(super) struct Struct<C> {
        inner: C,
        raw: bool,
    }

    impl<S> Serializer for Bridge<S>
    where
        S: Serializer,
    {
        type Ok = S::Ok;
        type Error = S::Error;
        type SerializeSeq = Compound<S::SerializeSeq>;
        type SerializeTuple = Compound<S::SerializeTuple>;
        type SerializeTupleStruct = Compound<S::SerializeTupleStruct>;
        type SerializeTupleVariant = Compound<S::SerializeTupleVariant>;
        type SerializeMap = Compound<S::SerializeMap>;
        type SerializeStruct = Struct<S::SerializeStruct>;
        type SerializeStructVariant = Compound<S::SerializeStructVariant>;

        fn serialize_bool(self, value: bool) -> Result<S::Ok, S::Error> {
            self.0.serialize_bool(value)
        }

        fn serialize_i8(self, value: i8) -> Result<S::Ok, S::Error> {
            self.0.serialize_i8(value)
        }

        fn serialize_i16(self, value: i16) -> Result<S::Ok, S::Error> {
            self.0.serialize_i16(value)
        }

        fn serialize_i32(self, value: i32) -> Result<S::Ok, S::Error> {
            self.0.serialize_i32(value)
        }

        fn serialize_i64(self, value: i64) -> Result<S::Ok, S::Error> {
            self.0.serialize_i64(value)
        }

        fn serialize_i128(self, value: i128) -> Result<S::Ok, S::Error> {
            self.0.serialize_i128(value)
        }

        fn serialize_u8(self, value: u8) -> Result<S::Ok, S::Error> {
            self.0.serialize_u8(value)
        }

        fn serialize_u16(self, value: u16) -> Result<S::Ok, S::Error> {
            self.0.serialize_u16(value)
        }

        fn serialize_u32(self, value: u32) -> Result<S::Ok, S::Error> {
            self.0.serialize_u32(value)
        }

        fn serialize_u64(self, value: u64) -> Result<S::Ok, S::Error> {
            self.0.serialize_u64(value)
        }

        fn serialize_u128(self, value: u128) -> Result<S::Ok, S::Error> {
            self.0.serialize_u128(value)
        }

        fn serialize_f32(self, value: f32) -> Result<S::Ok, S::Error> {
            self.0.serialize_f32(value)
        }

        fn serialize_f64(self, value: f64) -> Result<S::Ok, S::Error> {
            self.0.serialize_f64(value)
        }

        fn serialize_char(self, value: char) -> Result<S::Ok, S::Error> {
            self.0.serialize_char(value)
        }

        fn serialize_str(self, value: &str) -> Result<S::Ok, S::Error> {
            self.0.serialize_str(value)
        }

        fn serialize_bytes(self, value: &[u8]) -> Result<S::Ok, S::Error> {
            self.0.serialize_bytes(value)
        }

        fn serialize_none(self) -> Result<S::Ok, S::Error> {
            self.0.serialize_none()
        }

        fn serialize_some<T>(self, value: &T) -> Result<S::Ok, S::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_some(&Wrap(value))
        }

        fn serialize_unit(self) -> Result<S::Ok, S::Error> {
            self.0.serialize_unit()
        }

        fn serialize_unit_struct(self, name: &'static str) -> Result<S::Ok, S::Error> {
            self.0.serialize_unit_struct(name)
        }

        fn serialize_unit_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
        ) -> Result<S::Ok, S::Error> {
            self.0.serialize_unit_variant(name, index, variant)
        }

        fn serialize_newtype_struct<T>(
            self,
            name: &'static str,
            value: &T,
        ) -> Result<S::Ok, S::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_newtype_struct(name, &Wrap(value))
        }

        fn serialize_newtype_variant<T>(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
            value: &T,
        ) -> Result<S::Ok, S::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_newtype_variant(name, index, variant, &Wrap(value))
        }

        fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, S::Error> {
            self.0.serialize_seq(len).map(Compound)
        }

        fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, S::Error> {
            self.0.serialize_tuple(len).map(Compound)
        }

        fn serialize_tuple_struct(
            self,
            name: &'static str,
            len: usize,
        ) -> Result<Self::SerializeTupleStruct, S::Error> {
            self.0.serialize_tuple_struct(name, len).map(Compound)
        }

        fn serialize_tuple_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Result<Self::SerializeTupleVariant, S::Error> {
            self.0.serialize_tuple_variant(name, index, variant, len).map(Compound)
        }

        fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, S::Error> {
            self.0.serialize_map(len).map(Compound)
        }

        fn serialize_struct(
            self,
            name: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStruct, S::Error> {
            let raw = name == SERDE_JSON_TOKEN;
            let name = if raw { SPLICE_TOKEN } else { name };
            self.0.serialize_struct(name, len).map(|inner| Struct { inner, raw })
        }

        fn serialize_struct_variant(
            self,
            name: &'static str,
            index: u32,
            variant: &'static str,
            len: usize,
        ) -> Result<Self::SerializeStructVariant, S::Error> {
            self.0.serialize_struct_variant(name, index, variant, len).map(Compound)
        }

        fn collect_str<T>(self, value: &T) -> Result<S::Ok, S::Error>
        where
            T: Display + ?Sized,
        {
            self.0.collect_str(value)
        }

        fn is_human_readable(&self) -> bool {
            self.0.is_human_readable()
        }
    }

    impl<C> SerializeSeq for Compound<C>
    where
        C: SerializeSeq,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_element<T>(&mut self, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_element(&Wrap(value))
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }

    impl<C> SerializeTuple for Compound<C>
    where
        C: SerializeTuple,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_element<T>(&mut self, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_element(&Wrap(value))
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }

    impl<C> SerializeTupleStruct for Compound<C>
    where
        C: SerializeTupleStruct,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_field<T>(&mut self, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_field(&Wrap(value))
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }

    impl<C> SerializeTupleVariant for Compound<C>
    where
        C: SerializeTupleVariant,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_field<T>(&mut self, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_field(&Wrap(value))
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }

    impl<C> SerializeMap for Compound<C>
    where
        C: SerializeMap,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_key<T>(&mut self, key: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_key(key)
        }

        fn serialize_value<T>(&mut self, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_value(&Wrap(value))
        }

        fn serialize_entry<K, V>(&mut self, key: &K, value: &V) -> Result<(), C::Error>
        where
            K: Serialize + ?Sized,
            V: Serialize + ?Sized,
        {
            self.0.serialize_entry(key, &Wrap(value))
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }

    impl<C> SerializeStruct for Struct<C>
    where
        C: SerializeStruct,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            if self.raw && key == SERDE_JSON_TOKEN {
                // The field is the text as a `&str`, with nothing inside it to
                // rename, so it goes to sonic-rs's raw writer unwrapped.
                self.inner.serialize_field(SPLICE_TOKEN, value)
            } else {
                self.inner.serialize_field(key, &Wrap(value))
            }
        }

        fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
            self.inner.skip_field(key)
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.inner.end()
        }
    }

    impl<C> SerializeStructVariant for Compound<C>
    where
        C: SerializeStructVariant,
    {
        type Ok = C::Ok;
        type Error = C::Error;

        fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), C::Error>
        where
            T: Serialize + ?Sized,
        {
            self.0.serialize_field(key, &Wrap(value))
        }

        fn skip_field(&mut self, key: &'static str) -> Result<(), C::Error> {
            self.0.skip_field(key)
        }

        fn end(self) -> Result<C::Ok, C::Error> {
            self.0.end()
        }
    }
}

#[cfg(test)]
pub(crate) use selected::Serializer;
pub(crate) use selected::{
    Deserializer, Error, SPLICE_TOKEN, error_position, from_str, is_syntax, to_writer,
};

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
