//! Tests for keeping a request's credentials out of connection errors.
//!
//! The cases port the upstream Python SDK's `tests/test_logging.py`
//! (`test_exception_redaction_escaped_values`,
//! `test_exception_redaction_preserves_network_diagnostics`). Every error
//! is built the way a failed attempt builds it: the transport's error goes
//! through [`transport::connection`], then through [`transport::redacted`]
//! with the headers of the request.

use std::io;

use http::{HeaderMap, Method, Uri};

use super::*;
use crate::{
    Error, ErrorKind,
    config::{Config, Explicit},
    transport::{self, BoxError, Exchange},
};

// ------------------------------------------------------------- fixtures

/// A header map of `pairs`.
fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            HeaderName::from_bytes(name.as_bytes()).expect("a valid name"),
            HeaderValue::from_str(value).expect("a valid value"),
        );
    }
    map
}

/// The `Authorization` value the client sends: flagged sensitive.
fn sensitive(value: &str) -> HeaderValue {
    let mut value = HeaderValue::from_str(value).expect("a valid value");
    value.set_sensitive(true);
    value
}

/// The error an attempt with `base` and `call` headers fails with when its
/// transport fails with `source`.
fn failed(
    source: impl Into<BoxError>,
    base: &HeaderMap,
    call: &[(HeaderName, HeaderValue)],
) -> Error {
    let uri = Uri::from_static("https://api.typesafe.ai/v1/models");
    let config = Config::resolve(
        Explicit {
            api_key: Some("test-key".into()),
            max_response_bytes: Some(1024),
            ..Explicit::default()
        },
        |_: &str| None::<String>,
    )
    .expect("the test configuration resolves");
    let exchange = Exchange {
        method: &Method::GET,
        uri: &uri,
        base_headers: base,
        call_headers: call,
        deadline: None,
        config: &config,
    };
    transport::redacted(transport::connection(source), exchange)
}

/// Every rendering of `error` a caller can reach: `Display`, `{:?}` and
/// `{:#?}` of it and of every link of its `source()` chain.
fn every_rendering(error: &Error) -> Vec<String> {
    let mut renderings = vec![error.to_string(), format!("{error:?}"), format!("{error:#?}")];
    let mut link = StdError::source(error);
    while let Some(current) = link {
        renderings.extend([current.to_string(), format!("{current:?}"), format!("{current:#?}")]);
        link = current.source();
    }
    renderings
}

/// Asserts that no form of any credential occurs in any rendering of
/// `error`.
#[track_caller]
fn assert_no_variant(error: &Error, credentials: &Credentials) {
    for rendering in every_rendering(error) {
        let found: Vec<_> = credentials.matches(&rendering).map(|at| &rendering[at]).collect();
        assert!(found.is_empty(), "{found:?} left in {rendering}");
    }
}

/// An error link whose three renderings are given, so a test decides which
/// of them holds a credential.
struct Link {
    display: String,
    debug: String,
    alternate: String,
    source: Option<Box<Link>>,
}

impl Link {
    fn new(display: &str, debug: &str, alternate: &str, source: Option<Link>) -> Self {
        Self {
            display: display.to_owned(),
            debug: debug.to_owned(),
            alternate: alternate.to_owned(),
            source: source.map(Box::new),
        }
    }

    /// A link whose three renderings are all `text`.
    fn plain(text: &str, source: Option<Link>) -> Self {
        Self::new(text, text, text, source)
    }
}

impl fmt::Display for Link {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.display)
    }
}

impl fmt::Debug for Link {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(if formatter.alternate() { &self.alternate } else { &self.debug })
    }
}

impl StdError for Link {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|link| link as &(dyn StdError + 'static))
    }
}

/// An error whose `Display` is `a<TAB>b` and whose `Debug` names only its
/// type, so no rendering of it holds the text `a\tb`.
#[derive(Debug)]
struct OpaqueError;

impl fmt::Display for OpaqueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a\tb")
    }
}

impl StdError for OpaqueError {}

// ------------------------------------------------------------ the forms

