//! What the derive accepts, and the whole message of everything it refuses.
//!
//! The spans of these errors are checked by the main crate's trybuild cases
//! (`crates/sdk/tests/ui`); here each message is compared in full.

use syn::parse_quote;

use super::*;

fn parsed(input: &DeriveInput) -> QuestionSetInput {
    parse(input).unwrap_or_else(|error| panic!("the input is accepted, but: {error}"))
}

/// Every message of a refused input, in the order they are reported.
fn refused(input: &DeriveInput) -> Vec<String> {
    let error = parse(input).expect_err("the input is refused");
    error.into_iter().map(|error| error.to_string()).collect()
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

// ------------------------------------------------------------- accepted

/// The example set of the SDK's documentation, member by member.
#[test]
fn the_documented_example_reads_as_three_questions() {
    let set = parsed(&parse_quote! {
        struct Ticket {
            #[noul(instructions = "Is this about billing?", yes = "payments or invoices")]
            billing: NoulAnswer,
            #[choice(instructions = "What is the tone?", options("calm" = "neutral or polite", "angry"))]
            tone: ChoiceAnswer,
            #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
            urgency: ScoreAnswer,
        }
    });
    assert_eq!(set.ident, "Ticket");
    assert_eq!(set.root.to_string(), ":: typesafe_sdk");
    assert_eq!(
        set.fields,
        [
            QuestionField {
                member: parse_quote!(billing),
                ty_span: Span::call_site(),
                name: "billing".to_owned(),
                question: Question::Noul {
                    instructions: text("Is this about billing?"),
                    yes: text("payments or invoices"),
                    no: None,
                },
            },
            QuestionField {
                member: parse_quote!(tone),
                ty_span: Span::call_site(),
                name: "tone".to_owned(),
                question: Question::Choice {
                    instructions: text("What is the tone?"),
                    options: vec![
                        ("calm".to_owned(), text("neutral or polite")),
                        ("angry".to_owned(), None),
                    ],
                },
            },
            QuestionField {
                member: parse_quote!(urgency),
                ty_span: Span::call_site(),
                name: "urgency".to_owned(),
                question: Question::Score {
                    instructions: text("How urgent?"),
                    levels: vec!["can wait".to_owned(), "this week".to_owned(), "today".to_owned()],
                },
            },
        ]
    );
}

/// Every key of every kind is optional except a choice's options and a
/// score's levels, as in the runtime builders; keys may come in any order,
/// with a trailing comma; string escapes are decoded.
#[test]
fn optional_keys_order_and_escapes() {
    let set = parsed(&parse_quote! {
        struct Minimal {
            #[noul]
            a: NoulAnswer,
            #[noul()]
            b: NoulAnswer,
            #[noul(no = "N", yes = "Y", instructions = "I",)]
            c: NoulAnswer,
            #[choice(options())]
            d: ChoiceAnswer,
            #[choice(options("x",), instructions = "tab\there")]
            e: ChoiceAnswer,
            #[score(levels(r"raw \n", "\u{1F30D}"))]
            f: ScoreAnswer,
        }
    });
    let questions: Vec<_> = set.fields.into_iter().map(|field| field.question).collect();
    assert_eq!(
        questions,
        [
            Question::Noul { instructions: None, yes: None, no: None },
            Question::Noul { instructions: None, yes: None, no: None },
            Question::Noul { instructions: text("I"), yes: text("Y"), no: text("N") },
            Question::Choice { instructions: None, options: vec![] },
            Question::Choice {
                instructions: text("tab\there"),
                options: vec![("x".to_owned(), None)],
            },
            Question::Score {
                instructions: None,
                levels: vec!["raw \\n".to_owned(), "\u{1F30D}".to_owned()],
            },
        ]
    );
}

/// A field's name is its question's name, `r#` removed, unless
/// `#[question(name = "...")]` sets one, before or after the question
/// attribute; the empty name is allowed, as the runtime allows it.
#[test]
fn names_come_from_the_field_or_the_question_attribute() {
    let set = parsed(&parse_quote! {
        struct Named {
            #[noul]
            r#type: NoulAnswer,
            #[question(name = "is-spam")]
            #[noul]
            spam: NoulAnswer,
            #[noul]
            #[question(name = "")]
            blank: NoulAnswer,
            /// Other attributes are left alone.
            #[doc(hidden)]
            #[noul]
            documented: NoulAnswer,
        }
    });
    let names: Vec<_> =
        set.fields.iter().map(|field| (field.member.to_string(), field.name.as_str())).collect();
    assert_eq!(
        names,
        [
            ("r#type".to_owned(), "type"),
            ("spam".to_owned(), "is-spam"),
            ("blank".to_owned(), ""),
            ("documented".to_owned(), "documented"),
        ]
    );
}

/// The answer type is matched by its last path segment, through
/// parentheses.
#[test]
fn answer_types_are_matched_by_their_last_segment() {
    let set = parsed(&parse_quote! {
        struct Paths {
            #[noul] a: typesafe_sdk::NoulAnswer,
            #[choice(options("x"))] b: ::typesafe_sdk::response::ChoiceAnswer,
            #[score(levels("x"))] c: (ScoreAnswer),
        }
    });
    assert_eq!(set.fields.len(), 3);
}

#[test]
fn the_crate_path_can_be_overridden() {
    let set = parsed(&parse_quote! {
        #[question_set(crate = crate::reexport::sdk)]
        struct Renamed {
            #[noul] a: NoulAnswer,
        }
    });
    assert_eq!(set.root.to_string(), "crate :: reexport :: sdk");
}

// ------------------------------------------------------------- the container

#[test]
fn only_a_struct_with_named_fields_is_a_question_set() {
    let cases: [(DeriveInput, &str); 5] = [
        (
            parse_quote!(
                enum Mood {
                    Calm,
                }
            ),
            "`Mood` is an enum: `#[derive(QuestionSet)]` needs a struct with one named field per question",
        ),
        (
            parse_quote!(union Bits { a: u8 }),
            "`Bits` is a union: `#[derive(QuestionSet)]` needs a struct with one named field per question",
        ),
        (
            parse_quote!(
                struct Pair(NoulAnswer, NoulAnswer);
            ),
            "`Pair` is a tuple struct: a question set needs named fields, because a field's name is \
             the name its question is asked under; write `struct Pair { name: NoulAnswer, ... }`",
        ),
        (
            parse_quote!(
                struct Nothing;
            ),
            "`Nothing` is a unit struct: a question set needs one named field per question; write \
             `struct Nothing { name: NoulAnswer, ... }`",
        ),
        (
            parse_quote!(
                struct Empty {}
            ),
            "At least one question is required: `Empty` has no fields; add one field per question, \
             such as `#[noul(instructions = \"...\")] spam: NoulAnswer`",
        ),
    ];
    for (input, message) in cases {
        assert_eq!(refused(&input), [message], "input `{}`", input.ident);
    }
}

#[test]
fn a_question_set_cannot_be_generic() {
    let message = |name: &str| {
        format!(
            "`{name}` cannot be generic: a question set's questions are serialized once, when the \
             program is compiled, so they cannot depend on a type parameter; remove the generics"
        )
    };
    let cases: [DeriveInput; 4] = [
        parse_quote!(
            struct A<T> {
                #[noul]
                a: NoulAnswer,
                t: T,
            }
        ),
        parse_quote!(
            struct B<'a> {
                #[noul]
                a: NoulAnswer,
            }
        ),
        parse_quote!(
            struct C<const N: usize> {
                #[noul]
                a: NoulAnswer,
            }
        ),
        parse_quote!(
            struct D
            where
                u8: Copy,
            {
                #[noul]
                a: NoulAnswer,
            }
        ),
    ];
    let expected = [
        // `A`'s field `t` has no question: both problems are reported.
        vec![
            message("A"),
            "field `t` is not a question: every field of a question set asks one; add \
             `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`"
                .to_owned(),
        ],
        vec![message("B")],
        vec![message("C")],
        vec![message("D")],
    ];
    for (input, expected) in cases.iter().zip(expected) {
        assert_eq!(refused(input), expected, "input `{}`", input.ident);
    }
}

