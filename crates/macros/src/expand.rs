//! The code `#[derive(QuestionSet)]` expands to.
//!
//! Two implementations, inside an anonymous `const _: () = { ... };` block so
//! that the helper types they need are invisible outside it:
//!
//! - `QuestionSet`, whose `prepared()` returns a `static` built from the JSON
//!   produced by [`json::prepare`](crate::json::prepare). The static is
//!   initialized when the program is compiled, so no call ever allocates or
//!   runs an initializer.
//! - `AnswerSet`, following that trait's contract: the key is matched by a
//!   field identifier that only compares text, each field is one `Option`, the
//!   first answer of a name is kept, anything else is skipped with
//!   `IgnoredAny`, and a missing answer is `missing_field(<name>)`.
//!
//! Every path starts at the SDK's `__private` module, imported once at the top
//! of the block from the crate root (`::decision_model_sdk`, or what
//! `#[question_set(crate = ...)]` names); the import shadows anything of that
//! name around the block. The helper items carry names no caller is expected
//! to use, and a struct named like one of them is refused (see [`RESERVED`]).
//! So the expansion means the same wherever it lands, a
//! `#![no_implicit_prelude]` module or one with its own `Result` included.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::DeriveInput;

use crate::{json, parse};

/// The module every generated path starts at, imported once per expansion.
const PRIVATE: &str = "__private";
/// The enum the generated code matches an answer's key into.
const FIELD: &str = "__QuestionSetField";
/// The visitor that reads a key into a [`FIELD`].
const FIELD_VISITOR: &str = "__QuestionSetFieldVisitor";
/// The visitor that reads the answers into the derived struct.
const VISITOR: &str = "__QuestionSetVisitor";
/// The deserializer's type parameter of `deserialize_answers`.
const DESERIALIZER: &str = "__D";
/// The map's type parameter of the visitor's `visit_map`.
const MAP: &str = "__M";

/// The names a derived struct cannot have: each is declared by the expansion
/// in a scope that also names the struct, where it would stand for the helper
/// instead. The expansion's other helpers (`__D2`, `__E`, `PREPARED`) never
/// share a scope with the struct's name, so any struct may be called that.
pub(crate) const RESERVED: [&str; 6] = [FIELD, FIELD_VISITOR, VISITOR, DESERIALIZER, MAP, PRIVATE];

