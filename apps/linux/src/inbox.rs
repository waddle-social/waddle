//! The unread overview (web's `/unread`): one card per room or DM with
//! unread messages, and for each unread thread its unread replies.
//!
//! Everything here is plain data and decisions. `ui/inbox.rs` draws the
//! cards; the one adapter, [`Session::inbox_section`], asks the archive.
//! Unread counts come from the XEP-0430 inbox (kept by the window); the
//! messages themselves are read from XEP-0313 archives, with the Waddle
//! MAM thread filter for a room's threads.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use jid::BareJid;
use waddle_client_session::IncomingMessage;
use waddle_xmpp_client_ffi::{WaddleArchivedMessage, WaddleMamPage};

use crate::runtime;
use crate::session::{archived_from, Conversation, Kind, Me, Session};
use crate::threads::{self, Thread};

/// Web parity: the unread count plus some context, capped.
pub fn fetch_size(unread: u32) -> u32 {
    unread.saturating_add(20).min(100)
}

/// Sections fetched at once.
pub const CONCURRENCY: usize = 4;

/// What the window knows about a conversation's newest inbox row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// Waddle `last-updated`, epoch seconds.
    pub last_updated: Option<i64>,
    pub last_stanza_id: Option<String>,
}

/// One section of a card: a feed or one thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionPlan {
    pub conversation: Conversation,
    pub unread: u32,
    pub mention: bool,
    pub meta: Meta,
}

impl SectionPlan {
    /// What a cached fetch must still match to be current.
    pub fn signature(&self) -> Signature {
        Signature {
            unread: self.unread,
            last_updated: self.meta.last_updated,
            last_stanza_id: self.meta.last_stanza_id.clone(),
        }
    }
}

/// A room or DM with something unread, before any message is fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardPlan {
    pub jid: String,
    pub kind: Kind,
    pub title: String,
    /// The conversation's own feed, when it has unread rows.
    pub feed: Option<SectionPlan>,
    /// Unread threads, newest first. A sub-thread is its own row.
    pub threads: Vec<SectionPlan>,
    /// Newest activity of the feed and its threads, epoch seconds.
    pub last_updated: Option<i64>,
}

impl CardPlan {
    /// The card's badge. The server counts a room's thread replies in the
    /// room's row, while a live reply only bumps its thread, so neither
    /// sum alone is right.
    pub fn unread(&self) -> u32 {
        let feed = self.feed.as_ref().map_or(0, |feed| feed.unread);
        let threads = self
            .threads
            .iter()
            .fold(0u32, |sum, thread| sum.saturating_add(thread.unread));
        feed.max(threads)
    }

    pub fn mention(&self) -> bool {
        self.feed.iter().chain(&self.threads).any(|s| s.mention)
    }

    pub fn sections(&self) -> impl Iterator<Item = &SectionPlan> {
        self.feed.iter().chain(&self.threads)
    }

    /// Every conversation this card clears when marked read.
    pub fn conversations(&self) -> Vec<Conversation> {
        self.sections()
            .map(|section| section.conversation.clone())
            .collect()
    }
}

/// Cards for every conversation with unread rows, newest first (ties by
/// JID). `conversations` holds a room's or DM's own entry and, after it,
/// the threads discovered for it. `activity` is newer than `meta` when a
/// message arrived live.
pub fn plan(
    conversations: &[Conversation],
    unread: &HashMap<String, u32>,
    mentions: &HashSet<String>,
    meta: &HashMap<String, Meta>,
    activity: &HashMap<String, i64>,
) -> Vec<CardPlan> {
    let section = |conversation: &Conversation| -> Option<SectionPlan> {
        let key = conversation.key();
        let count = unread.get(&key).copied().filter(|count| *count > 0)?;
        let mut meta = meta.get(&key).cloned().unwrap_or_default();
        // A live arrival is newer than the snapshot it follows.
        if let Some(seen) = activity.get(&key) {
            meta.last_updated = meta.last_updated.max(Some(*seen));
        }
        Some(SectionPlan {
            conversation: conversation.clone(),
            unread: count,
            mention: mentions.contains(&key),
            meta,
        })
    };
    let mut cards: Vec<CardPlan> = Vec::new();
    for conversation in conversations {
        let Some(section) = section(conversation) else {
            continue;
        };
        let at = match cards.iter().position(|card| card.jid == conversation.jid) {
            Some(at) => at,
            None => {
                let base = conversations
                    .iter()
                    .find(|known| known.jid == conversation.jid && known.thread.is_none())
                    .unwrap_or(conversation);
                cards.push(CardPlan {
                    jid: base.jid.clone(),
                    kind: base.kind,
                    title: base.name.clone(),
                    feed: None,
                    threads: Vec::new(),
                    last_updated: None,
                });
                cards.len() - 1
            }
        };
        let card = &mut cards[at];
        card.last_updated = card.last_updated.max(section.meta.last_updated);
        if conversation.thread.is_some() {
            card.threads.push(section);
        } else {
            card.feed = Some(section);
        }
    }
    for card in &mut cards {
        card.threads.sort_by(|a, b| {
            newer(
                a.meta.last_updated,
                &a.conversation.key(),
                b.meta.last_updated,
                &b.conversation.key(),
            )
        });
    }
    cards.sort_by(|a, b| newer(a.last_updated, &a.jid, b.last_updated, &b.jid));
    cards
}