/// A struct named like an item the expansion declares next to the struct's
/// own name is refused at its name, with the whole reserved set; the list is
/// the expansion's, so a helper renamed there is renamed here.
#[test]
fn a_struct_cannot_be_named_like_a_helper_of_the_expansion() {
    let reserved = "`__QuestionSetField`, `__QuestionSetFieldVisitor`, `__QuestionSetVisitor`, \
                    `__D`, `__M` and `__private`";
    let message = |name: &str| {
        format!(
            "`{name}` is a name the derive's generated code gives to an item of its own, so a \
             question set cannot be called that: the reserved names are {reserved}; rename the \
             struct"
        )
    };
    // A struct asking one yes/no question, named `name`.
    let named = |name: &str| -> DeriveInput {
        let ident: Ident = syn::parse_str(name)
            .unwrap_or_else(|error| panic!("{name:?} is not an identifier: {error}"));
        parse_quote!(
            struct #ident {
                #[noul]
                a: NoulAnswer,
            }
        )
    };
    let reserved_inputs = [
        "__QuestionSetField",
        "__QuestionSetFieldVisitor",
        "__QuestionSetVisitor",
        "__D",
        "__M",
        "__private",
        "r#__D",
    ];
    for name in reserved_inputs {
        assert_eq!(refused(&named(name)), [message(name)], "input `{name}`");
    }
    assert_eq!(reserved_names(), reserved);

    // Helpers that never share a scope with the struct's name, and names
    // that only resemble the reserved ones, are free.
    for name in ["__D2", "__E", "PREPARED", "__QuestionSet", "__Private"] {
        let input = named(name);
        assert_eq!(parsed(&input).ident, input.ident, "input `{name}`");
    }

    // The problems of the rest of the input are still reported with it.
    assert_eq!(
        refused(&parse_quote!(
            struct __M<T> {
                t: T,
            }
        )),
        [
            message("__M"),
            "`__M` cannot be generic: a question set's questions are serialized once, when the \
             program is compiled, so they cannot depend on a type parameter; remove the generics"
                .to_owned(),
            "field `t` is not a question: every field of a question set asks one; add \
             `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`"
                .to_owned(),
        ]
    );
}

