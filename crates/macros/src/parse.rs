//! Reading a `#[derive(QuestionSet)]` input into the questions it declares.
//!
//! Everything the runtime `Questions::prepare` rejects is rejected here, at
//! the attribute that causes it: a set with no questions and a score with no
//! levels. So is everything that cannot become a struct whose fields hold the
//! answers: a field without a question, a field with two, a field whose type
//! is not the answer type of its question, two fields answering to one name,
//! an option listed twice, any input that is not a plain struct with named
//! fields, and a struct named like one of the expansion's own items. Nothing
//! else is limited: the API documents its option and level counts as subject
//! to change, so no count is checked. Every refusal about a field names that
//! field.

use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote};
use syn::{
    Attribute, Data, DeriveInput, Fields, GenericArgument, Ident, LitStr, Meta, Path,
    PathArguments, Token, Type, ext::IdentExt, meta::ParseNestedMeta, parenthesized,
    parse::ParseStream, spanned::Spanned,
};

use crate::expand;

/// A derive input, read.
#[derive(Debug)]
pub(crate) struct QuestionSetInput {
    /// The path the generated code reaches the SDK through.
    pub(crate) root: TokenStream,
    pub(crate) ident: Ident,
    pub(crate) fields: Vec<QuestionField>,
}

/// One field and the question it answers.
#[derive(Debug, Clone)]
pub(crate) struct QuestionField {
    /// The field as the struct declares it, `r#` included.
    pub(crate) member: Ident,
    /// The name the question is sent under and its answer comes back under.
    pub(crate) name: String,
    pub(crate) question: Question,
    /// Where the field's type is written: an answer of the wrong type is
    /// reported there rather than at the derive.
    pub(crate) ty_span: Span,
}

/// Two fields are equal when they declare the same question; where their
/// types are written is not part of that.
impl PartialEq for QuestionField {
    fn eq(&self, other: &Self) -> bool {
        (&self.member, &self.name, &self.question) == (&other.member, &other.name, &other.question)
    }
}

impl Eq for QuestionField {}

/// A question, with text content only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Question {
    Noul { instructions: Option<String>, yes: Option<String>, no: Option<String> },
    Choice { instructions: Option<String>, options: Vec<(String, Option<String>)> },
    Score { instructions: Option<String>, levels: Vec<String> },
}

impl Question {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Noul { .. } => Kind::Noul,
            Self::Choice { .. } => Kind::Choice,
            Self::Score { .. } => Kind::Score,
        }
    }
}

/// The three question kinds, each with its attribute and its answer type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Noul,
    Choice,
    Score,
}

impl Kind {
    const ALL: [Self; 3] = [Self::Noul, Self::Choice, Self::Score];

    pub(crate) fn attribute(self) -> &'static str {
        match self {
            Self::Noul => "noul",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }

    pub(crate) fn answer(self) -> &'static str {
        match self {
            Self::Noul => "NoulAnswer",
            Self::Choice => "ChoiceAnswer",
            Self::Score => "ScoreAnswer",
        }
    }

    fn of_attribute(attr: &Attribute) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| attr.path().is_ident(kind.attribute()))
    }

    fn of_answer(name: &Ident) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| name == kind.answer())
    }

    /// What an attribute of this kind looks like, for messages.
    fn example(self) -> &'static str {
        match self {
            Self::Noul => "#[noul(instructions = \"...\")]",
            Self::Choice => "#[choice(instructions = \"...\", options(\"a\" = \"...\", \"b\"))]",
            Self::Score => "#[score(instructions = \"...\", levels(\"low\", \"high\"))]",
        }
    }
}

