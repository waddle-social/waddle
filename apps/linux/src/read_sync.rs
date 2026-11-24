//! Read markers: what to tell the other side and our other devices when a
//! conversation is read, and what to make of what they tell us.
//!
//! * XEP-0333 `<displayed/>`: sent for the newest message from someone
//!   else. In a room it names the room's stanza id. In a DM it names the
//!   message's `@id` and goes only to a sender who asked (`<markable/>`).
//! * XEP-0490 Message Displayed Synchronization: a per-chat cursor on the
//!   account (a PEP item), published with the stanza id and the entity that
//!   assigned it: the room in a room, our own account in a DM. A cursor
//!   another device publishes moves our unread count.
//!
//! The server inbox read (XEP-0430, `Session::mark_read`) stays as it is;
//! this adds the cursor beside it. No GTK here.

use std::collections::{HashMap, HashSet};

use waddle_client_session::{ItemKey, TimelineItem};
use waddle_xmpp_client_ffi::{WaddleMdsDisplayedEntry, WaddleMessage};

use crate::session::Kind;

/// A XEP-0490 cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub conversation: String,
    pub stanza_id: String,
    /// The `by` of the stanza id: who assigned it.
    pub by: String,
}

impl From<WaddleMdsDisplayedEntry> for Cursor {
    fn from(entry: WaddleMdsDisplayedEntry) -> Self {
        Self {
            conversation: entry.chat_id,
            stanza_id: entry.stanza_id,
            by: entry.stanza_id_by,
        }
    }
}

/// What to send for one conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatch {
    /// XEP-0333: the id to mark displayed, when the sender may be told.
    pub marker: Option<String>,
    /// XEP-0490: the cursor to publish.
    pub cursor: Option<Cursor>,
}

/// What a message says about reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
    /// A DM from someone else asked to be told when it is displayed.
    Markable {
        conversation: String,
        /// Our timeline's id for it (origin id, else `@id`).
        reference: String,
        /// The `@id` the marker must copy.
        message_id: String,
    },
    /// The peer displayed one of our messages (XEP-0333, DM).
    PeerDisplayed {
        conversation: String,
        /// Our id for the message they read.
        id: String,
    },
    /// Our account's cursors (XEP-0490 PEP event).
    Cursors(Vec<Cursor>),
}

/// The signals in a live message, if any.
pub fn signals(message: &WaddleMessage, me: &jid::BareJid) -> Vec<Signal> {
    let mut found = Vec::new();
    if let Some(entries) = &message.mds_displayed {
        // Only our own account can publish our cursors.
        let from_account = message
            .from
            .as_deref()
            .and_then(|from| from.parse::<jid::Jid>().ok())
            .is_some_and(|from| from.to_bare() == *me);
        if from_account {
            found.push(Signal::Cursors(
                entries.iter().cloned().map(Cursor::from).collect(),
            ));
        }
        return found;
    }
    let Some(from) = message
        .from
        .as_deref()
        .and_then(|from| from.parse::<jid::Jid>().ok())
    else {
        return found;
    };
    let direct = message.message_type == "chat" && !message.muc_user && !message.is_muc;
    let peer = from.to_bare();
    if !direct || peer == *me {
        return found;
    }
    if message.displayed_marker_requested {
        if let Some(message_id) = message.id.clone() {
            let reference = message
                .origin_id
                .clone()
                .unwrap_or_else(|| message_id.clone());
            found.push(Signal::Markable {
                conversation: peer.to_string(),
                reference,
                message_id,
            });
        }
    }
    if let Some(id) = &message.displayed_marker_id {
        found.push(Signal::PeerDisplayed {
            conversation: peer.to_string(),
            id: id.clone(),
        });
    }
    found
}

/// Whether a message is just a marker or cursor, with nothing to show.
pub fn is_receipt_only(message: &WaddleMessage) -> bool {
    let empty = message.body.as_deref().is_none_or(str::is_empty);
    empty && (message.displayed_marker_id.is_some() || message.mds_displayed.is_some())
}

/// The newest message from someone else, as a target.
struct Target {
    /// The timeline's id for it.
    reference: String,
    stanza_id: Option<String>,
}

fn newest_incoming(items: &[TimelineItem]) -> Option<Target> {
    let item = items
        .iter()
        .rev()
        .find(|item| !item.own && !item.retracted && item.reference_id.is_some())?;
    Some(Target {
        reference: item.reference_id.clone()?,
        stanza_id: match &item.key {
            ItemKey::Stanza(id) => Some(id.clone()),
            ItemKey::Origin(_) => None,
        },
    })
}

