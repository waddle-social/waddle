//! XEP-0449 stickers: the account's packs, sending a sticker with its
//! pack, and the pack a received sticker names.
//!
//! A sticker message carries the sticker's text (its description, for
//! clients that draw no stickers), the picture as an inline XEP-0447 file
//! whose `<desc/>` and XEP-0300 hash come from the pack item, and a
//! `<sticker pack=…/>` marker. The picture stays where the pack author
//! uploaded it, so nothing is uploaded when sending.

use url::Url;
use waddle_client_session::Attachment;
use waddle_xmpp_client_ffi::{
    WaddlePackSticker, WaddleSharedFile, WaddleStickerHash, WaddleStickerPack, WaddleStickerRef,
};

use crate::link_cards::web_url;

/// The text of a sticker published without a description: a sticker
/// message always carries text.
const FALLBACK_TEXT: &str = "Sticker";

/// One sticker in a pack that has a picture to fetch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sticker {
    pub desc: String,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub hashes: Vec<WaddleStickerHash>,
    /// Where the picture can be fetched, in the pack's order.
    pub sources: Vec<Url>,
}

impl Sticker {
    fn from_wire(sticker: WaddlePackSticker) -> Option<Self> {
        let sources: Vec<Url> = sticker.sources.iter().filter_map(|s| web_url(s)).collect();
        if sources.is_empty() {
            return None;
        }
        Some(Self {
            desc: sticker.desc,
            media_type: sticker.media_type,
            size: sticker.size,
            width: sticker.width,
            height: sticker.height,
            hashes: sticker.hashes,
            sources,
        })
    }

    /// The description, or "Sticker" for one published without any.
    pub fn text(&self) -> String {
        let desc = self.desc.trim();
        if desc.is_empty() {
            FALLBACK_TEXT.to_string()
        } else {
            self.desc.clone()
        }
    }

    /// The picture as the inline file a sticker message carries.
    pub fn shared_file(&self) -> WaddleSharedFile {
        WaddleSharedFile {
            url: self.sources[0].to_string(),
            name: None,
            media_type: self.media_type.clone(),
            size: self.size,
            width: self.width,
            height: self.height,
            desc: Some(self.text()),
            hashes: self.hashes.clone(),
            disposition: "inline".to_string(),
            encrypted: None,
        }
    }

    /// The picture as the core fetches and decodes shared files.
    pub fn attachment(&self) -> Attachment {
        Attachment {
            url: self.sources[0].to_string(),
            name: None,
            media_type: self.media_type.clone(),
            size: self.size,
            inline: true,
            encryption: None,
        }
    }
}

/// One of the account's sticker packs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    pub id: String,
    pub name: Option<String>,
    pub summary: Option<String>,
    /// The author asked that others neither use nor copy the pack.
    pub restricted: bool,
    pub stickers: Vec<Sticker>,
}

impl Pack {
    pub fn from_wire(pack: WaddleStickerPack) -> Self {
        Self {
            id: pack.id,
            name: pack.name,
            summary: pack.summary,
            restricted: pack.restricted,
            stickers: pack
                .stickers
                .into_iter()
                .filter_map(Sticker::from_wire)
                .collect(),
        }
    }

    /// The name, or "Stickers" for a pack published without one.
    pub fn title(&self) -> String {
        match self.name.as_deref().map(str::trim) {
            Some(name) if !name.is_empty() => name.to_string(),
            _ => "Stickers".to_string(),
        }
    }

    /// The pack a sticker sent from here names. The sender's own
    /// `urn:xmpp:stickers:0` node, so no address or node goes with it.
    pub fn reference(&self) -> WaddleStickerRef {
        WaddleStickerRef {
            pack_id: self.id.clone(),
            pack_jid: None,
            pack_node: None,
        }
    }
}

/// The packs worth listing: those with a sticker to send.
pub fn usable(packs: Vec<WaddleStickerPack>) -> Vec<Pack> {
    packs
        .into_iter()
        .map(Pack::from_wire)
        .filter(|pack| !pack.stickers.is_empty())
        .collect()
}

/// The pack a received sticker says it is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub pack_id: String,
    /// Set when the pack is kept outside the sender's own PEP node.
    pub pack_jid: Option<String>,
    pub pack_node: Option<String>,
}

impl Mark {
    pub fn from_wire(reference: &WaddleStickerRef) -> Option<Self> {
        let pack_id = reference.pack_id.trim();
        (!pack_id.is_empty()).then(|| Self {
            pack_id: pack_id.to_string(),
            pack_jid: reference.pack_jid.clone(),
            pack_node: reference.pack_node.clone(),
        })
    }

