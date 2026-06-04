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

/// Media types Waddle accepts for trusted inline video previews.
///
/// Progressive container files (`Mp4`/`Webm`/`Ogg`/`QuickTime`) play natively in
/// a `<video>` element. `Hls` is an adaptive-streaming manifest
/// (`application/vnd.apple.mpegurl`) which plays natively on Safari and via a
/// lazily-loaded `hls.js` player elsewhere; it is accepted for the page-advertised
/// `og:video` native path (clients fall back gracefully when it cannot play).
/// DASH and provider embed pages remain excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectVideoMediaType {
    Mp4,
    Webm,
    Ogg,
    QuickTime,
    Hls,
}

impl DirectVideoMediaType {
    /// Borrow the canonical MIME text used on the wire.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::Webm => "video/webm",
            Self::Ogg => "video/ogg",
            Self::QuickTime => "video/quicktime",
            Self::Hls => "application/vnd.apple.mpegurl",
        }
    }
}

impl fmt::Display for DirectVideoMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DirectVideoMediaType {
    type Err = InvalidDirectVideoMediaType;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "video/mp4" => Ok(Self::Mp4),
            "video/webm" => Ok(Self::Webm),
            "video/ogg" => Ok(Self::Ogg),
            "video/quicktime" => Ok(Self::QuickTime),
            // HLS manifests appear under several historical aliases in the wild.
            "application/vnd.apple.mpegurl"
            | "application/x-mpegurl"
            | "audio/x-mpegurl"
            | "audio/mpegurl"
            | "application/mpegurl" => Ok(Self::Hls),
            _ => Err(InvalidDirectVideoMediaType),
        }
    }
}

/// Error returned for unsupported direct-video MIME text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDirectVideoMediaType;

impl fmt::Display for InvalidDirectVideoMediaType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unsupported direct video MIME type")
    }
}

impl std::error::Error for InvalidDirectVideoMediaType {}

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

    #[test]
    fn parses_supported_direct_video_media_types_case_insensitively() {
        assert_eq!(
            "VIDEO/MP4".parse::<DirectVideoMediaType>(),
            Ok(DirectVideoMediaType::Mp4)
        );
        assert_eq!(
            "video/webm".parse::<DirectVideoMediaType>(),
            Ok(DirectVideoMediaType::Webm)
        );
        assert_eq!(
            "video/ogg".parse::<DirectVideoMediaType>(),
            Ok(DirectVideoMediaType::Ogg)
        );
        assert_eq!(
            "video/quicktime".parse::<DirectVideoMediaType>(),
            Ok(DirectVideoMediaType::QuickTime)
        );
        assert_eq!(DirectVideoMediaType::Mp4.as_str(), "video/mp4");
    }

    #[test]
    fn parses_hls_media_type_aliases_to_canonical() {
        for alias in [
            "application/vnd.apple.mpegurl",
            "application/x-mpegURL",
            "application/x-mpegurl",
            "audio/x-mpegurl",
            "audio/mpegurl",
            "application/mpegurl",
        ] {
            assert_eq!(
                alias.parse::<DirectVideoMediaType>(),
                Ok(DirectVideoMediaType::Hls),
                "alias {alias} must map to HLS"
            );
        }
        assert_eq!(
            DirectVideoMediaType::Hls.as_str(),
            "application/vnd.apple.mpegurl"
        );
    }

    #[test]
    fn rejects_non_direct_video_media_types() {
        assert!("text/html".parse::<DirectVideoMediaType>().is_err());
        // DASH manifests need a separate player and are not supported.
        assert!("application/dash+xml"
            .parse::<DirectVideoMediaType>()
            .is_err());
    }
}
