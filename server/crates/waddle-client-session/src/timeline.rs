//! One conversation's timeline.
//!
//! Inputs are archived pages (older history, newest page first), live
//! messages, the user's own sends (local echo), and delivery outcomes. Each
//! returns the [`TimelineDiff`]s that bring a UI list in step, so a GTK
//! `ListModel`, a Compose list or a SwiftUI list updates only what changed.
//!
//! Identity follows XEP-0359. A message's [`ItemKey`] is its server-assigned
//! stanza id when known, otherwise the sender's origin id. A live echo of
//! the user's own send is matched to its local echo by origin id and then
//! takes over the server's stanza id, so history paged in later dedupes
//! against it.

use std::collections::HashMap;

/// Same-author messages closer than this group under one header
/// (inclusive; ADR-0019 behaviour table).
pub const GROUPING_WINDOW_MS: i64 = 5 * 60 * 1000;

/// A stable identity for one timeline row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemKey {
    /// XEP-0359 `<stanza-id/>` assigned by the archive.
    Stanza(String),
    /// XEP-0359 `<origin-id/>` set by the sender, until the archive id is
    /// known.
    Origin(String),
}

/// Where a row stands in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// Someone else's message, from history or live.
    Received,
    /// The user's send, written but not yet acknowledged by the server.
    Sending,
    /// The user's send, acknowledged (XEP-0198) or reflected by the room.
    Sent,
    /// The user's send failed; it can be retried or discarded.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    pub key: ItemKey,
    /// Display name of the sender: the room nick, or the DM peer's local
    /// part.
    pub sender: String,
    /// True for the user's own messages.
    pub own: bool,
    pub body: String,
    /// UTC milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
    pub delivery: Delivery,
    /// XEP-0308: the body was corrected.
    pub edited: bool,
    /// XEP-0424/0425: the message was retracted or moderated away.
    pub retracted: bool,
}

/// A message from an archive page (XEP-0313).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryMessage {
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    pub sender: String,
    pub own: bool,
    pub body: Option<String>,
    pub timestamp_ms: i64,
    pub replaces_id: Option<String>,
    pub retracted: bool,
}

/// A message that arrived live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveMessage {
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    pub sender: String,
    pub own: bool,
    pub body: Option<String>,
    pub timestamp_ms: i64,
    /// XEP-0308 correction of this id.
    pub replaces_id: Option<String>,
    /// XEP-0424 retraction of this id.
    pub retracts_id: Option<String>,
}

/// A change to the list a UI shows. Indices refer to the list as it stands
/// after every earlier diff in the same batch has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineDiff {
    /// Insert these items at the front (older history).
    PushFront { items: Vec<TimelineItem> },
    /// Append these items at the end (newer messages).
    Append { items: Vec<TimelineItem> },
    /// Replace the item at this index.
    Set { index: u32, item: TimelineItem },
}

#[derive(Debug, Default)]
pub struct Timeline {
    items: Vec<TimelineItem>,
    /// Every known id (stanza and origin) to the row's current position.
    index: HashMap<String, usize>,
    /// Oldest archive id seen, the cursor for the next older page.
    oldest_archive_id: Option<String>,
    history_complete: bool,
}

