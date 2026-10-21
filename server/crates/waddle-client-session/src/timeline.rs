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

use crate::judgments::{SafetyScores, SafetyScoresFastening, ScoreTarget, StampedScores};

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

/// Application-level evidence takes precedence over transport outcomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryEvidence {
    Reflected,
    Rejected,
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

/// A file shared in a message (XEP-0447), with its XEP-0448 envelope when
/// the bytes at `url` are ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub url: String,
    pub name: Option<String>,
    pub media_type: Option<String>,
    /// Plaintext size in bytes.
    pub size: Option<u64>,
    /// XEP-0446 `disposition='inline'`: show it in the message.
    pub inline: bool,
    pub encryption: Option<Encryption>,
}

/// XEP-0448: how to decrypt an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encryption {
    pub cipher: String,
    pub key_b64: String,
    pub iv_b64: String,
    /// SHA-256 hashes of the ciphertext, base64.
    pub sha256_b64: Vec<String>,
    /// Where the ciphertext can be fetched; never empty.
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineItem {
    pub key: ItemKey,
    /// Display name of the sender: the room nick, or the DM peer's local
    /// part.
    pub sender: String,
    /// The sender's stable identity when the archive exposes it (a room's
    /// real JID). A nick can pass to someone else; this cannot.
    pub author_id: Option<String>,
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
    pub attachments: Vec<Attachment>,
    /// Server judgments for this exact body revision.
    pub scores: Option<SafetyScores>,
    score_target: Option<ScoreTarget>,
    source_revision_id: Option<String>,
    scores_at: Option<i64>,
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
    /// See [`TimelineItem::author_id`].
    pub author_id: Option<String>,
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
    /// Verified room archive identity; absent for unverified IDs and DMs.
    pub score_target: Option<ScoreTarget>,
    /// Trusted room-authored XEP-0422 judgment update, never occupant data.
    pub safety_scores: Option<SafetyScoresFastening>,
    pub attachments: Vec<Attachment>,
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
    pub attachments: Vec<Attachment>,
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

/// Who sent something, as far as the conversation can tell: the room nick
/// or DM display name, and whether it is the user.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Author {
    sender: String,
    own: bool,
}

impl Author {
    fn of_item(item: &TimelineItem) -> Self {
        Self {
            sender: item.sender.clone(),
            own: item.own,
        }
    }

    fn of(message: &IncomingMessage) -> Self {
        Self {
            sender: message.sender.clone(),
            own: message.own,
        }
    }
}

#[derive(Debug, Clone)]
struct Reacted {
    at: i64,
    emojis: Vec<String>,
}

#[derive(Debug, Clone)]
struct Edit {
    source_revision_id: Option<String>,
    author_id: Option<String>,
    body: String,
    at: i64,
}

