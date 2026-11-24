//! XEP-0106: JID Escaping — dedicated conformance suite.
//!
//! Pins:
//! - the §4 character → escape-sequence table,
//! - round-trip stability (every input through `escape_node` →
//!   `unescape_node` returns the original),
//! - the Unicode-safety guarantee that the in-crate tests left
//!   uncovered: `escape_node` must operate on byte offsets internally
//!   even when iterating chars. A previous revision called
//!   `enumerate()` over `chars()` and then sliced `input[i + 1..]`,
//!   conflating char index with byte offset. For any input with two
//!   non-ASCII chars before a backslash, that slice landed inside a
//!   UTF-8 continuation byte and `escape_node` panicked. The
//!   regression tests below feed it exactly those shapes so a
//!   future copy-paste of the buggy pattern fails CI.

use waddle_xmpp::xep::xep0106::{
    escape_node as try_escape_node, is_escaped, needs_escaping, unescape_node, EscapeError,
};

/// Escape a localpart the test expects to be escapable.
fn escape_node(input: &str) -> String {
    try_escape_node(input).expect("input is escapable")
}

// ── §4 spec example table ────────────────────────────────────────────

#[test]
fn xep0106_section_4_examples() {
    // The character → escape mapping is verbatim from XEP-0106 §4.
    // These are the test vectors the spec itself ships.
    assert_eq!(escape_node("space cadet"), "space\\20cadet");
    assert_eq!(
        escape_node("call me \"ishmael\""),
        "call\\20me\\20\\22ishmael\\22"
    );
    assert_eq!(escape_node("at&t guy"), "at\\26t\\20guy");
    assert_eq!(escape_node("d'artagnan"), "d\\27artagnan");
    assert_eq!(escape_node("/.telekinesis"), "\\2f.telekinesis");
    assert_eq!(escape_node("thing1 & thing2"), "thing1\\20\\26\\20thing2");
}

#[test]
fn xep0106_each_special_char_maps_to_documented_sequence() {
    // Character-by-character coverage of the §4 mapping table.
    // A lone space is a boundary space and is rejected; see
    // `xep0106_leading_or_trailing_space_is_rejected`.
    assert_eq!(escape_node("a b"), "a\\20b");
    assert_eq!(escape_node("\""), "\\22");
    assert_eq!(escape_node("&"), "\\26");
    assert_eq!(escape_node("'"), "\\27");
    assert_eq!(escape_node("/"), "\\2f");
    assert_eq!(escape_node(":"), "\\3a");
    assert_eq!(escape_node("<"), "\\3c");
    assert_eq!(escape_node(">"), "\\3e");
    assert_eq!(escape_node("@"), "\\40");
    // A backslash is only escaped when it would otherwise start an
    // escape sequence (business rule 7); `\5c` itself comes from `\\5c`.
    assert_eq!(escape_node("\\5c"), "\\5c5c");
}

#[test]
fn xep0106_unescape_reverses_each_documented_sequence() {
    assert_eq!(unescape_node("\\20"), " ");
    assert_eq!(unescape_node("\\22"), "\"");
    assert_eq!(unescape_node("\\26"), "&");
    assert_eq!(unescape_node("\\27"), "'");
    assert_eq!(unescape_node("\\2f"), "/");
    assert_eq!(unescape_node("\\3a"), ":");
    assert_eq!(unescape_node("\\3c"), "<");
    assert_eq!(unescape_node("\\3e"), ">");
    assert_eq!(unescape_node("\\40"), "@");
    assert_eq!(unescape_node("\\5c"), "\\");
}

// ── Backslash treatment (business rule 7, examples 9-12) ─────────────

#[test]
fn xep0106_backslash_starting_an_escape_sequence_is_escaped_to_5c() {
    // Business rule 7 / algorithm step 3: when the source address
    // contains a sequence that corresponds to an escaping, the leading
    // backslash MUST be escaped to `\5c` so it is not unescaped later.
    assert_eq!(escape_node("user\\40host"), "user\\5c40host");
    assert_eq!(escape_node("a\\20b"), "a\\5c20b");
    assert_eq!(escape_node("\\27"), "\\5c27");
    assert_eq!(escape_node("\\5c"), "\\5c5c");
    // Every sequence in the escape table is covered.
    for hex in ["20", "22", "26", "27", "2f", "3a", "3c", "3e", "40", "5c"] {
        let source = format!("x\\{hex}y");
        let escaped = escape_node(&source);
        assert_eq!(escaped, format!("x\\5c{hex}y"));
        assert_eq!(unescape_node(&escaped), source, "round trip for \\{hex}");
    }
}

#[test]
fn xep0106_backslash_not_starting_an_escape_sequence_is_left_alone() {
    // Exceptions: partial and unlisted sequences are not modified.
    assert_eq!(escape_node("\\"), "\\");
    assert_eq!(escape_node("foo\\bar"), "foo\\bar");
    assert_eq!(escape_node("a\\zzb"), "a\\zzb");
    assert_eq!(escape_node("\\2plus\\2is\\4"), "\\2plus\\2is\\4");
    assert_eq!(escape_node("foob\\41r"), "foob\\41r");
    assert_eq!(unescape_node("foob\\41r"), "foob\\41r");
}