/// The expansion of the derive for `input`.
pub(crate) fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let set = parse::parse(input)?;
    let prepared = json::prepare(&set.fields);
    let root = &set.root;
    // The one path that names the crate. Everything else goes through this
    // import, so a root that does not resolve is one error where the root is
    // written, not one per generated path.
    let private = format_ident!("{PRIVATE}");
    let field = format_ident!("{FIELD}");
    let field_visitor = format_ident!("{FIELD_VISITOR}");
    let visitor = format_ident!("{VISITOR}");
    let deserializer = format_ident!("{DESERIALIZER}");
    let map = format_ident!("{MAP}");
    let ident = &set.ident;

    let buf = Literal::string(&prepared.buf);
    let json_len = Literal::usize_unsuffixed(prepared.json_len);
    let name_ends = prepared.name_ends.iter().map(|end| Literal::usize_unsuffixed(*end));

    let variants: Vec<_> = (0..set.fields.len()).map(|index| format_ident!("F{index}")).collect();
    let slots: Vec<_> = (0..set.fields.len()).map(|index| format_ident!("__f{index}")).collect();
    let answer_types: Vec<_> = set
        .fields
        .iter()
        .map(|field| {
            let answer = format_ident!("{}", field.question.kind().answer());
            quote!(#private::#answer)
        })
        .collect();
    let names: Vec<_> = set.fields.iter().map(|field| Literal::string(&field.name)).collect();
    let byte_names: Vec<_> =
        set.fields.iter().map(|field| Literal::byte_string(field.name.as_bytes())).collect();
    // Each field's value is spanned at the field's type, so a field whose type
    // only shares the answer type's name is reported there.
    let values = set.fields.iter().zip(&slots).zip(&names).map(|((field, slot), name)| {
        let member = &field.member;
        quote_spanned! {field.ty_span=>
            #member: match #slot {
                #private::Some(__answer) => __answer,
                #private::None => {
                    return #private::Err(#private::Error::missing_field(#name));
                }
            }
        }
    });
    let expecting = Literal::string(&format!("the answers of `{ident}`"));

    Ok(quote! {
        const _: () = {
            use #root::#private;

            #[automatically_derived]
            impl #private::QuestionSet for #ident {
                fn prepared() -> &'static #private::PreparedQuestions {
                    static PREPARED: #private::PreparedQuestions =
                        #private::PreparedQuestions::from_static(#buf, #json_len, &[#(#name_ends),*]);
                    &PREPARED
                }
            }

            #[automatically_derived]
            impl #private::AnswerSet for #ident {
                fn deserialize_answers<'de, #deserializer>(
                    __deserializer: #deserializer,
                    _: #private::AnswerContext,
                ) -> #private::Result<Self, <#deserializer as #private::Deserializer<'de>>::Error>
                where
                    #deserializer: #private::Deserializer<'de>,
                {
                    enum #field {
                        #(#variants,)*
                        Other,
                    }

                    struct #field_visitor;

                    impl #private::Visitor<'_> for #field_visitor {
                        type Value = #field;

                        fn expecting(
                            &self,
                            __formatter: &mut #private::Formatter<'_>,
                        ) -> #private::fmt::Result {
                            #private::Formatter::write_str(__formatter, "a question name")
                        }

                        fn visit_str<__E>(
                            self,
                            __value: &#private::str,
                        ) -> #private::Result<#field, __E>
                        where
                            __E: #private::Error,
                        {
                            #private::Ok(match __value {
                                #(#names => #field::#variants,)*
                                _ => #field::Other,
                            })
                        }

                        fn visit_bytes<__E>(
                            self,
                            __value: &[#private::u8],
                        ) -> #private::Result<#field, __E>
                        where
                            __E: #private::Error,
                        {
                            #private::Ok(match __value {
                                #(#byte_names => #field::#variants,)*
                                _ => #field::Other,
                            })
                        }
                    }

                    impl<'de> #private::Deserialize<'de> for #field {
                        fn deserialize<__D2>(
                            __deserializer: __D2,
                        ) -> #private::Result<Self, <__D2 as #private::Deserializer<'de>>::Error>
                        where
                            __D2: #private::Deserializer<'de>,
                        {
                            #private::Deserializer::deserialize_str(
                                __deserializer,
                                #field_visitor,
                            )
                        }
                    }

                    struct #visitor;

                    impl<'de> #private::Visitor<'de> for #visitor {
                        type Value = #ident;

                        fn expecting(
                            &self,
                            __formatter: &mut #private::Formatter<'_>,
                        ) -> #private::fmt::Result {
                            #private::Formatter::write_str(__formatter, #expecting)
                        }

                        fn visit_map<#map>(
                            self,
                            mut __map: #map,
                        ) -> #private::Result<#ident, <#map as #private::MapAccess<'de>>::Error>
                        where
                            #map: #private::MapAccess<'de>,
                        {
                            #(
                                let mut #slots: #private::Option<#answer_types> = #private::None;
                            )*
                            while let #private::Some(__key) =
                                #private::MapAccess::next_key::<#field>(&mut __map)?
                            {
                                match __key {
                                    #(
                                        #field::#variants
                                            if #private::Option::is_none(&#slots) =>
                                        {
                                            #slots = #private::Some(
                                                #private::MapAccess::next_value::<#answer_types>(
                                                    &mut __map,
                                                )?,
                                            );
                                        }
                                    )*
                                    // An answer the struct has no field for, or
                                    // a later answer to a question already read:
                                    // the first answer of a name is the one kept.
                                    _ => {
                                        #private::MapAccess::next_value::<#private::IgnoredAny>(
                                            &mut __map,
                                        )?;
                                    }
                                }
                            }
                            #private::Ok(#ident { #(#values,)* })
                        }
                    }

                    #private::Deserializer::deserialize_map(__deserializer, #visitor)
                }
            }
        };
    })
}

#[cfg(test)]
#[path = "expand_tests.rs"]
mod tests;
