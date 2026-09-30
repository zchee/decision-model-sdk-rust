//! Tests for header redaction.
//!
//! The name cases are the parameters of the upstream Python SDK's
//! `tests/test_logging.py::test_secret_headers_redacted`. Upstream asserts on
//! captured log text; here the rendering is asserted whole, so a secret that
//! leaked and a visible value that went missing both fail.

use super::*;

// ------------------------------------------------------------- fixtures

/// A map from `(name, value)` pairs, appending so a repeated name keeps every
/// value.
fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        let name = HeaderName::from_bytes(name.as_bytes())
            .unwrap_or_else(|error| panic!("{name:?} is not a header name: {error}"));
        let value = HeaderValue::from_str(value)
            .unwrap_or_else(|error| panic!("{value:?} is not a header value: {error}"));
        map.append(name, value);
    }
    map
}

fn debug(map: &HeaderMap) -> String {
    format!("{:?}", redact(map))
}

// ------------------------------------------------------------ the rules

/// Upstream's nine header spellings: the six credential names, the `token`
/// and `secret` substrings, and a mixed-case spelling. `http` lower-cases a
/// name when it parses it, so the rendering shows the lower-cased form.
#[test]
fn every_upstream_secret_header_is_redacted_and_its_neighbour_is_not() {
    let names = [
        "Authorization",
        "Proxy-Authorization",
        "X-API-Key",
        "API-Key",
        "Cookie",
        "Set-Cookie",
        "X-Access-Token",
        "X-Client-Secret",
        "x-MiXeD-ToKeN",
    ];
    for name in names {
        let map = headers(&[(name, "request-credential"), ("x-visible", "request-visible")]);
        let lower = name.to_ascii_lowercase();

        assert_eq!(
            debug(&map),
            format!(r#"{{"{lower}": "***", "x-visible": "request-visible"}}"#),
            "header {name}"
        );
    }
}

/// The substring rule matches anywhere in the name; the credential names match
/// only exactly, as upstream matches them, so a name that merely contains one
/// of them is printed.
#[test]
fn the_substring_rule_is_token_and_secret_only() {
    let secret = ["token", "x-tokens", "tokenized", "secret", "x-secrets-hash", "client-secret-id"];
    for name in secret {
        let map = headers(&[(name, "value")]);
        assert_eq!(debug(&map), format!(r#"{{"{name}": "***"}}"#), "{name} must be redacted");
    }

    let visible =
        ["x-toke", "x-secre", "x-authorization", "authorization-hint", "cookies", "x-cookie"];
    for name in visible {
        let map = headers(&[(name, "value")]);
        assert_eq!(debug(&map), format!(r#"{{"{name}": "value"}}"#), "{name} must be printed");
    }
}

/// A value flagged sensitive is hidden under any name: this is the port's own
/// rule, on top of upstream's name rules.
#[test]
fn a_value_flagged_sensitive_is_redacted_under_any_name() {
    let mut map = headers(&[("x-visible", "shown")]);
    let mut hidden = HeaderValue::from_static("flagged-credential");
    hidden.set_sensitive(true);
    map.insert("x-innocent", hidden);

    assert_eq!(debug(&map), r#"{"x-visible": "shown", "x-innocent": "***"}"#);
}

/// A repeated name is rendered once per value, in order, and redaction
/// applies to each of them.
#[test]
fn every_value_of_a_repeated_name_is_rendered_and_redacted() {
    let map = headers(&[
        ("set-cookie", "session=first-secret"),
        ("x-tag", "a"),
        ("set-cookie", "csrf=second-secret"),
        ("x-tag", "b"),
    ]);

    assert_eq!(
        debug(&map),
        r#"{"set-cookie": "***", "set-cookie": "***", "x-tag": "a", "x-tag": "b"}"#
    );
}

// ------------------------------------------------------------- rendering

#[test]
fn an_empty_map_renders_as_empty_braces() {
    let map = HeaderMap::new();
    assert_eq!(debug(&map), "{}");
}

/// A byte above 0x7F is escaped, so a value cannot put arbitrary bytes into a
/// log line. A tab is the one control character `http` admits in a value; it
/// is written as it is, which cannot break a line.
#[test]
fn a_value_with_a_byte_above_ascii_is_escaped() {
    let mut map = HeaderMap::new();
    let value = HeaderValue::from_bytes(b"caf\xe9\tlatte")
        .expect("invariant: http accepts obs-text and tab in a value");
    map.insert("x-order", value);
    map.append("x-order", HeaderValue::from_static("tea\tpot"));

    assert_eq!(debug(&map), "{\"x-order\": \"caf\\xe9\tlatte\", \"x-order\": \"tea\tpot\"}");
}

/// A rendering of a map holding a real-looking key contains no piece of that
/// key: not the whole, not the part after `Bearer `, and no run of four of
/// its characters.
#[test]
fn a_real_looking_key_leaves_no_trace_in_the_rendering() {
    let key = "sk-live-9f8e7d6c5b4a3210ZYXWVUTSRQ";
    let bearer = format!("Bearer {key}");
    let map = headers(&[
        ("authorization", bearer.as_str()),
        ("x-api-key", key),
        ("x-request-id", "req-123"),
    ]);

    let rendered = debug(&map);
    assert!(rendered.contains("req-123"), "a visible value went missing: {rendered}");
    assert!(!rendered.contains("Bearer"), "the scheme leaked: {rendered}");
    let characters: Vec<char> = key.chars().collect();
    for window in characters.windows(4) {
        let piece: String = window.iter().collect();
        assert!(!rendered.contains(&piece), "{piece:?} of the key leaked: {rendered}");
    }
}

/// The view borrows the map and is `Copy`: it can be built once and handed to
/// several formatting calls without cloning anything.
#[test]
fn the_view_is_a_copyable_borrow() {
    let map = headers(&[("x-a", "1")]);
    let view = redact(&map);
    let copy = view;
    assert_eq!(format!("{view:?} {copy:?}"), r#"{"x-a": "1"} {"x-a": "1"}"#);
    assert_eq!(size_of::<RedactedHeaders<'_>>(), size_of::<&HeaderMap>());
}
