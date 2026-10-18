//! One conversation's timeline.
//!
//! Inputs are archived pages (older history, newest page first), live
//! messages, the user's own sends (local echo), and delivery outcomes. Each
//! returns the [`TimelineDiff`]s that bring a UI list in step, so a GTK
//! `ListModel`, a Compose list or a SwiftUI list updates only what changed.
//!
//! Identity follows XEP-0359. A message's [`ItemKey`] is its server-assigned
//! stanza id when known, otherwise the sender's origin id. Every id a row is
//! known by (stanza id, origin id, `@id`, reference id) resolves to it, so a
//! live echo of the user's own send is matched to its local echo by origin
//! id, and history paged in later dedupes against either.
//!
//! Corrections (XEP-0308), retractions (XEP-0424), moderation (XEP-0425)
//! and reactions (XEP-0444) are fastenings onto another message. Because
//! history arrives newest page first, a fastening often arrives before the
//! message it targets; it is held until that message is paged in. A
//! correction or retraction is honoured only from the original sender, and
//! of several corrections the latest wins whatever order they arrive in.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// Same-author messages closer than this group under one header
/// (inclusive; ADR-0019 behaviour table).
pub const GROUPING_WINDOW_MS: i64 = 5 * 60 * 1000;

/// Longest reply quote kept on a row, in characters.
const SNIPPET_CHARS: usize = 120;

/// A stable identity for one timeline row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemKey {
    /// XEP-0359 `<stanza-id/>` assigned by the archive.
    Stanza(String),
    /// XEP-0359 `<origin-id/>` set by the sender, until the archive id is
    /// known.
    Origin(String),
}

impl ItemKey {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stanza(id) | Self::Origin(id) => id,
        }
    }
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

/// The message a reply quotes (XEP-0461).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTo {
    /// The quoted message's reference id.
    pub id: String,
    /// The quoted author as the reply names them, when it does.
    pub sender: Option<String>,
}

/// What a row shows of the message it replies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyPreview {
    pub target_id: String,
    /// The quoted author: the held row's sender, else the reply's claim.
    pub sender: Option<String>,
    /// The quoted text, or `None` while that message is not loaded.
    pub snippet: Option<String>,
    /// The quoted message was retracted or moderated away.
    pub target_retracted: bool,
}

/// One emoji's tally on a row (XEP-0444).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    /// The user is among those who reacted with it.
    pub own: bool,
}

/// A sender's complete reaction set for one message (XEP-0444 §3: each
/// update replaces the sender's previous set; empty clears it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionUpdate {
    pub target_id: String,
    pub emojis: Vec<String>,
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
    /// The id replies, reactions and retractions reference: the room's
    /// stanza id in a room, the sender's id in a DM. `None` until known
    /// (a room send before the room reflects it).
    pub reference_id: Option<String>,
    /// The id a correction of this message references (XEP-0308): the
    /// sender's origin id or `@id`.
    pub correction_id: Option<String>,
    pub reply: Option<ReplyPreview>,
    /// Tallies in emoji order.
    pub reactions: Vec<Reaction>,
    /// When the applied correction was sent, so an older one arriving later
    /// cannot undo a newer one.
    edited_at: Option<i64>,
}

/// A message from an archive page (XEP-0313) or one that arrived live.
/// The adapter fills in what the wire carried; ids it has not verified
/// (XEP-0359 `by`) must be left out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncomingMessage {
    /// The archive's (or room's) verified stanza id.
    pub stanza_id: Option<String>,
    pub origin_id: Option<String>,
    /// The stanza's `@id`.
    pub message_id: Option<String>,
    /// See [`TimelineItem::reference_id`].
    pub reference_id: Option<String>,
    pub sender: String,
    pub own: bool,
    /// With any XEP-0461 fallback quote already removed
    /// ([`strip_fallback`]).
    pub body: Option<String>,
    pub timestamp_ms: i64,
    /// XEP-0308 correction of this id.
    pub replaces_id: Option<String>,
    /// XEP-0424 retraction of this id by its sender.
    pub retracts_id: Option<String>,
    /// XEP-0425 moderation of this id. Set only for the room's own
    /// broadcast, never for an occupant's message.
    pub moderates_id: Option<String>,
    /// An archive row that is already a retraction tombstone.
    pub tombstone: bool,
    pub reply_to: Option<ReplyTo>,
    pub reaction: Option<ReactionUpdate>,
}

/// The user's own send, shown before the server has seen it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalEcho {
    pub origin_id: String,
    pub sender: String,
    pub body: String,
    pub timestamp_ms: i64,
    /// Known up front in a DM (the origin id); a room assigns it.
    pub reference_id: Option<String>,
    pub reply_to: Option<ReplyTo>,
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
    Set { index: u32, item: Box<TimelineItem> },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Reactor {
    sender: String,
    own: bool,
}

#[derive(Debug, Clone)]
struct Reacted {
    at: i64,
    emojis: Vec<String>,
}