/// Marker and cursor state of one signed-in account.
#[derive(Debug, Default)]
pub struct ReadMarks {
    /// DMs that asked for a marker: our id for them, to the `@id`.
    markable: HashMap<String, String>,
    /// The newest id published per conversation, so a read is told once.
    published: HashMap<String, String>,
    /// The newest of our messages the peer has displayed, per DM: its
    /// timestamp. Everything of ours at or before it is read.
    peer_read_ms: HashMap<String, i64>,
    /// XEP-0490 §3: the server supports publish options (probed once).
    pub publish_supported: Option<bool>,
}

impl ReadMarks {
    pub fn note_markable(&mut self, reference: String, message_id: String) {
        // Bounded: only the newest few matter, and markers name the newest.
        if self.markable.len() > 512 {
            self.markable.clear();
        }
        self.markable.insert(reference, message_id);
    }

    /// What to send now that `conversation` has been read, or nothing if
    /// the newest message was already told or has no id to tell it by.
    pub fn plan(
        &self,
        conversation: &str,
        kind: Kind,
        items: &[TimelineItem],
        me: &jid::BareJid,
    ) -> Option<Dispatch> {
        let target = newest_incoming(items)?;
        let (marker, cursor, told) = match kind {
            Kind::Room => {
                let id = target.stanza_id?;
                (
                    Some(id.clone()),
                    Some(Cursor {
                        conversation: conversation.to_string(),
                        stanza_id: id.clone(),
                        by: conversation.to_string(),
                    }),
                    id,
                )
            }
            Kind::Direct => {
                let marker = self.markable.get(&target.reference).cloned();
                let cursor = target.stanza_id.clone().map(|id| Cursor {
                    conversation: conversation.to_string(),
                    stanza_id: id,
                    by: me.to_string(),
                });
                (
                    marker,
                    cursor,
                    target.stanza_id.unwrap_or_else(|| target.reference.clone()),
                )
            }
        };
        if marker.is_none() && cursor.is_none() {
            return None;
        }
        if self.published.get(conversation) == Some(&told) {
            return None;
        }
        Some(Dispatch { marker, cursor })
    }

    /// The dispatch was sent: remember the id it covered.
    pub fn published(&mut self, conversation: &str, dispatch: &Dispatch, kind: Kind) {
        let told = match (&dispatch.cursor, &dispatch.marker, kind) {
            (Some(cursor), _, _) => Some(cursor.stanza_id.clone()),
            (None, Some(marker), _) => Some(marker.clone()),
            _ => None,
        };
        if let Some(told) = told {
            self.published.insert(conversation.to_string(), told);
        }
    }

    /// A cursor from the server (a device of ours read here): it is also
    /// what we would publish, so do not publish it back.
    pub fn adopt(&mut self, cursor: &Cursor) {
        self.published
            .insert(cursor.conversation.clone(), cursor.stanza_id.clone());
    }

    /// The peer displayed our message `id`. True when the read mark moved.
    pub fn peer_displayed(&mut self, conversation: &str, id: &str, items: &[TimelineItem]) -> bool {
        let Some(item) = items.iter().find(|item| {
            item.own
                && (item.reference_id.as_deref() == Some(id)
                    || item.correction_id.as_deref() == Some(id))
        }) else {
            return false;
        };
        let at = item.timestamp_ms;
        let held = self
            .peer_read_ms
            .entry(conversation.to_string())
            .or_insert(i64::MIN);
        if at > *held {
            *held = at;
            true
        } else {
            false
        }
    }

    /// Whether the peer has read our message.
    pub fn is_read(&self, conversation: &str, item: &TimelineItem) -> bool {
        item.own
            && self
                .peer_read_ms
                .get(conversation)
                .is_some_and(|read| item.timestamp_ms <= *read)
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// A cursor from another device, applied to what is loaded: how many
/// unread messages remain after it, if it names a loaded message and is
/// one we trust. Only the room (in a room) or our account or its server
/// (in a DM) assigns the ids a cursor may name.
pub fn remaining_after(
    cursor: &Cursor,
    kind: Kind,
    items: &[TimelineItem],
    me: &jid::BareJid,
) -> Option<u32> {
    let trusted = match kind {
        Kind::Room => cursor.by == cursor.conversation,
        Kind::Direct => cursor.by == me.as_str() || cursor.by == me.domain().as_str(),
    };
    if !trusted {
        return None;
    }
    let at = items
        .iter()
        .rposition(|item| matches!(&item.key, ItemKey::Stanza(id) if *id == cursor.stanza_id))?;
    let remaining = items[at + 1..]
        .iter()
        .filter(|item| !item.own && !item.retracted)
        .count();
    Some(u32::try_from(remaining).unwrap_or(u32::MAX))
}

/// Conversations named in a set of cursors, newest per conversation.
pub fn latest_per_conversation(cursors: Vec<Cursor>) -> Vec<Cursor> {
    let mut seen = HashSet::new();
    let mut kept: Vec<Cursor> = Vec::new();
    for cursor in cursors.into_iter().rev() {
        if seen.insert(cursor.conversation.clone()) {
            kept.push(cursor);
        }
    }
    kept.reverse();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_client_session::{IncomingMessage, Timeline};

    fn me() -> jid::BareJid {
        "me@example.com".parse().unwrap()
    }

    fn stanza(id: &str, reference: &str, own: bool, at: i64) -> TimelineItem {
        built(Some(id), reference, own, at)
    }

    fn built(stanza: Option<&str>, reference: &str, own: bool, at: i64) -> TimelineItem {
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            stanza_id: stanza.map(str::to_string),
            reference_id: Some(reference.to_string()),
            origin_id: Some(reference.to_string()),
            message_id: Some(reference.to_string()),
            sender: "x".into(),
            own,
            body: Some("hi".into()),
            timestamp_ms: at,
            ..IncomingMessage::default()
        });
        timeline.items()[0].clone()
    }

