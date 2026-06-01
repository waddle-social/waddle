//! Shared link-preview URL eligibility helpers.

/// Return the first HTTPS URL-like token whose host looks web-addressable.
///
/// This intentionally stays string-based so crates that already parse into
/// their local URL type can do so at the boundary without adding URL parsing
/// dependencies to `waddle-xmpp-core`.
pub fn first_eligible_https_url_text(body: &str) -> Option<&str> {
    body.char_indices().find_map(|(idx, _)| {
        if !body[idx..]
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        {
            return None;
        }

        let candidate = body[idx..]
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\''))
            .next()
            .unwrap_or_default()
            .trim_end_matches([')', ']', '}', ',', '.', '!', '?', ';', ':']);
        let rest = candidate.get(8..)?;
        let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
        (!host.is_empty() && host.contains('.')).then_some(candidate)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_first_eligible_https_url() {
        assert_eq!(
            first_eligible_https_url_text(
                "see http://ignored.example then https://first.example/a, and https://second.example/b",
            ),
            Some("https://first.example/a")
        );
    }

    #[test]
    fn trims_wrapping_trailing_punctuation() {
        assert_eq!(
            first_eligible_https_url_text("see (https://example.com/a)."),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn rejects_host_without_dot() {
        assert_eq!(
            first_eligible_https_url_text("see https://localhost/a"),
            None
        );
    }
}