#[derive(Debug, Clone)]
struct Edit {
    sender: String,
    own: bool,
    body: String,
    at: i64,
}

#[derive(Debug, Clone)]
enum Removal {
    BySender { sender: String, own: bool },
    Moderated,
}

#[derive(Debug, Default)]
pub struct Timeline {
    items: Vec<TimelineItem>,
    /// Every id each row is known by, parallel to `items`.
    aliases: Vec<Vec<String>>,
    /// Every alias to its row's current position.
    index: HashMap<String, usize>,
    /// Corrections whose target is not held yet, latest per target.
    pending_edits: HashMap<String, Edit>,
    /// Retractions and moderations whose target is not held yet.
    pending_removals: HashMap<String, Removal>,
    /// Every sender's latest reaction set, by target id.
    reactions: HashMap<String, HashMap<Reactor, Reacted>>,
    /// Target id to the keys of rows that reply to it.
    replies: HashMap<String, Vec<String>>,
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

    /// The row known by `id` (any of its ids).
    pub fn get(&self, id: &str) -> Option<&TimelineItem> {
        self.index.get(id).and_then(|&at| self.items.get(at))
    }

    /// The RSM `before` cursor for the next older page.
    pub fn older_cursor(&self) -> Option<&str> {
        self.oldest_archive_id.as_deref()
    }

    pub fn history_complete(&self) -> bool {
        self.history_complete
    }

    /// The user's current reaction set on the row known by `id`, for
    /// building the complete set XEP-0444 sends.
    pub fn own_reactions(&self, id: &str) -> Vec<String> {
        let Some(&at) = self.index.get(id) else {
            return Vec::new();
        };
        let mut latest: Option<&Reacted> = None;
        for alias in &self.aliases[at] {
            let Some(by_sender) = self.reactions.get(alias) else {
                continue;
            };
            for (reactor, reacted) in by_sender {
                if reactor.own && latest.is_none_or(|seen| reacted.at >= seen.at) {
                    latest = Some(reacted);
                }
            }
        }
        latest
            .map(|reacted| reacted.emojis.clone())
            .unwrap_or_default()
    }

    /// Add an archive page of older history. `messages` are in archive
    /// order (oldest first); `first_id` is the page's oldest archive id.
    /// Fastenings in the page apply to rows in it, rows already held, or
    /// wait for rows still to be paged in.
    pub fn apply_history_page(
        &mut self,
        messages: Vec<IncomingMessage>,
        first_id: Option<String>,
        complete: bool,
    ) -> Vec<TimelineDiff> {
        if first_id.is_some() {
            self.oldest_archive_id = first_id;
        }
        self.history_complete = complete;

        let mut older = Vec::new();
        let mut older_aliases: Vec<Vec<String>> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut fastenings = Vec::new();
        for message in messages {
            if is_fastening(&message) {
                fastenings.push(message);
                continue;
            }
            let ids = ids_of(&message);
            if ids
                .iter()
                .any(|id| self.index.contains_key(id) || seen.contains(id))
            {
                continue;
            }
            let Some(item) = content_item(&message) else {
                continue;
            };
            seen.extend(ids.iter().cloned());
            older.push(item);
            older_aliases.push(ids);
        }

        let pushed = older.len();
        if pushed > 0 {
            older.append(&mut self.items);
            self.items = older;
            older_aliases.append(&mut self.aliases);
            self.aliases = older_aliases;
            self.reindex();
        }

        let mut touched = BTreeSet::new();
        for at in 0..pushed {
            self.settle(at, &mut touched);
        }
        for message in &fastenings {
            self.apply_fastening(message, &mut touched);
        }

        let mut diffs = Vec::new();
        if pushed > 0 {
            diffs.push(TimelineDiff::PushFront {
                items: self.items[..pushed].to_vec(),
            });
        }
        diffs.extend(self.sets(touched.into_iter().filter(|&at| at >= pushed)));
        diffs
    }

    /// The user sent a message: show it at once as `Sending`.
    pub fn add_local_echo(&mut self, echo: LocalEcho) -> Vec<TimelineDiff> {
        if self.index.contains_key(&echo.origin_id) {
            return Vec::new();
        }
        let mut aliases = vec![echo.origin_id.clone()];
        aliases.extend(echo.reference_id.iter().cloned());
        let item = TimelineItem {
            key: ItemKey::Origin(echo.origin_id.clone()),
            sender: echo.sender,
            own: true,
            body: echo.body,
            timestamp_ms: echo.timestamp_ms,
            delivery: Delivery::Sending,
            edited: false,
            retracted: false,
            reference_id: echo.reference_id,
            correction_id: Some(echo.origin_id),
            reply: echo.reply_to.map(unresolved_preview),
            reactions: Vec::new(),
            edited_at: None,
        };
        self.append(item, aliases)
    }