/// Reads the input, reporting every problem it finds rather than the first.
pub(crate) fn parse(input: &DeriveInput) -> syn::Result<QuestionSetInput> {
    let mut errors = Errors::default();
    let root = errors.keep(container_root(&input.attrs)).flatten();
    let ident = &input.ident;

    // A struct that shares a helper's name would be shadowed by that helper
    // inside the expansion, and rustc would report the clash in terms of code
    // the caller never wrote.
    if expand::RESERVED.iter().any(|reserved| ident.unraw() == reserved) {
        errors.push(syn::Error::new_spanned(
            ident,
            format!(
                "`{ident}` is a name the derive's generated code gives to an item of its own, so \
                 a question set cannot be called that: the reserved names are {}; rename the \
                 struct",
                reserved_names()
            ),
        ));
    }

    let generics: Option<&dyn ToTokens> = if !input.generics.params.is_empty() {
        Some(&input.generics)
    } else {
        // `Generics` prints its parameters only, so a where clause alone is
        // pointed at directly.
        input.generics.where_clause.as_ref().map(|clause| clause as &dyn ToTokens)
    };
    if let Some(generics) = generics {
        errors.push(syn::Error::new_spanned(
            generics,
            format!(
                "`{ident}` cannot be generic: a question set's questions are serialized once, \
                 when the program is compiled, so they cannot depend on a type parameter; \
                 remove the generics"
            ),
        ));
    }

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) if named.named.is_empty() => {
                errors.push(syn::Error::new_spanned(
                    &input.ident,
                    format!(
                        "At least one question is required: `{ident}` has no fields; add one \
                         field per question, such as `#[noul(instructions = \"...\")] spam: \
                         NoulAnswer`"
                    ),
                ));
                Vec::new()
            }
            Fields::Named(named) => named.named.iter().collect(),
            Fields::Unnamed(unnamed) => {
                errors.push(syn::Error::new_spanned(
                    unnamed,
                    format!(
                        "`{ident}` is a tuple struct: a question set needs named fields, because \
                         a field's name is the name its question is asked under; write \
                         `struct {ident} {{ name: NoulAnswer, ... }}`"
                    ),
                ));
                Vec::new()
            }
            Fields::Unit => {
                errors.push(syn::Error::new_spanned(
                    &input.ident,
                    format!(
                        "`{ident}` is a unit struct: a question set needs one named field per \
                         question; write `struct {ident} {{ name: NoulAnswer, ... }}`"
                    ),
                ));
                Vec::new()
            }
        },
        Data::Enum(data) => {
            errors.push(syn::Error::new(
                data.enum_token.span,
                format!(
                    "`{ident}` is an enum: `#[derive(QuestionSet)]` needs a struct with one \
                     named field per question"
                ),
            ));
            Vec::new()
        }
        Data::Union(data) => {
            errors.push(syn::Error::new(
                data.union_token.span,
                format!(
                    "`{ident}` is a union: `#[derive(QuestionSet)]` needs a struct with one \
                     named field per question"
                ),
            ));
            Vec::new()
        }
    };

    let mut questions: Vec<QuestionField> = Vec::with_capacity(fields.len());
    for field in fields {
        let Some(member) = &field.ident else { continue };
        let Some((question, name, name_span)) = errors.keep(field_question(field, member)) else {
            continue;
        };
        if let Some(first) = questions.iter().find(|known| known.name == name) {
            errors.push(syn::Error::new(
                name_span,
                format!(
                    "the question name {} is used twice, by field `{}` and by field `{}`: \
                     answers are matched to fields by name, so each field needs a name of its \
                     own; rename one field or give it `#[question(name = \"...\")]`",
                    quoted(&name),
                    first.member,
                    member,
                ),
            ));
            continue;
        }
        let ty_span = field.ty.span();
        questions.push(QuestionField { member: member.clone(), name, question, ty_span });
    }

    errors.finish()?;
    Ok(QuestionSetInput {
        root: root.unwrap_or_else(|| quote!(::decision_model_sdk)),
        ident: ident.clone(),
        fields: questions,
    })
}

