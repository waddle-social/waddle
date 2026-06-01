//! Shared link-preview protocol helpers.

use std::{fmt, str::FromStr};

/// Safe image MIME types Waddle accepts for cached link-preview media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewImageMediaType {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl PreviewImageMediaType {
    /// Borrow the canonical MIME text used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

impl fmt::Display for PreviewImageMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PreviewImageMediaType {
    type Err = InvalidPreviewImageMediaType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "image/png" => Ok(Self::Png),
            "image/jpeg" => Ok(Self::Jpeg),
            "image/gif" => Ok(Self::Gif),
            "image/webp" => Ok(Self::Webp),
            _ => Err(InvalidPreviewImageMediaType),
        }
    }
}

/// Error returned for unsupported preview-image MIME text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidPreviewImageMediaType;

impl fmt::Display for InvalidPreviewImageMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unsupported preview image MIME type")
    }
}

impl std::error::Error for InvalidPreviewImageMediaType {}

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

    #[test]
    fn parses_supported_preview_image_media_types_case_insensitively() {
        assert_eq!(
            "IMAGE/PNG".parse::<PreviewImageMediaType>(),
            Ok(PreviewImageMediaType::Png)
        );
        assert_eq!(PreviewImageMediaType::Webp.as_str(), "image/webp");
    }

    #[test]
    fn rejects_unsupported_preview_image_media_type() {
        assert!("image/svg+xml".parse::<PreviewImageMediaType>().is_err());
    }
}