    /// The server acknowledged (or failed) the send with this id.
    pub fn set_delivery(&mut self, id: &str, delivery: Delivery) -> Vec<TimelineDiff> {
        let Some(&at) = self.index.get(id) else {
            return Vec::new();
        };
        let item = &mut self.items[at];
        if !item.own || item.delivery == Delivery::Sent || item.delivery == delivery {
            return Vec::new();
        }
        item.delivery = delivery;
        self.sets([at])
    }

    /// A message arrived live.
    pub fn apply_live(&mut self, message: IncomingMessage) -> Vec<TimelineDiff> {
        let mut touched = BTreeSet::new();
        if self.apply_fastening(&message, &mut touched) {
            return self.sets(touched);
        }

        // The room reflecting the user's own send: the local echo takes
        // over the server's ids.
        if message.own {
            if let Some(&at) = message
                .origin_id
                .as_deref()
                .and_then(|origin| self.index.get(origin))
            {
                if self.items[at].own {
                    self.take_reflection(at, &message, &mut touched);
                    return self.sets(touched);
                }
            }
        }

        let ids = ids_of(&message);
        if ids.iter().any(|id| self.index.contains_key(id)) {
            return Vec::new();
        }
        let Some(item) = content_item(&message) else {
            return Vec::new();
        };
        self.append(item, ids)
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
            && item.reply.is_none()
            && item.timestamp_ms - previous.timestamp_ms <= GROUPING_WINDOW_MS
            && item.timestamp_ms >= previous.timestamp_ms
            && day(previous.timestamp_ms) == day(item.timestamp_ms)
    }

    fn append(&mut self, item: TimelineItem, aliases: Vec<String>) -> Vec<TimelineDiff> {
        let at = self.items.len();
        for alias in &aliases {
            self.index.insert(alias.clone(), at);
        }
        self.items.push(item);
        self.aliases.push(aliases);
        let mut touched = BTreeSet::new();
        self.settle(at, &mut touched);
        let mut diffs = vec![TimelineDiff::Append {
            items: vec![self.items[at].clone()],
        }];
        diffs.extend(self.sets(touched.into_iter().filter(|&row| row != at)));
        diffs
    }

    fn take_reflection(
        &mut self,
        at: usize,
        message: &IncomingMessage,
        touched: &mut BTreeSet<usize>,
    ) {
        let item = &mut self.items[at];
        if let Some(stanza_id) = &message.stanza_id {
            item.key = ItemKey::Stanza(stanza_id.clone());
        }
        if item.reference_id.is_none() {
            item.reference_id.clone_from(&message.reference_id);
        }
        item.delivery = Delivery::Sent;
        for id in ids_of(message) {
            if !self.aliases[at].contains(&id) {
                self.index.insert(id.clone(), at);
                self.aliases[at].push(id);
            }
        }
        touched.insert(at);
        self.settle(at, touched);
    }

    /// Bring a newly placed row up to date: fastenings that were waiting
    /// for it, its reactions, its reply quote, and the quotes of rows that
    /// reply to it.
    fn settle(&mut self, at: usize, touched: &mut BTreeSet<usize>) {
        let aliases = self.aliases[at].clone();
        for alias in &aliases {
            if let Some(edit) = self.pending_edits.remove(alias) {
                if self.apply_edit_at(at, &edit) {
                    touched.insert(at);
                }
            }
            if let Some(removal) = self.pending_removals.remove(alias) {
                if self.apply_removal_at(at, &removal) {
                    touched.insert(at);
                }
            }
        }
        let reactions = self.tally(at);
        if self.items[at].reactions != reactions {
            self.items[at].reactions = reactions;
            touched.insert(at);
        }
        if let Some(target) = self.items[at]
            .reply
            .as_ref()
            .map(|reply| reply.target_id.clone())
        {
            let key = self.items[at].key.as_str().to_string();
            let replies = self.replies.entry(target).or_default();
            if !replies.contains(&key) {
                replies.push(key);
            }
            if self.refresh_preview(at) {
                touched.insert(at);
            }
        }
        self.refresh_replies_to(at, touched);
    }

    /// Apply a correction, retraction, moderation or reaction. False when
    /// the message is none of these.
    fn apply_fastening(
        &mut self,
        message: &IncomingMessage,
        touched: &mut BTreeSet<usize>,
    ) -> bool {
        if let Some(update) = &message.reaction {
            let reactor = Reactor {
                sender: message.sender.clone(),
                own: message.own,
            };
            self.react(
                &update.target_id,
                reactor,
                message.timestamp_ms,
                &update.emojis,
                touched,
            );
            return true;
        }
        if let Some(target) = &message.moderates_id {
            self.remove(target, Removal::Moderated, touched);
            return true;
        }
        if let Some(target) = &message.retracts_id {
            let removal = Removal::BySender {
                sender: message.sender.clone(),
                own: message.own,
            };
            self.remove(target, removal, touched);
            return true;
        }
        if let Some(target) = &message.replaces_id {
            let Some(body) = message.body.clone() else {
                return true;
            };
            let edit = Edit {
                sender: message.sender.clone(),
                own: message.own,
                body,
                at: message.timestamp_ms,
            };
            match self.index.get(target).copied() {
                Some(at) => {
                    if self.apply_edit_at(at, &edit) {
                        touched.insert(at);
                        self.refresh_replies_to(at, touched);
                    }
                }
                None => {
                    if self
                        .pending_edits
                        .get(target)
                        .is_none_or(|held| edit.at >= held.at)
                    {
                        self.pending_edits.insert(target.clone(), edit);
                    }
                }
            }
            return true;
        }
        false
    }