fn newer(a: Option<i64>, a_key: &str, b: Option<i64>, b_key: &str) -> std::cmp::Ordering {
    let (a, b) = (a.unwrap_or(i64::MIN), b.unwrap_or(i64::MIN));
    b.cmp(&a).then_with(|| a_key.cmp(b_key))
}

/// The sidebar badge: what the cards add up to.
pub fn total_unread(cards: &[CardPlan]) -> u32 {
    cards
        .iter()
        .fold(0u32, |sum, card| sum.saturating_add(card.unread()))
}

/// The badge text, capped like the conversation rows.
pub fn badge_text(count: u32) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

/// One unread message as a card shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxMessage {
    pub sender: String,
    pub body: String,
    pub timestamp_ms: i64,
    /// What `ui/message_tools` can jump to: the archive id.
    pub target: String,
    pub mentions_me: bool,
}

/// One archived message with its routing metadata.
#[derive(Debug, Clone)]
pub struct Archived {
    pub thread: Option<Thread>,
    pub message: IncomingMessage,
    pub mentions_me: bool,
}

/// The newest `unread` messages from others that belong to `selected`
/// (a thread, or the main feed when `None`), oldest first. Reactions,
/// edits, judgments and removed messages are not rows; the root of a
/// thread is its context, not an unread reply.
pub fn select_unread(
    archived: Vec<Archived>,
    selected: Option<&Thread>,
    unread: u32,
) -> Vec<InboxMessage> {
    let removed: HashSet<String> = archived
        .iter()
        .flat_map(|entry| {
            [
                entry.message.retracts_id.clone(),
                entry.message.moderates_id.clone(),
            ]
        })
        .flatten()
        .collect();
    let mut rows: Vec<InboxMessage> = archived
        .into_iter()
        .filter(|entry| {
            let message = &entry.message;
            let fastening = message.safety_scores.is_some()
                || message.reaction.is_some()
                || message.replaces_id.is_some()
                || message.retracts_id.is_some()
                || message.moderates_id.is_some();
            !fastening
                && !message.own
                && !message.tombstone
                && threads::belongs_to_thread(message, entry.thread.as_ref(), selected)
                && !message
                    .reference_id
                    .as_ref()
                    .into_iter()
                    .chain(message.stanza_id.as_ref())
                    .any(|id| removed.contains(id))
        })
        .filter_map(|entry| {
            let message = entry.message;
            let body = message
                .body
                .clone()
                .filter(|body| !body.trim().is_empty())
                .or_else(|| (!message.attachments.is_empty()).then(|| "Sent a file".to_string()))?;
            let target = message
                .stanza_id
                .clone()
                .or_else(|| message.reference_id.clone())?;
            Some(InboxMessage {
                sender: message.sender,
                body,
                timestamp_ms: message.timestamp_ms,
                target,
                mentions_me: entry.mentions_me,
            })
        })
        .collect();
    rows.sort_by_key(|row| row.timestamp_ms);
    let keep = usize::try_from(unread).unwrap_or(usize::MAX);
    let skip = rows.len().saturating_sub(keep);
    rows.drain(..skip);
    rows
}

/// What a fetch must still match to be current: a message arriving or a
/// read elsewhere changes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub unread: u32,
    pub last_updated: Option<i64>,
    pub last_stanza_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loaded {
    Ready(Vec<InboxMessage>),
    /// The archive could not be read; the card says so.
    Failed,
}

