//! URL detection in message body text.
//!
//! Uses `linkify` to locate HTTP(S) URLs, skips content inside fenced
//! (```` ``` ````) and inline (`` ` ``) code blocks so shared snippets
//! don't get unfurled, and deduplicates by raw URL text keeping first
//! appearance order.
//!
//! Offsets are reported in **UTF-16 code units** to match the wire
//! convention used by `XEP-0372 <reference begin='' end=''>` attributes
//! and the receiver-side anti-spoof check (which slices the body using
//! JavaScript string indexing).

use linkify::{LinkFinder, LinkKind};

/// A URL discovered in a message body, with UTF-16 code-unit offsets
/// locating the URL span inside the body text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedUrl {
    pub url: String,
    pub utf16_begin: usize,
    pub utf16_end: usize,
}

/// Find HTTP(S) URLs in `body`, skipping fenced / inline code regions
/// and deduplicating by URL text (keeping first occurrence).
pub fn detect_urls(body: &str) -> Vec<DetectedUrl> {
    let masks = build_code_mask(body);

    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);

    let mut seen: Vec<String> = Vec::new();
    let mut out: Vec<DetectedUrl> = Vec::new();

    for link in finder.links(body) {
        let url = link.as_str();

        if !(url.starts_with("http://") || url.starts_with("https://")) {
            continue;
        }

        let byte_start = link.start();
        let byte_end = link.end();

        if is_inside_code(&masks, byte_start, byte_end) {
            continue;
        }

        if seen.iter().any(|u| u == url) {
            continue;
        }
        seen.push(url.to_owned());

        let utf16_begin = utf16_offset(body, byte_start);
        let utf16_end = utf16_begin + utf16_len(&body[byte_start..byte_end]);

        out.push(DetectedUrl {
            url: url.to_owned(),
            utf16_begin,
            utf16_end,
        });
    }

    out
}

fn utf16_offset(s: &str, byte_index: usize) -> usize {
    s[..byte_index].encode_utf16().count()
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// A list of (start_byte, end_byte) byte ranges covering code regions
/// where URLs must be ignored.
fn build_code_mask(body: &str) -> Vec<(usize, usize)> {
    let mut masks: Vec<(usize, usize)> = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Fenced code: ``` ... ```
        if bytes[i..].starts_with(b"```") {
            let fence_start = i;
            i += 3;
            // Find closing ```
            let mut j = i;
            let mut closed = false;
            while j + 3 <= bytes.len() {
                if bytes[j..].starts_with(b"```") {
                    masks.push((fence_start, j + 3));
                    i = j + 3;
                    closed = true;
                    break;
                }
                j += 1;
            }
            if !closed {
                // Unterminated fence consumes to EOF.
                masks.push((fence_start, bytes.len()));
                i = bytes.len();
            }
            continue;
        }

        // Inline code: ` ... `
        if bytes[i] == b'`' {
            let tick_start = i;
            i += 1;
            // Find closing `
            let mut j = i;
            let mut closed = false;
            while j < bytes.len() {
                if bytes[j] == b'`' {
                    masks.push((tick_start, j + 1));
                    i = j + 1;
                    closed = true;
                    break;
                }
                // Don't span newlines for inline code (markdown convention).
                if bytes[j] == b'\n' {
                    break;
                }
                j += 1;
            }
            if !closed {
                i += 1; // skip the lone backtick
            }
            continue;
        }

        i += 1;
    }

    masks
}

fn is_inside_code(masks: &[(usize, usize)], start: usize, end: usize) -> bool {
    masks.iter().any(|&(s, e)| start >= s && end <= e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_single_http_url() {
        let d = detect_urls("see http://example.com/a here");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].url, "http://example.com/a");
    }

    #[test]
    fn finds_single_https_url() {
        let d = detect_urls("see https://example.com/a here");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].url, "https://example.com/a");
    }

    #[test]
    fn returns_empty_when_no_urls() {
        assert!(detect_urls("hello world").is_empty());
    }

    #[test]
    fn ignores_mailto_ftp() {
        let d = detect_urls("mailto:bob@example.com or ftp://x.example/");
        assert!(d.is_empty());
    }

    #[test]
    fn finds_multiple_urls_in_order() {
        let d = detect_urls("a https://a.example/ b https://b.example/ c");
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].url, "https://a.example/");
        assert_eq!(d[1].url, "https://b.example/");
    }

    #[test]
    fn dedupes_identical_urls() {
        let d = detect_urls("https://example.com/ and again https://example.com/");
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn skips_urls_inside_fenced_code() {
        let d = detect_urls("before ``` https://hidden.example/ ``` after");
        assert!(d.is_empty());
    }

    #[test]
    fn skips_urls_inside_fenced_code_multiline() {
        let body = "pre\n```\nhttps://hidden.example/\n```\npost";
        assert!(detect_urls(body).is_empty());
    }

    #[test]
    fn detects_url_outside_fenced_code_when_body_has_fences() {
        let body = "outer https://shown.example/\n```\nhttps://hidden.example/\n```\n";
        let d = detect_urls(body);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].url, "https://shown.example/");
    }

    #[test]
    fn skips_urls_inside_inline_code() {
        let d = detect_urls("see `https://hidden.example/` end");
        assert!(d.is_empty());
    }

    #[test]
    fn inline_code_does_not_span_newlines() {
        // The lone ` before the URL doesn't match an inline-code region
        // (no closing ` on the same line), so the URL must be detected.
        let body = "broken `\nhttps://shown.example/";
        let d = detect_urls(body);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].url, "https://shown.example/");
    }

    #[test]
    fn utf16_offsets_for_ascii_body() {
        let d = detect_urls("see https://example.com/ end");
        assert_eq!(d[0].utf16_begin, 4);
        assert_eq!(d[0].utf16_end, 24);
    }

    #[test]
    fn utf16_offsets_skip_surrogate_pairs() {
        // 🚀 is a non-BMP codepoint (U+1F680) → 2 UTF-16 code units.
        // Space after = 1 unit. Total prefix = 3 units.
        let d = detect_urls("🚀 https://example.com/");
        assert_eq!(d[0].utf16_begin, 3);
        assert_eq!(d[0].utf16_end, 3 + "https://example.com/".len());
    }

    #[test]
    fn utf16_offsets_for_multibyte_bmp_prefix() {
        // café = 4 UTF-16 code units (each codepoint is BMP, 1 unit).
        let d = detect_urls("café https://example.com/");
        assert_eq!(d[0].utf16_begin, 5);
    }

    #[test]
    fn receivers_can_slice_body_with_returned_offsets() {
        // Verifies the core contract the receiver relies on: slicing the
        // body with [utf16_begin..utf16_end] via UTF-16 yields the URL.
        let body = "🚀 https://example.com/article end";
        let d = detect_urls(body);
        let u16: Vec<u16> = body.encode_utf16().collect();
        let sliced: String = String::from_utf16(&u16[d[0].utf16_begin..d[0].utf16_end])
            .expect("valid utf16");
        assert_eq!(sliced, d[0].url);
    }

    #[test]
    fn unterminated_fence_swallows_rest_of_body() {
        let d = detect_urls("before\n```\nhttps://hidden.example/\nunclosed");
        assert!(d.is_empty());
    }
}
