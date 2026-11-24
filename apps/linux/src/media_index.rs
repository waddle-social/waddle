//! What a message carries beyond text, kept beside the timeline.
//!
//! The shared timeline (`waddle-client-session`) models text, replies,
//! reactions and files, not link preview cards or the sticker marker, and
//! it is not this app's to change. So the session adapter records them
//! here as messages arrive or load from the archive, keyed by the
//! conversation and the id replies reference, and the row reads them back
//! when it is bound. The index is bounded; a message that fell out simply
//! shows without its card.

use std::collections::{HashMap, VecDeque};
use std::sync::{LazyLock, Mutex};

use waddle_xmpp_client_ffi::{WaddleLinkPreview, WaddleStickerRef};

use crate::link_cards::{self, LinkCard};
use crate::stickers::Mark;

const CAPACITY: usize = 4096;

/// The extras of one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageMedia {
    pub cards: Vec<LinkCard>,
    /// The message is a XEP-0449 sticker.
    pub sticker: bool,
    /// The pack it says it is from.
    pub pack: Option<Mark>,
}

impl MessageMedia {
    /// The extras of a wire message, or `None` when it has none.
    pub fn from_wire(
        previews: &[WaddleLinkPreview],
        is_sticker: bool,
        pack: Option<&WaddleStickerRef>,
    ) -> Option<Self> {
        let media = Self {
            cards: link_cards::cards(previews),
            sticker: is_sticker,
            pack: pack.and_then(Mark::from_wire).filter(|_| is_sticker),
        };
        (media != Self::default()).then_some(media)
    }
}

#[derive(Default)]
struct Index {
    entries: HashMap<String, MessageMedia>,
    order: VecDeque<String>,
}

impl Index {
    fn record(&mut self, key: String, media: MessageMedia) {
        if self.entries.insert(key.clone(), media).is_none() {
            self.order.push_back(key);
            while self.order.len() > CAPACITY {
                if let Some(oldest) = self.order.pop_front() {
                    self.entries.remove(&oldest);
                }
            }
        }
    }
}

static INDEX: LazyLock<Mutex<Index>> = LazyLock::new(Mutex::default);

fn key(conversation: &str, reference: &str) -> String {
    format!("{conversation}\n{reference}")
}

/// Remember a message's extras. `reference` is the id replies use for it.
pub fn record(conversation: &str, reference: Option<&str>, media: Option<MessageMedia>) {
    let (Some(reference), Some(media)) = (reference.filter(|id| !id.is_empty()), media) else {
        return;
    };
    if let Ok(mut index) = INDEX.lock() {
        index.record(key(conversation, reference), media);
    }
}

/// A message's extras, if it had any.
pub fn lookup(conversation: &str, reference: &str) -> Option<MessageMedia> {
    INDEX
        .lock()
        .ok()?
        .entries
        .get(&key(conversation, reference))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sticker() -> MessageMedia {
        MessageMedia {
            sticker: true,
            ..MessageMedia::default()
        }
    }

    #[test]
    fn plain_messages_have_no_extras() {
        assert!(MessageMedia::from_wire(&[], false, None).is_none());
        let pack = WaddleStickerRef {
            pack_id: "p".to_string(),
            pack_jid: None,
            pack_node: None,
        };
        // A pack reference without the sticker flag is not a sticker.
        assert!(MessageMedia::from_wire(&[], false, Some(&pack)).is_none());
        let media = MessageMedia::from_wire(&[], true, Some(&pack)).expect("sticker");
        assert!(media.sticker);
        assert_eq!(media.pack.expect("pack").pack_id, "p");
    }

    #[test]
    fn extras_are_found_by_conversation_and_id() {
        record("room-a@x", Some("id-1"), Some(sticker()));
        assert!(lookup("room-a@x", "id-1").is_some());
        assert!(lookup("room-b@x", "id-1").is_none());
        assert!(lookup("room-a@x", "id-2").is_none());
        record("room-a@x", None, Some(sticker()));
        record("room-a@x", Some(""), Some(sticker()));
        record("room-a@x", Some("id-3"), None);
        assert!(lookup("room-a@x", "id-3").is_none());
    }

    #[test]
    fn the_index_forgets_the_oldest() {
        let mut index = Index::default();
        for n in 0..CAPACITY + 10 {
            index.record(format!("k{n}"), sticker());
        }
        assert_eq!(index.entries.len(), CAPACITY);
        assert!(!index.entries.contains_key("k0"));
        assert!(index.entries.contains_key(&format!("k{}", CAPACITY + 9)));
    }
}