/// Fetched sections by conversation key, and the ones being fetched.
#[derive(Debug, Default)]
pub struct Sections {
    done: HashMap<String, (Signature, Loaded)>,
    in_flight: HashMap<String, Signature>,
    /// Bumped whenever what the cards show changes.
    version: u64,
}

impl Sections {
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn get(&self, key: &str, signature: &Signature) -> Option<&Loaded> {
        self.done
            .get(key)
            .filter(|(known, _)| known == signature)
            .map(|(_, loaded)| loaded)
    }

    /// Sections to fetch now: not current, not already on the way, and
    /// within the concurrency budget. A failed fetch is not retried until
    /// the section's signature changes.
    pub fn due<'a>(
        &mut self,
        wanted: impl IntoIterator<Item = &'a SectionPlan>,
    ) -> Vec<&'a SectionPlan> {
        let mut due = Vec::new();
        for section in wanted {
            if self.in_flight.len() >= CONCURRENCY {
                break;
            }
            let key = section.conversation.key();
            let signature = section.signature();
            if self.get(&key, &signature).is_some() || self.in_flight.get(&key) == Some(&signature)
            {
                continue;
            }
            self.in_flight.insert(key, signature);
            due.push(section);
        }
        due
    }

    /// Record a fetch. False when the section moved on while it ran.
    pub fn finish(&mut self, key: &str, signature: &Signature, loaded: Loaded) -> bool {
        if self.in_flight.get(key) != Some(signature) {
            return false;
        }
        self.in_flight.remove(key);
        self.version += 1;
        self.done
            .insert(key.to_string(), (signature.clone(), loaded));
        true
    }

    /// Drop one section so it is fetched again (a retry).
    pub fn forget(&mut self, key: &str) {
        self.done.remove(key);
        self.version += 1;
    }

    /// Forget what no card wants any more.
    pub fn retain(&mut self, keys: &HashSet<String>) {
        self.done.retain(|key, _| keys.contains(key));
    }
}

impl Session {
    /// A section's newest page: the room archive filtered by the Waddle
    /// thread field for a room thread, otherwise the conversation's
    /// archive, which a DM thread then filters itself.
    pub async fn inbox_section(&self, section: &SectionPlan) -> Result<Vec<InboxMessage>, String> {
        let client = Arc::clone(&self.client);
        let conversation = &section.conversation;
        let peer = conversation.jid.clone();
        let kind = conversation.kind;
        let thread_id = conversation.thread.as_ref().map(|thread| thread.id.clone());
        let max = fetch_size(section.unread);
        let page = runtime::spawn(async move {
            match (kind, thread_id) {
                (Kind::Room, Some(thread)) => client
                    .fetch_room_thread_history(peer, thread, max, None)
                    .await
                    .map_err(|error| format!("{error:?}")),
                (Kind::Room, None) => Ok(client.fetch_room_history(peer, max, None).await),
                (Kind::Direct, _) => Ok(client.fetch_dm_history(peer, max, None).await),
            }
        })
        .await
        .map_err(|_| "interrupted".to_string())?
        .map_err(|error| {
            tracing::debug!(%error, "could not fetch an inbox section");
            "The unread messages could not be loaded.".to_string()
        })?;
        let peer: BareJid = conversation
            .jid
            .parse()
            .map_err(|_| "The conversation address is invalid.".to_string())?;
        let archived = archive_entries(page, &peer, kind, &self.me);
        Ok(select_unread(
            archived,
            conversation.thread.as_ref(),
            section.unread,
        ))
    }
}

fn archive_entries(page: WaddleMamPage, peer: &BareJid, kind: Kind, me: &Me) -> Vec<Archived> {
    page.messages
        .into_iter()
        .filter_map(|wire| {
            let thread = Thread::from_wire(wire.thread.clone(), wire.parent_thread_id.clone());
            let mentions_me = mentions(&wire, me);
            Some(Archived {
                thread,
                message: archived_from(wire, peer, kind, me)?,
                mentions_me: mentions_me && kind == Kind::Room,
            })
        })
        .collect()
}