/// Upstream `test_exception_redaction_escaped_values`: the credential
/// `private'quoted"value\tail` (a backslash, not a tab), under each of four
/// secret headers, is replaced in every form Rust writes it in. The two
/// authorization headers carry it after a scheme. A form that only the
/// escaping of the message spells - a link's tab written as `\t`, two links
/// joined by `": "` - is replaced in the message of a redacted chain too,
/// whole, and the cut cannot leave part of it.
#[test]
fn every_escaped_form_of_a_credential_is_replaced() {
    let credential = r#"private'quoted"value\tail"#;
    let header_debug =
        format!("{:?}", HeaderValue::from_str(credential).expect("a valid header value"));
    let text = format!(
        "raw={credential}; debug={credential:?}; escape={}; header={header_debug}; bytes={:?}; \
         json={}",
        credential.escape_debug(),
        Bytes::from(credential),
        serde_json::to_string(credential).expect("a string encodes"),
    );
    let cases = [
        ("authorization", format!("Bearer {credential}")),
        ("proxy-authorization", format!("Basic {credential}")),
        ("x-api-key", credential.to_owned()),
        ("x-mixed-token", credential.to_owned()),
    ];
    for (index, (name, value)) in cases.iter().enumerate() {
        let pair = [(*name, value.as_str())];
        let map = headers(&pair);
        let credentials = Credentials::new(&map);
        assert_eq!(
            credentials.redact(&text),
            r#"raw=***; debug="***"; escape=***; header="***"; bytes=b"***"; json="***""#,
            "{name}"
        );

        // Half of the cases carry the header on the call rather than on the
        // client; both are the request's.
        let (base, call) = if index % 2 == 0 {
            (map, Vec::new())
        } else {
            let (name, value) = map.iter().next().expect("one header");
            (HeaderMap::new(), vec![(name.clone(), value.clone())])
        };
        let error = failed(io::Error::other(text.clone()), &base, &call);
        assert!(matches!(error.kind(), ErrorKind::Connection), "{name}: {error:?}");
        assert!(error.to_string().starts_with("Connection error: raw=***; "), "{name}: {error}");
        let source = StdError::source(&error).expect("a cause");
        assert!(source.downcast_ref::<io::Error>().is_none(), "{name}: a copy, not the original");
        assert_no_variant(&error, &credentials);
    }

    // A link whose derived `Debug` prints a `String` field that already holds
    // an escaped form escapes it a second time; that form is replaced too.
    let map = headers(&[("x-api-key", credential)]);
    let credentials = Credentials::new(&map);
    let held = format!("{credential:?}");
    let chain = Link::new("wrapped", &format!("Wrapped {{ text: {held:?} }}"), "Wrapped", None);
    let error = failed(chain, &map, &[]);
    assert_eq!(
        format!("{error:?}").matches(r#"Wrapped { text: "\"***\"" }"#).count(),
        1,
        "{error:?}"
    );
    assert_no_variant(&error, &credentials);

    // Each chain is replaced because a link's `{:?}` names a credential; the
    // message is built from the copy's `Display`, where the escaping spells
    // one: `a\` and `tb` across the escaped tab, `x: y` across the joiner.
    let spelled = headers(&[("x-api-key", r"a\"), ("api-key", "tb"), ("x-secret", "x: y")]);
    let credentials = Credentials::new(&spelled);
    let cases = [
        (Link::new("a\tb", r"Tab { a\ }", "Tab", None), "Connection error: ***"),
        (
            Link::new("x", "Joined { x: y }", "Joined", Some(Link::plain("y", None))),
            "Connection error: ***",
        ),
        (
            Link::new(&format!("{}a\tb", "z".repeat(198)), "Tab { tb }", "Tab", None),
            &*format!("Connection error: {}\u{2026}", "z".repeat(198)),
        ),
    ];
    for (chain, message) in cases {
        let error = failed(chain, &spelled, &[]);
        assert_eq!(error.to_string(), message);
        let source = StdError::source(&error).expect("a cause");
        assert!(source.downcast_ref::<Link>().is_none(), "a copy: {source:?}");
        assert_no_variant(&error, &credentials);
    }
}

/// The JSON form is written by this crate, not by `serde_json`, which is
/// only a test dependency. Every ASCII character, alone and between two
/// letters, is escaped exactly as `serde_json` escapes it; U+00E9 and U+2028
/// are left as they are by both.
#[test]
fn the_json_escape_matches_serde_json() {
    let mut texts: Vec<String> = Vec::new();
    for code in 0x00..=0x7F_u8 {
        let character = char::from(code);
        texts.push(character.to_string());
        texts.push(format!("a{character}b"));
    }
    texts.extend(["\u{e9}".to_owned(), "\u{2028}".to_owned()]);
    for text in &texts {
        let encoded = serde_json::to_string(text).expect("a string encodes");
        let inner = &encoded[1..encoded.len() - 1];
        assert_eq!(json_escape(text), inner, "{text:?}");
    }
    assert_eq!(json_escape("\u{e9}"), "\u{e9}");
    assert_eq!(json_escape("\u{2028}"), "\u{2028}");
}

/// Python's `value.split(maxsplit=1)` drops the whitespace before and after
/// the scheme and keeps what trails the credential; a value with no second
/// word has no credential after a scheme.
#[test]
fn the_credential_after_the_scheme_is_redacted_on_its_own() {
    let cases: [(&str, &str, Option<&str>); 7] = [
        ("authorization", "Bearer x", Some("x")),
        ("authorization", "Basic x", Some("x")),
        ("authorization", "Bearer  x ", Some("x ")),
        ("proxy-authorization", "Bearer x", Some("x")),
        ("authorization", "\tBearer\t\tx", Some("x")),
        ("authorization", "Bearer", None),
        ("authorization", "Bearer   ", None),
    ];
    for (name, value, credential) in cases {
        assert_eq!(
            after_scheme(value.as_bytes()),
            credential.map(str::as_bytes),
            "{name}: {value:?}"
        );
        let map = headers(&[(name, value)]);
        let credentials = Credentials::new(&map);
        assert!(credentials.variants.contains(&value.to_owned()), "{name}: {value:?}");
        match credential {
            Some(credential) => {
                assert!(
                    credentials.variants.contains(&credential.to_owned()),
                    "{name}: {value:?} gives {credential:?}"
                );
                let text = format!("sent {credential}|");
                assert_eq!(credentials.redact(&text), "sent ***|", "{name}: {value:?}");
            }
            None => assert!(
                credentials.variants.iter().all(|form| form.starts_with(value.trim())),
                "{name}: {value:?} has only its own forms: {:?}",
                credentials.variants
            ),
        }
    }
    assert_eq!(after_scheme(b""), None);
    assert_eq!(after_scheme(b" \t "), None);
}

/// A header is a credential by the same rule the log events redact with:
/// a secret name, `token` or `secret` in the name, or a value flagged
/// sensitive. Any other header, and an empty value, is not.
#[test]
fn only_secret_or_sensitive_headers_are_credentials() {
    let mut map = headers(&[
        ("x-client-secret", "one-secret"),
        ("x-mixed-token", "two-token"),
        ("x-default", "not-a-credential"),
        ("x-api-key", ""),
        ("cookie", "session=three"),
    ]);
    map.append("x-other", sensitive("four-flagged"));
    let credentials = Credentials::new(&map);

    for credential in ["one-secret", "two-token", "four-flagged", "session=three"] {
        assert!(credentials.variants.contains(&credential.to_owned()), "{credential}");
    }
    assert!(
        credentials.variants.iter().all(|form| !form.contains("not-a-credential")),
        "{:?}",
        credentials.variants
    );
    assert!(credentials.variants.iter().all(|form| !form.is_empty()));
    let mut sorted = credentials.variants.clone();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    sorted.dedup();
    assert_eq!(credentials.variants, sorted, "distinct, longest first");

    let none = Credentials::new(&headers(&[("x-default", "value"), ("x-api-key", "")]));
    assert!(none.variants.is_empty());
    assert!(!none.occur_in("value"));
    assert_eq!(none.redact("value"), "value");
}

/// Where two credentials overlap, the longer one is replaced, as the Python
/// SDK's alternation sorted longest first does.
#[test]
fn the_longest_variant_wins_where_two_overlap() {
    let credentials = Credentials::new(&headers(&[("x-api-key", "ab"), ("api-key", "abc")]));
    assert_eq!(credentials.redact("xabcx"), "x***x");
    assert_eq!(credentials.redact("xabx abc ab"), "x***x *** ***");
    assert_eq!(credentials.matches("abcab").collect::<Vec<_>>(), [0..3, 3..5]);
}

// --------------------------------------------------------------- chains

/// Upstream `test_exception_redaction_preserves_network_diagnostics`: a
/// chain that holds no credential is the transport's own, whatever its
/// depth. Two contrast cases: a one-character secret header value turns a
/// clean diagnostic into a copy, and a credential the message's escaping
/// forms by chance replaces the message and keeps the chain.
#[test]
fn a_chain_without_a_credential_is_kept_as_it_is() {
    let base = headers(&[("x-default", "visible")]);
    let mut with_key = base.clone();
    with_key.insert("authorization", sensitive("Bearer test-key"));
    let credentials = Credentials::new(&with_key);

    let unreachable = Link::plain(
        "ConnectError",
        Some(Link::plain("Network is unreachable (os error 101)", None)),
    );
    let error = failed(unreachable, &with_key, &[]);
    assert!(matches!(error.kind(), ErrorKind::Connection), "{error:?}");
    assert_eq!(
        error.to_string(),
        "Connection error: ConnectError: Network is unreachable (os error 101)"
    );
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<Link>().is_some(), "the original is kept: {source:?}");
    assert_eq!(
        source.source().map(ToString::to_string).as_deref(),
        Some("Network is unreachable (os error 101)")
    );
    assert_no_variant(&error, &credentials);

    // A secret header value of `1` is a substring of the diagnostic, which
    // is then a copy with the digit replaced, and no longer an `io::Error`.
    let short = headers(&[("x-csrf-token", "1")]);
    let refused =
        io::Error::new(io::ErrorKind::ConnectionRefused, "Connection refused (os error 61)");
    let error = failed(refused, &short, &[]);
    assert_eq!(error.to_string(), "Connection error: Connection refused (os error 6***)");
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<io::Error>().is_none(), "{source:?}");
    assert_eq!(source.to_string(), "Connection refused (os error 6***)");
    assert_no_variant(&error, &Credentials::new(&short));

    // No link holds `a\tb` spelled with a backslash, but the message writes
    // the link's tab that way: only the message is replaced.
    let mut spelled = with_key.clone();
    spelled.insert("x-api-key", HeaderValue::from_static(r"a\tb"));
    let credentials = Credentials::new(&spelled);
    let error = failed(OpaqueError, &spelled, &[]);
    assert_eq!(error.to_string(), "Connection error: ***");
    for shown in [format!("{error:?}"), format!("{error:#?}")] {
        assert!(!shown.contains(r"a\tb") && !shown.contains(r"a\\tb"), "{shown}");
    }
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<OpaqueError>().is_some(), "the original is kept: {source:?}");
    assert_eq!(source.to_string(), "a\tb");
    assert_no_variant(&error, &credentials);

    // The fixed `Connection error: ` prefix is the SDK's own text and is
    // never scanned: a secret that occurs only there keeps the chain and the
    // message, and a message-only replacement leaves the prefix whole.
    let prefix_only = headers(&[("x-csrf-token", "nect")]);
    let error = failed(Link::plain("refused", None), &prefix_only, &[]);
    assert_eq!(error.to_string(), "Connection error: refused");
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<Link>().is_some(), "the original is kept: {source:?}");

    let mut spelled_and_short = spelled.clone();
    spelled_and_short.insert("x-csrf-token", HeaderValue::from_static("on"));
    let error = failed(OpaqueError, &spelled_and_short, &[]);
    assert_eq!(error.to_string(), "Connection error: ***");
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<OpaqueError>().is_some(), "the original is kept: {source:?}");
    assert_no_variant(&error, &credentials);

    // A link that holds the secret is replaced by a copy; the prefix of its
    // message still reads as the SDK wrote it.
    let short = headers(&[("x-csrf-token", "on")]);
    let error = failed(Link::plain("connection refused", None), &short, &[]);
    assert_eq!(error.to_string(), "Connection error: c***necti*** refused");
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<Link>().is_none(), "a copy: {source:?}");
    assert_eq!(source.to_string(), "c***necti*** refused");
}