impl Timeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn items(&self) -> &[TimelineItem] {
        &self.items
    }

    /// The RSM `before` cursor for the next older page.
    pub fn older_cursor(&self) -> Option<&str> {
        self.oldest_archive_id.as_deref()
    }

    pub fn history_complete(&self) -> bool {
        self.history_complete
    }

    /// Add an archive page of older history. `messages` are in archive
    /// order (oldest first); `first_id` is the page's oldest archive id.
    /// Corrections and retractions in the page apply to rows already held.
    pub fn apply_history_page(
        &mut self,
        messages: Vec<HistoryMessage>,
        first_id: Option<String>,
        complete: bool,
    ) -> Vec<TimelineDiff> {
        if first_id.is_some() {
            self.oldest_archive_id = first_id;
        }
        self.history_complete = complete;

        let mut diffs = Vec::new();
        let mut older = Vec::new();
        let mut page_index: HashMap<String, usize> = HashMap::new();
        for message in messages {
            if let Some(target) = message.replaces_id.as_deref() {
                let body = message.body.unwrap_or_default();
                if let Some(&at) = page_index.get(target) {
                    let item: &mut TimelineItem = &mut older[at];
                    item.body = body;
                    item.edited = true;
                } else if let Some(diff) = self.edit_held(target, |item| {
                    item.body = body;
                    item.edited = true;
                }) {
                    diffs.push(diff);
                }
                continue;
            }
            let Some(key) = key_for(message.stanza_id.as_deref(), message.origin_id.as_deref())
            else {
                continue;
            };
            if self.knows(message.stanza_id.as_deref(), message.origin_id.as_deref()) {
                continue;
            }
            let Some(body) = message.body else {
                continue;
            };
            for id in [&message.stanza_id, &message.origin_id]
                .into_iter()
                .flatten()
            {
                page_index.insert(id.clone(), older.len());
            }
            older.push(TimelineItem {
                key,
                sender: message.sender,
                own: message.own,
                body,
                timestamp_ms: message.timestamp_ms,
                delivery: if message.own {
                    Delivery::Sent
                } else {
                    Delivery::Received
                },
                edited: false,
                retracted: message.retracted,
            });
        }

        if !older.is_empty() {
            let shift = older.len();
            let mut items = older.clone();
            items.append(&mut self.items);
            self.items = items;
            self.reindex_from(0);
            // Edits to already-held rows were computed before the shift.
            for diff in &mut diffs {
                if let TimelineDiff::Set { index, .. } = diff {
                    *index += u32::try_from(shift).unwrap_or(u32::MAX);
                }
            }
            diffs.insert(0, TimelineDiff::PushFront { items: older });
        }
        diffs
    }

    /// The user sent a message: show it at once as `Sending`.
    pub fn add_local_echo(
        &mut self,
        origin_id: String,
        sender: String,
        body: String,
        timestamp_ms: i64,
    ) -> Vec<TimelineDiff> {
        if self.index.contains_key(&origin_id) {
            return Vec::new();
        }
        let item = TimelineItem {
            key: ItemKey::Origin(origin_id.clone()),
            sender,
            own: true,
            body,
            timestamp_ms,
            delivery: Delivery::Sending,
            edited: false,
            retracted: false,
        };
        self.index.insert(origin_id, self.items.len());
        self.items.push(item.clone());
        vec![TimelineDiff::Append { items: vec![item] }]
    }

    /// The server acknowledged (or failed) the send with this id.
    pub fn set_delivery(&mut self, id: &str, delivery: Delivery) -> Vec<TimelineDiff> {
        self.edit_held(id, |item| {
            if item.own && item.delivery != Delivery::Sent {
                item.delivery = delivery;
            }
        })
        .into_iter()
        .collect()
    }

    /// A message arrived live.
    pub fn apply_live(&mut self, message: LiveMessage) -> Vec<TimelineDiff> {
        if let Some(target) = message.retracts_id.as_deref() {
            return self
                .edit_held(target, |item| {
                    item.retracted = true;
                    item.body.clear();
                })
                .into_iter()
                .collect();
        }
        if let Some(target) = message.replaces_id.as_deref() {
            let body = message.body.unwrap_or_default();
            return self
                .edit_held(target, |item| {
                    item.body = body;
                    item.edited = true;
                })
                .into_iter()
                .collect();
        }
        let Some(body) = message.body else {
            return Vec::new();
        };

        // The room reflecting the user's own send: take over the local
        // echo and its server-assigned id.
        if let Some(origin) = message.origin_id.as_deref() {
            if let Some(&at) = self.index.get(origin) {
                let item = &mut self.items[at];
                if let Some(stanza_id) = message.stanza_id.clone() {
                    item.key = ItemKey::Stanza(stanza_id.clone());
                    self.index.insert(stanza_id, at);
                }
                if item.own {
                    item.delivery = Delivery::Sent;
                }
                let item = item.clone();
                return vec![TimelineDiff::Set {
                    index: u32::try_from(at).unwrap_or(u32::MAX),
                    item,
                }];
            }
        }
        if self.knows(message.stanza_id.as_deref(), message.origin_id.as_deref()) {
            return Vec::new();
        }
        let Some(key) = key_for(message.stanza_id.as_deref(), message.origin_id.as_deref()) else {
            return Vec::new();
        };
        let item = TimelineItem {
            key,
            sender: message.sender,
            own: message.own,
            body,
            timestamp_ms: message.timestamp_ms,
            delivery: if message.own {
                Delivery::Sent
            } else {
                Delivery::Received
            },
            edited: false,
            retracted: false,
        };
        let at = self.items.len();
        for id in [&message.stanza_id, &message.origin_id]
            .into_iter()
            .flatten()
        {
            self.index.insert(id.clone(), at);
        }
        self.items.push(item.clone());
        vec![TimelineDiff::Append { items: vec![item] }]
    }

    /// Whether the item at `index` continues the group of the one before
    /// it: same sender, within [`GROUPING_WINDOW_MS`] (inclusive), and on
    /// the same day in the given UTC offset. A UI shows the sender header
    /// only when this is false.
    pub fn continues_group(&self, index: usize, utc_offset_ms: i64) -> bool {
        let (Some(previous), Some(item)) = (
            index.checked_sub(1).and_then(|at| self.items.get(at)),
            self.items.get(index),
        ) else {
            return false;
        };
        let day = |ms: i64| (ms + utc_offset_ms).div_euclid(24 * 60 * 60 * 1000);
        previous.sender == item.sender
            && previous.own == item.own
            && !previous.retracted
            && !item.retracted
            && item.timestamp_ms - previous.timestamp_ms <= GROUPING_WINDOW_MS
            && item.timestamp_ms >= previous.timestamp_ms
            && day(previous.timestamp_ms) == day(item.timestamp_ms)
    }

    fn knows(&self, stanza_id: Option<&str>, origin_id: Option<&str>) -> bool {
        stanza_id.is_some_and(|id| self.index.contains_key(id))
            || origin_id.is_some_and(|id| self.index.contains_key(id))
    }

    fn edit_held(
        &mut self,
        id: &str,
        edit: impl FnOnce(&mut TimelineItem),
    ) -> Option<TimelineDiff> {
        let at = *self.index.get(id)?;
        let item = self.items.get_mut(at)?;
        let before = item.clone();
        edit(item);
        (item != &before).then(|| TimelineDiff::Set {
            index: u32::try_from(at).unwrap_or(u32::MAX),
            item: item.clone(),
        })
    }

    fn reindex_from(&mut self, start: usize) {
        self.index.retain(|_, at| *at < start);
        for (at, item) in self.items.iter().enumerate().skip(start) {
            match &item.key {
                ItemKey::Stanza(id) | ItemKey::Origin(id) => {
                    self.index.insert(id.clone(), at);
                }
            }
        }
        // Origin ids of rows that have taken over a stanza id stay
        // resolvable through the live path's own inserts; after a reindex
        // only the current key is kept, which is all dedupe needs once the
        // stanza id is known.
    }
}

