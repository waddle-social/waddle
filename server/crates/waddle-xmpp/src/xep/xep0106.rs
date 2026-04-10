//! XEP-0106: JID Escaping
//!
//! Provides functions to escape and unescape special characters in the
//! local part (node) of XMPP JIDs. This allows usernames containing
//! characters that would otherwise be invalid in a JID.
//!
//! ## Escaped Characters
//!
//! | Character | Escape Sequence |
//! |-----------|----------------|
//! | ` ` (space) | `\20` |
//! | `"` | `\22` |
//! | `&` | `\26` |
//! | `'` | `\27` |
//! | `/` | `\2f` |
//! | `:` | `\3a` |
//! | `<` | `\3c` |
//! | `>` | `\3e` |
//! | `@` | `\40` |
//! | `\` | `\5c` |
//!
//! ## Examples
//!
//! - `user@host` in local part → `user\40host`
//! - `space cadet` → `space\20cadet`
//! - `"quoted"` → `\22quoted\22`

/// Escape special characters in a JID local part per XEP-0106.
///
/// Transforms characters that are forbidden or special in JID node
/// identifiers into their `\HH` escape sequences.
pub fn escape_node(input: &str) -> String {
    let mut result = String::with_capacity(input.len());

    for (i, ch) in input.chars().enumerate() {
        match ch {
            ' ' => result.push_str("\\20"),
            '"' => result.push_str("\\22"),
            '&' => result.push_str("\\26"),
            '\'' => result.push_str("\\27"),
            '/' => result.push_str("\\2f"),
            ':' => result.push_str("\\3a"),
            '<' => result.push_str("\\3c"),
            '>' => result.push_str("\\3e"),
            '@' => result.push_str("\\40"),
            '\\' => {
                // Check if this backslash is already an escape sequence
                // If followed by a valid 2-digit hex, don't double-escape
                if i + 2 < input.len() {
                    let next_two: String = input[i + 1..].chars().take(2).collect();
                    if is_escape_hex(&next_two) {
                        result.push('\\');
                        continue;
                    }
                }
                result.push_str("\\5c");
            }
            _ => result.push(ch),
        }
    }

    result
}

