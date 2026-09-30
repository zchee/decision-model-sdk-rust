//! The check every test of a rendering of text this SDK did not write
//! shares. Only tests compile it. It holds no tests of a `rendering.rs`;
//! the `_tests` suffix keeps this test-only file out of the line-coverage
//! count, as it does for every test file here.

/// No byte a terminal or a log reader would act on, and none of the
/// characters that hide or reorder text.
pub(crate) fn assert_printable(rendered: &str) {
    assert!(
        !rendered.bytes().any(|byte| byte < 0x20 || byte == 0x7f),
        "a control byte reached the text: {rendered:?}"
    );
    for hidden in ['\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}', '\u{2028}', '\u{85}'] {
        assert!(!rendered.contains(hidden), "{hidden:?} reached the text: {rendered:?}");
    }
}
