// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Text sanitization for terminal safety.
//!
//! All server-provided strings (message bodies, nicknames, embed content,
//! etc.) MUST be passed through [`sanitize_for_terminal`] before storing or
//! rendering in the TUI. This prevents terminal escape sequence injection
//! attacks where a malicious user could embed ANSI escape codes in a message.

/// Returns true if `ch` is a Unicode bidirectional override/isolate character
/// that can be used for display spoofing (e.g. making text appear right-to-left
/// to disguise a URL or filename).
fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{200E}' // LEFT-TO-RIGHT MARK
        | '\u{200F}' // RIGHT-TO-LEFT MARK
        | '\u{202A}' // LEFT-TO-RIGHT EMBEDDING
        | '\u{202B}' // RIGHT-TO-LEFT EMBEDDING
        | '\u{202C}' // POP DIRECTIONAL FORMATTING
        | '\u{202D}' // LEFT-TO-RIGHT OVERRIDE
        | '\u{202E}' // RIGHT-TO-LEFT OVERRIDE
        | '\u{2066}' // LEFT-TO-RIGHT ISOLATE
        | '\u{2067}' // RIGHT-TO-LEFT ISOLATE
        | '\u{2068}' // FIRST STRONG ISOLATE
        | '\u{2069}' // POP DIRECTIONAL ISOLATE
    )
}

/// Skip a string-terminated escape sequence (DCS, SOS, PM, APC, OSC).
/// These are terminated by ST (ESC \) or, for OSC, also BEL (\x07).
fn skip_string_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, allow_bel: bool) {
    for c in chars.by_ref() {
        if allow_bel && c == '\x07' {
            break;
        }
        if c == '\x1b' {
            if chars.peek() == Some(&'\\') {
                chars.next(); // consume the backslash (ST terminator)
            }
            break;
        }
    }
}

