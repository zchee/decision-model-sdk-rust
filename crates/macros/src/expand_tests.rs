//! The generated code, compared whole.
//!
//! `expand_tests/ticket.rs` is the expansion for the SDK's example set,
//! formatted by rustfmt and reviewed by hand; it is compared token for token,
//! so formatting does not matter and nothing else can differ. When the
//! expansion changes on purpose, the failing test prints the new one.

use syn::parse_quote;

use super::*;

/// The expansion for the documentation's `Ticket`.
const TICKET: &str = include_str!("expand_tests/ticket.rs");

fn ticket() -> DeriveInput {
    parse_quote! {
        struct Ticket {
            #[noul(instructions = "Is this about billing?", yes = "payments or invoices")]
            billing: NoulAnswer,
            #[choice(instructions = "What is the tone?", options("calm" = "neutral or polite", "angry"))]
            tone: ChoiceAnswer,
            #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
            urgency: ScoreAnswer,
        }
    }
}

/// The tokens of a stream as the comparison sees them: [`flatten`]ed, and
/// without the trailing commas rustfmt adds and removes (a comma right before
/// a closing `)`, `]`, `}` or generic `>` means nothing).
fn tokens(stream: TokenStream) -> Vec<String> {
    let flat = flatten(stream);
    let closes = |token: Option<&String>| {
        token.is_some_and(|token| matches!(token.as_str(), ")" | "]" | "}" | ">"))
    };
    flat.iter()
        .enumerate()
        .filter(|(index, token)| *token != "," || !closes(flat.get(index + 1)))
        .map(|(_, token)| token.clone())
        .collect()
}

/// The tokens of a stream, one string each, with the delimiters of groups
/// spelled out. Unlike `to_string`, it does not depend on whether a
/// punctuation character was joined to the next one where it was written
/// (`&'static` and `& 'static` are the same tokens).
fn flatten(stream: TokenStream) -> Vec<String> {
    use proc_macro2::{Delimiter, TokenTree};

    let mut out = Vec::new();
    for tree in stream {
        match tree {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Brace => ("{", "}"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::None => ("", ""),
                };
                out.push(open.to_owned());
                out.extend(flatten(group.stream()));
                out.push(close.to_owned());
            }
            other => out.push(other.to_string()),
        }
    }
    out
}

#[test]
fn the_ticket_expansion_is_the_reviewed_one() {
    let expanded = expand(&ticket()).expect("the example expands");
    let reviewed: TokenStream = syn::parse_str(TICKET).expect("the reviewed expansion is tokens");
    println!("{expanded}");
    assert_eq!(
        tokens(expanded),
        tokens(reviewed),
        "the expansion changed; the new one is printed above"
    );
}

/// With `#[question_set(crate = ...)]`, the one import of the SDK starts at
/// the given path, every other path starts at that import, and nothing names
/// `decision_model_sdk`.
#[test]
fn the_crate_path_is_named_once_and_everything_goes_through_it() {
    let input: DeriveInput = parse_quote! {
        #[question_set(crate = crate::reexported::sdk)]
        struct Spam {
            #[noul]
            spam: NoulAnswer,
        }
    };
    let expanded = expand(&input).expect("the input expands").to_string();
    assert!(expanded.starts_with("const _ : () = { use crate :: reexported :: sdk :: __private ;"));
    assert_eq!(expanded.matches("reexported").count(), 1, "{expanded}");
    assert!(!expanded.contains("decision_model_sdk"), "{expanded}");
    let tokens = flatten(expand(&input).expect("the input expands"));
    for (index, token) in tokens.iter().enumerate() {
        if token == "__private" {
            // `::` is two `:` tokens; one `:` is a type ascription.
            let continues_a_path =
                index >= 2 && tokens[index - 1] == ":" && tokens[index - 2] == ":";
            let after = tokens.get(index + 1).map(String::as_str);
            assert!(
                (!continues_a_path && after == Some(":"))
                    || (continues_a_path && after == Some(";")),
                "`__private` at token {index} is neither the import nor a path's first segment: {:?}",
                &tokens[index.saturating_sub(4)..(index + 4).min(tokens.len())]
            );
        }
    }
}

/// A refused input expands to nothing: the error is returned for the entry
/// point to turn into `compile_error!`s.
#[test]
fn a_refused_input_is_an_error_not_code() {
    let input: DeriveInput = parse_quote!(
        struct Empty {}
    );
    let error = expand(&input).expect_err("an empty struct is refused");
    let compile_error = error.into_compile_error().to_string();
    assert!(compile_error.starts_with(":: core :: compile_error !"), "{compile_error}");
    assert!(compile_error.contains("At least one question is required"), "{compile_error}");
}

/// A name that needs escaping is escaped in the JSON, spelled out in the key
/// matcher, and passed to `missing_field` as written.
#[test]
fn names_reach_the_key_matcher_unescaped() {
    let input: DeriveInput = parse_quote! {
        struct Odd {
            #[question(name = "say \"hi\"\n")]
            #[noul]
            odd: NoulAnswer,
        }
    };
    let expanded = expand(&input).expect("the input expands").to_string();
    // The static's text: the JSON with the name escaped for JSON, then the
    // name itself; both then escaped once more as a Rust string literal.
    // The JSON is 32 bytes and the name 9.
    assert!(
        expanded.contains(
            r#"from_static ("{\"say \\\"hi\\\"\\n\":{\"type\":\"noul\"}}say \"hi\"\n" , 32 , & [41])"#
        ),
        "{expanded}"
    );
    assert!(expanded.contains(r#""say \"hi\"\n" => __QuestionSetField :: F0"#), "{expanded}");
    assert!(expanded.contains(r#"b"say \"hi\"\n" => __QuestionSetField :: F0"#), "{expanded}");
    assert!(expanded.contains(r#"missing_field ("say \"hi\"\n")"#), "{expanded}");
}