/// The crate path of `#[question_set(crate = ...)]`, if the attribute gives
/// one.
fn container_root(attrs: &[Attribute]) -> syn::Result<Option<TokenStream>> {
    let mut root: Option<Path> = None;
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("question_set")) {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("crate") {
                return Err(meta.error(format!(
                    "unknown key `{}` in `#[question_set(...)]`: the one key is `crate = \
                     <path>`, the path the SDK is reached through",
                    path_text(&meta.path)
                )));
            }
            if root.is_some() {
                return Err(meta.error("`crate` is given twice in `#[question_set(...)]`"));
            }
            let path = meta.value()?.parse::<Path>()?;
            root = Some(path);
            Ok(())
        })?;
    }
    Ok(root.map(ToTokens::into_token_stream))
}

/// The question of one field, its name, and the span to report the name at.
fn field_question(field: &syn::Field, member: &Ident) -> syn::Result<(Question, String, Span)> {
    let mut errors = Errors::default();
    let mut kind_attr: Option<(Kind, &Attribute)> = None;
    // Kept apart from `errors`: a field with two question attributes has no
    // one kind to check its type or its attribute against.
    let mut two_kinds: Option<syn::Error> = None;
    let mut rename: Option<LitStr> = None;
    let mut rename_attr: Option<&Attribute> = None;
    for attr in &field.attrs {
        if let Some(kind) = Kind::of_attribute(attr) {
            match kind_attr {
                None => kind_attr = Some((kind, attr)),
                Some((first, _)) => {
                    let error = syn::Error::new_spanned(
                        attr,
                        format!(
                            "field `{member}` has two question attributes, `#[{}]` and `#[{}]`: \
                             a field answers one question; keep one of them",
                            first.attribute(),
                            kind.attribute(),
                        ),
                    );
                    match &mut two_kinds {
                        Some(earlier) => earlier.combine(error),
                        None => two_kinds = Some(error),
                    }
                }
            }
        } else if attr.path().is_ident("question") {
            if rename_attr.is_some() {
                errors.push(syn::Error::new_spanned(
                    attr,
                    format!(
                        "field `{member}` has two `#[question(...)]` attributes; give its name \
                         once"
                    ),
                ));
                continue;
            }
            rename_attr = Some(attr);
            if let Some(name) = errors.keep(question_name(attr).map_err(|e| in_field(e, member))) {
                rename = Some(name);
            }
        }
    }

    if let Some(error) = two_kinds {
        return Err(errors.with(error));
    }
    let Some((kind, attr)) = kind_attr else {
        return Err(errors.with(syn::Error::new_spanned(
            member,
            format!(
                "field `{member}` is not a question: every field of a question set asks one; \
                 add `#[noul(...)]`, `#[choice(...)]` or `#[score(...)]`"
            ),
        )));
    };

    let name = rename.as_ref().map_or_else(|| member.unraw().to_string(), LitStr::value);
    let name_span = rename.as_ref().map_or_else(|| member.span(), LitStr::span);
    // The attribute comes before the type in the source, so its error is
    // reported first.
    let question = question(attr, kind, member, &name);
    let typed = check_answer_type(&field.ty, kind, member);
    match (question, typed) {
        (Ok(question), Ok(())) => errors.finish().map(|()| (question, name, name_span)),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(errors.with(error)),
        (Err(first), Err(second)) => {
            errors.push(first);
            Err(errors.with(second))
        }
    }
}

/// The wire name of `#[question(name = "...")]`.
fn question_name(attr: &Attribute) -> syn::Result<LitStr> {
    let mut name = None;
    attr.parse_nested_meta(|meta| {
        if !meta.path.is_ident("name") {
            return Err(meta.error(format!(
                "unknown key `{}` in `#[question(...)]`: the one key is `name = \"...\"`, the \
                 name the question is asked under",
                path_text(&meta.path)
            )));
        }
        if name.is_some() {
            return Err(meta.error("`name` is given twice in `#[question(...)]`"));
        }
        name = Some(value(&meta, "name")?);
        Ok(())
    })?;
    name.ok_or_else(|| syn::Error::new_spanned(attr, "`#[question(...)]` needs `name = \"...\"`"))
}

