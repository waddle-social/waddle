//! Pins, archive search and moderation over the session's `WaddleClient`.
//!
//! Like `session.rs` this only adapts: wire records become the plain types
//! `pins.rs` and `message_actions.rs` work with.

use std::sync::Arc;

use jid::{BareJid, Jid};
use waddle_xmpp_client_ffi::{
    WaddleMessage, WaddlePinAction, WaddlePinEntry, WaddlePinEvent, WaddlePinPermission,
    WaddlePinPreview,
};

use crate::message_actions::PinPermission;
use crate::pins::{Pin, PinChange};
use crate::runtime;
use crate::session::{archived_from, Kind, Me, Session};

/// Archive search results one query returns.
const SEARCH_LIMIT: u32 = 50;

/// One archive search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// The message's archive stanza id, to jump to.
    pub stanza_id: String,
    pub sender: String,
    pub body: String,
    pub timestamp_ms: i64,
}

/// A pin or unpin broadcast placed in its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinUpdate {
    /// The room's, or the DM peer's, bare JID.
    pub conversation: String,
    pub change: PinChange,
}

impl Session {
    /// The conversation's pinned messages, newest first. A DM's list is
    /// asked of the server by the peer's JID, as a room's is by the room's.
    pub async fn pins(&self, jid: &str) -> Result<Vec<Pin>, String> {
        let client = Arc::clone(&self.client);
        let jid = jid.to_string();
        match runtime::spawn(async move { client.fetch_room_pins(jid).await }).await {
            Ok(Ok(entries)) => Ok(entries.into_iter().map(pin_from_entry).collect()),
            Ok(Err(error)) => {
                tracing::debug!(?error, "could not fetch the pinned messages");
                Err("The pinned messages could not be loaded.".to_string())
            }
            Err(_) => Err("The pinned messages could not be loaded.".to_string()),
        }
    }

    /// Pin or unpin the message with archive id `target`. True when the
    /// request was written; the broadcast confirms it.
    pub async fn set_pinned(&self, jid: &str, kind: Kind, target: String, pinned: bool) -> bool {
        let client = Arc::clone(&self.client);
        let peer = jid.to_string();
        runtime::spawn(async move {
            match (kind, pinned) {
                (Kind::Room, true) => client.pin_message(peer, target).await,
                (Kind::Room, false) => client.unpin_message(peer, target).await,
                (Kind::Direct, true) => client.pin_direct_message(peer, target).await,
                (Kind::Direct, false) => client.unpin_direct_message(peer, target).await,
            }
        })
        .await
        .unwrap_or(false)
    }

    /// Who may pin in the room, from its disco#info.
    pub async fn pin_permission(&self, room: &str) -> Option<PinPermission> {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        match runtime::spawn(async move { client.room_pin_permission(room).await }).await {
            Ok(Ok(Some(WaddlePinPermission::Anyone))) => Some(PinPermission::Anyone),
            Ok(Ok(Some(WaddlePinPermission::AdminsOnly))) => Some(PinPermission::AdminsOnly),
            _ => None,
        }
    }

    /// XEP-0313 full-text search of the conversation's archive, newest
    /// results as the server returns them.
    pub async fn search(
        &self,
        jid: &str,
        kind: Kind,
        query: String,
    ) -> Result<Vec<SearchHit>, String> {
        let peer: BareJid = jid
            .parse()
            .map_err(|_| "The conversation address is invalid.".to_string())?;
        let client = Arc::clone(&self.client);
        let address = jid.to_string();
        let page = runtime::spawn(async move {
            match kind {
                Kind::Room => {
                    client
                        .search_room_history(address, query, SEARCH_LIMIT)
                        .await
                }
                Kind::Direct => client.search_dm_history(address, query, SEARCH_LIMIT).await,
            }
        })
        .await
        .map_err(|_| "The search was interrupted.".to_string())?;
        Ok(hits_from(page.messages, &peer, kind, &self.me))
    }

    /// XEP-0425: a moderator removes the room message with archive id
    /// `target`.
    pub async fn remove_message(&self, room: &str, target: String) -> bool {
        let client = Arc::clone(&self.client);
        let room = room.to_string();
        runtime::spawn(async move { client.send_moderation(room, target, None).await })
            .await
            .unwrap_or(false)
    }
}

fn pin_from_preview(target: String, pinner: String, preview: WaddlePinPreview) -> Pin {
    Pin {
        target,
        pinner,
        message_at: preview.message_timestamp,
        author_nick: preview.author_nick,
        author_jid: preview.author_jid,
        text: preview.text,
    }
}

fn pin_from_entry(entry: WaddlePinEntry) -> Pin {
    pin_from_preview(entry.target_stanza_id, entry.pinner_jid, entry.preview)
}

fn change_from_event(event: WaddlePinEvent) -> PinChange {
    match event.action {
        WaddlePinAction::Pinned => PinChange::Pinned {
            pin: event
                .preview
                .map(|preview| pin_from_preview(event.target_stanza_id.clone(), event.by, preview)),
            target: event.target_stanza_id,
        },
        WaddlePinAction::Unpinned => PinChange::Unpinned {
            target: event.target_stanza_id,
        },
    }
}