/// The ids one row is known by, split by who chose them.
#[derive(Debug, Clone, Default)]
struct RowIds {
    /// XEP-0359 stanza id, verified by the adapter.
    stanza: Option<String>,
    /// What replies, reactions and retractions reference. In a room this is
    /// the room's stanza id; in a DM the sender's origin id or `@id`.
    reference: Option<String>,
    /// The sender's origin id and `@id`. Anyone can choose any value, so
    /// these resolve only together with the sender.
    chosen: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Timeline {
    items: Vec<TimelineItem>,
    /// Ids of each row, parallel to `items`.
    ids: Vec<RowIds>,
    /// A serial per row, parallel to `items`, stable across prepends.
    serials: Vec<u64>,
    next_serial: u64,
    by_serial: HashMap<u64, usize>,
    by_stanza: HashMap<String, usize>,
    /// Exact judgment identity to stable row serials, including ambiguous
    /// identities so a later duplicate invalidates already shown scores.
    by_score_target: HashMap<ScoreTarget, BTreeSet<u64>>,
    by_reference: HashMap<String, usize>,
    by_chosen: HashMap<(Author, String), usize>,
    /// The user's own rows by origin id and `@id`, for delivery outcomes.
    own_chosen: HashMap<String, usize>,
    /// Confirmed reflections and explicit rejections, by stable row serial.
    /// Transport acknowledgments alone do not establish acceptance.
    delivery_evidence: HashMap<u64, DeliveryEvidence>,
    /// Corrections whose target is not held yet, by sender name and
    /// target. Every candidate is kept: one claiming the same name under
    /// another identity must not displace the real sender's.
    pending_edits: HashMap<(Author, String), Vec<Edit>>,
    /// Retractions by their sender whose target is not held yet, each with
    /// the sender's stable identity when known.
    pending_retractions: HashMap<(Author, String), Vec<Option<String>>>,
    /// Moderations whose target is not held yet.
    pending_moderations: HashSet<String>,
    /// Every sender's latest reaction set, by target id.
    reactions: HashMap<String, HashMap<Author, Reacted>>,
    /// Latest trusted batch per target and accepted body revision. Keep
    /// unseen targets/revisions for newest-first archive pagination.
    judgments: HashMap<(ScoreTarget, String), StampedScores>,
    /// Target id to the serials of rows that reply to it.
    replies: HashMap<String, Vec<u64>>,
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

    /// The row a reply, reaction or retraction would reference by `id`.
    pub fn get(&self, id: &str) -> Option<&TimelineItem> {
        self.referenced(id).map(|at| &self.items[at])
    }

    /// The RSM `before` cursor for the next older page.
    pub fn older_cursor(&self) -> Option<&str> {
        self.oldest_archive_id.as_deref()
    }

    pub fn history_complete(&self) -> bool {
        self.history_complete
    }

    /// The user's current reaction set on the row referenced by `id`, for
    /// building the complete set XEP-0444 sends.
    pub fn own_reactions(&self, id: &str) -> Vec<String> {
        let Some(at) = self.referenced(id) else {
            return Vec::new();
        };
        let mut latest: Option<&Reacted> = None;
        for target in self.reaction_targets(at) {
            for (author, reacted) in self.reactions.get(target).into_iter().flatten() {
                if author.own && latest.is_none_or(|seen| reacted.at >= seen.at) {
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

        let mut older: Vec<TimelineItem> = Vec::new();
        let mut older_ids: Vec<RowIds> = Vec::new();
        let mut fastenings = Vec::new();
        for message in messages {
            if is_fastening(&message) {
                fastenings.push(message);
                continue;
            }
            let ids = ids_of(&message);
            let author = Author::of(&message);
            let in_page = older
                .iter()
                .zip(&older_ids)
                .any(|(item, held)| same_row(&ids, &author, held, &Author::of_item(item)));
            if in_page || self.holds(&ids, &author) {
                continue;
            }
            let Some(item) = content_item(&message) else {
                continue;
            };
            older.push(item);
            older_ids.push(ids);
        }

        let pushed = older.len();
        let mut touched = BTreeSet::new();
        if pushed > 0 {
            // References an older row may now take over from a newer one.
            let previous: Vec<(String, usize)> = self
                .by_reference
                .iter()
                .map(|(id, &at)| (id.clone(), at + pushed))
                .collect();
            let serials: Vec<u64> = (0..pushed).map(|_| self.serial()).collect();
            older.append(&mut self.items);
            self.items = older;
            older_ids.append(&mut self.ids);
            self.ids = older_ids;
            let mut all_serials = serials;
            all_serials.append(&mut self.serials);
            self.serials = all_serials;
            self.reindex();
            for (id, displaced) in previous {
                if self.by_reference.get(&id) != Some(&displaced) {
                    let reactions = self.tally(displaced);
                    if self.items[displaced].reactions != reactions {
                        self.items[displaced].reactions = reactions;
                        touched.insert(displaced);
                    }
                }
            }
        }

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
        if self.own_chosen.contains_key(&echo.origin_id) {
            return Vec::new();
        }
        let ids = RowIds {
            stanza: None,
            reference: echo.reference_id.clone(),
            chosen: vec![echo.origin_id.clone()],
        };
        let item = TimelineItem {
            key: ItemKey::Origin(echo.origin_id.clone()),
            sender: echo.sender,
            author_id: None,
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
            attachments: echo.attachments,
            scores: None,
            score_target: None,
            source_revision_id: None,
            scores_at: None,
            edited_at: None,
        };
        self.append(item, ids)
    }

    /// The server acknowledged (or failed) the user's send with this
    /// origin id.
    pub fn set_delivery(&mut self, origin_id: &str, delivery: Delivery) -> Vec<TimelineDiff> {
        let Some(&at) = self.own_chosen.get(origin_id) else {
            return Vec::new();
        };
        if self.delivery_evidence.contains_key(&self.serials[at]) {
            return Vec::new();
        }
        let item = &mut self.items[at];
        if item.delivery == Delivery::Sent || item.delivery == delivery {
            return Vec::new();
        }
        item.delivery = delivery;
        self.sets([at])
    }

    /// Apply an explicit server rejection after the adapter has correlated
    /// its message id and addresses with a retained outbound send. Unlike a
    /// transport failure, this can supersede a stream acknowledgment. An
    /// actual reflection remains proof that the message was accepted.
    pub fn reject_send(&mut self, origin_id: &str) -> Vec<TimelineDiff> {
        let Some(&at) = self.own_chosen.get(origin_id) else {
            return Vec::new();
        };
        let serial = self.serials[at];
        if self.delivery_evidence.get(&serial) == Some(&DeliveryEvidence::Reflected) {
            return Vec::new();
        }
        self.delivery_evidence
            .insert(serial, DeliveryEvidence::Rejected);
        let item = &mut self.items[at];
        if item.delivery == Delivery::Failed {
            return Vec::new();
        }
        item.delivery = Delivery::Failed;
        self.sets([at])
    }

    /// Put a row's content back as it was in `previous`, undoing an
    /// optimistic edit or delete whose send failed.
    pub fn restore(&mut self, previous: &TimelineItem) -> Vec<TimelineDiff> {
        let at = match &previous.key {
            ItemKey::Stanza(id) => self.by_stanza.get(id).copied(),
            ItemKey::Origin(id) => self
                .own_chosen
                .get(id)
                .or_else(|| self.by_chosen.get(&(Author::of_item(previous), id.clone())))
                .copied(),
        };
        let Some(at) = at else {
            return Vec::new();
        };
        let item = &mut self.items[at];
        if item.body == previous.body
            && item.edited == previous.edited
            && item.retracted == previous.retracted
        {
            return Vec::new();
        }
        item.body.clone_from(&previous.body);
        item.edited = previous.edited;
        item.edited_at = previous.edited_at;
        item.retracted = previous.retracted;
        item.attachments.clone_from(&previous.attachments);
        item.scores.clone_from(&previous.scores);
        item.source_revision_id
            .clone_from(&previous.source_revision_id);
        item.scores_at = previous.scores_at;
        let mut touched = BTreeSet::from([at]);
        if let Some(target) = self.items[at].score_target.clone() {
            self.invalidate_ambiguous_judgments(&target, &mut touched);
        }
        self.refresh_replies_to(at, &mut touched);
        self.sets(touched)
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
                .and_then(|origin| self.own_chosen.get(origin))
            {
                self.take_reflection(at, &message, &mut touched);
                return self.sets(touched);
            }
        }

        let ids = ids_of(&message);
        if self.holds(&ids, &Author::of(&message)) {
            return Vec::new();
        }
        let Some(item) = content_item(&message) else {
            return Vec::new();
        };
        self.append(item, ids)
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
        let author = Author {
            sender: sender.to_string(),
            own: true,
        };
        self.react(target_id, author, timestamp_ms, emojis, &mut touched);
        self.sets(touched)
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

    fn serial(&mut self) -> u64 {
        self.next_serial += 1;
        self.next_serial
    }

    /// A row referenced by a room-assigned or reference id.
    fn referenced(&self, id: &str) -> Option<usize> {
        self.by_reference
            .get(id)
            .or_else(|| self.by_stanza.get(id))
            .copied()
    }

    /// A row its sender identifies by an id they chose, falling back to a
    /// referenced row of theirs (some clients correct by stanza id). A
    /// referenced row by someone else is not the target: in a DM the
    /// reference is sender-chosen, so the real target may still be paged in.
    fn by_sender(&self, author: &Author, author_id: Option<&str>, id: &str) -> Option<usize> {
        self.by_chosen
            .get(&(author.clone(), id.to_string()))
            .copied()
            .or_else(|| {
                self.referenced(id)
                    .filter(|&at| same_author(&self.items[at], author, author_id))
            })
    }

    /// Whether a message with these ids from this author is already shown:
    /// the same verified stanza id, or the same sender-chosen id from the
    /// same sender.
    fn holds(&self, ids: &RowIds, author: &Author) -> bool {
        // When both carry verified stanza ids, those decide: a sender may
        // reuse an `@id` for different messages.
        ids.stanza
            .as_ref()
            .is_some_and(|id| self.by_stanza.contains_key(id))
            || ids.chosen.iter().any(|id| {
                self.by_chosen
                    .get(&(author.clone(), id.clone()))
                    .is_some_and(|&at| ids.stanza.is_none() || self.ids[at].stanza.is_none())
            })
    }

    fn append(&mut self, item: TimelineItem, ids: RowIds) -> Vec<TimelineDiff> {
        let at = self.items.len();
        let serial = self.serial();
        self.items.push(item);
        self.ids.push(ids);
        self.serials.push(serial);
        self.index_row(at);
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
        let reflected = ids_of(message);
        let item = &mut self.items[at];
        if let Some(stanza_id) = &message.stanza_id {
            item.key = ItemKey::Stanza(stanza_id.clone());
        }
        if item.reference_id.is_none() {
            item.reference_id.clone_from(&message.reference_id);
        }
        if item.score_target.is_none() {
            item.score_target.clone_from(&message.score_target);
        }
        if !item.edited {
            item.source_revision_id.clone_from(&message.stanza_id);
        }
        item.delivery = Delivery::Sent;
        self.delivery_evidence
            .insert(self.serials[at], DeliveryEvidence::Reflected);
        let ids = &mut self.ids[at];
        if ids.stanza.is_none() {
            ids.stanza = reflected.stanza;
        }
        if ids.reference.is_none() {
            ids.reference = reflected.reference;
        }
        for id in reflected.chosen {
            if !ids.chosen.contains(&id) {
                ids.chosen.push(id);
            }
        }
        self.index_row(at);
        touched.insert(at);
        self.settle(at, touched);
    }

    /// Bring a newly placed row up to date: fastenings that were waiting
    /// for it, its reactions, its reply quote, and the quotes of rows that
    /// reply to it.
    fn settle(&mut self, at: usize, touched: &mut BTreeSet<usize>) {
        let author = Author::of_item(&self.items[at]);
        let ids = self.ids[at].clone();
        let all: Vec<&String> = ids
            .chosen
            .iter()
            .chain(ids.stanza.iter())
            .chain(ids.reference.iter())
            .collect();
        for id in &all {
            let key = (author.clone(), (*id).clone());
            // The latest correction from the real sender, ignoring any that
            // only share the name.
            let edit = self
                .pending_edits
                .remove(&key)
                .unwrap_or_default()
                .into_iter()
                .filter(|edit| same_author(&self.items[at], &author, edit.author_id.as_deref()))
                .max_by_key(|edit| edit.at);
            if let Some(edit) = edit {
                if self.apply_edit_at(at, &author, &edit) {
                    touched.insert(at);
                }
            }
            for author_id in self.pending_retractions.remove(&key).unwrap_or_default() {
                if self.apply_removal_at(at, Some((&author, author_id.as_deref()))) {
                    touched.insert(at);
                }
            }
        }
        for id in ids.stanza.iter().chain(ids.reference.iter()) {
            if self.pending_moderations.remove(id) && self.apply_removal_at(at, None) {
                touched.insert(at);
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
            let serial = self.serials[at];
            let replies = self.replies.entry(target).or_default();
            if !replies.contains(&serial) {
                replies.push(serial);
            }
            if self.refresh_preview(at) {
                touched.insert(at);
            }
        }
        self.refresh_replies_to(at, touched);
        self.settle_judgments(at, touched);
    }

    /// Apply a correction, retraction, moderation or reaction. False when
    /// the message is none of these.
    fn apply_fastening(
        &mut self,
        message: &IncomingMessage,
        touched: &mut BTreeSet<usize>,
    ) -> bool {
        let author = Author::of(message);
        if let Some(update) = &message.safety_scores {
            let key = (update.target.clone(), update.source_revision_id.clone());
            if self
                .judgments
                .get(&key)
                .is_none_or(|held| message.timestamp_ms >= held.at)
            {
                self.judgments.insert(
                    key,
                    StampedScores {
                        at: message.timestamp_ms,
                        scores: update.scores.clone(),
                    },
                );
                self.settle_judgment_target(&update.target, touched);
            }
            return true;
        }
        if let Some(update) = &message.reaction {
            self.react(
                &update.target_id,
                author,
                message.timestamp_ms,
                &update.emojis,
                touched,
            );
            return true;
        }
        if let Some(target) = &message.moderates_id {
            match self.referenced(target) {
                Some(at) => {
                    if self.apply_removal_at(at, None) {
                        touched.insert(at);
                        self.refresh_replies_to(at, touched);
                    }
                }
                None => {
                    self.pending_moderations.insert(target.clone());
                }
            }
            return true;
        }
        if let Some(target) = &message.retracts_id {
            match self.by_sender(&author, message.author_id.as_deref(), target) {
                Some(at) => {
                    if self.apply_removal_at(at, Some((&author, message.author_id.as_deref()))) {
                        touched.insert(at);
                        self.refresh_replies_to(at, touched);
                    }
                }
                None => {
                    self.pending_retractions
                        .entry((author, target.clone()))
                        .or_default()
                        .push(message.author_id.clone());
                }
            }
            return true;
        }
        if let Some(target) = &message.replaces_id {
            let Some(body) = message.body.clone() else {
                return true;
            };
            let edit = Edit {
                source_revision_id: message.stanza_id.clone(),
                author_id: message.author_id.clone(),
                body,
                at: message.timestamp_ms,
            };
            match self.by_sender(&author, message.author_id.as_deref(), target) {
                Some(at) => {
                    if self.apply_edit_at(at, &author, &edit) {
                        touched.insert(at);
                        self.refresh_replies_to(at, touched);
                        self.settle_judgments(at, touched);
                    }
                }
                None => {
                    self.pending_edits
                        .entry((author, target.clone()))
                        .or_default()
                        .push(edit);
                }
            }
            return true;
        }
        false
    }

    fn settle_judgments(&mut self, at: usize, touched: &mut BTreeSet<usize>) {
        if let Some(target) = self.items[at].score_target.clone() {
            self.settle_judgment_target(&target, touched);
        }
    }

    /// Ambiguous identities cannot retain a batch applied before the
    /// duplicate arrived. Stable serials remain valid when history prepends.
    fn invalidate_ambiguous_judgments(
        &mut self,
        target: &ScoreTarget,
        touched: &mut BTreeSet<usize>,
    ) -> bool {
        let Some(rows) = self
            .by_score_target
            .get(target)
            .filter(|rows| rows.len() > 1)
        else {
            return false;
        };
        for serial in rows {
            if let Some(&at) = self.by_serial.get(serial) {
                let item = &mut self.items[at];
                if item.scores.take().is_some() {
                    touched.insert(at);
                }
                item.scores_at = None;
            }
        }
        true
    }

    /// A judgment visits only the rows bearing its exact target identity;
    /// the ordinary unique-target case performs no timeline scan.
    fn settle_judgment_target(&mut self, target: &ScoreTarget, touched: &mut BTreeSet<usize>) {
        if self.invalidate_ambiguous_judgments(target, touched) {
            return;
        }
        let Some(at) = self
            .by_score_target
            .get(target)
            .and_then(|rows| rows.first())
            .and_then(|serial| self.by_serial.get(serial))
            .copied()
        else {
            return;
        };
        let item = &self.items[at];
        let Some(revision) = &item.source_revision_id else {
            return;
        };
        if item.retracted {
            return;
        }
        let Some(batch) = self.judgments.get(&(target.clone(), revision.clone())) else {
            return;
        };
        if item.scores_at.is_some_and(|held| held > batch.at) {
            return;
        }
        if item.scores.as_ref() != Some(&batch.scores) || item.scores_at != Some(batch.at) {
            let item = &mut self.items[at];
            item.scores = Some(batch.scores.clone());
            item.scores_at = Some(batch.at);
            touched.insert(at);
        }
    }

    fn react(
        &mut self,
        target: &str,
        author: Author,
        at: i64,
        emojis: &[String],
        touched: &mut BTreeSet<usize>,
    ) {
        let by_sender = self.reactions.entry(target.to_string()).or_default();
        if by_sender.get(&author).is_some_and(|held| held.at > at) {
            return;
        }
        let mut unique: Vec<String> = Vec::new();
        for emoji in emojis {
            if !emoji.is_empty() && !unique.contains(emoji) {
                unique.push(emoji.clone());
            }
        }
        by_sender.insert(author, Reacted { at, emojis: unique });
        if let Some(row) = self.referenced(target) {
            let reactions = self.tally(row);
            if self.items[row].reactions != reactions {
                self.items[row].reactions = reactions;
                touched.insert(row);
            }
        }
    }

    /// The ids that reference the row at `at`: its reference and stanza
    /// ids, where they resolve to it. When two rows share a DM reference,
    /// the older holds it, so a later reuse cannot claim its reactions.
    fn reaction_targets(&self, at: usize) -> Vec<&String> {
        let ids = &self.ids[at];
        let mut targets: Vec<&String> = Vec::new();
        for id in ids.reference.iter().chain(ids.stanza.iter()) {
            if !targets.contains(&id) && self.referenced(id) == Some(at) {
                targets.push(id);
            }
        }
        targets
    }

    /// The row's reactions, each sender's latest set counted once.
    fn tally(&self, at: usize) -> Vec<Reaction> {
        let mut latest: HashMap<&Author, &Reacted> = HashMap::new();
        for target in self.reaction_targets(at) {
            for (author, reacted) in self.reactions.get(target).into_iter().flatten() {
                let newer = latest.get(author).is_none_or(|seen| reacted.at >= seen.at);
                if newer {
                    latest.insert(author, reacted);
                }
            }
        }
        // Count, whether the user is among them, and when the emoji was
        // first used, so chips keep their place as counts change.
        let mut counts: BTreeMap<&str, (u32, bool, i64)> = BTreeMap::new();
        for (author, reacted) in latest {
            for emoji in &reacted.emojis {
                let entry = counts.entry(emoji).or_insert((0, false, reacted.at));
                entry.0 += 1;
                entry.1 |= author.own;
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

    fn apply_edit_at(&mut self, at: usize, author: &Author, edit: &Edit) -> bool {
        let item = &mut self.items[at];
        if item.retracted || !same_author(item, author, edit.author_id.as_deref()) {
            return false;
        }
        if item.edited_at.is_some_and(|applied| edit.at < applied) {
            return false;
        }
        item.edited_at = Some(edit.at);
        let changed = !item.edited
            || item.body != edit.body
            || item.source_revision_id != edit.source_revision_id;
        if changed {
            item.body.clone_from(&edit.body);
            item.edited = true;
            item.source_revision_id.clone_from(&edit.source_revision_id);
            item.scores = None;
            item.scores_at = None;
        }
        changed
    }

    /// Retract the row: by its sender (`Some`), or by the room (`None`).
    fn apply_removal_at(&mut self, at: usize, by: Option<(&Author, Option<&str>)>) -> bool {
        let item = &mut self.items[at];
        if item.retracted {
            return false;
        }
        if let Some((author, author_id)) = by {
            if !same_author(item, author, author_id) {
                return false;
            }
        }
        item.retracted = true;
        item.body.clear();
        item.attachments.clear();
        item.scores = None;
        item.scores_at = None;
        true
    }

    /// Re-quote the rows replying to the row at `at`.
    fn refresh_replies_to(&mut self, at: usize, touched: &mut BTreeSet<usize>) {
        let serials: Vec<u64> = self
            .reaction_targets(at)
            .into_iter()
            .filter_map(|target| self.replies.get(target))
            .flatten()
            .copied()
            .collect();
        for serial in serials {
            if let Some(&row) = self.by_serial.get(&serial) {
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
        let Some(target) = self.referenced(&reply.target_id) else {
            return false;
        };
        let quoted = &self.items[target];
        let next = ReplyPreview {
            target_id: reply.target_id.clone(),
            sender: Some(quoted.sender.clone()),
            snippet: (!quoted.retracted).then(|| snippet(&quoted.body)),
            target_retracted: quoted.retracted,
        };
        match self.items[at].reply.as_mut() {
            Some(reply) if *reply != next => {
                *reply = next;
                true
            }
            _ => false,
        }
    }

    fn index_row(&mut self, at: usize) {
        let item = &self.items[at];
        let ids = &self.ids[at];
        let author = Author::of_item(item);
        self.by_serial.insert(self.serials[at], at);
        if let Some(target) = &item.score_target {
            self.by_score_target
                .entry(target.clone())
                .or_default()
                .insert(self.serials[at]);
        }
        if let Some(stanza) = &ids.stanza {
            self.by_stanza.entry(stanza.clone()).or_insert(at);
        }
        if let Some(reference) = &ids.reference {
            // First come keeps it: in a room this is the room's own id, so
            // it never collides; in a DM a collision is the peer confusing
            // their own conversation.
            self.by_reference.entry(reference.clone()).or_insert(at);
        }
        for id in &ids.chosen {
            self.by_chosen.insert((author.clone(), id.clone()), at);
            if item.own {
                self.own_chosen.insert(id.clone(), at);
            }
        }
    }

    fn reindex(&mut self) {
        self.by_serial.clear();
        self.by_stanza.clear();
        self.by_score_target.clear();
        self.by_reference.clear();
        self.by_chosen.clear();
        self.own_chosen.clear();
        for at in 0..self.items.len() {
            self.index_row(at);
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

/// Whether a fastening from `author` may change `item`: the same side
/// (user or not) and, when both carry a stable identity (a room's real
/// JID), the same one; otherwise the same name.
fn same_author(item: &TimelineItem, author: &Author, author_id: Option<&str>) -> bool {
    if item.own != author.own {
        return false;
    }
    match (item.author_id.as_deref(), author_id) {
        (Some(held), Some(claimed)) => held == claimed,
        _ => item.sender == author.sender,
    }
}

/// Whether two messages in one page are the same row.
fn same_row(ids: &RowIds, author: &Author, held: &RowIds, held_author: &Author) -> bool {
    let same_stanza = ids.stanza.is_some() && ids.stanza == held.stanza;
    let unverified = ids.stanza.is_none() || held.stanza.is_none();
    let same_chosen =
        unverified && author == held_author && ids.chosen.iter().any(|id| held.chosen.contains(id));
    same_stanza || same_chosen
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
    message.safety_scores.is_some()
        || message.reaction.is_some()
        || message.moderates_id.is_some()
        || message.retracts_id.is_some()
        || message.replaces_id.is_some()
}

fn ids_of(message: &IncomingMessage) -> RowIds {
    let non_empty = |id: &Option<String>| id.clone().filter(|id| !id.is_empty());
    let mut chosen: Vec<String> = Vec::new();
    for id in [&message.origin_id, &message.message_id]
        .into_iter()
        .filter_map(non_empty)
    {
        if !chosen.contains(&id) {
            chosen.push(id);
        }
    }
    RowIds {
        stanza: non_empty(&message.stanza_id),
        reference: non_empty(&message.reference_id),
        chosen,
    }
}

fn content_item(message: &IncomingMessage) -> Option<TimelineItem> {
    let key = message
        .stanza_id
        .clone()
        .map(ItemKey::Stanza)
        .or_else(|| message.origin_id.clone().map(ItemKey::Origin))?;
    let body = if message.tombstone {
        String::new()
    } else if message.attachments.is_empty() {
        message.body.clone()?
    } else {
        // Files may be sent with no text, or with their URL as the text
        // for clients that cannot show them.
        message
            .body
            .clone()
            .filter(|body| !message.attachments.iter().any(|file| file.url == *body))
            .unwrap_or_default()
    };
    Some(TimelineItem {
        key,
        sender: message.sender.clone(),
        author_id: message.author_id.clone(),
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
        attachments: if message.tombstone {
            Vec::new()
        } else {
            message.attachments.clone()
        },
        scores: None,
        score_target: message.score_target.clone(),
        source_revision_id: message.stanza_id.clone(),
        scores_at: None,
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
            attachments: Vec::new(),
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
        assert!(timeline.get("s9").is_some());
        assert!(
            timeline.get("o1").is_none(),
            "sender-chosen ids are not references"
        );
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
        // Her message is hers: shown on its own, neither hidden nor merged.
        assert_eq!(bodies(&timeline), ["mine", "stolen"]);
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
    fn explicit_rejection_overrides_stream_ack_and_survives_reindex() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "hi"));
        timeline.set_delivery("o1", Delivery::Sent);
        let diffs = timeline.reject_send("o1");
        assert!(
            matches!(diffs.as_slice(), [TimelineDiff::Set { item, .. }] if item.delivery == Delivery::Failed)
        );

        // History prepends move the row, but do not lose its rejection.
        timeline.apply_history_page(vec![message("older", "bob", "earlier", 1)], None, true);
        assert!(timeline.set_delivery("o1", Delivery::Sent).is_empty());
        assert!(timeline.set_delivery("o1", Delivery::Sending).is_empty());
        assert!(timeline.reject_send("o1").is_empty());
        assert_eq!(timeline.items()[1].delivery, Delivery::Failed);
        assert_eq!(timeline.items()[0].delivery, Delivery::Received);
        assert!(timeline.reject_send("unknown").is_empty());
        assert!(timeline.reject_send("older").is_empty());
    }

    #[test]
    fn explicit_rejection_before_stream_ack_remains_failed() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "hi"));
        timeline.reject_send("o1");
        assert!(timeline.set_delivery("o1", Delivery::Sent).is_empty());
        assert_eq!(timeline.items()[0].delivery, Delivery::Failed);
    }

    #[test]
    fn actual_reflection_supersedes_rejection_and_remains_terminal() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "hi"));
        timeline.reject_send("o1");
        timeline.apply_live(IncomingMessage {
            origin_id: Some("o1".into()),
            own: true,
            ..message("s9", "me", "hi", 11)
        });
        assert_eq!(timeline.items()[0].delivery, Delivery::Sent);
        assert!(timeline.reject_send("o1").is_empty());
        assert!(timeline.set_delivery("o1", Delivery::Failed).is_empty());
        assert_eq!(timeline.items()[0].delivery, Delivery::Sent);
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

    fn sent(stanza: &str, origin: &str, sender: &str, body: &str, at: i64) -> IncomingMessage {
        IncomingMessage {
            origin_id: Some(origin.into()),
            message_id: Some(origin.into()),
            ..message(stanza, sender, body, at)
        }
    }

    fn senders(timeline: &Timeline) -> Vec<(&str, &str)> {
        timeline
            .items()
            .iter()
            .map(|item| (item.sender.as_str(), item.body.as_str()))
            .collect()
    }

    /// Mallory reuses Bob's room stanza id as her origin id: it must not
    /// hide Bob's message or take its reactions and replies.
    #[test]
    fn a_sender_chosen_id_cannot_hijack_someone_elses_message() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![
                sent("S5", "S1", "mallory", "I agree to pay mallory $500", 5),
                reaction("ann", "S1", &["👍"], 6),
                IncomingMessage {
                    reply_to: Some(ReplyTo {
                        id: "S1".into(),
                        sender: Some("bob".into()),
                    }),
                    ..sent("S7", "o7", "cat", "+1", 7)
                },
            ],
            Some("S5".into()),
            false,
        );
        timeline.apply_history_page(
            vec![sent("S1", "ob1", "bob", "lunch at noon?", 1)],
            Some("S1".into()),
            true,
        );
        assert_eq!(
            senders(&timeline),
            [
                ("bob", "lunch at noon?"),
                ("mallory", "I agree to pay mallory $500"),
                ("cat", "+1")
            ]
        );
        assert_eq!(tallies(&timeline.items()[0]), [("👍", 1, false)]);
        assert!(timeline.items()[1].reactions.is_empty());
        let quote = timeline.items()[2].reply.as_ref().expect("a quote");
        assert_eq!(quote.sender.as_deref(), Some("bob"));
        assert_eq!(quote.snippet.as_deref(), Some("lunch at noon?"));
    }

    #[test]
    fn two_senders_reusing_an_id_keep_both_messages() {
        let mut timeline = Timeline::new();
        let from = |stanza: &str, sender: &str, body: &str| IncomingMessage {
            message_id: Some("1".into()),
            ..message(stanza, sender, body, 1)
        };
        timeline.apply_history_page(
            vec![from("S1", "ann", "hi"), from("S2", "bob", "hello")],
            None,
            true,
        );
        assert_eq!(timeline.items().len(), 2);
        // The same sender's copy without a stanza id is still a duplicate.
        assert!(timeline
            .apply_live(IncomingMessage {
                stanza_id: None,
                reference_id: None,
                ..from("x", "ann", "hi")
            })
            .is_empty());
    }

    #[test]
    fn someone_elses_held_retraction_does_not_cancel_the_senders() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![
                IncomingMessage {
                    retracts_id: Some("ob1".into()),
                    body: None,
                    ..sent("S2", "o2", "bob", "", 2)
                },
                IncomingMessage {
                    retracts_id: Some("ob1".into()),
                    body: None,
                    ..sent("S3", "o3", "mallory", "", 3)
                },
            ],
            None,
            false,
        );
        timeline.apply_history_page(
            vec![sent("S1", "ob1", "bob", "my password is hunter2", 1)],
            None,
            true,
        );
        assert!(timeline.items()[0].retracted);
        assert_eq!(timeline.items()[0].body, "");
    }

    #[test]
    fn someone_elses_held_correction_does_not_cancel_the_senders() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![
                IncomingMessage {
                    replaces_id: Some("ob1".into()),
                    ..sent("S2", "o2", "bob", "meet at 3pm", 2)
                },
                IncomingMessage {
                    replaces_id: Some("ob1".into()),
                    ..sent("S3", "o3", "mallory", "x", 3)
                },
            ],
            None,
            false,
        );
        timeline.apply_history_page(vec![sent("S1", "ob1", "bob", "meet at 1pm", 1)], None, true);
        assert_eq!(timeline.items()[0].body, "meet at 3pm");
    }

    /// A nick can pass to someone else; a real JID cannot.
    #[test]
    fn a_later_holder_of_the_same_nick_cannot_edit_with_another_identity() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![
                IncomingMessage {
                    author_id: Some("bob@waddle.social".into()),
                    ..sent("S1", "ob1", "bob", "original", 1)
                },
                IncomingMessage {
                    author_id: Some("impostor@waddle.social".into()),
                    replaces_id: Some("ob1".into()),
                    ..sent("S2", "o2", "bob", "forged", 2)
                },
            ],
            None,
            true,
        );
        assert_eq!(timeline.items()[0].body, "original");
    }

    #[test]
    fn a_failed_edit_or_delete_is_restored() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(echo("o1", "draft"));
        let before = timeline.items()[0].clone();
        timeline.apply_live(IncomingMessage {
            replaces_id: Some("o1".into()),
            sender: "me".into(),
            own: true,
            body: Some("final".into()),
            timestamp_ms: 20,
            ..IncomingMessage::default()
        });
        assert_eq!(bodies(&timeline), ["final"]);
        let diffs = timeline.restore(&before);
        assert!(
            matches!(&diffs[0], TimelineDiff::Set { index: 0, item } if item.body == "draft" && !item.edited)
        );
    }

    fn file(url: &str) -> Attachment {
        Attachment {
            url: url.into(),
            name: Some("cat.png".into()),
            media_type: Some("image/png".into()),
            size: Some(3),
            inline: true,
            encryption: None,
        }
    }

    #[test]
    fn files_show_without_text_and_hide_a_url_fallback_body() {
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            body: None,
            attachments: vec![file("https://up.example/a")],
            ..message("s1", "ann", "", 1)
        });
        timeline.apply_live(IncomingMessage {
            attachments: vec![file("https://up.example/b")],
            ..message("s2", "ann", "https://up.example/b", 2)
        });
        timeline.apply_live(IncomingMessage {
            attachments: vec![file("https://up.example/c")],
            ..message("s3", "ann", "look at this", 3)
        });
        assert_eq!(bodies(&timeline), ["", "", "look at this"]);
        assert!(timeline
            .items()
            .iter()
            .all(|item| item.attachments.len() == 1));
    }

    #[test]
    fn deleting_a_message_drops_its_files() {
        let mut timeline = Timeline::new();
        timeline.apply_live(IncomingMessage {
            attachments: vec![file("https://up.example/a")],
            ..message("s1", "ann", "pic", 1)
        });
        timeline.apply_live(IncomingMessage {
            retracts_id: Some("s1".into()),
            ..message("s2", "ann", "", 2)
        });
        assert!(timeline.items()[0].attachments.is_empty());
    }
}