/// Unescape a JID local part, reversing XEP-0106 escaping.
///
/// Converts `\HH` escape sequences back to their original characters.
pub fn unescape_node(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\\' && i + 2 < chars.len() {
            let hex: String = chars[i + 1..i + 3].iter().collect();
            match hex.as_str() {
                "20" => {
                    result.push(' ');
                    i += 3;
                }
                "22" => {
                    result.push('"');
                    i += 3;
                }
                "26" => {
                    result.push('&');
                    i += 3;
                }
                "27" => {
                    result.push('\'');
                    i += 3;
                }
                "2f" => {
                    result.push('/');
                    i += 3;
                }
                "3a" => {
                    result.push(':');
                    i += 3;
                }
                "3c" => {
                    result.push('<');
                    i += 3;
                }
                "3e" => {
                    result.push('>');
                    i += 3;
                }
                "40" => {
                    result.push('@');
                    i += 3;
                }
                "5c" => {
                    result.push('\\');
                    i += 3;
                }
                _ => {
                    result.push('\\');
                    i += 1;
                }
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    result
}

/// Check if a local part contains characters that need escaping.
pub fn needs_escaping(input: &str) -> bool {
    input.chars().any(|ch| matches!(ch, ' ' | '"' | '&' | '\'' | '/' | ':' | '<' | '>' | '@' | '\\'))
}

/// Check if a local part contains escape sequences.
pub fn is_escaped(input: &str) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 2 < chars.len() {
            let hex: String = chars[i + 1..i + 3].iter().collect();
            if is_escape_hex(&hex) {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_escape_hex(s: &str) -> bool {
    matches!(
        s,
        "20" | "22" | "26" | "27" | "2f" | "3a" | "3c" | "3e" | "40" | "5c"
    )
}

/// Trait for types that can have their node part escaped/unescaped.
pub trait JidEscaping {
    /// The escaped form of this value.
    fn escaped(&self) -> String;
    /// The unescaped form of this value.
    fn unescaped(&self) -> String;
}

impl JidEscaping for str {
    fn escaped(&self) -> String {
        escape_node(self)
    }
    fn unescaped(&self) -> String {
        unescape_node(self)
    }
}

impl JidEscaping for String {
    fn escaped(&self) -> String {
        escape_node(self)
    }
    fn unescaped(&self) -> String {
        unescape_node(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_space() {
        assert_eq!(escape_node("space cadet"), "space\\20cadet");
    }

    #[test]
    fn test_escape_at() {
        assert_eq!(escape_node("user@host"), "user\\40host");
    }

    #[test]
    fn test_escape_quote() {
        assert_eq!(escape_node("\"quoted\""), "\\22quoted\\22");
    }

    #[test]
    fn test_escape_ampersand() {
        assert_eq!(escape_node("a&b"), "a\\26b");
    }

    #[test]
    fn test_escape_slash() {
        assert_eq!(escape_node("a/b"), "a\\2fb");
    }

    #[test]
    fn test_escape_colon() {
        assert_eq!(escape_node("a:b"), "a\\3ab");
    }

    #[test]
    fn test_escape_angle_brackets() {
        assert_eq!(escape_node("<tag>"), "\\3ctag\\3e");
    }

    #[test]
    fn test_escape_apostrophe() {
        assert_eq!(escape_node("it's"), "it\\27s");
    }

    #[test]
    fn test_escape_backslash() {
        assert_eq!(escape_node("back\\slash"), "back\\5cslash");
    }

    #[test]
    fn test_escape_no_change() {
        assert_eq!(escape_node("simple"), "simple");
        assert_eq!(escape_node("user123"), "user123");
    }

    #[test]
    fn test_escape_multiple() {
        assert_eq!(
            escape_node("user@host/resource"),
            "user\\40host\\2fresource"
        );
    }

    #[test]
    fn test_escape_empty() {
        assert_eq!(escape_node(""), "");
    }

    #[test]
    fn test_unescape_space() {
        assert_eq!(unescape_node("space\\20cadet"), "space cadet");
    }

    #[test]
    fn test_unescape_at() {
        assert_eq!(unescape_node("user\\40host"), "user@host");
    }

    #[test]
    fn test_unescape_all() {
        assert_eq!(unescape_node("\\22\\26\\27\\2f\\3a\\3c\\3e\\40\\5c"), "\"&'/:<>@\\");
    }

    #[test]
    fn test_unescape_no_change() {
        assert_eq!(unescape_node("simple"), "simple");
    }

    #[test]
    fn test_unescape_partial_escape() {
        // Backslash not followed by valid escape → preserved
        assert_eq!(unescape_node("a\\zzb"), "a\\zzb");
    }

    #[test]
    fn test_unescape_trailing_backslash() {
        assert_eq!(unescape_node("a\\"), "a\\");
    }

    #[test]
    fn test_roundtrip() {
        let inputs = [
            "simple",
            "space cadet",
            "user@host",
            "\"quoted\"",
            "a&b",
            "it's me",
            "a/b/c",
            "x:y:z",
            "<tag>",
            "back\\slash",
            "all @\"&'/::<>@\\ chars",
        ];

        for input in inputs {
            let escaped = escape_node(input);
            let unescaped = unescape_node(&escaped);
            assert_eq!(
                unescaped, input,
                "roundtrip failed for {input:?}: escaped={escaped:?}"
            );
        }
    }

    #[test]
    fn test_needs_escaping() {
        assert!(needs_escaping("user@host"));
        assert!(needs_escaping("space here"));
        assert!(needs_escaping("has\"quote"));
        assert!(!needs_escaping("simple"));
        assert!(!needs_escaping("user123"));
    }

    #[test]
    fn test_is_escaped() {
        assert!(is_escaped("user\\40host"));
        assert!(is_escaped("space\\20cadet"));
        assert!(!is_escaped("simple"));
        assert!(!is_escaped("a\\zzb")); // invalid escape
    }

    #[test]
    fn test_jid_escaping_trait() {
        assert_eq!("user@host".escaped(), "user\\40host");
        assert_eq!("user\\40host".unescaped(), "user@host");

        let s = String::from("space cadet");
        assert_eq!(s.escaped(), "space\\20cadet");
    }

    // XEP-0106 Section 4 examples
    #[test]
    fn test_spec_examples() {
        assert_eq!(escape_node("space cadet"), "space\\20cadet");
        assert_eq!(escape_node("call me \"ishmael\""), "call\\20me\\20\\22ishmael\\22");
        assert_eq!(escape_node("at&t guy"), "at\\26t\\20guy");
        assert_eq!(escape_node("d'artagnan"), "d\\27artagnan");
        assert_eq!(escape_node("/.telekinesis"), "\\2f.telekinesis");
        assert_eq!(escape_node("thing1 & thing2"), "thing1\\20\\26\\20thing2");
    }
}