    #[test]
    fn a_room_read_marks_the_rooms_newest_stanza_id_and_publishes_with_the_room_as_authority() {
        let marks = ReadMarks::default();
        let items = [
            stanza("s1", "s1", false, 1),
            stanza("s2", "s2", false, 2),
            stanza("s3", "s3", true, 3),
        ];
        let dispatch = marks
            .plan("room@muc.example.com", Kind::Room, &items, &me())
            .unwrap();
        assert_eq!(dispatch.marker.as_deref(), Some("s2"));
        assert_eq!(
            dispatch.cursor,
            Some(Cursor {
                conversation: "room@muc.example.com".into(),
                stanza_id: "s2".into(),
                by: "room@muc.example.com".into()
            })
        );
    }

    #[test]
    fn a_dm_marker_goes_only_to_a_sender_who_asked_and_names_the_message_id() {
        let mut marks = ReadMarks::default();
        let items = [stanza("a1", "origin-1", false, 1)];
        let quiet = marks
            .plan("bo@example.com", Kind::Direct, &items, &me())
            .unwrap();
        assert_eq!(quiet.marker, None, "no markable, no marker");
        assert_eq!(quiet.cursor.as_ref().unwrap().by, "me@example.com");

        marks.note_markable("origin-1".into(), "msg-id-1".into());
        let asked = marks
            .plan("bo@example.com", Kind::Direct, &items, &me())
            .unwrap();
        assert_eq!(asked.marker.as_deref(), Some("msg-id-1"));
    }

    #[test]
    fn a_read_is_told_once_and_again_after_something_newer() {
        let mut marks = ReadMarks::default();
        let mut items = vec![stanza("s1", "s1", false, 1)];
        let dispatch = marks.plan("room", Kind::Room, &items, &me()).unwrap();
        marks.published("room", &dispatch, Kind::Room);
        assert!(marks.plan("room", Kind::Room, &items, &me()).is_none());
        items.push(stanza("s2", "s2", false, 2));
        assert!(marks.plan("room", Kind::Room, &items, &me()).is_some());
    }

    #[test]
    fn nothing_to_tell_without_an_incoming_message_or_an_id() {
        let marks = ReadMarks::default();
        assert!(marks.plan("room", Kind::Room, &[], &me()).is_none());
        assert!(marks
            .plan("room", Kind::Room, &[stanza("s", "s", true, 1)], &me())
            .is_none());
        // An echo with no stanza id yet cannot name a cursor in a room.
        let unarchived = [built(None, "o", false, 1)];
        assert!(marks.plan("room", Kind::Room, &unarchived, &me()).is_none());
        // Retracted messages are not what was read.
        let mut gone = stanza("s", "s", false, 1);
        gone.retracted = true;
        assert!(marks.plan("room", Kind::Room, &[gone], &me()).is_none());
    }

    #[test]
    fn a_cursor_from_the_server_is_not_published_back() {
        let mut marks = ReadMarks::default();
        let items = [stanza("s1", "s1", false, 1)];
        marks.adopt(&Cursor {
            conversation: "room".into(),
            stanza_id: "s1".into(),
            by: "room".into(),
        });
        assert!(marks.plan("room", Kind::Room, &items, &me()).is_none());
    }

    #[test]
    fn a_cursor_sets_the_unread_count_after_the_message_it_names() {
        let items = [
            stanza("s1", "s1", false, 1),
            stanza("s2", "s2", false, 2),
            stanza("s3", "s3", true, 3),
            stanza("s4", "s4", false, 4),
        ];
        let cursor = |id: &str, by: &str| Cursor {
            conversation: "room".into(),
            stanza_id: id.into(),
            by: by.into(),
        };
        assert_eq!(
            remaining_after(&cursor("s2", "room"), Kind::Room, &items, &me()),
            Some(1)
        );
        assert_eq!(
            remaining_after(&cursor("s4", "room"), Kind::Room, &items, &me()),
            Some(0)
        );
        // Unknown message: ignored. Wrong authority: not trusted.
        assert_eq!(
            remaining_after(&cursor("zz", "room"), Kind::Room, &items, &me()),
            None
        );
        assert_eq!(
            remaining_after(&cursor("s2", "evil@example.com"), Kind::Room, &items, &me()),
            None
        );
    }