/// Strip ANSI escape sequences, control characters, and bidi overrides.
///
/// Keeps printable characters, newlines, and tabs. Removes:
/// - ANSI CSI sequences (`ESC [ ... final_byte`)
/// - OSC sequences (`ESC ] ... BEL/ST`)
/// - DCS sequences (`ESC P ... ST`)
/// - SOS sequences (`ESC X ... ST`)
/// - PM sequences (`ESC ^ ... ST`)
/// - APC sequences (`ESC _ ... ST`)
/// - Other escape-initiated sequences (`ESC` followed by a single byte)
/// - ASCII control characters (0x00-0x08, 0x0B-0x0C, 0x0E-0x1F, 0x7F)
/// - Unicode bidirectional override/isolate characters
///
/// Truncates to `max_len` bytes (if provided) at a valid UTF-8 boundary.
pub fn sanitize_for_terminal(input: &str, max_len: Option<usize>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: ESC [ ... (final byte in 0x40-0x7E)
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: ESC ] ... (terminated by BEL or ST)
                    chars.next();
                    skip_string_sequence(&mut chars, true);
                }
                Some('P') => {
                    // DCS (Device Control String): ESC P ... ST
                    chars.next();
                    skip_string_sequence(&mut chars, false);
                }
                Some('X') => {
                    // SOS (Start of String): ESC X ... ST
                    chars.next();
                    skip_string_sequence(&mut chars, false);
                }
                Some('^') => {
                    // PM (Privacy Message): ESC ^ ... ST
                    chars.next();
                    skip_string_sequence(&mut chars, false);
                }
                Some('_') => {
                    // APC (Application Program Command): ESC _ ... ST
                    chars.next();
                    skip_string_sequence(&mut chars, false);
                }
                Some(_) => {
                    // Other escape sequence (e.g. ESC c for RIS): skip next char
                    chars.next();
                }
                None => {}
            }
            continue;
        }

        // Strip bidi override/isolate characters
        if is_bidi_control(ch) {
            continue;
        }

        // Allow printable chars, newline, tab
        if ch == '\n' || ch == '\t' || (!ch.is_control()) {
            out.push(ch);
        }
        // else: drop control character (including DEL 0x7F)
    }

    // Truncate at valid UTF-8 boundary
    if let Some(max) = max_len {
        if out.len() > max {
            let mut end = max;
            while end > 0 && !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(sanitize_for_terminal("Hello, world!", None), "Hello, world!");
    }

    #[test]
    fn strips_ansi_csi_bold() {
        assert_eq!(sanitize_for_terminal("\x1b[1mBold\x1b[0m", None), "Bold");
    }

    #[test]
    fn strips_ansi_color() {
        assert_eq!(sanitize_for_terminal("\x1b[31mRed\x1b[0m", None), "Red");
    }

    #[test]
    fn strips_osc_title_with_bel() {
        assert_eq!(
            sanitize_for_terminal("\x1b]0;evil title\x07safe text", None),
            "safe text"
        );
    }

    #[test]
    fn strips_osc_title_with_st() {
        assert_eq!(
            sanitize_for_terminal("\x1b]0;evil title\x1b\\safe text", None),
            "safe text"
        );
    }

    #[test]
    fn strips_dcs_sequence() {
        assert_eq!(
            sanitize_for_terminal("\x1bPsome device control\x1b\\ok", None),
            "ok"
        );
    }

    #[test]
    fn strips_sos_sequence() {
        assert_eq!(
            sanitize_for_terminal("\x1bXstart of string\x1b\\ok", None),
            "ok"
        );
    }

    #[test]
    fn strips_pm_sequence() {
        assert_eq!(
            sanitize_for_terminal("\x1b^privacy message\x1b\\ok", None),
            "ok"
        );
    }

    #[test]
    fn strips_apc_sequence() {
        assert_eq!(
            sanitize_for_terminal("\x1b_app command\x1b\\ok", None),
            "ok"
        );
    }

    #[test]
    fn strips_control_chars() {
        assert_eq!(
            sanitize_for_terminal("hello\x00\x01\x02world", None),
            "helloworld"
        );
    }

    #[test]
    fn preserves_newlines_and_tabs() {
        assert_eq!(
            sanitize_for_terminal("line1\nline2\ttab", None),
            "line1\nline2\ttab"
        );
    }

    #[test]
    fn strips_bidi_overrides() {
        // RLO + LRO characters around text
        assert_eq!(
            sanitize_for_terminal("hello\u{202E}evil\u{202C}world", None),
            "helloevilworld"
        );
    }

    #[test]
    fn strips_bidi_isolates() {
        assert_eq!(
            sanitize_for_terminal("a\u{2066}b\u{2069}c", None),
            "abc"
        );
    }

    #[test]
    fn truncates_at_max_len() {
        assert_eq!(sanitize_for_terminal("Hello, world!", Some(5)), "Hello");
    }

    #[test]
    fn truncates_at_utf8_boundary() {
        // '🐧' is 4 bytes, 'h' is 1 byte = 5 bytes total fits in max 5
        let input = "🐧hello";
        let result = sanitize_for_terminal(input, Some(5));
        assert_eq!(result, "🐧h");

        // But max 3 can't fit the penguin (4 bytes)
        let result = sanitize_for_terminal(input, Some(3));
        assert_eq!(result, "");
    }

    #[test]
    fn empty_input() {
        assert_eq!(sanitize_for_terminal("", None), "");
    }

    #[test]
    fn complex_escape_combo() {
        assert_eq!(
            sanitize_for_terminal("\x1b[2J\x1b[H\x1b[31;1mevil\x1b[0m ok", None),
            "evil ok"
        );
    }

    #[test]
    fn strips_delete_char() {
        assert_eq!(sanitize_for_terminal("hello\x7fworld", None), "helloworld");
    }

    #[test]
    fn unicode_preserved() {
        assert_eq!(
            sanitize_for_terminal("こんにちは 🌍", None),
            "こんにちは 🌍"
        );
    }

    #[test]
    fn nested_escape_sequences() {
        // CSI inside OSC — the OSC is terminated by BEL after "foo"
        assert_eq!(
            sanitize_for_terminal("\x1b]0;title\x07\x1b[1mfoo\x1b[0mbar", None),
            "foobar"
        );
    }

    #[test]
    fn unterminated_osc_consumes_to_end() {
        // OSC without BEL or ST — consumes everything
        assert_eq!(sanitize_for_terminal("\x1b]0;no end", None), "");
    }

    #[test]
    fn ris_escape_stripped() {
        // ESC c = Reset to Initial State — should be stripped
        assert_eq!(sanitize_for_terminal("\x1bcok", None), "ok");
    }

    #[test]
    fn max_len_zero() {
        assert_eq!(sanitize_for_terminal("hello", Some(0)), "");
    }

    #[test]
    fn max_len_larger_than_input() {
        assert_eq!(sanitize_for_terminal("hi", Some(100)), "hi");
    }
}