#[cfg(test)]
mod judgment_tests {
    use super::*;
    use crate::judgments::{Probability, SafetyCategory, SafetyScore};

    fn target() -> ScoreTarget {
        ScoreTarget {
            origin_id: "origin".into(),
            stanza_id: "original".into(),
            stanza_by: "room@rooms.example".parse().unwrap(),
        }
    }
    fn original() -> IncomingMessage {
        IncomingMessage {
            stanza_id: Some("original".into()),
            origin_id: Some("origin".into()),
            reference_id: Some("original".into()),
            sender: "alice".into(),
            body: Some("A question?".into()),
            timestamp_ms: 1,
            score_target: Some(target()),
            ..IncomingMessage::default()
        }
    }
    fn batch(revision: &str, probability: f64, at: i64) -> IncomingMessage {
        IncomingMessage {
            timestamp_ms: at,
            safety_scores: Some(SafetyScoresFastening {
                target: target(),
                source_revision_id: revision.into(),
                scores: SafetyScores {
                    model_version: "model-v1".into(),
                    scores: vec![SafetyScore {
                        category: SafetyCategory::IsQuestion,
                        probability: Probability::new(probability).unwrap(),
                        taxonomy_version: "question-v1".into(),
                    }],
                },
            }),
            ..IncomingMessage::default()
        }
    }
    fn correction() -> IncomingMessage {
        IncomingMessage {
            stanza_id: Some("revision-2".into()),
            replaces_id: Some("origin".into()),
            sender: "alice".into(),
            body: Some("Now an answer.".into()),
            timestamp_ms: 5,
            ..IncomingMessage::default()
        }
    }
    fn probability(timeline: &Timeline) -> Option<f64> {
        timeline.items()[0]
            .scores
            .as_ref()
            .map(|batch| batch.scores[0].probability.value())
    }
    #[test]
    fn xep0422_scores_wait_for_original_and_newest_batch_wins() {
        let mut timeline = Timeline::new();
        assert!(timeline.apply_live(batch("original", 0.9, 10)).is_empty());
        assert!(timeline.items().is_empty());
        timeline.apply_history_page(
            vec![original(), batch("original", 0.6, 4)],
            Some("page-1".into()),
            true,
        );
        assert_eq!(timeline.items().len(), 1);
        assert_eq!(probability(&timeline), Some(0.9));
        assert!(timeline.apply_live(batch("original", 0.5, 3)).is_empty());
        assert_eq!(probability(&timeline), Some(0.9));
    }
    #[test]
    fn xep0422_full_archive_identity_and_revision_must_match() {
        let mut timeline = Timeline::new();
        timeline.apply_live(original());
        for corrupt in 0..4 {
            let mut message = batch("original", 0.95, 10);
            let fastening = message.safety_scores.as_mut().unwrap();
            match corrupt {
                0 => fastening.target.origin_id = "other-origin".into(),
                1 => fastening.target.stanza_id = "other-stanza".into(),
                2 => fastening.target.stanza_by = "other@rooms.example".parse().unwrap(),
                _ => fastening.source_revision_id = "other-revision".into(),
            }
            assert!(timeline.apply_live(message).is_empty());
            assert_eq!(probability(&timeline), None);
        }
        assert!(!timeline.apply_live(batch("original", 0.95, 10)).is_empty());
        assert_eq!(probability(&timeline), Some(0.95));
    }
    #[test]
    fn xep0422_correction_invalidates_scores_and_requires_accepted_revision() {
        let mut timeline = Timeline::new();
        timeline.apply_live(original());
        timeline.apply_live(batch("original", 0.9, 2));
        timeline.apply_live(correction());
        assert_eq!(probability(&timeline), None);
        timeline.apply_live(batch("original", 0.99, 20));
        assert_eq!(probability(&timeline), None);
        timeline.apply_live(batch("revision-2", 0.8, 8));
        assert_eq!(probability(&timeline), Some(0.8));
        let mut stale = correction();
        stale.stanza_id = Some("stale-revision".into());
        stale.timestamp_ms = 3;
        timeline.apply_live(stale);
        assert_eq!(probability(&timeline), Some(0.8));
        let mut spoof = correction();
        spoof.sender = "mallory".into();
        spoof.body = Some("spoof".into());
        timeline.apply_live(spoof);
        assert_eq!(probability(&timeline), Some(0.8));
    }
    #[test]
    fn xep0422_corrections_and_scores_paged_before_target_settle_on_revision() {
        let mut timeline = Timeline::new();
        timeline.apply_history_page(
            vec![correction(), batch("revision-2", 0.8, 8)],
            Some("new-page".into()),
            false,
        );
        assert!(timeline.items().is_empty());
        timeline.apply_history_page(
            vec![original(), batch("original", 0.99, 4)],
            Some("old-page".into()),
            true,
        );
        assert_eq!(probability(&timeline), Some(0.8));
        assert_eq!(timeline.items()[0].body, "Now an answer.");
    }
    #[test]
    fn xep0422_replayed_own_reflection_cannot_restore_old_revision_scores() {
        let mut timeline = Timeline::new();
        timeline.add_local_echo(LocalEcho {
            origin_id: "origin".into(),
            sender: "alice".into(),
            body: "A question?".into(),
            timestamp_ms: 1,
            reference_id: None,
            reply_to: None,
            attachments: vec![],
        });
        let mut reflection = original();
        reflection.own = true;
        timeline.apply_live(reflection.clone());
        timeline.apply_live(batch("original", 0.9, 2));
        assert_eq!(probability(&timeline), Some(0.9));
        let mut edit = correction();
        edit.own = true;
        timeline.apply_live(edit);
        timeline.apply_live(batch("revision-2", 0.8, 8));
        timeline.apply_live(reflection);
        assert_eq!(probability(&timeline), Some(0.8));
        assert_eq!(timeline.items()[0].body, "Now an answer.");
    }

