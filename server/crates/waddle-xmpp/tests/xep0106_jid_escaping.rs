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

use waddle_xmpp::xep::xep0106::{escape_node, is_escaped, needs_escaping, unescape_node};

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
    assert_eq!(escape_node(" "), "\\20");
    assert_eq!(escape_node("\""), "\\22");
    assert_eq!(escape_node("&"), "\\26");
    assert_eq!(escape_node("'"), "\\27");
    assert_eq!(escape_node("/"), "\\2f");
    assert_eq!(escape_node(":"), "\\3a");
    assert_eq!(escape_node("<"), "\\3c");
    assert_eq!(escape_node(">"), "\\3e");
    assert_eq!(escape_node("@"), "\\40");
    assert_eq!(escape_node("\\"), "\\5c");
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

// ── Backslash treatment (§4 corner case) ─────────────────────────────

#[test]
fn xep0106_existing_escape_sequences_preserved_not_double_escaped() {
    // §4: a backslash that already begins a valid escape sequence
    // must NOT be re-escaped, otherwise round-trips through escape →
    // unescape would lose information.
    assert_eq!(escape_node("user\\40host"), "user\\40host");
    assert_eq!(escape_node("a\\20b"), "a\\20b");
}

#[test]
fn xep0106_lone_backslash_or_invalid_followup_is_escaped_as_5c() {
    // A backslash not followed by a valid `HH` pair is itself a
    // special character — escape it as `\5c`.
    assert_eq!(escape_node("\\"), "\\5c");
    assert_eq!(escape_node("back\\slash"), "back\\5cslash");
    // Invalid hex pair (`zz` isn't in the §4 table) → escape the
    // backslash and pass the rest through.
    assert_eq!(escape_node("a\\zzb"), "a\\5czzb");
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
        result.ends_with("\\5c"),
        "trailing backslash with no valid followup is escaped: {result:?}"
    );
}

#[test]
fn xep0106_escape_preserves_existing_escape_after_multibyte_prefix() {
    // §4 escape-detection has to work correctly even when the bytes
    // BEFORE the backslash include multi-byte UTF-8. The previous
    // revision would misalign the lookahead slice and either panic
    // or detect the wrong "next two chars".
    assert_eq!(escape_node("éé\\20"), "éé\\20");
}

#[test]
fn xep0106_escape_handles_multibyte_chars_with_special_chars_intermixed() {
    // Multi-byte char, then a special char, then a multi-byte char.
    // Each special character's lookahead (the backslash check) must
    // stay byte-correct regardless of where multi-byte chars fall.
    assert_eq!(escape_node("é@é"), "é\\40é");
    assert_eq!(escape_node("é\\é"), "é\\5cé");
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