    /// Whose PEP node to open the pack on: the sender's, and only when
    /// the sticker says the pack is on it. Opening a pack shows our
    /// address to that account, so a sticker that points somewhere else
    /// (or a sender the conversation does not vouch for) is not followed.
    pub fn owner<'a>(&self, sender: Option<&'a str>) -> Option<&'a str> {
        if self.pack_jid.is_some() || self.pack_node.is_some() {
            return None;
        }
        sender.filter(|sender| !sender.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire(desc: &str, sources: &[&str]) -> WaddlePackSticker {
        WaddlePackSticker {
            desc: desc.to_string(),
            media_type: Some("image/png".to_string()),
            size: Some(10),
            width: Some(64),
            height: Some(64),
            hashes: vec![WaddleStickerHash {
                algo: "sha-256".to_string(),
                value_b64: "abc=".to_string(),
            }],
            sources: sources.iter().map(|s| s.to_string()).collect(),
            suggests: Vec::new(),
        }
    }

    fn pack(stickers: Vec<WaddlePackSticker>) -> WaddleStickerPack {
        WaddleStickerPack {
            id: "pack-1".to_string(),
            name: Some(" Marsey ".to_string()),
            summary: None,
            restricted: false,
            stickers,
        }
    }

    #[test]
    fn a_sticker_needs_a_web_source() {
        let pack = Pack::from_wire(pack(vec![
            wire("a", &["ftp://x.example/a.png", "https://x.example/a.png"]),
            wire("b", &["javascript:alert(1)"]),
            wire("c", &[]),
        ]));
        assert_eq!(pack.stickers.len(), 1);
        assert_eq!(pack.stickers[0].sources.len(), 1);
    }

    #[test]
    fn packs_without_stickers_are_not_listed() {
        let listed = usable(vec![
            pack(vec![wire("a", &["https://x.example/a.png"])]),
            pack(vec![wire("b", &[])]),
        ]);
        assert_eq!(listed.len(), 1);
    }

    #[test]
    fn a_sent_sticker_carries_its_text_file_and_pack() {
        let pack = Pack::from_wire(pack(vec![wire(
            "😘",
            &["https://x.example/kiss.png", "https://y.example/kiss.png"],
        )]));
        let sticker = &pack.stickers[0];
        let file = sticker.shared_file();
        assert_eq!(sticker.text(), "😘");
        assert_eq!(file.url, "https://x.example/kiss.png");
        assert_eq!(file.desc.as_deref(), Some("😘"));
        assert_eq!(file.disposition, "inline");
        assert_eq!(file.hashes.len(), 1);
        assert!(file.encrypted.is_none());
        let reference = pack.reference();
        assert_eq!(reference.pack_id, "pack-1");
        assert_eq!((reference.pack_jid, reference.pack_node), (None, None));
    }

    #[test]
    fn a_sticker_without_a_description_says_sticker() {
        let pack = Pack::from_wire(pack(vec![wire("  ", &["https://x.example/a.png"])]));
        assert_eq!(pack.stickers[0].text(), "Sticker");
        assert_eq!(
            pack.stickers[0].shared_file().desc.as_deref(),
            Some("Sticker")
        );
    }

    #[test]
    fn titles_fall_back() {
        let mut pack = Pack::from_wire(pack(Vec::new()));
        assert_eq!(pack.title(), "Marsey");
        pack.name = Some("  ".to_string());
        assert_eq!(pack.title(), "Stickers");
        pack.name = None;
        assert_eq!(pack.title(), "Stickers");
    }

    #[test]
    fn a_pack_is_opened_only_on_the_senders_own_node() {
        let local = Mark::from_wire(&WaddleStickerRef {
            pack_id: " p1 ".to_string(),
            pack_jid: None,
            pack_node: None,
        })
        .expect("mark");
        assert_eq!(local.pack_id, "p1");
        assert_eq!(
            local.owner(Some("ann@example.com")),
            Some("ann@example.com")
        );
        assert_eq!(local.owner(None), None);
        assert_eq!(local.owner(Some("")), None);

        let elsewhere = Mark {
            pack_jid: Some("other@example.com".to_string()),
            ..local.clone()
        };
        assert_eq!(elsewhere.owner(Some("ann@example.com")), None);
        let on_node = Mark {
            pack_node: Some("node".to_string()),
            ..local
        };
        assert_eq!(on_node.owner(Some("ann@example.com")), None);
    }

    #[test]
    fn an_empty_pack_reference_is_no_mark() {
        assert!(Mark::from_wire(&WaddleStickerRef {
            pack_id: "  ".to_string(),
            pack_jid: None,
            pack_node: None,
        })
        .is_none());
    }
}
