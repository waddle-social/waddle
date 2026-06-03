//! Allowlist for embeddable `og:video` player iframes.
//!
//! The allowlist is the security boundary for the embedded-player feature: only
//! frame origins on this list are ever sealed into a link-preview token or
//! rendered as an `<iframe>`. YouTube watch pages advertise an
//! `https://www.youtube.com/embed/...` player; we rewrite that to the
//! cookie-reduced `www.youtube-nocookie.com` host before sealing.

use url::Url;

/// One allowlist entry: an embed origin we accept, with an optional canonical
/// host rewrite applied before the embed URL is sealed.
struct PlayerEmbedRule {
    /// `scheme://host[:port]` the page-advertised embed URL must match.
    match_origin: &'static str,
    /// Host substituted before sealing (privacy/canonicalization), when set.
    host_rewrite: Option<&'static str>,
}

const PLAYER_EMBED_RULES: &[PlayerEmbedRule] = &[
    PlayerEmbedRule {
        match_origin: "https://www.youtube.com",
        host_rewrite: Some("www.youtube-nocookie.com"),
    },
    PlayerEmbedRule {
        match_origin: "https://www.youtube-nocookie.com",
        host_rewrite: None,
    },
    PlayerEmbedRule {
        match_origin: "https://player.vimeo.com",
        host_rewrite: None,
    },
];

/// Return the `scheme://host[:port]` origin text for `url`, or `None` when the
/// URL has no host (e.g. opaque origins).
fn origin_text(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    match url.port() {
        Some(port) => Some(format!("{}://{}:{}", url.scheme(), host, port)),
        None => Some(format!("{}://{}", url.scheme(), host)),
    }
}

/// Validate a page-advertised embed URL against the allowlist and return the
/// canonical sealed form (host-rewritten when the rule asks for it). Returns
/// `None` when the origin is not allowlisted.
pub(crate) fn normalize_allowed_player_embed(url: &Url) -> Option<Url> {
    if url.scheme() != "https" {
        return None;
    }
    // Credentials in an https iframe src are never legitimate for an embed and
    // would survive the host rewrite — reject them at the allowlist boundary.
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let origin = origin_text(url)?;
    let rule = PLAYER_EMBED_RULES
        .iter()
        .find(|rule| rule.match_origin == origin)?;
    let mut sealed = url.clone();
    if let Some(host) = rule.host_rewrite {
        sealed.set_host(Some(host)).ok()?;
    }
    strip_playlist_param(&mut sealed);
    Some(sealed)
}

/// Drop the `list` query parameter so a YouTube `…/embed/<id>?list=<playlist>`
/// player renders the single intended video rather than auto-advancing a
/// playlist. Other query params (e.g. Vimeo's `?h=` privacy hash) are kept.
fn strip_playlist_param(url: &mut Url) {
    if url.query().is_none() {
        return;
    }
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "list")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    if kept.is_empty() {
        url.set_query(None);
    } else {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        for (key, value) in kept {
            pairs.append_pair(&key, &value);
        }
    }
}

/// Re-validate an already-sealed embed URL (post-rewrite) at send time. True
/// only when the URL is a final allowlisted embed origin. Re-applies the same
/// scheme/userinfo constraints as [`normalize_allowed_player_embed`] so the
/// defense-in-depth check cannot be weaker than the seal-time gate.
pub(crate) fn is_sealed_player_embed_allowed(url: &Url) -> bool {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(origin) = origin_text(url) else {
        return false;
    };
    PLAYER_EMBED_RULES.iter().any(|rule| {
        let final_origin = match rule.host_rewrite {
            Some(host) => format!("https://{host}"),
            None => rule.match_origin.to_string(),
        };
        final_origin == origin
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_youtube_watch_embed_to_nocookie_host() {
        let url = Url::parse("https://www.youtube.com/embed/429A_VugWW0").expect("url");
        let sealed = normalize_allowed_player_embed(&url).expect("allowlisted");
        assert_eq!(
            sealed.as_str(),
            "https://www.youtube-nocookie.com/embed/429A_VugWW0"
        );
    }

    #[test]
    fn accepts_vimeo_player_without_rewrite() {
        let url = Url::parse("https://player.vimeo.com/video/12345").expect("url");
        let sealed = normalize_allowed_player_embed(&url).expect("allowlisted");
        assert_eq!(sealed, url);
    }

    #[test]
    fn rejects_non_allowlisted_origin() {
        let url = Url::parse("https://evil.example.com/embed/x").expect("url");
        assert!(normalize_allowed_player_embed(&url).is_none());
    }

    #[test]
    fn rejects_non_https_embed() {
        let url = Url::parse("http://www.youtube.com/embed/x").expect("url");
        assert!(normalize_allowed_player_embed(&url).is_none());
    }

    #[test]
    fn rejects_userinfo_in_embed_url() {
        let url = Url::parse("https://user@www.youtube.com/embed/x").expect("url");
        assert!(normalize_allowed_player_embed(&url).is_none());
    }

    #[test]
    fn sealed_check_accepts_final_origins_only() {
        assert!(is_sealed_player_embed_allowed(
            &Url::parse("https://www.youtube-nocookie.com/embed/x").expect("url")
        ));
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://www.youtube.com/embed/x").expect("url")
        ));
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://evil.example.com/embed/x").expect("url")
        ));
    }

    #[test]
    fn sealed_check_rejects_userinfo_and_non_https() {
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://user@www.youtube-nocookie.com/embed/x").expect("url")
        ));
        assert!(!is_sealed_player_embed_allowed(
            &Url::parse("https://user:pass@player.vimeo.com/video/1").expect("url")
        ));
    }

    #[test]
    fn strips_youtube_playlist_param_keeps_others() {
        let sealed = normalize_allowed_player_embed(
            &Url::parse("https://www.youtube.com/embed/VID?list=PL123&start=5").expect("url"),
        )
        .expect("allowlisted");
        assert_eq!(
            sealed.as_str(),
            "https://www.youtube-nocookie.com/embed/VID?start=5"
        );
    }

    #[test]
    fn strips_query_entirely_when_only_playlist_param() {
        let sealed = normalize_allowed_player_embed(
            &Url::parse("https://www.youtube.com/embed/VID?list=PL123").expect("url"),
        )
        .expect("allowlisted");
        assert_eq!(
            sealed.as_str(),
            "https://www.youtube-nocookie.com/embed/VID"
        );
    }

    #[test]
    fn preserves_vimeo_privacy_hash_param() {
        let sealed = normalize_allowed_player_embed(
            &Url::parse("https://player.vimeo.com/video/12345?h=abc123").expect("url"),
        )
        .expect("allowlisted");
        assert_eq!(
            sealed.as_str(),
            "https://player.vimeo.com/video/12345?h=abc123"
        );
    }
}