    /// Record the user's own reaction set before the server has it
    /// (optimistic), or roll it back. See [`Timeline::own_reactions`].
    pub fn apply_own_reaction(
        &mut self,
        target_id: &str,
        sender: &str,
        emojis: &[String],
        timestamp_ms: i64,
    ) -> Vec<TimelineDiff> {
        let mut touched = BTreeSet::new();
        let reactor = Reactor {
            sender: sender.to_string(),
            own: true,
        };
        self.react(target_id, reactor, timestamp_ms, emojis, &mut touched);
        self.sets(touched)
    }

    fn react(
        &mut self,
        target: &str,
        reactor: Reactor,
        at: i64,
        emojis: &[String],
        touched: &mut BTreeSet<usize>,
    ) {
        let by_sender = self.reactions.entry(target.to_string()).or_default();
        if by_sender.get(&reactor).is_some_and(|held| held.at > at) {
            return;
        }
        let mut unique: Vec<String> = Vec::new();
        for emoji in emojis {
            if !emoji.is_empty() && !unique.contains(emoji) {
                unique.push(emoji.clone());
            }
        }
        by_sender.insert(reactor, Reacted { at, emojis: unique });
        if let Some(&row) = self.index.get(target) {
            let reactions = self.tally(row);
            if self.items[row].reactions != reactions {
                self.items[row].reactions = reactions;
                touched.insert(row);
            }
        }
    }

    /// The row's reactions across every id it is known by, each sender's
    /// latest set counted once.
    fn tally(&self, at: usize) -> Vec<Reaction> {
        let mut latest: HashMap<&Reactor, &Reacted> = HashMap::new();
        for alias in &self.aliases[at] {
            for (reactor, reacted) in self.reactions.get(alias).into_iter().flatten() {
                let newer = latest.get(reactor).is_none_or(|seen| reacted.at >= seen.at);
                if newer {
                    latest.insert(reactor, reacted);
                }
            }
        }
        // Count, whether the user is among them, and when the emoji was
        // first used, so chips keep their place as counts change.
        let mut counts: BTreeMap<&str, (u32, bool, i64)> = BTreeMap::new();
        for (reactor, reacted) in latest {
            for emoji in &reacted.emojis {
                let entry = counts.entry(emoji).or_insert((0, false, reacted.at));
                entry.0 += 1;
                entry.1 |= reactor.own;
                entry.2 = entry.2.min(reacted.at);
            }
        }
        let mut tallies: Vec<(&str, (u32, bool, i64))> = counts.into_iter().collect();
        tallies.sort_by_key(|(emoji, (_, _, first))| (*first, *emoji));
        tallies
            .into_iter()
            .map(|(emoji, (count, own, _))| Reaction {
                emoji: emoji.to_string(),
                count,
                own,
            })
            .collect()
    }

    fn remove(&mut self, target: &str, removal: Removal, touched: &mut BTreeSet<usize>) {
        match self.index.get(target).copied() {
            Some(at) => {
                if self.apply_removal_at(at, &removal) {
                    touched.insert(at);
                    self.refresh_replies_to(at, touched);
                }
            }
            None => {
                // A moderation outranks a sender's own retraction.
                let keep_held = matches!(
                    (self.pending_removals.get(target), &removal),
                    (Some(Removal::Moderated), Removal::BySender { .. })
                );
                if !keep_held {
                    self.pending_removals.insert(target.to_string(), removal);
                }
            }
        }
    }

    fn apply_edit_at(&mut self, at: usize, edit: &Edit) -> bool {
        let item = &mut self.items[at];
        if item.retracted || item.sender != edit.sender || item.own != edit.own {
            return false;
        }
        if item.edited_at.is_some_and(|applied| edit.at < applied) {
            return false;
        }
        item.edited_at = Some(edit.at);
        if item.edited && item.body == edit.body {
            return false;
        }
        item.body.clone_from(&edit.body);
        item.edited = true;
        true
    }

    fn apply_removal_at(&mut self, at: usize, removal: &Removal) -> bool {
        let item = &mut self.items[at];
        if item.retracted {
            return false;
        }
        if let Removal::BySender { sender, own } = removal {
            if &item.sender != sender || item.own != *own {
                return false;
            }
        }
        item.retracted = true;
        item.body.clear();
        true
    }