/// A `<pin-event/>` message as a change to its conversation's pins, or
/// `None` for any other message.
///
/// A room's broadcast is trusted only from the bare room: an occupant can
/// send any payload under their own nick. A DM's comes from the server in
/// the sender's name, so the peer is whichever side is not this account.
pub fn pin_update(message: &WaddleMessage, me: &Me) -> Option<PinUpdate> {
    let event = message.pin_event.clone()?;
    let from: Jid = message.from.as_deref()?.parse().ok()?;
    let conversation = match message.message_type.as_str() {
        "groupchat" if from.resource().is_none() => from.to_bare(),
        "chat" if !message.muc_user && !message.is_muc => {
            if from.to_bare() == me.bare {
                message.to.as_deref()?.parse::<Jid>().ok()?.to_bare()
            } else {
                from.to_bare()
            }
        }
        _ => return None,
    };
    Some(PinUpdate {
        conversation: conversation.to_string(),
        change: change_from_event(event),
    })
}

fn hits_from(
    messages: Vec<waddle_xmpp_client_ffi::WaddleArchivedMessage>,
    conversation: &BareJid,
    kind: Kind,
    me: &Me,
) -> Vec<SearchHit> {
    let mut hits: Vec<SearchHit> = messages
        .into_iter()
        .filter_map(|message| archived_from(message, conversation, kind, me))
        // A correction or retraction is not a message of its own.
        .filter(|message| message.replaces_id.is_none() && !message.tombstone)
        .filter_map(|message| {
            Some(SearchHit {
                stanza_id: message.stanza_id?,
                sender: message.sender,
                body: message.body.filter(|body| !body.is_empty())?,
                timestamp_ms: message.timestamp_ms,
            })
        })
        .collect();
    // Newest first, as a results list reads.
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.timestamp_ms));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> Me {
        Me {
            bare: "me@waddle.test".parse().unwrap_or_else(|_| unreachable!()),
            nick: "me".into(),
        }
    }

    fn event(action: WaddlePinAction) -> WaddlePinEvent {
        WaddlePinEvent {
            action,
            target_stanza_id: "s1".into(),
            by: "admin@waddle.test".into(),
            reason: None,
            preview: Some(WaddlePinPreview {
                author_jid: "ann@waddle.test".into(),
                author_nick: Some("ann".into()),
                text: "hello".into(),
                message_timestamp: "2026-05-08T11:55:00+00:00".into(),
            }),
        }
    }

    fn wire(from: &str, to: &str, kind: &str, pin: WaddlePinEvent) -> WaddleMessage {
        let mut message = crate::session::tests::wire(from, kind, "admin pinned");
        message.to = Some(to.into());
        message.pin_event = Some(pin);
        message
    }

    #[test]
    fn a_room_broadcast_from_the_room_updates_its_pins() {
        let update = pin_update(
            &wire(
                "room@muc.waddle.test",
                "me@waddle.test/x",
                "groupchat",
                event(WaddlePinAction::Pinned),
            ),
            &me(),
        )
        .expect("a pin update");
        assert_eq!(update.conversation, "room@muc.waddle.test");
        match update.change {
            PinChange::Pinned { target, pin } => {
                assert_eq!(target, "s1");
                let pin = pin.expect("preview");
                assert_eq!(pin.pinner, "admin@waddle.test");
                assert_eq!(pin.text, "hello");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_occupant_cannot_forge_a_room_broadcast() {
        assert_eq!(
            pin_update(
                &wire(
                    "room@muc.waddle.test/mallory",
                    "me@waddle.test",
                    "groupchat",
                    event(WaddlePinAction::Unpinned)
                ),
                &me()
            ),
            None
        );
    }

    #[test]
    fn a_dm_broadcast_belongs_to_the_other_person() {
        let incoming = pin_update(
            &wire(
                "bob@waddle.test",
                "me@waddle.test",
                "chat",
                event(WaddlePinAction::Unpinned),
            ),
            &me(),
        )
        .expect("update");
        assert_eq!(incoming.conversation, "bob@waddle.test");
        assert_eq!(
            incoming.change,
            PinChange::Unpinned {
                target: "s1".into()
            }
        );
        // The copy for this account's other resource, from itself.
        let mirrored = pin_update(
            &wire(
                "me@waddle.test",
                "bob@waddle.test",
                "chat",
                event(WaddlePinAction::Unpinned),
            ),
            &me(),
        )
        .expect("update");
        assert_eq!(mirrored.conversation, "bob@waddle.test");
    }

    #[test]
    fn other_messages_are_not_pin_updates() {
        let mut plain = crate::session::tests::wire("bob@waddle.test", "chat", "hi");
        plain.to = Some("me@waddle.test".into());
        assert_eq!(pin_update(&plain, &me()), None);
    }
}
