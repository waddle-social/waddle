//! Link preview cards the server attached to a message
//! (`urn:waddle:link-preview:0`, rendered from XEP-0511 metadata).
//!
//! The server fetched the page and cached its image; the client only
//! shows what it was given. Titles and descriptions are sender-influenced
//! page text, so they are plain text here and the target is opened only
//! when it is `http` or `https`.

use url::Url;
use waddle_xmpp_client_ffi::WaddleLinkPreview;

/// The most cards shown under one message.
pub const MAX_CARDS: usize = 3;
const MAX_TITLE: usize = 200;
const MAX_DESCRIPTION: usize = 400;

/// One card, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkCard {
    /// Where a click goes: the page's canonical address when it has one.
    pub url: Url,
    pub title: Option<String>,
    pub description: Option<String>,
    /// The server-cached preview image. The client never fetches remote
    /// media the server did not cache.
    pub image: Option<Url>,
}

impl LinkCard {
    /// The host to show as the card's site line.
    pub fn site(&self) -> String {
        self.url
            .host_str()
            .map(|host| host.strip_prefix("www.").unwrap_or(host).to_string())
            .unwrap_or_else(|| self.url.to_string())
    }
}

/// An `http` or `https` address without credentials.
pub fn web_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

fn text(value: Option<&str>, limit: usize) -> Option<String> {
    let collapsed = value?.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    Some(match collapsed.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", collapsed[..end].trim_end()),
        None => collapsed,
    })
}

fn card(preview: &WaddleLinkPreview) -> Option<LinkCard> {
    let url = preview
        .normalized_url
        .as_deref()
        .and_then(web_url)
        .or_else(|| web_url(&preview.original_url))?;
    let card = LinkCard {
        url,
        title: text(preview.title.as_deref(), MAX_TITLE),
        description: text(preview.description.as_deref(), MAX_DESCRIPTION),
        // Only an `https` image: it is fetched without the user's session.
        image: preview
            .image
            .as_ref()
            .and_then(|image| web_url(&image.url))
            .filter(|image| image.scheme() == "https"),
    };
    // A card with nothing to say beyond the address adds nothing to the
    // link already in the message.
    (card.title.is_some() || card.description.is_some() || card.image.is_some()).then_some(card)
}

/// The cards for a message's previews: at most [`MAX_CARDS`], one per
/// address.
pub fn cards(previews: &[WaddleLinkPreview]) -> Vec<LinkCard> {
    let mut cards: Vec<LinkCard> = Vec::new();
    for card in previews.iter().filter_map(card) {
        if cards.len() == MAX_CARDS {
            break;
        }
        if cards.iter().all(|seen| seen.url != card.url) {
            cards.push(card);
        }
    }
    cards
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client_ffi::WaddleLinkPreviewImage;

    fn preview(url: &str) -> WaddleLinkPreview {
        WaddleLinkPreview {
            original_url: url.to_string(),
            normalized_url: None,
            title: Some("A page".to_string()),
            description: None,
            image: None,
            video: None,
            player_embed: None,
            remote_media_unavailable: false,
        }
    }

    fn image(url: &str) -> WaddleLinkPreviewImage {
        WaddleLinkPreviewImage {
            url: url.to_string(),
            media_type: "image/png".to_string(),
            width: None,
            height: None,
            alt: None,
        }
    }

    #[test]
    fn a_card_opens_the_canonical_address_and_names_its_site() {
        let mut wire = preview("https://example.com/a?utm=1");
        wire.normalized_url = Some("https://www.example.com/a".to_string());
        let card = card(&wire).expect("card");
        assert_eq!(card.url.as_str(), "https://www.example.com/a");
        assert_eq!(card.site(), "example.com");
    }

    #[test]
    fn only_web_addresses_without_credentials_open() {
        assert!(web_url("https://example.com").is_some());
        assert!(web_url("http://example.com/x").is_some());
        assert!(web_url("javascript:alert(1)").is_none());
        assert!(web_url("file:///etc/passwd").is_none());
        assert!(web_url("xmpp:room@example.com").is_none());
        assert!(web_url("https://user:pw@example.com").is_none());
        assert!(card(&preview("ftp://example.com/x")).is_none());
    }

    #[test]
    fn a_bad_canonical_address_falls_back_to_the_original() {
        let mut wire = preview("https://example.com/a");
        wire.normalized_url = Some("javascript:alert(1)".to_string());
        assert_eq!(
            card(&wire).expect("card").url.as_str(),
            "https://example.com/a"
        );
    }

    #[test]
    fn text_is_collapsed_and_truncated() {
        let mut wire = preview("https://example.com");
        wire.title = Some("  Two\n\nlines   here ".to_string());
        wire.description = Some("x".repeat(1000));
        let card = card(&wire).expect("card");
        assert_eq!(card.title.as_deref(), Some("Two lines here"));
        let description = card.description.expect("description");
        assert_eq!(description.chars().count(), MAX_DESCRIPTION + 1);
        assert!(description.ends_with('…'));
    }

    #[test]
    fn the_image_must_be_https() {
        let mut wire = preview("https://example.com");
        wire.image = Some(image("http://cdn.example.com/a.png"));
        assert!(card(&wire).expect("card").image.is_none());
        wire.image = Some(image("https://cdn.example.com/a.png"));
        assert!(card(&wire).expect("card").image.is_some());
    }

    #[test]
    fn a_card_with_nothing_to_show_is_dropped() {
        let mut wire = preview("https://example.com");
        wire.title = Some("   ".to_string());
        assert!(card(&wire).is_none());
    }

    #[test]
    fn cards_are_capped_and_deduplicated() {
        let wires: Vec<_> = (0..6)
            .map(|n| preview(&format!("https://example.com/{}", n % 4)))
            .collect();
        let shown = cards(&wires);
        assert_eq!(shown.len(), MAX_CARDS);
        assert_eq!(shown[2].url.path(), "/2");
        let same = vec![preview("https://example.com/a"); 2];
        assert_eq!(cards(&same).len(), 1);
    }
}