/// XEP-0372 mention of this account or the whole room.
fn mentions(message: &WaddleArchivedMessage, me: &Me) -> bool {
    message.broadcast_mention.is_some()
        || message
            .mention_uris
            .iter()
            .any(|uri| uri.strip_prefix("xmpp:") == Some(me.bare.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn room(jid: &str) -> Conversation {
        Conversation {
            jid: jid.into(),
            name: jid.split('@').next().unwrap_or(jid).into(),
            kind: Kind::Room,
            autojoin: false,
            thread: None,
        }
    }

    fn direct(jid: &str) -> Conversation {
        Conversation {
            kind: Kind::Direct,
            ..room(jid)
        }
    }

    fn thread_of(base: &Conversation, id: &str) -> Conversation {
        Conversation {
            thread: Some(Thread {
                id: id.into(),
                parent: None,
            }),
            name: format!("{id} title"),
            ..base.clone()
        }
    }

    fn counts(pairs: &[(&Conversation, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(c, n)| (c.key(), *n)).collect()
    }

    fn at(conversation: &Conversation, seconds: i64) -> (String, Meta) {
        (
            conversation.key(),
            Meta {
                last_updated: Some(seconds),
                last_stanza_id: None,
            },
        )
    }

    #[test]
    fn cards_list_unread_conversations_newest_first_with_their_threads() {
        let general = room("general@muc.waddle.social");
        let random = room("random@muc.waddle.social");
        let quiet = room("quiet@muc.waddle.social");
        let ann = direct("ann@waddle.social");
        let (t1, t2) = (thread_of(&general, "t1"), thread_of(&general, "t2"));
        let known = [
            general.clone(),
            t1.clone(),
            t2.clone(),
            random.clone(),
            quiet.clone(),
            ann.clone(),
        ];
        let unread = counts(&[(&general, 3), (&t1, 1), (&t2, 2), (&random, 1), (&ann, 4)]);
        let meta: HashMap<_, _> = [
            at(&general, 100),
            at(&t1, 400),
            at(&t2, 300),
            at(&random, 200),
            at(&quiet, 900),
            at(&ann, 250),
        ]
        .into();
        let cards = plan(&known, &unread, &HashSet::new(), &meta, &HashMap::new());
        let order: Vec<&str> = cards.iter().map(|card| card.jid.as_str()).collect();
        // The newest thread activity lifts the whole card.
        assert_eq!(
            order,
            [
                "general@muc.waddle.social",
                "ann@waddle.social",
                "random@muc.waddle.social"
            ]
        );
        let general_card = &cards[0];
        assert_eq!(general_card.feed.as_ref().map(|s| s.unread), Some(3));
        let thread_ids: Vec<&str> = general_card
            .threads
            .iter()
            .filter_map(|s| s.conversation.thread.as_ref().map(|t| t.id.as_str()))
            .collect();
        assert_eq!(thread_ids, ["t1", "t2"]);
        assert_eq!(cards[1].kind, Kind::Direct);
    }

    #[test]
    fn a_card_can_hold_only_unread_threads() {
        let general = room("general@muc.waddle.social");
        let t1 = thread_of(&general, "t1");
        let known = [general.clone(), t1.clone()];
        let cards = plan(
            &known,
            &counts(&[(&t1, 2)]),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(cards.len(), 1);
        assert!(cards[0].feed.is_none());
        assert_eq!(cards[0].title, "general");
        assert_eq!(cards[0].unread(), 2);
    }

    #[test]
    fn read_conversations_leave_the_overview() {
        let general = room("general@muc.waddle.social");
        let cards = plan(
            std::slice::from_ref(&general),
            &counts(&[(&general, 0)]),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(cards.is_empty());
        assert_eq!(total_unread(&cards), 0);
    }

    #[test]
    fn live_activity_lifts_a_card_and_ties_break_by_jid() {
        let a = room("a@muc.waddle.social");
        let b = room("b@muc.waddle.social");
        let known = [a.clone(), b.clone()];
        let unread = counts(&[(&a, 1), (&b, 1)]);
        let tie = plan(
            &known,
            &unread,
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(tie[0].jid, a.jid);
        let meta: HashMap<_, _> = [at(&a, 10), at(&b, 5)].into();
        let live: HashMap<_, _> = [(b.key(), 50)].into();
        let cards = plan(&known, &unread, &HashSet::new(), &meta, &live);
        assert_eq!(cards[0].jid, b.jid);
    }

    #[test]
    fn badge_counts_the_larger_of_room_and_thread_unread_and_caps_display() {
        let general = room("general@muc.waddle.social");
        let (t1, t2) = (thread_of(&general, "t1"), thread_of(&general, "t2"));
        let known = [general.clone(), t1.clone(), t2.clone()];
        let none = (&HashSet::new(), &HashMap::new(), &HashMap::new());
        // The server's room row already counts the replies.
        let counted = plan(
            &known,
            &counts(&[(&general, 5), (&t1, 2), (&t2, 1)]),
            none.0,
            none.1,
            none.2,
        );
        assert_eq!(total_unread(&counted), 5);
        // A live reply bumped only its thread.
        let live = plan(
            &known,
            &counts(&[(&general, 1), (&t1, 2), (&t2, 1)]),
            none.0,
            none.1,
            none.2,
        );
        assert_eq!(total_unread(&live), 3);
        assert_eq!(badge_text(7), "7");
        assert_eq!(badge_text(100), "99+");
    }

    #[test]
    fn mentions_mark_their_section_and_card() {
        let general = room("general@muc.waddle.social");
        let t1 = thread_of(&general, "t1");
        let known = [general.clone(), t1.clone()];
        let mentions: HashSet<_> = [t1.key()].into();
        let cards = plan(
            &known,
            &counts(&[(&general, 1), (&t1, 1)]),
            &mentions,
            &HashMap::new(),
            &HashMap::new(),
        );
        assert!(cards[0].mention());
        assert!(!cards[0].feed.as_ref().is_some_and(|feed| feed.mention));
        assert!(cards[0].threads[0].mention);
    }

    #[test]
    fn marking_a_card_read_covers_its_feed_and_threads() {
        let general = room("general@muc.waddle.social");
        let t1 = thread_of(&general, "t1");
        let known = [general.clone(), t1.clone()];
        let cards = plan(
            &known,
            &counts(&[(&general, 1), (&t1, 1)]),
            &HashSet::new(),
            &HashMap::new(),
            &HashMap::new(),
        );
        let keys: Vec<String> = cards[0].conversations().iter().map(|c| c.key()).collect();
        assert_eq!(keys, [general.key(), t1.key()]);
    }

    fn archived(id: &str, sender: &str, ms: i64, thread: Option<&str>) -> Archived {
        Archived {
            thread: thread.map(|id| Thread {
                id: id.into(),
                parent: None,
            }),
            message: IncomingMessage {
                stanza_id: Some(id.into()),
                reference_id: Some(id.into()),
                sender: sender.into(),
                body: Some(format!("body {id}")),
                timestamp_ms: ms,
                ..IncomingMessage::default()
            },
            mentions_me: false,
        }
    }

    fn targets(rows: &[InboxMessage]) -> Vec<&str> {
        rows.iter().map(|row| row.target.as_str()).collect()
    }

    #[test]
    fn feed_rows_are_others_unthreaded_messages_capped_at_the_unread_count() {
        let mut mine = archived("m2", "me", 2, None);
        mine.message.own = true;
        let page = vec![
            archived("m1", "ann", 1, None),
            mine,
            archived("m3", "ann", 3, Some("t1")),
            archived("m4", "bob", 4, None),
            archived("m5", "ann", 5, None),
        ];
        assert_eq!(targets(&select_unread(page.clone(), None, 2)), ["m4", "m5"]);
        assert_eq!(targets(&select_unread(page, None, 10)), ["m1", "m4", "m5"]);
    }

    #[test]
    fn thread_rows_are_replies_without_the_root_or_other_threads() {
        let selected = Thread {
            id: "root".into(),
            parent: None,
        };
        let mut legacy = archived("r2", "bob", 3, None);
        legacy.message.reply_to = Some(waddle_client_session::ReplyTo {
            id: "root".into(),
            sender: None,
        });
        let page = vec![
            archived("root", "ann", 1, None),
            archived("r1", "ann", 2, Some("root")),
            legacy,
            archived("x1", "ann", 4, Some("other")),
            archived("r3", "bob", 5, Some("root")),
        ];
        let rows = select_unread(page, Some(&selected), 10);
        assert_eq!(targets(&rows), ["r1", "r2", "r3"]);
    }

    #[test]
    fn fastenings_attachments_and_removed_messages_are_handled() {
        let mut reaction = archived("rx", "ann", 2, None);
        reaction.message.reaction = Some(waddle_client_session::ReactionUpdate {
            target_id: "m1".into(),
            emojis: vec!["+1".into()],
        });
        let mut retraction = archived("rt", "ann", 4, None);
        retraction.message.retracts_id = Some("m3".into());
        let mut tombstone = archived("m5", "ann", 5, None);
        tombstone.message.tombstone = true;
        let mut file = archived("m6", "ann", 6, None);
        file.message.body = None;
        file.message.attachments = vec![waddle_client_session::Attachment {
            url: "https://files.example/a.png".into(),
            name: None,
            media_type: None,
            size: None,
            inline: false,
            encryption: None,
        }];
        let mut empty = archived("m7", "ann", 7, None);
        empty.message.body = Some("  ".into());
        let page = vec![
            archived("m1", "ann", 1, None),
            reaction,
            archived("m3", "ann", 3, None),
            retraction,
            tombstone,
            file,
            empty,
        ];
        let rows = select_unread(page, None, 10);
        assert_eq!(targets(&rows), ["m1", "m6"]);
        assert_eq!(rows[1].body, "Sent a file");
    }

    #[test]
    fn rows_come_oldest_first_whatever_the_page_order() {
        let page = vec![archived("b", "ann", 2, None), archived("a", "ann", 1, None)];
        assert_eq!(targets(&select_unread(page, None, 5)), ["a", "b"]);
    }

    #[test]
    fn mentions_carry_through_to_rows() {
        let mut page = archived("a", "ann", 1, None);
        page.mentions_me = true;
        assert!(select_unread(vec![page], None, 5)[0].mentions_me);
    }

    #[test]
    fn fetch_size_adds_context_and_caps_at_a_hundred() {
        assert_eq!(fetch_size(0), 20);
        assert_eq!(fetch_size(5), 25);
        assert_eq!(fetch_size(95), 100);
        assert_eq!(fetch_size(u32::MAX), 100);
    }

    #[test]
    fn sections_fetch_once_per_signature_within_the_budget() {
        let base = room("general@muc.waddle.social");
        let sections: Vec<SectionPlan> = (0..6)
            .map(|n| SectionPlan {
                conversation: thread_of(&base, &format!("t{n}")),
                unread: 1,
                mention: false,
                meta: Meta::default(),
            })
            .collect();
        let mut state = Sections::default();
        let first = state.due(&sections);
        assert_eq!(first.len(), CONCURRENCY);
        // Those are in flight; nothing new is due until one finishes.
        assert!(state.due(&sections).is_empty());
        let done = first[0];
        let (key, signature) = (done.conversation.key(), done.signature());
        assert!(state.finish(&key, &signature, Loaded::Ready(Vec::new())));
        assert_eq!(
            state.get(&key, &signature),
            Some(&Loaded::Ready(Vec::new()))
        );
        let next = state.due(&sections);
        assert_eq!(next.len(), 1);
        assert_eq!(
            next[0].conversation.thread.as_ref().map(|t| &t.id[..]),
            Some("t4")
        );
    }

    #[test]
    fn a_changed_section_is_fetched_again_and_a_failed_one_waits_for_a_change() {
        let base = room("general@muc.waddle.social");
        let mut section = SectionPlan {
            conversation: base,
            unread: 1,
            mention: false,
            meta: Meta::default(),
        };
        let mut state = Sections::default();
        let key = section.conversation.key();
        let signature = section.signature();
        assert_eq!(state.due([&section]).len(), 1);
        state.finish(&key, &signature, Loaded::Failed);
        assert!(state.due([&section]).is_empty());
        // Another message arrives.
        section.unread = 2;
        assert_eq!(state.due([&section]).len(), 1);
        // The older fetch finishing late is not shown as current.
        assert!(!state.finish(&key, &signature, Loaded::Failed));
        assert!(state.get(&key, &section.signature()).is_none());
    }

    #[test]
    fn sections_no_card_wants_are_forgotten() {
        let base = room("general@muc.waddle.social");
        let section = SectionPlan {
            conversation: base,
            unread: 1,
            mention: false,
            meta: Meta::default(),
        };
        let mut state = Sections::default();
        let key = section.conversation.key();
        state.due([&section]);
        state.finish(&key, &section.signature(), Loaded::Ready(Vec::new()));
        state.retain(&HashSet::new());
        assert!(state.get(&key, &section.signature()).is_none());
    }

    #[test]
    fn a_failed_section_is_fetched_again_when_forgotten() {
        let section = SectionPlan {
            conversation: room("general@muc.waddle.social"),
            unread: 1,
            mention: false,
            meta: Meta::default(),
        };
        let mut state = Sections::default();
        let key = section.conversation.key();
        state.due([&section]);
        state.finish(&key, &section.signature(), Loaded::Failed);
        assert!(state.due([&section]).is_empty());
        state.forget(&key);
        assert_eq!(state.due([&section]).len(), 1);
    }
}