    /// Re-quote the rows replying to the row at `at`.
    fn refresh_replies_to(&mut self, at: usize, touched: &mut BTreeSet<usize>) {
        let keys: Vec<String> = self.aliases[at]
            .iter()
            .filter_map(|alias| self.replies.get(alias))
            .flatten()
            .cloned()
            .collect();
        for key in keys {
            if let Some(&row) = self.index.get(&key) {
                if self.refresh_preview(row) {
                    touched.insert(row);
                }
            }
        }
    }

    /// Fill the reply quote at `at` from its target, when held.
    fn refresh_preview(&mut self, at: usize) -> bool {
        let Some(reply) = self.items[at].reply.as_ref() else {
            return false;
        };
        let Some(&target) = self.index.get(&reply.target_id) else {
            return false;
        };
        let quoted = &self.items[target];
        let next = ReplyPreview {
            target_id: reply.target_id.clone(),
            sender: Some(quoted.sender.clone()),
            snippet: (!quoted.retracted).then(|| snippet(&quoted.body)),
            target_retracted: quoted.retracted,
        };
        let reply = self.items[at].reply.as_mut();
        match reply {
            Some(reply) if *reply != next => {
                *reply = next;
                true
            }
            _ => false,
        }
    }

    fn reindex(&mut self) {
        self.index.clear();
        for (at, aliases) in self.aliases.iter().enumerate() {
            for alias in aliases {
                self.index.insert(alias.clone(), at);
            }
        }
    }

    fn sets(&self, rows: impl IntoIterator<Item = usize>) -> Vec<TimelineDiff> {
        rows.into_iter()
            .filter_map(|at| {
                Some(TimelineDiff::Set {
                    index: u32::try_from(at).ok()?,
                    item: Box::new(self.items.get(at)?.clone()),
                })
            })
            .collect()
    }
}

/// Remove a XEP-0428 fallback range (Unicode scalar offsets, `end`
/// exclusive) from `body`, as XEP-0461 replies carry their quote. An
/// invalid range leaves the body as it is.
pub fn strip_fallback(body: &str, start: u32, end: u32) -> String {
    let (Ok(start), Ok(end)) = (usize::try_from(start), usize::try_from(end)) else {
        return body.to_string();
    };
    let length = body.chars().count();
    if start > end || end > length {
        return body.to_string();
    }
    body.chars()
        .enumerate()
        .filter(|(at, _)| *at < start || *at >= end)
        .map(|(_, character)| character)
        .collect()
}

/// The XEP-0461 fallback quote for a reply to `sender`'s `quoted` text,
/// prepended to `body`, and the range it occupies (XEP-0428 offsets) so
/// receivers that support replies hide it.
pub fn with_reply_fallback(sender: &str, quoted: &str, body: &str) -> (String, u32, u32) {
    let mut prefix = format!("> {sender} wrote:\n");
    for line in snippet(quoted).lines() {
        prefix.push_str("> ");
        prefix.push_str(line);
        prefix.push('\n');
    }
    let end = u32::try_from(prefix.chars().count()).unwrap_or(u32::MAX);
    prefix.push_str(body);
    (prefix, 0, end)
}

fn snippet(body: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<&str>>().join(" ");
    if flat.chars().count() <= SNIPPET_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(SNIPPET_CHARS).collect();
    cut.push('…');
    cut
}

fn is_fastening(message: &IncomingMessage) -> bool {
    message.reaction.is_some()
        || message.moderates_id.is_some()
        || message.retracts_id.is_some()
        || message.replaces_id.is_some()
}