#[test]
fn xep0106_section_examples_9_to_12_backslash_rows() {
    assert_eq!(escape_node("c:\\net"), "c\\3a\\net");
    assert_eq!(escape_node("c:\\\\net"), "c\\3a\\\\net");
    assert_eq!(escape_node("c:\\cool stuff"), "c\\3a\\cool\\20stuff");
    assert_eq!(escape_node("c:\\5commas"), "c\\3a\\5c5commas");
    assert_eq!(unescape_node("c\\3a\\5c5commas"), "c:\\5commas");
    // Wireless Village example from the algorithm discussion.
    assert_eq!(escape_node("\\3and\\2is\\5cool"), "\\5c3and\\2is\\5c5cool");
}

// ── Boundary space (business rule 6) ─────────────────────────────────

#[test]
fn xep0106_leading_or_trailing_space_is_rejected() {
    // `\20` MUST NOT be the first or last character of an escaped
    // localpart, so such sources have no valid escaped form.
    for input in [" ops", "ops ", " ", "  ", " a b "] {
        assert_eq!(
            try_escape_node(input),
            Err(EscapeError::BoundarySpace),
            "{input:?}"
        );
    }
    // Interior spaces are fine.
    assert_eq!(escape_node("ops team"), "ops\\20team");
    // A source that merely contains `\20` text is not a boundary space.
    assert_eq!(escape_node("\\20x"), "\\5c20x");
}

// ── Unicode safety (regression: char/byte index conflation panic) ────

#[test]
fn xep0106_escape_does_not_panic_with_multibyte_chars_before_backslash() {
    // The exact shape that broke `enumerate()`+byte-slice mixing:
    // two 2-byte UTF-8 chars push the char index 2 positions behind
    // the byte offset; at the backslash, `input[char_idx + 1..]`
    // landed inside a UTF-8 continuation byte and panicked.
    //
    // We don't assert a specific output here beyond "didn't panic
    // and produced something" — the spec doesn't prescribe how
    // non-XEP-0106 chars round-trip through escape. The point is
    // that the call MUST complete without unwinding.
    let result = escape_node("éé\\");
    assert!(
        result.starts_with("éé"),
        "non-special prefix is preserved: {result:?}"
    );
    assert!(
        result.ends_with('\\'),
        "trailing backslash with no valid followup is left alone: {result:?}"
    );
}

#[test]
fn xep0106_escape_preserves_existing_escape_after_multibyte_prefix() {
    // §4 escape-detection has to work correctly even when the bytes
    // BEFORE the backslash include multi-byte UTF-8. The previous
    // revision would misalign the lookahead slice and either panic
    // or detect the wrong "next two chars".
    assert_eq!(escape_node("éé\\20x"), "éé\\5c20x");
}

#[test]
fn xep0106_escape_handles_multibyte_chars_with_special_chars_intermixed() {
    // Multi-byte char, then a special char, then a multi-byte char.
    // Each special character's lookahead (the backslash check) must
    // stay byte-correct regardless of where multi-byte chars fall.
    assert_eq!(escape_node("é@é"), "é\\40é");
    assert_eq!(escape_node("é\\é"), "é\\é");
    assert_eq!(escape_node("é\\40é"), "é\\5c40é");
    assert_eq!(escape_node("é é"), "é\\20é");
}

#[test]
fn xep0106_escape_terminates_cleanly_on_multibyte_only_input() {
    // No special chars at all — every char should pass through
    // unchanged. This also exercises the no-match arm of the loop
    // for non-ASCII bytes.
    assert_eq!(escape_node("éàü"), "éàü");
    assert_eq!(escape_node(""), "");
    assert_eq!(escape_node("simple"), "simple");
}

// ── Round-trip stability ────────────────────────────────────────────

#[test]
fn xep0106_round_trip_through_escape_unescape_returns_original() {
    let inputs: &[&str] = &[
        // ASCII-only specials
        "user@host",
        "space cadet",
        "\"quoted\"",
        "a&b",
        "it's me",
        "a/b/c",
        "x:y:z",
        "<tag>",
        "back\\slash",
        // Mix of every special at once
        "all @\"&'/::<>@\\ chars",
        // Unicode + specials
        "café@example.com",
        "naïve user",
        "用户@主机",
        // Bare multi-byte
        "éà",
        // Empty
        "",
    ];

    for input in inputs {
        let escaped = escape_node(input);
        let restored = unescape_node(&escaped);
        assert_eq!(
            &restored, input,
            "round-trip failed: input={input:?}, escaped={escaped:?}, restored={restored:?}"
        );
    }
}

// ── Classifier helpers ──────────────────────────────────────────────

#[test]
fn xep0106_needs_escaping_detects_each_special_char() {
    for c in [' ', '"', '&', '\'', '/', ':', '<', '>', '@', '\\'] {
        let one = c.to_string();
        assert!(
            needs_escaping(&one),
            "needs_escaping must detect {c:?} as requiring an escape"
        );
    }
    assert!(!needs_escaping("simple"));
    assert!(!needs_escaping(""));
    // Multi-byte chars are not §4 specials, so don't trigger.
    assert!(!needs_escaping("éà"));
}

#[test]
fn xep0106_is_escaped_detects_at_least_one_valid_sequence() {
    assert!(is_escaped("user\\40host"));
    assert!(is_escaped("space\\20cadet"));
    assert!(is_escaped("\\5c"));
    assert!(!is_escaped("simple"));
    // `\zz` is not in §4's table — must not be classified as escaped.
    assert!(!is_escaped("a\\zzb"));
    // Multi-byte chars don't confuse the detector.
    assert!(is_escaped("café\\40example"));
    assert!(!is_escaped("café"));
}