/// The derive's documentation lists the reserved names the check uses.
#[test]
fn the_reserved_names_are_documented() {
    let documentation = include_str!("lib.rs");
    for name in crate::expand::RESERVED {
        assert!(documentation.contains(&format!("`{name}`")), "`{name}` is not in the rustdoc");
    }
}

#[test]
fn the_container_attribute_takes_only_one_crate_path() {
    assert_eq!(
        refused(&parse_quote! {
            #[question_set(krate = x)]
            struct A { #[noul] a: NoulAnswer }
        }),
        ["unknown key `krate` in `#[question_set(...)]`: the one key is `crate = <path>`, the \
             path the SDK is reached through"]
    );
    assert_eq!(
        refused(&parse_quote! {
            #[question_set(crate = x, crate = y)]
            struct A { #[noul] a: NoulAnswer }
        }),
        ["`crate` is given twice in `#[question_set(...)]`"]
    );
    assert_eq!(
        refused(&parse_quote! {
            #[question_set(crate = x)]
            #[question_set(crate = y)]
            struct A { #[noul] a: NoulAnswer }
        }),
        ["`crate` is given twice in `#[question_set(...)]`"]
    );
}

// ------------------------------------------------------------- fields

#[test]
fn every_field_asks_exactly_one_question() {
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[noul]
                a: NoulAnswer,
                plain: NoulAnswer,
            }
        )),
        ["field `plain` is not a question: every field of a question set asks one; add \
             `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`"]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[question(name = "x")]
                renamed: NoulAnswer,
            }
        )),
        ["field `renamed` is not a question: every field of a question set asks one; add \
             `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`"]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[noul]
                #[choice(options("x"))]
                both: NoulAnswer,
            }
        )),
        ["field `both` has two question attributes, `#[noul]` and `#[choice]`: a field answers \
             one question; keep one of them"]
    );
    assert_eq!(
        refused(&parse_quote! {
            struct A {
                #[question(name = "x")]
                #[question(name = "y")]
                #[noul]
                twice: NoulAnswer,
            }
        }),
        ["field `twice` has two `#[question(...)]` attributes; give its name once"]
    );
}

