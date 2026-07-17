use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use jid::{BareJid, Jid};
use serde::{Deserialize, Serialize};
use xmpp_parsers::message::MessageType;

use crate::mam::stanza_id_filter::MamFilterStanzaId;
use crate::xep0201::ThreadInfo;
use crate::xep0359::{OriginId, StanzaId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichMessageId(pub String);

impl RichMessageId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RichText(pub String);

impl RichText {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReply {
    pub id: RichMessageId,
    pub to: Option<Jid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReference {
    pub ref_type: RichText,
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub uri: Option<RichText>,
    pub anchor: Option<RichText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedMention {
    pub begin: Option<u32>,
    pub end: Option<u32>,
    pub jid: Option<BareJid>,
    pub occupant_id: Option<RichText>,
    pub mentions: Option<RichText>,
    pub uri: Option<RichText>,
    pub active: bool,
    pub noping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedReactionSet {
    pub target_id: RichMessageId,
    pub emojis: Vec<RichText>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedRetraction {
    pub target_id: RichMessageId,
    pub stamp: Option<DateTime<Utc>>,
    pub retraction_id: Option<RichMessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedModeration {
    pub target_id: RichMessageId,
    pub moderated_by: Jid,
    pub stamp: Option<DateTime<Utc>>,
    pub reason: Option<RichText>,
}

/// A XEP-0424 tombstone replacing a retracted message in the archive.
///
/// When the optional `moderation` is `Some`, this is a XEP-0425
/// moderation tombstone whose `<retracted/>` element wraps a
/// `<moderated by/>` annotation; otherwise it is a plain XEP-0424
/// sender-initiated retraction tombstone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedTombstone {
    /// Stanza-id of the retraction message that produced the
    /// tombstone (for clients to correlate the tombstone to the
    /// retraction event that caused it). May be `None` for
    /// IQ-driven moderation tombstones whose request is not itself
    /// archived as a separate row.
    pub retraction_id: Option<RichMessageId>,
    /// XEP-0424 §"`<retracted/>` SHOULD include a 'stamp' attribute
    /// indicating the time at which the retraction took place."
    pub stamp: DateTime<Utc>,
    /// When set, this tombstone is the result of XEP-0425 moderation
    /// rather than a sender-initiated XEP-0424 retraction.
    pub moderation: Option<ArchivedModeration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArchivedRichPayload {
    Correction {
        replaces_id: RichMessageId,
    },
    Retraction(ArchivedRetraction),
    Moderation(ArchivedModeration),
    Reactions(ArchivedReactionSet),
    /// In-place tombstone produced by XEP-0424 retraction or
    /// XEP-0425 moderation. The original row's `body` and
    /// leak-prone fields (`thread`, `reply`, `stanza_xml`,
    /// mentions, ...) are cleared when this variant is set, per
    /// XEP-0424 §Tombstones / XEP-0425 §Tombstones: "any related
    /// elements which might leak information about the original
    /// message" must be replaced.
    Tombstone(ArchivedTombstone),
}

/// XEP-0421 occupant-id captured for an archived groupchat row.
///
/// Typed newtype (not a raw `String` field) per the typed-payloads
/// hard rule. Carried in the `rich_payload` projection so the
/// non-`stanza_xml` fallback reconstruction can re-emit the
/// `<occupant-id/>` element XEP-0421 Business Rules require on every
/// message sent by a MUC — including MAM history (#1268).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedOccupantId(String);

impl ArchivedOccupantId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// XEP-0313 §MUC Archives real-JID disclosure for an archived
/// groupchat row: "In the case of non-anonymous rooms … the archive
/// message will use extended message information in an `<x/>` element
/// qualified by the 'http://jabber.org/protocol/muc#user' namespace
/// and containing an `<item/>` child with a 'jid' attribute
/// specifying the occupant's full JID."
///
/// The room chain captures the sender's authority at dispatch time so
/// MAM replay (both the `stanza_xml` path and the typed fallback) can
/// reproduce the room-authored `<x/>` without re-resolving room state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivedMucSender {
    /// The sender's real JID (full JID for live occupants).
    pub jid: Jid,
    /// XEP-0045 affiliation at message time.
    pub affiliation: crate::types::Affiliation,
    /// XEP-0045 role at message time.
    pub role: crate::types::Role,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArchivedRichMessage {
    pub payload: Option<ArchivedRichPayload>,
    pub reply: Option<ArchivedReply>,
    pub references: Vec<ArchivedReference>,
    pub mentions: Vec<ArchivedMention>,
    /// Client-authored message subjects keyed by their wire `xml:lang`
    /// value (`""` is the default language).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub subjects: BTreeMap<String, String>,
    /// XEP-0421 occupant-id of the sender (groupchat rows only).
    /// `#[serde(default)]` keeps previously-serialized rich payloads
    /// decodable.
    #[serde(default)]
    pub occupant_id: Option<ArchivedOccupantId>,
    /// XEP-0313 §MUC Archives non-anonymous real-JID item
    /// (groupchat rows only).
    #[serde(default)]
    pub muc_sender: Option<ArchivedMucSender>,
}

impl ArchivedRichMessage {
    /// Return a clone with the server-derived MUC identity fields
    /// cleared. These fields ([`Self::occupant_id`],
    /// [`Self::muc_sender`]) are stamped by the room service per
    /// dispatch, not authored by the client — in particular
    /// `muc_sender.jid` carries the sender's *per-session* full JID
    /// (a fresh random resource each reconnect). They MUST be excluded
    /// from XEP-0359 origin-id retry-dedup comparisons: a client that
    /// resends the same origin-id from a fresh session is the same
    /// logical message even though its resource (and possibly its
    /// affiliation/role) changed. Comparing the identity fields would
    /// break dedup and duplicate the row in the archive.
    pub fn content_only(&self) -> Self {
        Self {
            occupant_id: None,
            muc_sender: None,
            ..self.clone()
        }
    }

    /// True when this payload carries no client-authored content
    /// (`payload` / `reply` / `references` / `mentions` / `subjects`) and no
    /// server-derived MUC identity (`occupant_id` / `muc_sender`).
    pub fn is_empty(&self) -> bool {
        self.payload.is_none()
            && self.reply.is_none()
            && self.references.is_empty()
            && self.mentions.is_empty()
            && self.subjects.is_empty()
            && self.occupant_id.is_none()
            && self.muc_sender.is_none()
    }

    /// The content-only projection used for XEP-0359 origin-id retry
    /// dedup, normalized so an *empty* projection is `None` rather than
    /// `Some(default)`. This makes "a row whose only rich content was
    /// the server-stamped occupant-id / real-JID" compare equal to a
    /// row that carried no `rich_payload` at all — the two are the same
    /// logical message for dedup purposes.
    pub fn dedup_content(&self) -> Option<Self> {
        let content = self.content_only();
        (!content.is_empty()).then_some(content)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadId(String);

impl ThreadId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

/// Archived message metadata.
///
/// `Serialize`/`Deserialize` are deliberately not derived: the
/// canonical archive row format is the SQL columns + the
/// JSON-encoded `rich_payload` column ([`ArchivedRichMessage`]),
/// not the whole struct. Embedded typed values
/// ([`ThreadInfo`], [`StanzaId`], [`OriginId`], [`ArchivedReply`])
/// keep their derives because they appear inside the JSON-encoded
/// payload column. Keeping `ArchivedMessage` itself non-serde keeps
/// the boundary between "row shape that touches storage" and
/// "rich payload blob that is JSON in the row" explicit.
#[derive(Debug, Clone)]
pub struct ArchivedMessage {
    /// Unique message ID.
    pub id: String,
    /// Timestamp when the message was received.
    pub timestamp: DateTime<Utc>,
    /// Sender JID.
    ///
    /// Typed as `jid::Jid` (not `String`) per the typed-payloads hard
    /// rule. Carries either a bare or full JID — the projection
    /// preserves the wire form (full JID for groupchat occupants,
    /// bare/full as appropriate for 1:1) so replay round-trips
    /// resource and domain components without lossy reparse.
    pub from: Jid,
    /// Recipient JID (room JID for MUC, or contact bare JID for 1:1).
    ///
    /// Typed as `jid::Jid` for the same reason as [`Self::from`]. The
    /// archive write site supplies the canonical addressing tuple
    /// upstream of this struct.
    pub to: Jid,
    /// Message body, preserving the wire-fidelity distinction between
    /// "no `<body>` element on the wire" (`None`) and "an empty
    /// `<body></body>` element" (`Some("")`).
    ///
    /// RFC 6121 §5.2.3 makes `<body>` optional: subject-only,
    /// reaction-only, and other annotation-only messages may omit it
    /// entirely. Earlier denormalizations collapsed both cases to the
    /// empty string, so consumers reading this field directly (rather
    /// than re-parsing `stanza_xml`) saw a misleading "empty body" for
    /// stanzas that had no `<body>` element at all. Preserving the
    /// distinction restores XEP-0313 §3 archive fidelity for the
    /// denormalized projection.
    pub body: Option<String>,
    /// XEP-0359 stanza id captured for the archived row.
    ///
    /// Typed as [`xep0359::StanzaId`] (`{ id, by }`) so the
    /// `by` attribute (REQUIRED per XEP-0359 §3) is structurally
    /// inseparable from the id value. The `by` JID is reconstructed
    /// from the storage row's archive JID at decode time per the
    /// locked Q4 design — there is no separate `by` column in the
    /// SQL schema.
    ///
    /// [`xep0359::StanzaId`]: crate::xep0359::StanzaId
    pub stanza_id: Option<StanzaId>,
    /// XEP-0201 thread reference (RFC 6121 `<thread/>` id plus
    /// optional nested-thread `parent` attribute).
    ///
    /// Collapsed from the previous `thread_id: Option<String>` and
    /// `parent_thread_id: Option<ThreadId>` pair into a single typed
    /// field: `thread.id` is the RFC 6121 thread id and
    /// `thread.parent` is the optional XEP-0201 nested-thread parent.
    /// Modelling them together makes the "parent without id" invalid
    /// state unrepresentable (you cannot construct
    /// [`ThreadInfo`] with a parent and no id), aligning with the
    /// typed-payloads hard rule.
    ///
    /// Cleared on XEP-0424 / XEP-0425 tombstones — see the
    /// [`ArchivedRichPayload::Tombstone`] doc comment for the full
    /// list of leak-prone fields.
    ///
    /// Storage layout is unchanged: the SQL schema still has two
    /// columns (`thread_id` and `parent_thread_id`); encode splits
    /// this struct into the two columns and decode combines them.
    pub thread: Option<ThreadInfo>,
    /// XEP-0461 reply reference: the id of the replied-to message and
    /// the optional original sender JID (`<reply id='X' to='Y'/>`).
    ///
    /// Collapsed from the previous `reply_to_id: Option<String>` and
    /// `reply_to_jid: Option<String>` pair into a single typed
    /// [`ArchivedReply`] field. Modelling them together makes the
    /// "reply target sender without reply target id" invalid state
    /// unrepresentable (you cannot construct [`ArchivedReply`] with a
    /// `to` JID and no id), aligning with the typed-payloads hard rule
    /// and matching the canonical typed shape already used inside
    /// [`ArchivedRichMessage::reply`].
    ///
    /// Cleared on XEP-0424 / XEP-0425 tombstones — see the
    /// [`ArchivedRichPayload::Tombstone`] doc comment for the full
    /// list of leak-prone fields.
    ///
    /// Storage layout is unchanged: the SQL schema still has two
    /// columns (`reply_to_id` and `reply_to_jid`) plus the
    /// `idx_mam_room_reply_to` index; encode splits this struct into
    /// the two columns and decode combines them.
    pub reply: Option<ArchivedReply>,
    /// XEP-0359 origin-id supplied by client.
    ///
    /// Typed as [`xep0359::OriginId`] for symmetry with
    /// [`Self::stanza_id`]. The newtype carries only the id value
    /// (XEP-0359 origin-ids have no `by` attribute).
    ///
    /// [`xep0359::OriginId`]: crate::xep0359::OriginId
    pub origin_id: Option<OriginId>,
    /// Wire `<message type='…'/>` attribute, typed exactly per
    /// RFC 6121 §5.2.2 (the closed set: `chat`, `error`, `groupchat`,
    /// `headline`, `normal`).
    ///
    /// Pre-#228 this was `String` with a `default_message_type() =
    /// "chat"` serde default. The string-typed shape papered over two
    /// problems: the lossy `mam_message_type(&MessageType) -> String`
    /// stringifier in the projection round-tripped the wire-typed
    /// value through a string back into a string-only column, and the
    /// `"chat"` serde default contradicted RFC 6121 §5.2.2 ("If
    /// absent, the message is implicitly of type `normal`."). Typing
    /// this field as [`MessageType`] propagates the wire-parsed value
    /// directly and makes [`MessageType::default()`] == [`Normal`]
    /// the source of the absent-type semantics — pinned by the MAM
    /// replay tests.
    ///
    /// [`Normal`]: xmpp_parsers::message::MessageType::Normal
    pub message_type: MessageType,
    /// Preserved full stanza XML for faithful replay of archived timeline events.
    pub stanza_xml: Option<String>,
    /// Typed rich-message payload and annotations used to reconstruct XMPP payloads.
    pub rich: Option<ArchivedRichMessage>,
    /// Per-XEP-0308 §3 occupancy generation for the sender's MUC nickname
    /// at archive-write time. Only set for `groupchat` rows; `None`
    /// otherwise. Used to disallow corrections across leave/rejoin
    /// cycles — the correction handler refuses if the room's current
    /// generation for the same nickname has advanced.
    pub nickname_generation: Option<u64>,
}

impl ArchivedMessage {
    /// Test-only constructor that stamps `timestamp = Utc::now()` and
    /// fills every other optional field with its no-op default. Pulled
    /// in to replace `..Default::default()` ergonomics in test fixtures
    /// after the `Default` impl was dropped (no sensible default for
    /// [`Jid`]).
    ///
    /// `from` and `to` are mandatory because they have no safe
    /// fallback — the previous `Default::default()` returned an empty
    /// `String` which silently bypassed JID validity. Callers must
    /// supply real typed JIDs.
    ///
    /// Exported (`pub`, `#[doc(hidden)]`) so test fixtures in dependent
    /// crates (e.g. `waddle-xmpp`, `waddle-server`) can use it. Gated
    /// behind `cfg(test)` for in-crate tests and the `test-utils`
    /// Cargo feature for cross-crate consumers — production builds do
    /// not pull in this fixture constructor.
    #[cfg(any(test, feature = "test-utils"))]
    #[doc(hidden)]
    pub fn for_test(from: Jid, to: Jid) -> Self {
        Self {
            id: String::new(),
            timestamp: Utc::now(),
            from,
            to,
            body: None,
            stanza_id: None,
            thread: None,
            reply: None,
            origin_id: None,
            // RFC 6121 §5.2.2: absent type defaults to `normal`.
            // Identical to `MessageType::default()` — written
            // explicitly here to make the conformance contract
            // visible at the test-fixture site.
            message_type: MessageType::Normal,
            stanza_xml: None,
            rich: None,
            nickname_generation: None,
        }
    }
}

/// MAM query parameters.
#[derive(Debug, Clone, Default)]
pub struct MamQuery {
    /// Start time filter.
    pub start: Option<DateTime<Utc>>,
    /// End time filter.
    pub end: Option<DateTime<Utc>>,
    /// Filter by sender or recipient JID per XEP-0313 §4.1.5 `with`
    /// field. Typed as `jid::Jid` (not `String`) per the typed-payloads
    /// hard rule; parsing happens once at the IQ-form parse boundary
    /// inside the MAM data form parser and a malformed value is rejected as
    /// `bad-request` rather than silently substituted.
    pub with: Option<Jid>,
    /// Filter by Waddle thread root id.
    pub thread_id: Option<ThreadId>,
    /// XEP-0431 full-text search terms.
    pub fulltext: Option<RichText>,
    /// Maximum results to return.
    pub max: Option<u32>,
    /// Extended MAM filter: only messages before this archive ID.
    pub filter_before_id: Option<String>,
    /// Extended MAM filter: only messages after this archive ID.
    pub filter_after_id: Option<String>,
    /// Extended MAM filter: only these archive IDs.
    pub ids: Vec<String>,
    /// Waddle-specific MAM stanza-id filter (XEP-0313 §4.2 + XEP-0068):
    /// only these XEP-0359 stanza-ids. Form-field var is
    /// `{urn:waddle:mam-stanza-id:0}stanza-id` (see `STANZA_ID_FILTER_FIELD`).
    ///
    /// Distinct from `ids` (extended-MAM archive ids). The chat client
    /// uses this to materialize pinned messages by their pin
    /// `target_stanza_id` without first round-tripping for archive ids.
    pub stanza_ids: Vec<MamFilterStanzaId>,
    /// RSM pagination cursor: before this ID.
    pub before_id: Option<String>,
    /// RSM pagination cursor: after this ID.
    pub after_id: Option<String>,
}

/// MAM query result.
#[derive(Debug, Clone)]
pub struct MamResult {
    /// Retrieved messages.
    pub messages: Vec<ArchivedMessage>,
    /// Whether there are more messages available.
    pub complete: bool,
    /// First message ID in the result set.
    pub first_id: Option<String>,
    /// Last message ID in the result set.
    pub last_id: Option<String>,
    /// Total count (if available).
    pub count: Option<u32>,
}
