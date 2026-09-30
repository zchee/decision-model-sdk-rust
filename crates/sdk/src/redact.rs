//! The credentials of a request, kept out of the error its transport failed
//! with.
//!
//! A transport's error can print what it was sent: a proxy that echoes a
//! rejected header, a custom service that formats the request into its
//! message. When an attempt fails, the values of the request's credential
//! headers are looked for in every rendering of the error chain - its
//! `Display`, its `Debug` and its alternate `Debug` - in the forms Rust's
//! formatting, `http`, `bytes` and JSON write them in, and replaced by `***`.
//! Nothing here runs before a failure: the credentials are borrowed from the
//! headers the request was built from only once the attempt has failed.
//!
//! Nothing in this module depends on the `tracing` feature: an error reaches
//! the caller whether or not events are compiled in.

use std::{error::Error as StdError, fmt, ops::Range};

use bytes::Bytes;
use http::{HeaderName, HeaderValue, header};

use crate::{config::is_python_space, constants::SECRET_HEADERS};

/// The most links of an error chain that are scanned. A longer chain cannot
/// be shown to be free of a credential, so it is always replaced by a copy of
/// this many links.
pub(crate) const MAX_SCANNED_LINKS: usize = 32;

/// What credentials are replaced with.
const REDACTED: &str = "***";

/// Whether the value of this header must not be printed.
///
/// `http` stores every header name lower-cased, which is what makes the
/// comparisons here case-insensitive without lower-casing anything.
pub(crate) fn is_secret(name: &HeaderName, value: &HeaderValue) -> bool {
    let name = name.as_str();
    value.is_sensitive()
        || SECRET_HEADERS.contains(&name)
        || name.contains("token")
        || name.contains("secret")
}

// ------------------------------------------------------------- credentials

/// Every form in which the credentials of one request can appear in an
/// error's text.
///
/// A credential is the value of a header [`is_secret`] matches, and for
/// `Authorization` and `Proxy-Authorization` also the part after the scheme,
/// split as Python's `str.split(maxsplit=1)` splits it. Empty values are
/// skipped. Each credential is looked for as it is and as `{:?}` of a `str`,
/// `str::escape_debug`, `{:?}` of an `http::HeaderValue`, `{:?}` of a
/// `bytes::Bytes` and a JSON string write it, each without its quotes. Each
/// of those forms is also looked for as `{:?}` of a `str` writes it once
/// more, without the quotes: that is how a derived `Debug` prints a `String`
/// field that already holds an escaped form.
///
/// The matcher is a plain left-to-right scan that tries the longest form
/// first, as the Python SDK's regular expression alternation does.
/// `aho-corasick` would fit, but it is not a dependency of this crate, and
/// this runs only after a failure, over a few short forms.
pub(crate) struct Credentials {
    /// Distinct and non-empty, longest first.
    variants: Vec<String>,
}

impl Credentials {
    /// The credentials among `headers`.
    pub(crate) fn new<'a>(
        headers: impl IntoIterator<Item = (&'a HeaderName, &'a HeaderValue)>,
    ) -> Self {
        let mut variants = Vec::new();
        for (name, value) in headers {
            if value.is_empty() {
                continue;
            }
            if is_secret(name, value) {
                push_variants(&mut variants, value.as_bytes());
            }
            if (name == header::AUTHORIZATION || name == header::PROXY_AUTHORIZATION)
                && let Some(credential) = after_scheme(value.as_bytes())
            {
                push_variants(&mut variants, credential);
            }
        }
        variants.sort_unstable_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        variants.dedup();
        Self { variants }
    }

    /// Whether any form of a credential occurs in `text`.
    pub(crate) fn occur_in(&self, text: &str) -> bool {
        self.next_match(text, 0).is_some()
    }

    /// `text` with every form of a credential replaced by `***`.
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut redacted = String::with_capacity(text.len());
        let mut copied = 0;
        for found in self.matches(text) {
            redacted.push_str(&text[copied..found.start]);
            redacted.push_str(REDACTED);
            copied = found.end;
        }
        redacted.push_str(&text[copied..]);
        redacted
    }

    /// The byte ranges of `text` that hold a form of a credential, left to
    /// right and not overlapping; where two forms start at the same place, the
    /// longer one is taken.
    pub(crate) fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let mut from = 0;
        std::iter::from_fn(move || {
            let found = self.next_match(text, from)?;
            from = found.end;
            Some(found)
        })
    }

    /// The first match at or after byte `from`. Every form is UTF-8 and
    /// starts with a leading byte, so it can only match at a character
    /// boundary, and stepping one byte at a time is safe.
    fn next_match(&self, text: &str, from: usize) -> Option<Range<usize>> {
        let bytes = text.as_bytes();
        (from..bytes.len()).find_map(|start| {
            let rest = &bytes[start..];
            self.variants
                .iter()
                .find(|variant| rest.starts_with(variant.as_bytes()))
                .map(|variant| start..start + variant.len())
        })
    }
}

/// The credential after the scheme of an `Authorization` value, as Python's
/// `value.split(maxsplit=1)` gives it: the value's leading whitespace and the
/// run of whitespace after the scheme are dropped, trailing whitespace is
/// kept, and a value with no second word has none.
fn after_scheme(value: &[u8]) -> Option<&[u8]> {
    let is_space = |byte: &u8| is_python_space(char::from(*byte));
    let start = value.iter().position(|byte| !is_space(byte))?;
    let value = &value[start..];
    let scheme_end = value.iter().position(is_space)?;
    let rest = &value[scheme_end..];
    let credential_start = rest.iter().position(|byte| !is_space(byte))?;
    Some(&rest[credential_start..])
}