#[test]
fn the_field_type_is_the_answer_type_of_its_question() {
    let cases: [(DeriveInput, &str); 6] = [
        (
            parse_quote!(
                struct A {
                    #[noul]
                    tone: ChoiceAnswer,
                }
            ),
            "field `tone` has `#[noul]`, whose answer is a `NoulAnswer`, but its type is \
             `ChoiceAnswer`: write `#[choice(...)]` for a `ChoiceAnswer` field, or make the field a \
             `NoulAnswer`",
        ),
        (
            parse_quote!(
                struct A {
                    #[score(levels("x"))]
                    spam: typesafe_sdk::NoulAnswer,
                }
            ),
            "field `spam` has `#[score]`, whose answer is a `ScoreAnswer`, but its type is \
             `NoulAnswer`: write `#[noul(...)]` for a `NoulAnswer` field, or make the field a \
             `ScoreAnswer`",
        ),
        (
            parse_quote!(
                struct A {
                    #[noul]
                    spam: f64,
                }
            ),
            "field `spam` has `#[noul]`, so its type must be `NoulAnswer` (the type is matched by \
             name: `NoulAnswer` and `typesafe_sdk::NoulAnswer` work, a type alias does not)",
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options("x"))]
                    tone: Tone,
                }
            ),
            "field `tone` has `#[choice]`, so its type must be `ChoiceAnswer` (the type is matched \
             by name: `ChoiceAnswer` and `typesafe_sdk::ChoiceAnswer` work, a type alias does not)",
        ),
        (
            parse_quote!(
                struct A {
                    #[noul]
                    spam: NoulAnswer<u8>,
                }
            ),
            "field `spam` has `#[noul]`, so its type must be `NoulAnswer` (the type is matched by \
             name: `NoulAnswer` and `typesafe_sdk::NoulAnswer` work, a type alias does not)",
        ),
        (
            parse_quote!(
                struct A {
                    #[noul]
                    spam: &'static NoulAnswer,
                }
            ),
            "field `spam` has `#[noul]`, so its type must be `NoulAnswer` (the type is matched by \
             name: `NoulAnswer` and `typesafe_sdk::NoulAnswer` work, a type alias does not)",
        ),
    ];
    for (input, message) in cases {
        assert_eq!(refused(&input), [message]);
    }
}

/// An optional answer is refused whatever the answer type inside, with the
/// reason; an `Option` of something else is just the wrong type.
#[test]
fn an_optional_field_is_refused_with_the_reason() {
    let optional = |member: &str, answer: &str| {
        format!(
            "field `{member}` is optional, and a question set's fields cannot be in this version: \
             the API answers every question it is asked, and a response without one of the \
             answers is refused as malformed; make the field a `{answer}`"
        )
    };
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[noul]
                spam: Option<NoulAnswer>,
            }
        )),
        [optional("spam", "NoulAnswer")]
    );
    assert_eq!(
        refused(&parse_quote! {
            struct A { #[choice(options("x"))] tone: core::option::Option<typesafe_sdk::ScoreAnswer> }
        }),
        [optional("tone", "ChoiceAnswer")]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[noul]
                spam: Option<f64>,
            }
        )),
        ["field `spam` has `#[noul]`, so its type must be `NoulAnswer` (the type is matched by \
             name: `NoulAnswer` and `typesafe_sdk::NoulAnswer` work, a type alias does not)"]
    );
}