    #[test]
    fn xep0422_judgment_index_tracks_target_when_history_prepends() {
        let mut timeline = Timeline::new();
        for at in 0..2_000 {
            timeline.apply_live(IncomingMessage {
                stanza_id: Some(format!("unrelated-{at}")),
                sender: "bob".into(),
                body: Some("Unrelated".into()),
                ..IncomingMessage::default()
            });
        }
        timeline.apply_live(original());
        timeline.apply_live(batch("original", 0.9, 2));
        timeline.apply_history_page(
            vec![IncomingMessage {
                stanza_id: Some("older".into()),
                sender: "bob".into(),
                body: Some("Older".into()),
                ..IncomingMessage::default()
            }],
            Some("older".into()),
            true,
        );
        let diffs = timeline.apply_live(batch("original", 0.8, 3));
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], TimelineDiff::Set { index: 2_001, item }
            if item.scores.as_ref().unwrap().scores[0].probability.value() == 0.8));
        let target_rows = timeline.by_score_target.get(&target()).unwrap();
        assert_eq!(target_rows.len(), 1);
        assert_eq!(
            timeline.by_serial.get(target_rows.first().unwrap()),
            Some(&2_001)
        );
    }

    #[test]
    fn xep0422_ambiguous_target_clears_existing_scores_on_live_or_history_insert() {
        for archived in [false, true] {
            let mut timeline = Timeline::new();
            timeline.apply_live(original());
            timeline.apply_live(batch("original", 0.9, 2));
            let previous = timeline.items()[0].clone();
            // The adapter should never supply inconsistent identities, but
            // the store still fails closed if two rows claim one target.
            let mut duplicate = original();
            duplicate.stanza_id = Some("distinct-row".into());
            duplicate.origin_id = Some("distinct-origin".into());
            duplicate.reference_id = Some("distinct-row".into());
            let diffs = if archived {
                timeline.apply_history_page(vec![duplicate], Some("distinct-row".into()), true)
            } else {
                timeline.apply_live(duplicate)
            };
            assert_eq!(timeline.items().len(), 2);
            assert!(timeline.items().iter().all(|item| item.scores.is_none()));
            assert!(diffs.iter().any(|diff| matches!(diff, TimelineDiff::Set { item, .. } if item.key == previous.key && item.scores.is_none())));
            assert!(timeline.apply_live(batch("original", 0.99, 3)).is_empty());
            timeline.apply_live(correction());
            timeline.restore(&previous);
            assert!(timeline.items().iter().all(|item| item.scores.is_none()));
        }
    }

    #[test]
    fn xep0422_removal_hides_scores_and_failed_edit_restores_body_revision() {
        let mut timeline = Timeline::new();
        timeline.apply_live(original());
        timeline.apply_live(batch("original", 0.9, 2));
        let previous = timeline.items()[0].clone();
        timeline.apply_live(IncomingMessage {
            moderates_id: Some("original".into()),
            ..IncomingMessage::default()
        });
        assert_eq!(probability(&timeline), None);
        timeline.apply_live(batch("original", 0.99, 20));
        assert_eq!(probability(&timeline), None);
        timeline.restore(&previous);
        assert_eq!(probability(&timeline), Some(0.9));
        timeline.apply_live(correction());
        assert_eq!(probability(&timeline), None);
        timeline.restore(&previous);
        assert_eq!(probability(&timeline), Some(0.9));
    }
}