    #[test]
    fn a_dm_cursor_is_trusted_from_our_account_or_its_server() {
        let items = [stanza("a1", "a1", false, 1), stanza("a2", "a2", false, 2)];
        let cursor = |by: &str| Cursor {
            conversation: "bo@example.com".into(),
            stanza_id: "a1".into(),
            by: by.into(),
        };
        assert_eq!(
            remaining_after(&cursor("me@example.com"), Kind::Direct, &items, &me()),
            Some(1)
        );
        assert_eq!(
            remaining_after(&cursor("example.com"), Kind::Direct, &items, &me()),
            Some(1)
        );
        assert_eq!(
            remaining_after(&cursor("bo@example.com"), Kind::Direct, &items, &me()),
            None
        );
    }

    #[test]
    fn the_peer_reading_our_message_reads_everything_before_it() {
        let mut marks = ReadMarks::default();
        let items = [
            stanza("a1", "m1", true, 10),
            stanza("a2", "m2", true, 20),
            stanza("a3", "m3", true, 30),
        ];
        assert!(marks.peer_displayed("bo", "m2", &items));
        assert!(marks.is_read("bo", &items[0]));
        assert!(marks.is_read("bo", &items[1]));
        assert!(!marks.is_read("bo", &items[2]));
        assert!(!marks.is_read("other", &items[0]));
        // An older marker does not move it back; an unknown one does nothing.
        assert!(!marks.peer_displayed("bo", "m1", &items));
        assert!(!marks.peer_displayed("bo", "nope", &items));
        // Theirs is never "read".
        assert!(!marks.is_read("bo", &stanza("b1", "b1", false, 5)));
    }

    #[test]
    fn newest_cursor_per_conversation_wins() {
        let cursor = |chat: &str, id: &str| Cursor {
            conversation: chat.into(),
            stanza_id: id.into(),
            by: chat.into(),
        };
        let kept =
            latest_per_conversation(vec![cursor("a", "1"), cursor("b", "1"), cursor("a", "2")]);
        assert_eq!(kept, [cursor("b", "1"), cursor("a", "2")]);
    }

    fn wire(from: &str, kind: &str) -> WaddleMessage {
        let mut message = crate::session::tests::wire(from, kind, "hi");
        message.body = None;
        message
    }

    #[test]
    fn signals_read_markable_displayed_and_our_own_cursors() {
        let mut asked = wire("bo@example.com/phone", "chat");
        asked.displayed_marker_requested = true;
        asked.body = Some("hello".into());
        assert_eq!(
            signals(&asked, &me()),
            [Signal::Markable {
                conversation: "bo@example.com".into(),
                reference: "o1".into(),
                message_id: "m1".into()
            }]
        );

        let mut read = wire("bo@example.com/phone", "chat");
        read.displayed_marker_id = Some("mine".into());
        assert_eq!(
            signals(&read, &me()),
            [Signal::PeerDisplayed {
                conversation: "bo@example.com".into(),
                id: "mine".into()
            }]
        );
        assert!(is_receipt_only(&read));

        let mut pep = wire("me@example.com", "headline");
        pep.mds_displayed = Some(vec![WaddleMdsDisplayedEntry {
            chat_id: "room@muc.example.com".into(),
            stanza_id: "s9".into(),
            stanza_id_by: "room@muc.example.com".into(),
        }]);
        assert_eq!(
            signals(&pep, &me()),
            [Signal::Cursors(vec![Cursor {
                conversation: "room@muc.example.com".into(),
                stanza_id: "s9".into(),
                by: "room@muc.example.com".into()
            }])]
        );
        assert!(is_receipt_only(&pep));
    }

    #[test]
    fn a_cursor_event_from_anyone_but_our_account_is_ignored() {
        let mut pep = wire("mallory@example.com", "headline");
        pep.mds_displayed = Some(vec![WaddleMdsDisplayedEntry {
            chat_id: "room".into(),
            stanza_id: "s".into(),
            stanza_id_by: "room".into(),
        }]);
        assert!(signals(&pep, &me()).is_empty());
    }

    #[test]
    fn markers_from_rooms_and_from_our_own_devices_are_not_peer_reads() {
        let mut room = wire("room@muc.example.com/ann", "groupchat");
        room.displayed_marker_id = Some("x".into());
        assert!(signals(&room, &me()).is_empty());
        let mut own = wire("me@example.com/laptop", "chat");
        own.displayed_marker_id = Some("x".into());
        assert!(signals(&own, &me()).is_empty());
    }
}