/// A chain longer than the scan limit cannot be shown to be clean, so it is
/// replaced by a copy of its first [`MAX_SCANNED_LINKS`] links, even with no
/// credential in it. The message is the one the original would have had.
#[test]
fn a_chain_longer_than_the_scan_limit_is_always_replaced() {
    let mut chain = None;
    for index in (0..40).rev() {
        chain = Some(Link::plain(&format!("link {index}"), chain));
    }
    let chain = chain.expect("forty links");
    let map = headers(&[("authorization", "Bearer test-key")]);
    let error = failed(chain, &map, &[]);

    let expected: Vec<String> = (0..8).map(|index| format!("link {index}")).collect();
    assert_eq!(error.to_string(), format!("Connection error: {}", expected.join(": ")));
    let links: Vec<String> =
        std::iter::successors(StdError::source(&error), |link: &&(dyn StdError + 'static)| {
            (*link).source()
        })
        .map(ToString::to_string)
        .collect();
    let copied: Vec<String> = (0..MAX_SCANNED_LINKS).map(|index| format!("link {index}")).collect();
    assert_eq!(links, copied);
    let source = StdError::source(&error).expect("a cause");
    assert!(source.downcast_ref::<Link>().is_none(), "a copy: {source:?}");
    assert!(source.downcast_ref::<RedactedLink>().is_some(), "{source:?}");
    assert_no_variant(&error, &Credentials::new(&map));
}

