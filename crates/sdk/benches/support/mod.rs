//! Inputs shared by the bench targets: the response fixture, the questions it
//! answers, and generated `state` text.
//!
//! Everything here is built outside any measured section.

use typesafe_sdk::{Choice, Noul, PreparedQuestions, Questions, Score};

/// `RESULT` of the upstream `tests/test_clients.py:42-56`: a noul, a choice
/// and a score. The same bytes the allocation budgets are stated on.
pub(crate) const RESULT: &[u8] = include_bytes!("../../tests/fixtures/result.json");

/// The three questions [`RESULT`] answers, prepared once.
pub(crate) fn questions() -> PreparedQuestions {
    Questions::new()
        .noul("spam", Noul::new().instructions("Spam?"))
        .choice("tone", Choice::new(["friendly", "hostile"]).instructions("Tone?"))
        .score("quality", Score::new(["bad", "ok", "great"]).instructions("Quality?"))
        .prepare()
        .expect("the questions prepare")
}

/// Text of exactly `len` bytes that exercises the escaper the way a real
/// `state` does: straight quotes, a newline per sentence, and multi-byte
/// characters (U+00E9, U+20AC, U+2014) among plain words. A run of `x`
/// would take the codec's fastest path and flatter it.
pub(crate) fn text(len: usize) -> String {
    const SENTENCE: &str = "The customer wrote \"I was charged twice\" on the caf\u{e9} invoice \u{2014} 12\u{20ac}.\n";
    let mut text = String::with_capacity(len + SENTENCE.len());
    while text.len() < len {
        text.push_str(SENTENCE);
    }
    // Cut back to a character boundary at or below `len`, then pad with
    // ASCII so the length is exact.
    let mut end = len;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    while text.len() < len {
        text.push(' ');
    }
    text
}