/// Checks that a field's type is the answer type of its question.
///
/// The type is matched by its last path segment, so `NoulAnswer` and
/// `decision_model_sdk::NoulAnswer` both match; a type alias does not, because a
/// derive sees only the tokens of the type, not what they name.
fn check_answer_type(ty: &Type, kind: Kind, member: &Ident) -> syn::Result<()> {
    let expected = kind.answer();
    let last = last_segment(ty);
    let found = last.map(|(ident, _)| ident);
    if let Some((ident, arguments)) = last
        && ident == "Option"
        && inner_answer(arguments).is_some()
    {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "field `{member}` is optional, and a question set's fields cannot be in this \
                 version: the API answers every question it is asked, and a response without \
                 one of the answers is refused as malformed; make the field a `{expected}`"
            ),
        ));
    }
    match (found.and_then(Kind::of_answer), last) {
        (Some(actual), Some((_, PathArguments::None))) if actual == kind => Ok(()),
        (Some(actual), Some((_, PathArguments::None))) => Err(syn::Error::new_spanned(
            ty,
            format!(
                "field `{member}` has `#[{}]`, whose answer is a `{expected}`, but its type is \
                 `{}`: write `#[{}(...)]` for a `{}` field, or make the field a `{expected}`",
                kind.attribute(),
                actual.answer(),
                actual.attribute(),
                actual.answer(),
            ),
        )),
        _ => Err(syn::Error::new_spanned(
            ty,
            format!(
                "field `{member}` has `#[{}]`, so its type must be `{expected}` (the type is \
                 matched by name: `{expected}` and `decision_model_sdk::{expected}` work, a type alias \
                 does not)",
                kind.attribute(),
            ),
        )),
    }
}

/// The last segment of a plain path type, looking through parentheses and
/// the invisible groups a `macro_rules!` expansion wraps a type in.
fn last_segment(ty: &Type) -> Option<(&Ident, &PathArguments)> {
    match ty {
        Type::Group(group) => last_segment(&group.elem),
        Type::Paren(paren) => last_segment(&paren.elem),
        Type::Path(path) if path.qself.is_none() => {
            path.path.segments.last().map(|segment| (&segment.ident, &segment.arguments))
        }
        _ => None,
    }
}

/// The answer kind inside `Option<...>`'s angle brackets, if that is what
/// they hold.
fn inner_answer(arguments: &PathArguments) -> Option<Kind> {
    let PathArguments::AngleBracketed(arguments) = arguments else { return None };
    let mut types = arguments.args.iter().filter_map(|argument| match argument {
        GenericArgument::Type(ty) => Some(ty),
        _ => None,
    });
    let (ident, _) = last_segment(types.next()?)?;
    Kind::of_answer(ident)
}