/// Adds every form of the credential `bytes` to `variants`.
fn push_variants(variants: &mut Vec<String>, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    let quoted_debug = format!("{text:?}");
    // A sensitive value prints `Sensitive`; this one is not flagged, so it
    // prints what a transport that re-wrapped the bytes would print.
    let header_debug = format!(
        "{:?}",
        HeaderValue::from_bytes(bytes)
            .expect("invariant: the bytes are a header value's, or a part of one after a space")
    );
    let bytes_debug = format!("{:?}", Bytes::copy_from_slice(bytes));
    let forms = [
        text.to_string(),
        unquote(&quoted_debug, "\"").to_owned(),
        text.escape_debug().to_string(),
        unquote(&header_debug, "\"").to_owned(),
        unquote(&bytes_debug, "b\"").to_owned(),
        json_escape(&text),
    ];
    // A second level: a `String` holding one of the forms above, printed by
    // a derived `Debug`, escapes it once more. Duplicates of a first-level
    // form are removed with the rest in `Credentials::new`.
    let escaped_again = forms.each_ref().map(|form| {
        let quoted = format!("{form:?}");
        unquote(&quoted, "\"").to_owned()
    });
    variants.extend(forms.into_iter().chain(escaped_again).filter(|form| !form.is_empty()));
}

/// `text` without the `open` it starts with and the `"` it ends with.
fn unquote<'a>(text: &'a str, open: &str) -> &'a str {
    text.strip_prefix(open).and_then(|inner| inner.strip_suffix('"')).unwrap_or(text)
}

/// `text` as a JSON string's contents, escaped as `serde_json` escapes it:
/// `"` and `\`, the five short escapes, other control characters below U+0020
/// as `\u00xx` in lower-case hex; DEL and every other character as they are.
fn json_escape(text: &str) -> String {
    use fmt::Write as _;

    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\u{8}' => escaped.push_str("\\b"),
            '\u{c}' => escaped.push_str("\\f"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{0}'..='\u{1f}' => write!(escaped, "\\u{:04x}", u32::from(character))
                .expect("invariant: writing to a String cannot fail"),
            _ => escaped.push(character),
        }
    }
    escaped
}

// ------------------------------------------------------------ error chains

/// What becomes of an error chain once it has been scanned.
pub(crate) enum Outcome {
    /// No rendering of any link, nor the message, holds a credential: the
    /// original chain and message are kept.
    Kept,
    /// No link holds a credential, but the message built from the chain does:
    /// escaping is not reversible, so a link's tab written as `\t` can spell a
    /// credential no link holds. The message is replaced; the chain is kept.
    MessageOnly,
    /// A link holds a credential, or the chain is longer than
    /// [`MAX_SCANNED_LINKS`]: the chain is replaced by this redacted copy.
    Replaced(RedactedLink),
}

/// Scans `source` and the links below it, and the `message` already built
/// from them, for the credentials. `message` is the part after the fixed
/// `Connection error: ` prefix, which is the SDK's own text and is never
/// redacted.
///
/// Every link is rendered with `Display`, `{:?}` and `{:#?}`: an error's
/// `Debug` prints its source's `Debug`, and `{:#?}` passes the alternate flag
/// down to it, so each of the three can reach a caller. A kept original is
/// scanned once, here; a link whose rendering reads state that changes later
/// could still print a credential afterwards, which only a copy rules out.
pub(crate) fn copy_chain(
    source: &(dyn StdError + 'static),
    message: &str,
    credentials: &Credentials,
) -> Outcome {
    let mut texts = Vec::new();
    let mut found = false;
    let mut link = Some(source);
    while let Some(current) = link {
        if texts.len() == MAX_SCANNED_LINKS {
            found = true;
            break;
        }
        let rendered = [current.to_string(), format!("{current:?}"), format!("{current:#?}")];
        found |= rendered.iter().any(|text| credentials.occur_in(text));
        texts.push(rendered);
        link = current.source();
    }
    if !found {
        return if credentials.occur_in(message) { Outcome::MessageOnly } else { Outcome::Kept };
    }
    let mut below = None;
    for [display, debug, alternate_debug] in texts.into_iter().rev() {
        below = Some(Box::new(RedactedLink {
            display: credentials.redact(&display).into_boxed_str(),
            debug: credentials.redact(&debug).into_boxed_str(),
            alternate_debug: credentials.redact(&alternate_debug).into_boxed_str(),
            source: below,
        }));
    }
    let top = below.expect("invariant: the chain starts with `source`, so it has a link");
    Outcome::Replaced(*top)
}

/// One link of a redacted copy of an error chain: the texts the original
/// link rendered, with every credential replaced by `***`.
///
/// It keeps no reference to the original, so its type is private and cannot
/// be downcast to what the transport failed with.
pub(crate) struct RedactedLink {
    display: Box<str>,
    debug: Box<str>,
    alternate_debug: Box<str>,
    source: Option<Box<RedactedLink>>,
}

impl fmt::Display for RedactedLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl fmt::Debug for RedactedLink {
    /// The original's `Debug`, or its alternate `Debug` when `{:#?}` asks for
    /// it, redacted.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if formatter.alternate() { &self.alternate_debug } else { &self.debug })
    }
}

impl StdError for RedactedLink {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|link| link as &(dyn StdError + 'static))
    }
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