/// The runtime's one per-question rejection, with its message, then what to
/// write.
#[test]
fn a_score_without_levels_is_refused_with_the_runtime_message() {
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[score(instructions = "How urgent?", levels())]
                urgency: ScoreAnswer,
            }
        )),
        ["Score question \"urgency\" has no criteria; at least one score is required: list the \
             levels, lowest first, as `levels(\"low\", \"high\")`"]
    );
    assert_eq!(
        refused(&parse_quote! {
            struct A { #[question(name = "say \"hi\"\n")] #[score(levels())] urgency: ScoreAnswer }
        }),
        ["Score question \"say \\\"hi\\\"\\n\" has no criteria; at least one score is required: \
             list the levels, lowest first, as `levels(\"low\", \"high\")`"]
    );
}

#[test]
fn a_choice_needs_options_and_a_score_needs_levels() {
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[choice(instructions = "Tone?")]
                tone: ChoiceAnswer,
            }
        )),
        ["field `tone`: `#[choice]` needs its options: `options(\"a\", \"b\")`, each one \
             optionally described as `\"a\" = \"...\"`, as in `#[choice(instructions = \"...\", \
             options(\"a\" = \"...\", \"b\"))]`"]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[choice]
                tone: ChoiceAnswer,
            }
        )),
        ["field `tone`: `#[choice]` needs its options: `options(\"a\", \"b\")`, each one \
             optionally described as `\"a\" = \"...\"`, as in `#[choice(instructions = \"...\", \
             options(\"a\" = \"...\", \"b\"))]`"]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[score]
                urgency: ScoreAnswer,
            }
        )),
        ["field `urgency`: `#[score]` needs its levels, lowest first: `levels(\"low\", \
             \"high\")`, as in `#[score(instructions = \"...\", levels(\"low\", \"high\"))]`"]
    );
}

#[test]
fn an_option_is_listed_once() {
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[choice(options("calm" = "polite", "angry", "calm"))]
                tone: ChoiceAnswer,
            }
        )),
        ["field `tone`: the option \"calm\" is listed twice: each option of a choice is listed \
             once, with its description if it has one"]
    );
}

#[test]
fn a_key_is_given_once() {
    let cases: [(DeriveInput, &str); 5] = [
        (
            parse_quote!(
                struct A {
                    #[noul(instructions = "a", instructions = "b")]
                    a: NoulAnswer,
                }
            ),
            "`instructions` is given twice",
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(yes = "a", yes = "b")]
                    a: NoulAnswer,
                }
            ),
            "`yes` is given twice",
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(no = "a", no = "b")]
                    a: NoulAnswer,
                }
            ),
            "`no` is given twice",
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options("a"), options("b"))]
                    a: ChoiceAnswer,
                }
            ),
            "`options(...)` is given twice",
        ),
        (
            parse_quote!(
                struct A {
                    #[score(levels("a"), levels("b"))]
                    a: ScoreAnswer,
                }
            ),
            "`levels(...)` is given twice",
        ),
    ];
    // Every field of these inputs is `a`, and each message names it first.
    for (input, message) in cases {
        assert_eq!(refused(&input), [format!("field `a`: {message}")]);
    }
}

