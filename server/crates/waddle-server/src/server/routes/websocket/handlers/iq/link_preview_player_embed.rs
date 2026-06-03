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
    match rule.host_rewrite {
        Some(host) => {
            let mut rewritten = url.clone();
            rewritten.set_host(Some(host)).ok()?;
            Some(rewritten)
        }
        None => Some(url.clone()),
    }
}

/// Re-validate an already-sealed embed URL (post-rewrite) at send time. True
/// only when the URL's origin is a final allowlisted embed origin.
pub(crate) fn is_sealed_player_embed_allowed(url: &Url) -> bool {
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
}