fn ids_of(message: &IncomingMessage) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in [
        &message.stanza_id,
        &message.origin_id,
        &message.message_id,
        &message.reference_id,
    ]
    .into_iter()
    .flatten()
    {
        if !id.is_empty() && !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

fn content_item(message: &IncomingMessage) -> Option<TimelineItem> {
    let key = message
        .stanza_id
        .clone()
        .map(ItemKey::Stanza)
        .or_else(|| message.origin_id.clone().map(ItemKey::Origin))?;
    let body = if message.tombstone {
        String::new()
    } else {
        message.body.clone()?
    };
    Some(TimelineItem {
        key,
        sender: message.sender.clone(),
        own: message.own,
        body,
        timestamp_ms: message.timestamp_ms,
        delivery: if message.own {
            Delivery::Sent
        } else {
            Delivery::Received
        },
        edited: false,
        retracted: message.tombstone,
        reference_id: message.reference_id.clone(),
        correction_id: message
            .origin_id
            .clone()
            .or_else(|| message.message_id.clone()),
        reply: message.reply_to.clone().map(unresolved_preview),
        reactions: Vec::new(),
        edited_at: None,
    })
}

fn unresolved_preview(reply: ReplyTo) -> ReplyPreview {
    ReplyPreview {
        target_id: reply.id,
        sender: reply.sender,
        snippet: None,
        target_retracted: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: i64 = 60 * 1000;

    fn message(stanza: &str, sender: &str, body: &str, at: i64) -> IncomingMessage {
        IncomingMessage {
            stanza_id: Some(stanza.into()),
            reference_id: Some(stanza.into()),
            sender: sender.into(),
            body: Some(body.into()),
            timestamp_ms: at,
            ..IncomingMessage::default()
        }
    }

    fn correction(id: &str, sender: &str, target: &str, body: &str, at: i64) -> IncomingMessage {
        IncomingMessage {
            replaces_id: Some(target.into()),
            ..message(id, sender, body, at)
        }
    }

    fn reaction(sender: &str, target: &str, emojis: &[&str], at: i64) -> IncomingMessage {
        IncomingMessage {
            sender: sender.into(),
            timestamp_ms: at,
            reaction: Some(ReactionUpdate {
                target_id: target.into(),
                emojis: emojis.iter().map(|emoji| (*emoji).to_string()).collect(),
            }),
            ..IncomingMessage::default()
        }
    }

    fn echo(origin: &str, body: &str) -> LocalEcho {
        LocalEcho {
            origin_id: origin.into(),
            sender: "me".into(),
            body: body.into(),
            timestamp_ms: 10,
            reference_id: None,
            reply_to: None,
        }
    }

    fn bodies(timeline: &Timeline) -> Vec<&str> {
        timeline
            .items()
            .iter()
            .map(|item| item.body.as_str())
            .collect()
    }

    fn tallies(item: &TimelineItem) -> Vec<(&str, u32, bool)> {
        item.reactions
            .iter()
            .map(|reaction| (reaction.emoji.as_str(), reaction.count, reaction.own))
            .collect()
    }

    #[test]
    fn history_pages_prepend_in_order_and_move_the_cursor() {
        let mut timeline = Timeline::new();
        let diffs = timeline.apply_history_page(
            vec![
                message("s3", "bob", "three", 3),
                message("s4", "bob", "four", 4),
            ],
            Some("s3".into()),
            false,
        );
        assert_eq!(diffs.len(), 1);
        assert_eq!(timeline.older_cursor(), Some("s3"));

        let diffs = timeline.apply_history_page(
            vec![
                message("s1", "ann", "one", 1),
                message("s2", "ann", "two", 2),
            ],
            Some("s1".into()),
            true,
        );
        assert!(matches!(&diffs[0], TimelineDiff::PushFront { items } if items.len() == 2));
        assert_eq!(bodies(&timeline), ["one", "two", "three", "four"]);
        assert!(timeline.history_complete());
    }

    #[test]
    fn history_skips_rows_already_held_and_duplicates_within_a_page() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s2", "bob", "two", 2));
        let diffs = timeline.apply_history_page(
            vec![
                message("s1", "ann", "one", 1),
                message("s1", "ann", "one", 1),
                message("s2", "bob", "two", 2),
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
        let diffs = timeline.add_local_echo(echo("o1", "hello"));
        assert!(
            matches!(&diffs[0], TimelineDiff::Append { items } if items[0].delivery == Delivery::Sending)
        );

        let diffs = timeline.apply_live(IncomingMessage {
            origin_id: Some("o1".into()),
            own: true,
            ..message("s9", "me", "hello", 11)
        });
        let [TimelineDiff::Set { index: 0, item }] = diffs.as_slice() else {
            panic!("expected the echo to be replaced in place: {diffs:?}");
        };
        assert_eq!(item.key, ItemKey::Stanza("s9".into()));
        assert_eq!(item.reference_id.as_deref(), Some("s9"));
        assert_eq!(item.correction_id.as_deref(), Some("o1"));
        assert_eq!(item.delivery, Delivery::Sent);

        // The same message paged in from history later is not duplicated,
        // even after the index is rebuilt by an older page.
        timeline.apply_history_page(
            vec![message("s0", "ann", "earlier", 1)],
            Some("s0".into()),
            false,
        );
        let diffs = timeline.apply_history_page(
            vec![IncomingMessage {
                origin_id: Some("o1".into()),
                own: true,
                ..message("s9", "me", "hello", 11)
            }],
            Some("s9".into()),
            true,
        );
        assert!(diffs.is_empty());
        assert_eq!(bodies(&timeline), ["earlier", "hello"]);
        assert!(timeline.get("o1").is_some());
    }

    #[test]
    fn someone_else_cannot_take_over_a_local_echo() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "mine"));
        timeline.apply_live(IncomingMessage {
            origin_id: Some("o1".into()),
            ..message("s5", "mallory", "stolen", 11)
        });
        assert_eq!(timeline.items()[0].delivery, Delivery::Sending);
        assert_eq!(bodies(&timeline), ["mine"]);
    }

    #[test]
    fn delivery_outcomes_update_only_pending_own_sends() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "hi"));
        let diffs = timeline.set_delivery("o1", Delivery::Failed);
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { item, .. } if item.delivery == Delivery::Failed)
        );
        assert!(timeline.set_delivery("unknown", Delivery::Sent).is_empty());

        timeline.apply_live(message("s1", "bob", "yo", 2));
        assert!(timeline.set_delivery("s1", Delivery::Failed).is_empty());
    }

    #[test]
    fn corrections_and_retractions_edit_in_place() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "helo", 1));
        let diffs = timeline.apply_live(correction("s2", "bob", "s1", "hello", 2));
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { index: 0, item } if item.body == "hello" && item.edited)
        );

        let diffs = timeline.apply_live(IncomingMessage {
            retracts_id: Some("s1".into()),
            body: None,
            ..message("s3", "bob", "", 3)
        });
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { index: 0, item } if item.retracted && item.body.is_empty())
        );
        assert_eq!(timeline.items().len(), 1);
    }

    #[test]
    fn only_the_sender_can_correct_or_retract_but_the_room_can_moderate() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "original", 1));
        assert!(timeline
            .apply_live(correction("s2", "mallory", "s1", "forged", 2))
            .is_empty());
        assert!(timeline
            .apply_live(IncomingMessage {
                retracts_id: Some("s1".into()),
                ..message("s3", "mallory", "", 3)
            })
            .is_empty());
        assert_eq!(bodies(&timeline), ["original"]);

        let diffs = timeline.apply_live(IncomingMessage {
            moderates_id: Some("s1".into()),
            ..IncomingMessage::default()
        });
        assert!(matches!(&diffs[0], TimelineDiff::Set { item, .. } if item.retracted));
    }

    #[test]
    fn a_correction_paged_in_before_its_target_waits_for_it() {
        let mut timeline = Timeline::new();
        // Newest page first: it holds the correction, not the original.
        timeline.apply_history_page(
            vec![correction("s5", "bob", "s1", "fixed", 5)],
            Some("s5".into()),
            false,
        );
        assert!(timeline.items().is_empty());
        let diffs = timeline.apply_history_page(
            vec![message("s1", "bob", "fxied", 1)],
            Some("s1".into()),
            true,
        );
        let [TimelineDiff::PushFront { items }] = diffs.as_slice() else {
            panic!("{diffs:?}");
        };
        assert_eq!(items[0].body, "fixed");
        assert!(items[0].edited);
    }

    #[test]
    fn a_held_correction_from_someone_else_is_dropped_when_the_target_arrives() {
        let mut timeline = Timeline::new();
        timeline.apply_live(correction("s5", "mallory", "s1", "forged", 5));
        timeline.apply_history_page(vec![message("s1", "bob", "real", 1)], None, true);
        assert_eq!(bodies(&timeline), ["real"]);
        assert!(!timeline.items()[0].edited);
    }

    #[test]
    fn the_latest_correction_wins_whatever_order_they_arrive_in() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "v1", 1));
        timeline.apply_live(correction("s3", "bob", "s1", "v3", 3));
        assert!(timeline
            .apply_live(correction("s2", "bob", "s1", "v2", 2))
            .is_empty());
        assert_eq!(bodies(&timeline), ["v3"]);
    }

    #[test]
    fn edits_to_held_rows_from_an_older_page_shift_their_index() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s5", "bob", "typo", 5));
        let diffs = timeline.apply_history_page(
            vec![
                message("s1", "ann", "first", 1),
                correction("s6", "bob", "s5", "fixed", 6),
            ],
            Some("s1".into()),
            false,
        );
        assert!(matches!(&diffs[0], TimelineDiff::PushFront { items } if items.len() == 1));
        assert!(matches!(&diffs[1], TimelineDiff::Set { index: 1, item } if item.body == "fixed"));
        assert_eq!(bodies(&timeline), ["first", "fixed"]);
    }

    #[test]
    fn a_retraction_waits_for_its_target_and_tombstones_stay_empty() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![IncomingMessage {
                retracts_id: Some("s1".into()),
                ..message("s9", "bob", "", 9)
            }],
            None,
            false,
        );
        timeline.apply_history_page(
            vec![
                message("s1", "bob", "oops", 1),
                IncomingMessage {
                    tombstone: true,
                    ..message("s2", "ann", "gone", 2)
                },
            ],
            None,
            true,
        );
        assert!(timeline.items().iter().all(|item| item.retracted));
        assert_eq!(bodies(&timeline), ["", ""]);
    }

    #[test]
    fn reactions_replace_each_senders_set_and_tally_across_senders() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "lunch?", 1));
        timeline.apply_live(reaction("ann", "s1", &["👍", "🍕"], 2));
        timeline.apply_live(reaction("cat", "s1", &["👍"], 3));
        let diffs = timeline.apply_live(IncomingMessage {
            own: true,
            ..reaction("me", "s1", &["👍", "👍"], 4)
        });
        let [TimelineDiff::Set { item, .. }] = diffs.as_slice() else {
            panic!("{diffs:?}");
        };
        assert_eq!(tallies(item), [("🍕", 1, false), ("👍", 3, true)]);

        // Ann replaces her set; cat clears hers.
        timeline.apply_live(reaction("ann", "s1", &["🍕"], 5));
        timeline.apply_live(reaction("cat", "s1", &[], 6));
        assert_eq!(
            tallies(&timeline.items()[0]),
            [("👍", 1, true), ("🍕", 1, false)]
        );
        assert_eq!(timeline.own_reactions("s1"), ["👍"]);
    }

    #[test]
    fn an_older_reaction_paged_in_later_does_not_undo_a_newer_one() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![message("s1", "bob", "hi", 1), reaction("ann", "s1", &[], 9)],
            None,
            false,
        );
        // An older page carries Ann's earlier reaction, and the message it
        // targets is already held.
        timeline.apply_history_page(vec![reaction("ann", "s1", &["❤️"], 2)], None, true);
        assert!(timeline.items()[0].reactions.is_empty());
    }

    #[test]
    fn reactions_that_arrive_before_their_message_are_tallied_when_it_lands() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(vec![reaction("ann", "s1", &["🎉"], 5)], None, false);
        let diffs =
            timeline.apply_history_page(vec![message("s1", "bob", "shipped", 1)], None, true);
        let [TimelineDiff::PushFront { items }] = diffs.as_slice() else {
            panic!("{diffs:?}");
        };
        assert_eq!(tallies(&items[0]), [("🎉", 1, false)]);
    }

    #[test]
    fn own_reactions_apply_optimistically_and_roll_back() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "hi", 1));
        timeline.apply_own_reaction("s1", "me", &["👋".to_string()], 2);
        assert_eq!(tallies(&timeline.items()[0]), [("👋", 1, true)]);
        timeline.apply_own_reaction("s1", "me", &[], 3);
        assert!(timeline.items()[0].reactions.is_empty());
    }

    #[test]
    fn replies_quote_their_target_and_follow_its_edits() {
        let mut timeline = Timeline::new();
        timeline.apply_live(message("s1", "bob", "Bake a cake?", 1));
        let diffs = timeline.apply_live(IncomingMessage {
            reply_to: Some(ReplyTo {
                id: "s1".into(),
                sender: None,
            }),
            ..message("s2", "ann", "Yes!", 2)
        });
        let [TimelineDiff::Append { items }] = diffs.as_slice() else {
            panic!("{diffs:?}");
        };
        let reply = items[0].reply.as_ref().expect("a reply quote");
        assert_eq!(reply.sender.as_deref(), Some("bob"));
        assert_eq!(reply.snippet.as_deref(), Some("Bake a cake?"));

        let diffs = timeline.apply_live(correction("s3", "bob", "s1", "Bake a pie?", 3));
        assert_eq!(diffs.len(), 2);
        assert_eq!(
            timeline.items()[1]
                .reply
                .as_ref()
                .and_then(|reply| reply.snippet.as_deref()),
            Some("Bake a pie?")
        );

        timeline.apply_live(IncomingMessage {
            retracts_id: Some("s1".into()),
            ..message("s4", "bob", "", 4)
        });
        let reply = timeline.items()[1].reply.as_ref().expect("a reply quote");
        assert!(reply.target_retracted);
        assert_eq!(reply.snippet, None);
    }

    #[test]
    fn a_reply_to_a_message_not_yet_loaded_is_quoted_once_it_is() {
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            reply_to: Some(ReplyTo {
                id: "s1".into(),
                sender: Some("bob".into()),
            }),
            ..message("s2", "ann", "Agreed", 2)
        });
        assert_eq!(
            timeline.items()[0]
                .reply
                .as_ref()
                .and_then(|reply| reply.snippet.clone()),
            None
        );
        let diffs =
            timeline.apply_history_page(vec![message("s1", "bob", "Ship it", 1)], None, true);
        assert!(
            matches!(&diffs[1], TimelineDiff::Set { index: 1, item } if item.reply.as_ref().and_then(|reply| reply.snippet.as_deref()) == Some("Ship it"))
        );
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
            timeline.apply_live(message(id, sender, "x", at));
        }
        let groups: Vec<bool> = (0..5).map(|i| timeline.continues_group(i, 0)).collect();
        assert_eq!(groups, [false, true, false, false, false]);
    }

    #[test]
    fn messages_without_identity_or_body_are_ignored() {
        let mut timeline = Timeline::new();
        assert!(timeline
            .apply_live(IncomingMessage {
                stanza_id: None,
                reference_id: None,
                ..message("x", "bob", "no id", 1)
            })
            .is_empty());
        assert!(timeline
            .apply_live(IncomingMessage {
                body: None,
                ..message("s1", "bob", "", 1)
            })
            .is_empty());
    }

    #[test]
    fn fallback_quotes_round_trip() {
        let (body, start, end) =
            with_reply_fallback("Anna", "We should bake a cake", "Great idea!");
        assert_eq!(body, "> Anna wrote:\n> We should bake a cake\nGreat idea!");
        assert_eq!(strip_fallback(&body, start, end), "Great idea!");
        // Offsets count characters, not bytes.
        assert_eq!(strip_fallback("🎂🎂ok", 0, 2), "ok");
        assert_eq!(strip_fallback("short", 2, 99), "short");
        assert_eq!(strip_fallback("short", 3, 2), "short");
    }
}