/// Reads the question attribute of field `member`, whose question is named
/// `name`.
fn question(attr: &Attribute, kind: Kind, member: &Ident, name: &str) -> syn::Result<Question> {
    let grammar = |error| in_field(error, member);
    let mut instructions = None;
    let mut yes = None;
    let mut no = None;
    let mut options: Option<Vec<(String, Option<String>)>> = None;
    let mut levels: Option<(Vec<String>, Span)> = None;

    let keys = match kind {
        Kind::Noul => "`instructions`, `yes` and `no`",
        Kind::Choice => "`instructions` and `options(...)`",
        Kind::Score => "`instructions` and `levels(...)`",
    };
    match &attr.meta {
        // A bare `#[noul]` asks a noul with nothing set, as `Noul::new()`
        // does; `#[choice]` and `#[score]` are reported below as missing
        // their options or levels.
        Meta::Path(_) => {}
        Meta::NameValue(_) => {
            return Err(grammar(syn::Error::new_spanned(
                attr,
                format!("expected `{}`, not `#[{} = ...]`", kind.example(), kind.attribute()),
            )));
        }
        Meta::List(_) => attr
            .parse_nested_meta(|meta| {
                let key = path_text(&meta.path);
                let slot = match (kind, key.as_str()) {
                    (_, "instructions") => &mut instructions,
                    (Kind::Noul, "yes") => &mut yes,
                    (Kind::Noul, "no") => &mut no,
                    (Kind::Choice, "options") => {
                        if options.is_some() {
                            return Err(meta.error("`options(...)` is given twice"));
                        }
                        options = Some(choice_options(&meta)?);
                        return Ok(());
                    }
                    (Kind::Score, "levels") => {
                        if levels.is_some() {
                            return Err(meta.error("`levels(...)` is given twice"));
                        }
                        levels = Some((score_levels(&meta)?, meta.path.span()));
                        return Ok(());
                    }
                    _ => {
                        return Err(meta.error(format!(
                            "unknown key `{key}` in `#[{}(...)]`: the keys are {keys}",
                            kind.attribute()
                        )));
                    }
                };
                if slot.is_some() {
                    return Err(meta.error(format!("`{key}` is given twice")));
                }
                *slot = Some(value(&meta, &key)?.value());
                Ok(())
            })
            .map_err(grammar)?,
    }

    match kind {
        Kind::Noul => Ok(Question::Noul { instructions, yes, no }),
        Kind::Choice => {
            let options = options.ok_or_else(|| {
                grammar(syn::Error::new_spanned(
                    attr,
                    format!(
                        "`#[choice]` needs its options: `options(\"a\", \"b\")`, each one \
                         optionally described as `\"a\" = \"...\"`, as in `{}`",
                        kind.example()
                    ),
                ))
            })?;
            Ok(Question::Choice { instructions, options })
        }
        Kind::Score => {
            let (levels, span) = levels.ok_or_else(|| {
                grammar(syn::Error::new_spanned(
                    attr,
                    format!(
                        "`#[score]` needs its levels, lowest first: `levels(\"low\", \"high\")`, \
                         as in `{}`",
                        kind.example()
                    ),
                ))
            })?;
            if levels.is_empty() {
                // The runtime's message, then what to write instead. It
                // names the question, which is how the runtime reports it.
                return Err(syn::Error::new(
                    span,
                    format!(
                        "Score question {} has no criteria; at least one score is required: \
                         list the levels, lowest first, as `levels(\"low\", \"high\")`",
                        quoted(name)
                    ),
                ));
            }
            Ok(Question::Score { instructions, levels })
        }
    }
}

/// `options("a" = "description", "b")`: names in order, each with an
/// optional description.
fn choice_options(meta: &ParseNestedMeta<'_>) -> syn::Result<Vec<(String, Option<String>)>> {
    let content = parenthesized_list(meta, "options")?;
    let mut options: Vec<(String, Option<String>)> = Vec::new();
    while !content.is_empty() {
        let name = string(&content, "an option name")?;
        let description = if content.peek(Token![=]) {
            content.parse::<Token![=]>()?;
            Some(string(&content, "a description")?.value())
        } else {
            None
        };
        let value = name.value();
        if options.iter().any(|(known, _)| *known == value) {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "the option {} is listed twice: each option of a choice is listed once, \
                     with its description if it has one",
                    quoted(&value)
                ),
            ));
        }
        options.push((value, description));
        if !content.is_empty() {
            content.parse::<Token![,]>()?;
        }
    }
    Ok(options)
}

/// `levels("low", "high")`: descriptions, lowest first.
fn score_levels(meta: &ParseNestedMeta<'_>) -> syn::Result<Vec<String>> {
    let content = parenthesized_list(meta, "levels")?;
    let mut levels = Vec::new();
    while !content.is_empty() {
        levels.push(string(&content, "a level description")?.value());
        if !content.is_empty() {
            content.parse::<Token![,]>()?;
        }
    }
    Ok(levels)
}

