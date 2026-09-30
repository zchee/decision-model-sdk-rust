//! Tests for the rendering of text this SDK did not write. The field-path
//! rules it serves are pinned by the codec's own tests, which run through it.

use super::*;
use crate::rendering_tests::assert_printable;

#[test]
fn control_and_format_characters_are_escaped_and_printable_text_is_kept() {
    let mut text = SafeText::new(usize::MAX, Backslash::Double);
    text.untrusted("a\nb\r\tc\u{1b}[31m\u{202e}d\u{85}e caf\u{e9} \u{54c1}", usize::MAX);
    let rendered = text.into_string();
    assert_eq!(rendered, "a\\nb\\r\\tc\\u{1b}[31m\\u{202e}d\\u{85}e caf\u{e9} \u{54c1}");
    assert_printable(&rendered);
}

#[test]
fn a_backslash_is_doubled_or_kept_as_asked() {
    let mut doubled = SafeText::new(usize::MAX, Backslash::Double);
    doubled.untrusted(r"C:\path \u{1b}", usize::MAX);
    assert_eq!(doubled.into_string(), r"C:\\path \\u{1b}");

    let mut kept = SafeText::new(usize::MAX, Backslash::Keep);
    kept.untrusted("already \\n escaped, a real \n", usize::MAX);
    assert_eq!(kept.into_string(), r"already \n escaped, a real \n");
}

#[test]
fn a_cut_counts_escapes_whole_and_is_marked() {
    // The whole limit: 5 characters, and `\n` is two of them.
    let mut text = SafeText::new(5, Backslash::Double);
    text.untrusted("abc\n", usize::MAX);
    assert_eq!(text.into_string(), "abc\\n", "exactly at the limit is not cut");

    let mut text = SafeText::new(5, Backslash::Double);
    text.untrusted("abc\nd", usize::MAX);
    assert_eq!(text.into_string(), "abc\\n\u{2026}");

    let mut text = SafeText::new(5, Backslash::Double);
    text.untrusted("abcd\n", usize::MAX);
    assert_eq!(text.into_string(), "abcd\u{2026}", "the escape is dropped whole, not split");

    // A piece's own cap, inside a larger limit.
    let mut text = SafeText::new(100, Backslash::Double);
    text.untrusted("abcdef", 3);
    text.fixed(".");
    text.untrusted("xy", 3);
    assert_eq!(text.into_string(), "abc\u{2026}.xy");
}

#[test]
fn a_prefix_is_kept_and_not_counted() {
    let mut text = SafeText::after("Connection error: ".to_owned(), 3, Backslash::Keep);
    text.untrusted("abcdef", usize::MAX);
    assert_eq!(text.into_string(), "Connection error: abc\u{2026}");
}

#[test]
fn a_display_streams_through_the_writer_and_stops_at_the_limit() {
    let long = "x".repeat(100_000);
    let mut text = SafeText::new(10, Backslash::Keep);
    write!(text.untrusted_writer(), "\u{1b}{long}").expect("the writer never fails");
    // A later write after the limit is dropped too, and still succeeds.
    write!(text.untrusted_writer(), "more").expect("the writer never fails");
    assert_eq!(text.into_string(), format!("\\u{{1b}}{}\u{2026}", "x".repeat(4)));
}