/// A credential in one link's `{:?}` only, or in its `{:#?}` only, is enough
/// to replace the chain. The copy prints each link's redacted text in the
/// form the formatter asks for, and keeps the links in order.
#[test]
fn a_redacted_link_prints_its_redacted_debug_and_keeps_the_chain_order() {
    let map = headers(&[("x-api-key", "sk-live")]);
    let credentials = Credentials::new(&map);
    let cases = [
        ("debug", Link::new("second", "Second { key: sk-live }", "Second {\n    ..\n}", None)),
        ("alternate", Link::new("second", "Second", "Second {\n    key: \"sk-live\",\n}", None)),
    ];
    for (case, second) in cases {
        let chain = Link::plain("first", Some(second));
        let chain = Link::new("zeroth", "Zeroth", "Zeroth {}", Some(chain));
        let error = failed(chain, &map, &[]);

        assert_eq!(error.to_string(), "Connection error: zeroth: first: second", "{case}");
        let links: Vec<&(dyn StdError + 'static)> =
            std::iter::successors(StdError::source(&error), |link: &&(dyn StdError + 'static)| {
                (*link).source()
            })
            .collect();
        let shown: Vec<[String; 3]> = links
            .iter()
            .map(|link| [link.to_string(), format!("{link:?}"), format!("{link:#?}")])
            .collect();
        let expected_second = match case {
            "debug" => ["second", "Second { key: *** }", "Second {\n    ..\n}"],
            _ => ["second", "Second", "Second {\n    key: \"***\",\n}"],
        };
        assert_eq!(
            shown,
            [
                ["zeroth", "Zeroth", "Zeroth {}"].map(str::to_owned),
                ["first"; 3].map(str::to_owned),
                expected_second.map(str::to_owned),
            ],
            "{case}"
        );
        assert!(links.iter().all(|link| link.downcast_ref::<Link>().is_none()), "{case}");
        let alternate = format!("{error:#?}");
        assert!(alternate.contains("Zeroth {}"), "{case}: {alternate}");
        assert_no_variant(&error, &credentials);
    }
}