fn key_for(stanza_id: Option<&str>, origin_id: Option<&str>) -> Option<ItemKey> {
    stanza_id
        .map(|id| ItemKey::Stanza(id.to_string()))
        .or_else(|| origin_id.map(|id| ItemKey::Origin(id.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60 * 1000;

    fn history(stanza: &str, sender: &str, body: &str, at: i64) -> HistoryMessage {
        HistoryMessage {
            stanza_id: Some(stanza.into()),
            origin_id: None,
            sender: sender.into(),
            own: false,
            body: Some(body.into()),
            timestamp_ms: at,
            replaces_id: None,
            retracted: false,
        }
    }

    fn live(stanza: &str, sender: &str, body: &str, at: i64) -> LiveMessage {
        LiveMessage {
            stanza_id: Some(stanza.into()),
            origin_id: None,
            sender: sender.into(),
            own: false,
            body: Some(body.into()),
            timestamp_ms: at,
            replaces_id: None,
            retracts_id: None,
        }
    }

    fn bodies(timeline: &Timeline) -> Vec<&str> {
        timeline
            .items()
            .iter()
            .map(|item| item.body.as_str())
            .collect()
    }

    #[test]
    fn history_pages_prepend_in_order_and_move_the_cursor() {
        let mut timeline = Timeline::new();
        let diffs = timeline.apply_history_page(
            vec![
                history("s3", "bob", "three", 3),
                history("s4", "bob", "four", 4),
            ],
            Some("s3".into()),
            false,
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(timeline.older_cursor(), Some("s3"));

        let diffs = timeline.apply_history_page(
            vec![
                history("s1", "ann", "one", 1),
                history("s2", "ann", "two", 2),
            ],
            Some("s1".into()),
            true,
        );
        assert!(matches!(&diffs[0], TimelineDiff::PushFront { items } if items.len() == 2));
        assert_eq!(bodies(&timeline), ["one", "two", "three", "four"]);
        assert!(timeline.history_complete());
    }

    #[test]
    fn history_skips_rows_already_held() {
        let mut timeline = Timeline::new();
        timeline.apply_live(live("s2", "bob", "two", 2));
        let diffs = timeline.apply_history_page(
            vec![
                history("s1", "ann", "one", 1),
                history("s2", "bob", "two", 2),
            ],
            Some("s1".into()),
            true,
        );
        assert!(matches!(&diffs[0], TimelineDiff::PushFront { items } if items.len() == 1));
        assert_eq!(bodies(&timeline), ["one", "two"]);
    }

    #[test]
    fn own_send_echoes_then_takes_the_reflection() {
        let mut timeline = Timeline::new();
        let diffs = timeline.add_local_echo("o1".into(), "me".into(), "hello".into(), 10);
        assert!(
            matches!(&diffs[0], TimelineDiff::Append { items } if items[0].delivery == Delivery::Sending)
        );

        let diffs = timeline.apply_live(LiveMessage {
            stanza_id: Some("s9".into()),
            origin_id: Some("o1".into()),
            sender: "me".into(),
            own: true,
            body: Some("hello".into()),
            timestamp_ms: 11,
            replaces_id: None,
            retracts_id: None,
        });
        let [TimelineDiff::Set { index: 0, item }] = diffs.as_slice() else {
            panic!("expected the echo to be replaced in place: {diffs:?}");
        };
        assert_eq!(item.key, ItemKey::Stanza("s9".into()));
        assert_eq!(item.delivery, Delivery::Sent);
        assert_eq!(timeline.items().len(), 1);

        // The same message paged in from history later is not duplicated.
        let diffs = timeline.apply_history_page(
            vec![history("s9", "me", "hello", 11)],
            Some("s9".into()),
            true,
        );
        assert!(diffs.is_empty());
        assert_eq!(timeline.items().len(), 1);
    }

    #[test]
    fn delivery_outcomes_update_only_pending_own_sends() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo("o1".into(), "me".into(), "hi".into(), 1);
        let diffs = timeline.set_delivery("o1", Delivery::Failed);
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { item, .. } if item.delivery == Delivery::Failed)
        );
        assert!(timeline.set_delivery("unknown", Delivery::Sent).is_empty());

        timeline.apply_live(live("s1", "bob", "yo", 2));
        assert!(timeline.set_delivery("s1", Delivery::Failed).is_empty());
    }

    #[test]
    fn corrections_and_retractions_edit_in_place() {
        let mut timeline = Timeline::new();
        timeline.apply_live(live("s1", "bob", "helo", 1));
        let diffs = timeline.apply_live(LiveMessage {
            replaces_id: Some("s1".into()),
            ..live("s2", "bob", "hello", 2)
        });
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { index: 0, item } if item.body == "hello" && item.edited)
        );

        let diffs = timeline.apply_live(LiveMessage {
            retracts_id: Some("s1".into()),
            body: None,
            ..live("s3", "bob", "", 3)
        });
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { index: 0, item } if item.retracted && item.body.is_empty())
        );
        assert_eq!(timeline.items().len(), 1);
    }

    #[test]
    fn edits_to_held_rows_from_an_older_page_shift_their_index() {
        let mut timeline = Timeline::new();
        timeline.apply_live(live("s5", "bob", "typo", 5));
        // An older page that holds an earlier message and, oddly, a later
        // correction of the held row (archives return corrections in place).
        let diffs = timeline.apply_history_page(
            vec![
                history("s1", "ann", "first", 1),
                HistoryMessage {
                    replaces_id: Some("s5".into()),
                    ..history("s6", "bob", "fixed", 6)
                },
            ],
            Some("s1".into()),
            false,
        );
        assert!(matches!(&diffs[0], TimelineDiff::PushFront { items } if items.len() == 1));
        assert!(matches!(&diffs[1], TimelineDiff::Set { index: 1, item } if item.body == "fixed"));
        assert_eq!(bodies(&timeline), ["first", "fixed"]);
    }

    #[test]
    fn grouping_is_same_sender_within_five_minutes_inclusive_and_same_day() {
        let mut timeline = Timeline::new();
        let day = 24 * 60 * MINUTE;
        for (id, sender, at) in [
            ("a", "bob", day + 10 * MINUTE),
            ("b", "bob", day + 15 * MINUTE), // exactly 5 minutes: continues
            ("c", "bob", day + 20 * MINUTE + 1), // 5 min + 1 ms: new group
            ("d", "ann", day + 21 * MINUTE), // other sender
            ("e", "ann", 2 * day + MINUTE),  // next day
        ] {
            timeline.apply_live(live(id, sender, "x", at));
        }
        let groups: Vec<bool> = (0..5).map(|i| timeline.continues_group(i, 0)).collect();
        assert_eq!(groups, [false, true, false, false, false]);
    }

    #[test]
    fn messages_without_identity_or_body_are_ignored() {
        let mut timeline = Timeline::new();
        assert!(timeline
            .apply_live(LiveMessage {
                stanza_id: None,
                origin_id: None,
                ..live("x", "bob", "no id", 1)
            })
            .is_empty());
        assert!(timeline
            .apply_live(LiveMessage {
                body: None,
                ..live("s1", "bob", "", 1)
            })
            .is_empty());
    }
}