/// The inside of `key(...)`.
fn parenthesized_list<'a>(
    meta: &ParseNestedMeta<'a>,
    key: &str,
) -> syn::Result<syn::parse::ParseBuffer<'a>> {
    if !meta.input.peek(syn::token::Paren) {
        return Err(meta.error(format!("expected `{key}(...)`, a parenthesized list")));
    }
    let content;
    parenthesized!(content in meta.input);
    Ok(content)
}

/// The string of `key = "..."`.
fn value(meta: &ParseNestedMeta<'_>, key: &str) -> syn::Result<LitStr> {
    if !meta.input.peek(Token![=]) {
        return Err(meta.error(format!("expected `{key} = \"...\"`")));
    }
    meta.input.parse::<Token![=]>()?;
    string(meta.input, "a string literal")
}

/// A plain string literal: text content is all the derive takes.
fn string(input: ParseStream<'_>, what: &str) -> syn::Result<LitStr> {
    if !input.peek(LitStr) {
        return Err(input.error(format!(
            "expected {what}: the derive takes text as string literals only; object or array \
             content is a runtime `Questions` feature"
        )));
    }
    let literal = input.parse::<LitStr>()?;
    if !literal.suffix().is_empty() {
        return Err(syn::Error::new(
            literal.span(),
            format!("unexpected suffix `{}` on a string literal", literal.suffix()),
        ));
    }
    Ok(literal)
}

/// `error` with each of its messages prefixed by the field it is about, at
/// the same source.
///
/// Every refusal of a field's attribute passes through here, syn's own
/// included ("expected `,`"), so none of them leaves the reader to work out
/// the field from the span alone. A `syn::Error` spans from a first token to
/// a last one, and `Error::span` cannot hand that range back (it would have to
/// join the two, which stable rustc does not do). `to_compile_error` can:
/// its tokens start at the first span and end at the last, and `new_spanned`
/// spans from its first token to its last, so the rewritten message covers
/// exactly the source the original did.
fn in_field(error: syn::Error, member: &Ident) -> syn::Error {
    let mut named = (&error).into_iter().map(|message| {
        syn::Error::new_spanned(message.to_compile_error(), format!("field `{member}`: {message}"))
    });
    match named.next() {
        Some(mut first) => {
            first.extend(named);
            first
        }
        // A `syn::Error` holds at least one message; if one ever held none,
        // it is returned unchanged rather than lost.
        None => error,
    }
}

/// [`expand::RESERVED`] as a message lists it: `a`, `b` and `c`.
fn reserved_names() -> String {
    let quoted: Vec<String> = expand::RESERVED.iter().map(|name| format!("`{name}`")).collect();
    match quoted.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

fn path_text(path: &Path) -> String {
    path.to_token_stream().to_string().replace(' ', "")
}

/// A name as a message shows it: in double quotes, with Rust's escapes for
/// anything that would break the line or hide.
fn quoted(name: &str) -> String {
    format!("{name:?}")
}

/// Errors gathered across fields, so one build reports all of them.
#[derive(Default)]
struct Errors(Option<syn::Error>);

impl Errors {
    fn push(&mut self, error: syn::Error) {
        match &mut self.0 {
            Some(first) => first.combine(error),
            None => self.0 = Some(error),
        }
    }

    /// The value of `result`, or `None` with its error recorded.
    fn keep<T>(&mut self, result: syn::Result<T>) -> Option<T> {
        result.map_err(|error| self.push(error)).ok()
    }

    fn finish(self) -> syn::Result<()> {
        self.0.map_or(Ok(()), Err)
    }

    /// Every error gathered so far, then `last`.
    fn with(self, last: syn::Error) -> syn::Error {
        match self.0 {
            Some(mut first) => {
                first.combine(last);
                first
            }
            None => last,
        }
    }
}

#[cfg(test)]
#[path = "parse_tests.rs"]
mod tests;
