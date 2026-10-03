// The expansion of `#[derive(QuestionSet)]` for the example `Ticket` of the
// SDK's documentation, formatted by rustfmt. `expand_tests.rs` compares it
// token for token with what the derive generates; it is not compiled.

const _: () = {
    use ::decision_model_sdk::__private;
    #[automatically_derived]
    impl __private::QuestionSet for Ticket {
        fn prepared() -> &'static __private::PreparedQuestions {
            static PREPARED: __private::PreparedQuestions =
                __private::PreparedQuestions::from_static(
                    "{\"billing\":{\"type\":\"noul\",\"instructions\":\"Is this about billing?\",\"criteria\":{\"true\":\"payments or invoices\"}},\"tone\":{\"type\":\"choice\",\"instructions\":\"What is the tone?\",\"criteria\":{\"calm\":\"neutral or polite\",\"angry\":null}},\"urgency\":{\"type\":\"score\",\"instructions\":\"How urgent?\",\"criteria\":[\"can wait\",\"this week\",\"today\"]}}billingtoneurgency",
                    323,
                    &[330, 334, 341],
                );
            &PREPARED
        }
    }
    #[automatically_derived]
    impl __private::AnswerSet for Ticket {
        fn deserialize_answers<'de, __D>(
            __deserializer: __D,
            _: __private::AnswerContext,
        ) -> __private::Result<Self, <__D as __private::Deserializer<'de>>::Error>
        where
            __D: __private::Deserializer<'de>,
        {
            enum __QuestionSetField {
                F0,
                F1,
                F2,
                Other,
            }
            struct __QuestionSetFieldVisitor;
            impl __private::Visitor<'_> for __QuestionSetFieldVisitor {
                type Value = __QuestionSetField;
                fn expecting(
                    &self,
                    __formatter: &mut __private::Formatter<'_>,
                ) -> __private::fmt::Result {
                    __private::Formatter::write_str(__formatter, "a question name")
                }
                fn visit_str<__E>(
                    self,
                    __value: &__private::str,
                ) -> __private::Result<__QuestionSetField, __E>
                where
                    __E: __private::Error,
                {
                    __private::Ok(match __value {
                        "billing" => __QuestionSetField::F0,
                        "tone" => __QuestionSetField::F1,
                        "urgency" => __QuestionSetField::F2,
                        _ => __QuestionSetField::Other,
                    })
                }
                fn visit_bytes<__E>(
                    self,
                    __value: &[__private::u8],
                ) -> __private::Result<__QuestionSetField, __E>
                where
                    __E: __private::Error,
                {
                    __private::Ok(match __value {
                        b"billing" => __QuestionSetField::F0,
                        b"tone" => __QuestionSetField::F1,
                        b"urgency" => __QuestionSetField::F2,
                        _ => __QuestionSetField::Other,
                    })
                }
            }
            impl<'de> __private::Deserialize<'de> for __QuestionSetField {
                fn deserialize<__D2>(
                    __deserializer: __D2,
                ) -> __private::Result<Self, <__D2 as __private::Deserializer<'de>>::Error>
                where
                    __D2: __private::Deserializer<'de>,
                {
                    __private::Deserializer::deserialize_str(
                        __deserializer,
                        __QuestionSetFieldVisitor,
                    )
                }
            }
            struct __QuestionSetVisitor;
            impl<'de> __private::Visitor<'de> for __QuestionSetVisitor {
                type Value = Ticket;
                fn expecting(
                    &self,
                    __formatter: &mut __private::Formatter<'_>,
                ) -> __private::fmt::Result {
                    __private::Formatter::write_str(__formatter, "the answers of `Ticket`")
                }
                fn visit_map<__M>(
                    self,
                    mut __map: __M,
                ) -> __private::Result<Ticket, <__M as __private::MapAccess<'de>>::Error>
                where
                    __M: __private::MapAccess<'de>,
                {
                    let mut __f0: __private::Option<__private::NoulAnswer> = __private::None;
                    let mut __f1: __private::Option<__private::ChoiceAnswer> = __private::None;
                    let mut __f2: __private::Option<__private::ScoreAnswer> = __private::None;
                    while let __private::Some(__key) =
                        __private::MapAccess::next_key::<__QuestionSetField>(&mut __map)?
                    {
                        match __key {
                            __QuestionSetField::F0 if __private::Option::is_none(&__f0) => {
                                __f0 = __private::Some(__private::MapAccess::next_value::<
                                    __private::NoulAnswer,
                                >(
                                    &mut __map
                                )?);
                            }
                            __QuestionSetField::F1 if __private::Option::is_none(&__f1) => {
                                __f1 = __private::Some(__private::MapAccess::next_value::<
                                    __private::ChoiceAnswer,
                                >(
                                    &mut __map
                                )?);
                            }
                            __QuestionSetField::F2 if __private::Option::is_none(&__f2) => {
                                __f2 = __private::Some(__private::MapAccess::next_value::<
                                    __private::ScoreAnswer,
                                >(
                                    &mut __map
                                )?);
                            }
                            _ => {
                                __private::MapAccess::next_value::<__private::IgnoredAny>(
                                    &mut __map,
                                )?;
                            }
                        }
                    }
                    __private::Ok(Ticket {
                        billing: match __f0 {
                            __private::Some(__answer) => __answer,
                            __private::None => {
                                return __private::Err(__private::Error::missing_field("billing"));
                            }
                        },
                        tone: match __f1 {
                            __private::Some(__answer) => __answer,
                            __private::None => {
                                return __private::Err(__private::Error::missing_field("tone"));
                            }
                        },
                        urgency: match __f2 {
                            __private::Some(__answer) => __answer,
                            __private::None => {
                                return __private::Err(__private::Error::missing_field("urgency"));
                            }
                        },
                    })
                }
            }
            __private::Deserializer::deserialize_map(__deserializer, __QuestionSetVisitor)
        }
    }
};