#[test]
fn unknown_keys_name_the_keys_there_are() {
    let cases: [(DeriveInput, &str); 4] = [
        (
            parse_quote!(
                struct A {
                    #[noul(options("x"))]
                    a: NoulAnswer,
                }
            ),
            "unknown key `options` in `#[noul(...)]`: the keys are `instructions`, `yes` and `no`",
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(yes = "x", options("x"))]
                    a: ChoiceAnswer,
                }
            ),
            "unknown key `yes` in `#[choice(...)]`: the keys are `instructions` and `options(...)`",
        ),
        (
            parse_quote!(
                struct A {
                    #[score(levels("x"), weight = "3")]
                    a: ScoreAnswer,
                }
            ),
            "unknown key `weight` in `#[score(...)]`: the keys are `instructions` and `levels(...)`",
        ),
        (
            parse_quote!(
                struct A {
                    #[question(rename = "x")]
                    #[noul]
                    a: NoulAnswer,
                }
            ),
            "unknown key `rename` in `#[question(...)]`: the one key is `name = \"...\"`, the name \
             the question is asked under",
        ),
    ];
    // Every field of these inputs is `a`, and each message names it first.
    for (input, message) in cases {
        assert_eq!(refused(&input), [format!("field `a`: {message}")]);
    }
}

/// Text is a plain string literal; anything else is refused with what the
/// derive takes.
#[test]
fn values_are_string_literals() {
    let literal_only = |what: &str| {
        format!(
            "expected {what}: the derive takes text as string literals only; object or array \
             content is a runtime `Questions` feature"
        )
    };
    let cases: [(DeriveInput, String); 9] = [
        (
            parse_quote!(
                struct A {
                    #[noul(instructions = 3)]
                    a: NoulAnswer,
                }
            ),
            literal_only("a string literal"),
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(instructions = INSTRUCTIONS)]
                    a: NoulAnswer,
                }
            ),
            literal_only("a string literal"),
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(instructions = b"bytes")]
                    a: NoulAnswer,
                }
            ),
            literal_only("a string literal"),
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options(calm))]
                    a: ChoiceAnswer,
                }
            ),
            literal_only("an option name"),
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options("calm" = 1))]
                    a: ChoiceAnswer,
                }
            ),
            literal_only("a description"),
        ),
        (
            parse_quote!(
                struct A {
                    #[score(levels("low", 2))]
                    a: ScoreAnswer,
                }
            ),
            literal_only("a level description"),
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(instructions = "x"suffix)]
                    a: NoulAnswer,
                }
            ),
            "unexpected suffix `suffix` on a string literal".to_owned(),
        ),
        (
            parse_quote!(
                struct A {
                    #[noul(instructions)]
                    a: NoulAnswer,
                }
            ),
            "expected `instructions = \"...\"`".to_owned(),
        ),
        (
            parse_quote!(
                struct A {
                    #[question(name)]
                    #[noul]
                    a: NoulAnswer,
                }
            ),
            "expected `name = \"...\"`".to_owned(),
        ),
    ];
    // Every field of these inputs is `a`, and each message names it first.
    for (input, message) in cases {
        assert_eq!(refused(&input), [format!("field `a`: {message}")]);
    }
}

#[test]
fn malformed_attributes_say_what_is_expected() {
    let cases: [(DeriveInput, &str); 5] = [
        (
            parse_quote!(
                struct A {
                    #[noul = "x"]
                    a: NoulAnswer,
                }
            ),
            "expected `#[noul(instructions = \"...\")]`, not `#[noul = ...]`",
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options = "x")]
                    a: ChoiceAnswer,
                }
            ),
            "expected `options(...)`, a parenthesized list",
        ),
        (
            parse_quote!(
                struct A {
                    #[score(levels = "x")]
                    a: ScoreAnswer,
                }
            ),
            "expected `levels(...)`, a parenthesized list",
        ),
        (
            parse_quote!(
                struct A {
                    #[choice(options("a" "b"))]
                    a: ChoiceAnswer,
                }
            ),
            "expected `,`",
        ),
        (
            parse_quote!(
                struct A {
                    #[question]
                    #[noul]
                    a: NoulAnswer,
                }
            ),
            "expected attribute arguments in parentheses: #[question(...)]",
        ),
    ];
    // Every field of these inputs is `a`, and each message names it first.
    for (input, message) in cases {
        assert_eq!(refused(&input), [format!("field `a`: {message}")]);
    }
}

/// Answers are matched by name, so two fields cannot answer to one.
#[test]
fn a_name_is_used_once() {
    assert_eq!(
        refused(&parse_quote! {
            struct A {
                #[noul] spam: NoulAnswer,
                #[question(name = "spam")] #[noul] other: NoulAnswer,
                #[noul] r#spam2: NoulAnswer,
                #[question(name = "spam2")] #[noul] third: NoulAnswer,
            }
        }),
        [
            "the question name \"spam\" is used twice, by field `spam` and by field `other`: answers \
             are matched to fields by name, so each field needs a name of its own; rename one field \
             or give it `#[question(name = \"...\")]`",
            "the question name \"spam2\" is used twice, by field `r#spam2` and by field `third`: \
             answers are matched to fields by name, so each field needs a name of its own; rename \
             one field or give it `#[question(name = \"...\")]`",
        ]
    );
}

/// One build reports every field's problem, in source order, and a field's
/// attribute problem before its type problem.
#[test]
fn every_problem_is_reported_at_once() {
    assert_eq!(
        refused(&parse_quote! {
            struct A {
                #[score(levels())] urgency: NoulAnswer,
                plain: NoulAnswer,
                #[noul(bad = "x")] spam: NoulAnswer,
            }
        }),
        [
            "Score question \"urgency\" has no criteria; at least one score is required: list the \
             levels, lowest first, as `levels(\"low\", \"high\")`",
            "field `urgency` has `#[score]`, whose answer is a `ScoreAnswer`, but its type is \
             `NoulAnswer`: write `#[noul(...)]` for a `NoulAnswer` field, or make the field a \
             `ScoreAnswer`",
            "field `plain` is not a question: every field of a question set asks one; add \
             `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`",
            "field `spam`: unknown key `bad` in `#[noul(...)]`: the keys are `instructions`, `yes` \
             and `no`",
        ]
    );
}

/// No count is limited: a choice of a thousand options and a score of a
/// hundred levels are read like any other.
#[test]
fn no_option_or_level_count_is_limited() {
    let options = (0..1000).map(|index| format!("option {index}"));
    let levels = (0..100).map(|index| format!("level {index}"));
    let input: DeriveInput = parse_quote! {
        struct Many {
            #[choice(options(#(#options),*))] tone: ChoiceAnswer,
            #[score(levels(#(#levels),*))] urgency: ScoreAnswer,
        }
    };
    let set = parsed(&input);
    let [tone, urgency] = &set.fields[..] else { panic!("two fields: {:?}", set.fields) };
    let Question::Choice { options, .. } = &tone.question else { panic!("{tone:?}") };
    let Question::Score { levels, .. } = &urgency.question else { panic!("{urgency:?}") };
    assert_eq!((options.len(), levels.len()), (1000, 100));
    assert_eq!(options[999], ("option 999".to_owned(), None));
    assert_eq!(levels[99], "level 99");
}

#[test]
fn the_question_attribute_gives_one_name() {
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[question(name = "a", name = "b")]
                #[noul]
                a: NoulAnswer,
            }
        )),
        ["field `a`: `name` is given twice in `#[question(...)]`"]
    );
    assert_eq!(
        refused(&parse_quote!(
            struct A {
                #[question()]
                #[noul]
                a: NoulAnswer,
            }
        )),
        ["field `a`: `#[question(...)]` needs `name = \"...\"`"]
    );
}

/// The crate path is a path; syn's own message says what else it found.
#[test]
fn the_crate_path_is_a_path() {
    assert_eq!(
        refused(&parse_quote! {
            #[question_set(crate = "typesafe_sdk")]
            struct A { #[noul] a: NoulAnswer }
        }),
        ["expected identifier"]
    );
}
