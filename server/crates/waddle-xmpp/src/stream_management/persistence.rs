//! XEP-0198 stream-management durable persistence (issue #209, slice
//! (d), locked Q8 = B).
//!
//! The legacy [`super::session_registry::InMemorySmSessionRegistry`]
//! holds detached sessions and their unacked queues purely in memory:
//! a server crash loses every in-flight session and every unacked
//! stanza. Locked Q8 = B requires sessions and unacked queues to
//! survive a restart so SM resumption (XEP-0198 §5) works after crash
//! and so XEP-0198 §5 line 364 ("treat unacked as if never received")
//! has a durable home for promotion into `pending_delivery`.
//!
//! This module defines the typed persistence contract — the trait and
//! the typed value records — separated from the in-memory registry
//! so the migration can land incrementally:
//!
//! 1. **This commit** — trait + record types + `InMemoryPersistence`
//!    fake (no behavior change to the live system; structural
//!    groundwork only).
//! 2. **Follow-up** — `DatabaseSmPersistence` libSQL/Postgres impl in
//!    `waddle-server`, schema migration with `sm_sessions` and
//!    `sm_unacked` tables.
//! 3. **Follow-up** — `InMemorySmSessionRegistry::store_session` /
//!    `record_detached_outbound` etc. wired through the persistence
//!    storage so writes persist immediately. Restart restores the
//!    in-memory view from the persistence layer.
//! 4. **Follow-up** — graceful-shutdown drain that walks every live
//!    session, runs each unacked stanza through `classify_dm_intake`
//!    and `pending_delivery` insertion (Q6 promotion).

use crate::protocol::frame::MAX_FRAME_SIZE;
use crate::stream_management::{
    ProtectedSpaceReplyDescriptor, ProtectedSpaceReplyKind, SmIngressAppendKey,
    SmIngressFrameReceipt, MAX_PROTECTED_SPACE_REPLY_BYTES,
};
use crate::tombstone::TombstoneTarget;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine};
use jid::FullJid;
use thiserror::Error;
use xmpp_parsers::presence::Show;

use crate::auth::AuthenticatedPrincipalRef;
use crate::ownership::{ClaimEpoch, CurrentNodeIdentityGuard, Entity, NodeIdentity};
use crate::pending_delivery::SmSessionId;
use crate::postgres_identity::ClusterColocationIdentities;
use crate::Stanza;

/// Immutable ownership context authorizing one clustered SM persistence write.
///
/// The claim epoch is meaningful only together with the exact process
/// incarnation that obtained it. Keeping both values in one typed payload
/// prevents a cached pre-self-fence epoch from being paired with a newer
/// [`NodeIdentity`] after claim-epoch ABA.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmClaimFence {
    owner: NodeIdentity,
    epoch: ClaimEpoch,
}

impl SmClaimFence {
    pub fn new(owner: NodeIdentity, epoch: ClaimEpoch) -> Self {
        Self { owner, epoch }
    }

    pub fn owner(&self) -> &NodeIdentity {
        &self.owner
    }

    pub fn epoch(&self) -> ClaimEpoch {
        self.epoch
    }
}

/// Errors returned by [`SmPersistenceStorage`] implementations.
#[derive(Debug, Error)]
pub enum SmPersistenceError {
    #[error("SM persistence error: {0}")]
    Other(String),

    /// A live queue attempt already owns this new delivery. No snapshot or
    /// custody proof was written; callers must not interpret this as acceptance.
    #[error("live ingress delivery prevents a new detached allocation")]
    IngressDeliveryBlocked,

    /// An existing allocation disagrees with its durable sequence watermark or payload.
    #[error("promotion inventory sequence conflict for '{stream_id}' at {sequence}")]
    PromotionSequenceConflict {
        stream_id: SmSessionId,
        sequence: u32,
    },

    /// A row for an exact typed stream id exists but cannot be decoded into
    /// the typed persistence model. Recovery may quarantine that stream's
    /// durable session and unacked queue; ordinary backend errors must never
    /// take that destructive path.
    #[error("corrupt SM persistence for '{stream_id}': {detail}")]
    Corrupt {
        stream_id: SmSessionId,
        detail: String,
    },

    /// A clustered detached append had no captured ownership fence. The
    /// operation was rejected before checking durable ownership or writing any
    /// session, replay row or ingress proof. This does not establish a takeover:
    /// capture recovery can also fail when storage or authority is unavailable.
    #[error("missing captured ownership fence for entity '{entity}'")]
    MissingClaimFence { entity: Entity },

    /// A fenced write's own fencing check (ADR-0017 Phase 3 Slice 4,
    /// element 4) observed that this node no longer holds — or never
    /// acquired — the entity's ownership claim at the epoch it believed
    /// was current: the `SELECT ... FOR SHARE` issued inside the write's
    /// own transaction returned zero rows. The write was rolled back
    /// before touching `sm_sessions`/`sm_unacked`. Only ever returned by
    /// the Postgres-fenced implementation (the portable, single-node
    /// implementation has no fencing concept and never returns this).
    ///
    /// Distinct from [`Self::Other`] so Slice 5/6 callers can react to a
    /// lost claim (demote the in-memory session, attempt a fresh
    /// acquire/steal) instead of treating this as an opaque backend
    /// failure.
    ///
    /// `entity` is the typed [`Entity`] the fencing check was performed
    /// against (FIX 2, typed-payloads hard rule) — never a pre-formatted
    /// `String` key. [`Entity`] implements `Display` as the same
    /// `<entity_type_tag>:<id>` encoding the SQL layer uses, so this
    /// variant's `#[error]` text renders identically to the pre-FIX-2
    /// bare-`String` form; only the field's *type* changed.
    #[error(
        "fencing check failed: this node does not hold entity '{entity}' at the expected claim epoch"
    )]
    NotOwner { entity: Entity },

    /// Startup-time misconfiguration (ADR-0017 Phase 3 Slice 4 FIX 4): the
    /// physical SM storage identity does not match the clustering global
    /// database identity while clustering is enabled. The Postgres-fenced
    /// `SmPersistenceStorage`'s fencing `SELECT ... FOR SHARE` targets
    /// `clustering_claims`, which lives in the clustering global database —
    /// clustered SM persistence and the claims tables must be co-located in
    /// the same Postgres schema boundary, never two independently opened
    /// ones (a second, unrelated boundary might not even have a
    /// `clustering_claims` table to fence against).
    ///
    #[error(
        "clustered SM persistence must be co-located with the clustering claims tables: \
         physical SM storage identity does not match the clustering global database identity"
    )]
    ClusterColocationMismatch {
        identities: Box<ClusterColocationIdentities>,
    },

    /// Clustering cannot use the portable SQLite/in-memory persistence
    /// implementation because its ownership fence lives in Postgres.
    #[error("clustered SM persistence requires a PostgreSQL database")]
    ClusterRequiresPostgres,

    /// Startup wiring enabled clustering without providing the live claim
    /// store and rotating node identity needed by fenced persistence.
    #[error("clustered SM persistence started without live claim-store handles")]
    ClusterClaimHandlesUnavailable,

    /// The store was asked to create its schema through a database handle
    /// that holds no DDL privilege. The schema owner prepares it and the
    /// store attaches to it.
    #[error(
        "the SM persistence schema is prepared by the schema owner, not by this database handle"
    )]
    OwnerPreparationRequired,
}

/// A durable record of an XEP-0198 detached session.
///
/// Mirrors the fields on
/// [`super::session_registry::DetachedSession`] that need to survive a
/// process restart. Notable difference: timestamps are typed as
/// [`chrono::DateTime<chrono::Utc>`] rather than [`std::time::Instant`]
/// — `Instant` is process-relative and cannot be persisted.
///
/// `unacked_stanzas` carries the wire XML for each unacked stanza,
/// keyed by its server-side outbound sequence. Locked Q6c requires
/// each stanza to retain its **original receipt time** (so promoted
/// rows stamp `<delay/>` per XEP-0203 §4.1 + XEP-0198 §5 line 364);
/// the `original_receipt_at` field on
/// [`PersistedUnackedStanza`] holds that value.
#[derive(Debug, Clone)]
pub struct PersistedSession {
    pub stream_id: SmSessionId,
    pub user_id: String,
    pub jid: FullJid,
    pub occupancy_session: waddle_xmpp_core::OccupancySessionGeneration,
    pub inbound_count: u32,
    pub outbound_count: u32,
    pub last_acked: u32,
    pub replay_gap_through: Option<u32>,
    pub max_resume_time: Option<u32>,
    /// First durable detach time; refreshes preserve it until source deletion.
    /// Clustered Postgres stamps its own clock on the initial insert.
    pub detached_at: chrono::DateTime<chrono::Utc>,
    pub max_resume_duration: Duration,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub presence_available: bool,
    pub presence_show: Option<Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
    /// The resource's presence extension payloads (XEP-0115 `<c/>` caps,
    /// XEP-0319 `<idle/>`, arbitrary extensions) as last broadcast while
    /// available, so a session rehydrated from durable storage (restart or
    /// cross-node resume) relays the resource's own advertisements verbatim
    /// instead of coming back caps-less (issue #1206, follow-up to #1101 /
    /// #1103). Typed arbitrary-XML per the typed-payloads hard rule; the
    /// libSQL/Postgres backend serializes to a single TEXT column on write
    /// and parses back to typed on read.
    pub presence_payloads: Vec<minidom::Element>,
}

const PROTECTED_SPACE_STORAGE_PREFIX: &str = "sm-protected-space:1:";
const MAX_ENCODED_SPACE_DESCRIPTOR: usize = (MAX_PROTECTED_SPACE_REPLY_BYTES * 4).div_ceil(3);
const MAX_ENCODED_SPACE_REPLY: usize = (MAX_FRAME_SIZE * 4).div_ceil(3);

/// A typed stanza and the immutable wire representation of this replay entry.
///
/// SM persistence is part of the retransmission log: reserializing the typed
/// value can change the frame. Both fields are private so a caller cannot pair
/// arbitrary bytes with a different stanza. Parsing a stored frame retains its
/// original bytes alongside the typed value used by protocol logic.
#[derive(Debug, Clone)]
pub struct SmReplayPayload {
    stanza: Box<Stanza>,
    wire: String,
    protected_space_reply: Option<ProtectedSpaceReplyDescriptor>,
}

impl SmReplayPayload {
    /// Whether a retained value needs protected replay handling.
    pub fn is_protected_storage(value: &str) -> bool {
        value.starts_with("sm-protected-space:")
    }

    /// Parse one wire stanza. Protected storage envelopes are not wire XML.
    pub fn from_wire(wire: String) -> Result<Self, SmPersistenceError> {
        Self::parse_wire(wire, None, None)
    }

    /// Pair the response bytes with their original Space decision. Validate the
    /// exact result identity before typed stanza parsing can normalize it.
    pub fn from_protected_space_reply(
        wire: String,
        descriptor: ProtectedSpaceReplyDescriptor,
    ) -> Result<Self, SmPersistenceError> {
        if wire.len() > MAX_FRAME_SIZE {
            return Err(invalid_protected_space_reply());
        }
        Self::parse_wire(wire, None, Some(descriptor))
    }

    /// Decode the text stored in the replay row. Older rows retain their XML
    /// representation. The protected envelope is never usable as wire XML.
    pub fn from_storage(value: String) -> Result<Self, SmPersistenceError> {
        let Some(envelope) = value.strip_prefix(PROTECTED_SPACE_STORAGE_PREFIX) else {
            return Self::from_wire(value);
        };
        if envelope.len() > MAX_ENCODED_SPACE_DESCRIPTOR + 1 + MAX_ENCODED_SPACE_REPLY {
            return Err(invalid_protected_space_reply());
        }
        let (descriptor, wire) = envelope
            .split_once('.')
            .ok_or_else(invalid_protected_space_reply)?;
        if descriptor.len() > MAX_ENCODED_SPACE_DESCRIPTOR || wire.len() > MAX_ENCODED_SPACE_REPLY {
            return Err(invalid_protected_space_reply());
        }
        let descriptor_bytes = STANDARD_NO_PAD
            .decode(descriptor)
            .map_err(|_| invalid_protected_space_reply())?;
        let descriptor = ProtectedSpaceReplyDescriptor::decode(&descriptor_bytes)
            .map_err(|_| invalid_protected_space_reply())?;
        let wire = STANDARD_NO_PAD
            .decode(wire)
            .map_err(|_| invalid_protected_space_reply())?;
        let wire = String::from_utf8(wire).map_err(|_| invalid_protected_space_reply())?;
        Self::from_protected_space_reply(wire, descriptor)
    }

    /// Encode the full row value. Legacy XML decoders reject the entire
    /// protected envelope, which contains no raw reply XML.
    pub fn to_storage(&self) -> Cow<'_, str> {
        match &self.protected_space_reply {
            Some(descriptor) => Cow::Owned(format!(
                "{PROTECTED_SPACE_STORAGE_PREFIX}{}.{}",
                STANDARD_NO_PAD.encode(descriptor.encode()),
                STANDARD_NO_PAD.encode(self.wire.as_bytes()),
            )),
            None => Cow::Borrowed(self.wire()),
        }
    }

    pub fn protected_space_reply(&self) -> Option<&ProtectedSpaceReplyDescriptor> {
        self.protected_space_reply.as_ref()
    }

    /// Serialize a typed stanza that no transport has written yet. The bytes
    /// stored are the bytes a replay writes.
    pub fn from_stanza(stanza: &Stanza) -> Result<Self, SmPersistenceError> {
        let mut buffer = Vec::new();
        stanza
            .to_element()
            .write_to(&mut buffer)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let wire = String::from_utf8(buffer)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Self::from_wire_matching(wire, stanza)
    }

    /// Also require the captured XML to describe the supplied producer stanza.
    /// This is a structural comparison before typed parsing can discard fields.
    pub fn from_wire_matching(wire: String, expected: &Stanza) -> Result<Self, SmPersistenceError> {
        Self::parse_wire(wire, Some(expected), None)
    }

    fn parse_wire(
        wire: String,
        expected: Option<&Stanza>,
        protected_space_reply: Option<ProtectedSpaceReplyDescriptor>,
    ) -> Result<Self, SmPersistenceError> {
        let mut reader = std::io::Cursor::new(wire.as_bytes());
        let element = minidom::Element::from_reader(&mut reader)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        if reader.position() != wire.len() as u64 {
            return Err(SmPersistenceError::Other(
                "replay frame contains bytes after its stanza".to_owned(),
            ));
        }
        if expected.is_some_and(|expected| element != expected.to_element()) {
            return Err(SmPersistenceError::Other(
                "replay frame differs from its producer stanza".to_owned(),
            ));
        }
        if protected_space_reply
            .as_ref()
            .is_some_and(|descriptor| !matches_protected_space_reply(&element, descriptor))
        {
            return Err(invalid_protected_space_reply());
        }
        let stanza = match element.name() {
            "message" => xmpp_parsers::message::Message::try_from(element)
                .map(Stanza::Message)
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
            "iq" => xmpp_parsers::iq::Iq::try_from(element)
                .map(|iq| Stanza::Iq(Box::new(iq)))
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
            "presence" => xmpp_parsers::presence::Presence::try_from(element)
                .map(Stanza::Presence)
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
            other => {
                return Err(SmPersistenceError::Other(format!(
                    "unknown stanza element '{other}'"
                )))
            }
        };
        Ok(Self {
            stanza: Box::new(stanza),
            wire,
            protected_space_reply,
        })
    }

    /// Captured frame bytes for retransmission. A protected reply requires
    /// fresh admission before transfer. Storage writers use [Self::to_storage].
    pub fn wire(&self) -> &str {
        &self.wire
    }

    pub fn into_stanza(self) -> Stanza {
        *self.stanza
    }
}

impl From<Box<Stanza>> for SmReplayPayload {
    fn from(stanza: Box<Stanza>) -> Self {
        let mut bytes = Vec::new();
        stanza
            .to_element()
            .write_to(&mut bytes)
            .expect("serializing a stanza to a byte buffer cannot fail");
        let wire = String::from_utf8(bytes).expect("serialized stanza is UTF-8");
        Self {
            stanza,
            wire,
            protected_space_reply: None,
        }
    }
}

fn invalid_protected_space_reply() -> SmPersistenceError {
    SmPersistenceError::Other("invalid protected Space replay payload".to_owned())
}

fn matches_protected_space_reply(
    element: &minidom::Element,
    descriptor: &ProtectedSpaceReplyDescriptor,
) -> bool {
    if element.attr("from") != Some(descriptor.service().as_str())
        || element.attr("to") != Some(descriptor.recipient().as_str())
    {
        return false;
    }
    if descriptor.kind() == ProtectedSpaceReplyKind::BookmarkEvent {
        return matches_protected_bookmark_event(element, descriptor);
    }
    element.is("iq", "jabber:client")
        && element.attr("type") == Some("result")
        && element.attr("id") == Some(descriptor.id())
        && element.children().count() == 1
        && match descriptor.kind() {
            ProtectedSpaceReplyKind::DiscoInfo => element
                .get_child("query", "http://jabber.org/protocol/disco#info")
                .is_some_and(|query| query.attr("node") == Some(descriptor.node())),
            ProtectedSpaceReplyKind::DiscoItems => element
                .get_child("query", "http://jabber.org/protocol/disco#items")
                .filter(|query| query.attr("node") == Some(descriptor.node()))
                .is_some_and(|query| matches_protected_rooms(query, descriptor)),
            ProtectedSpaceReplyKind::PubSubItems => element
                .get_child("pubsub", crate::pubsub::NS_PUBSUB)
                .filter(|pubsub| pubsub.children().count() == 1)
                .and_then(|pubsub| pubsub.get_child("items", crate::pubsub::NS_PUBSUB))
                .filter(|items| items.attr("node") == Some(descriptor.node()))
                .is_some_and(|items| matches_protected_rooms(items, descriptor)),
            ProtectedSpaceReplyKind::Subscribe => {
                matches_protected_subscription(element, descriptor)
            }
            ProtectedSpaceReplyKind::OwnerAffiliations => {
                matches_protected_owner_affiliations(element, descriptor)
            }
            ProtectedSpaceReplyKind::BookmarkEvent => false,
        }
}

fn matches_protected_owner_affiliations(
    element: &minidom::Element,
    descriptor: &ProtectedSpaceReplyDescriptor,
) -> bool {
    use crate::pubsub::NS_PUBSUB_OWNER;

    // Match the enrolled scope's canonical grant-row limit.
    const MAX_AFFILIATIONS: usize = 200;

    if element.attrs().len() != 4 || !element.text().trim().is_empty() {
        return false;
    }
    let Some(affiliations) = element
        .get_child("pubsub", NS_PUBSUB_OWNER)
        .filter(|pubsub| {
            pubsub.attrs().is_empty()
                && pubsub.children().count() == 1
                && pubsub.text().trim().is_empty()
        })
        .and_then(|pubsub| pubsub.get_child("affiliations", NS_PUBSUB_OWNER))
        .filter(|affiliations| {
            affiliations.attrs().len() == 1
                && affiliations.attr("node") == Some(descriptor.node())
                && affiliations.text().trim().is_empty()
        })
    else {
        return false;
    };
    let mut subjects = HashSet::new();
    affiliations.children().enumerate().all(|(index, row)| {
        let Some(jid) = row.attr("jid") else {
            return false;
        };
        index < MAX_AFFILIATIONS
            && row.is("affiliation", NS_PUBSUB_OWNER)
            && row.attrs().len() == 2
            && row.children().next().is_none()
            && row.text().trim().is_empty()
            && matches!(
                row.attr("affiliation"),
                Some("owner" | "publisher" | "publish-only" | "member" | "outcast")
            )
            && jid
                .parse::<jid::BareJid>()
                .is_ok_and(|bare| bare.as_str() == jid)
            && subjects.insert(jid)
    })
}

/// A protected headline names one versioned bookmark mutation and no other data.
fn matches_protected_bookmark_event(
    element: &minidom::Element,
    descriptor: &ProtectedSpaceReplyDescriptor,
) -> bool {
    use crate::pubsub::NS_PUBSUB_EVENT;
    use crate::xep::xep0402::NS_BOOKMARKS2;

    let (Some(proof), [room]) = (descriptor.bookmark_event(), descriptor.room_dependencies())
    else {
        return false;
    };
    let id = (!descriptor.id().is_empty()).then_some(descriptor.id());
    if descriptor.subscription_id().is_none()
        || !element.is("message", "jabber:client")
        || element.attr("type") != Some("headline")
        || element.attr("id") != id
        || element.attrs().len() != 3 + usize::from(id.is_some())
        || element.children().count() != 1
        || !element.text().trim().is_empty()
    {
        return false;
    }
    let Some(items) = element
        .get_child("event", NS_PUBSUB_EVENT)
        .filter(|event| {
            event.attrs().is_empty()
                && event.children().count() == 1
                && event.text().trim().is_empty()
        })
        .and_then(|event| event.get_child("items", NS_PUBSUB_EVENT))
        .filter(|items| {
            items.attrs().len() == 1
                && items.attr("node") == Some(descriptor.node())
                && items.children().count() == 1
                && items.text().trim().is_empty()
        })
    else {
        return false;
    };
    let name = if proof.present() { "item" } else { "retract" };
    let Some(entry) = items.get_child(name, NS_PUBSUB_EVENT) else {
        return false;
    };
    if entry.attrs().len() != 1
        || entry.attr("id") != Some(room.room().as_str())
        || !entry.text().trim().is_empty()
    {
        return false;
    }
    let mut payloads = entry.children();
    match payloads.next() {
        None => true,
        Some(payload) => {
            proof.present() && payload.is("conference", NS_BOOKMARKS2) && payloads.next().is_none()
        }
    }
}

/// A local subscribe result confirms exactly one bare-JID subscription.
fn matches_protected_subscription(
    element: &minidom::Element,
    descriptor: &ProtectedSpaceReplyDescriptor,
) -> bool {
    let Some(subid) = descriptor.subscription_id() else {
        return false;
    };
    let bare_recipient = descriptor.recipient().to_bare();
    element
        .get_child("pubsub", crate::pubsub::NS_PUBSUB)
        .filter(|pubsub| pubsub.children().count() == 1 && pubsub.attrs().is_empty())
        .and_then(|pubsub| pubsub.get_child("subscription", crate::pubsub::NS_PUBSUB))
        .is_some_and(|subscription| {
            subscription.children().next().is_none()
                && subscription.text().trim().is_empty()
                && subscription.attrs().len() == 4
                && subscription.attr("node") == Some(descriptor.node())
                && subscription.attr("jid") == Some(bare_recipient.as_str())
                && subscription.attr("subid") == Some(subid.as_str())
                && subscription.attr("subscription") == Some("subscribed")
        })
}

/// Content replies cover exactly the listed managed rooms. Known image
/// items are owned by the Space and do not depend on room authority.
fn matches_protected_rooms(
    payload: &minidom::Element,
    descriptor: &ProtectedSpaceReplyDescriptor,
) -> bool {
    use crate::xep::xep0059::NS_RSM;
    use crate::xep::xep0503::{space_image_payload, SpaceImage};

    let Some(dependencies) = descriptor.content_dependencies() else {
        return false;
    };
    let mut rooms: HashSet<_> = dependencies
        .iter()
        .map(|room| room.room().as_str())
        .collect();
    let mut images = HashSet::new();
    let mut first = None;
    let mut last = None;
    let mut paging = None;
    for item in payload.children() {
        if descriptor.kind() == ProtectedSpaceReplyKind::DiscoItems && item.is("set", NS_RSM) {
            if paging.replace(item).is_some() {
                return false;
            }
            continue;
        }
        let Some((id, is_image)) = (match descriptor.kind() {
            ProtectedSpaceReplyKind::DiscoItems
                if item.is("item", "http://jabber.org/protocol/disco#items")
                    && item.attr("node").is_none() =>
            {
                match item.attr("jid") {
                    Some(jid) if jid == descriptor.service().as_str() => item
                        .attr("name")
                        .map(|id| (id, SpaceImage::from_item_id(id).is_some())),
                    jid => jid.map(|id| (id, false)),
                }
            }
            ProtectedSpaceReplyKind::PubSubItems if item.is("item", crate::pubsub::NS_PUBSUB) => {
                item.attr("id").and_then(|id| {
                    if SpaceImage::from_item_id(id).is_some() {
                        space_image_payload(item).ok().map(|_| (id, true))
                    } else {
                        (item.children().count() == 1
                            && item
                                .get_child("conference", crate::xep::xep0402::NS_BOOKMARKS2)
                                .is_some())
                        .then_some((id, false))
                    }
                })
            }
            _ => None,
        }) else {
            return false;
        };
        let new_item = if is_image {
            images.insert(id)
        } else {
            rooms.remove(id)
        };
        if !new_item {
            return false;
        }
        first.get_or_insert(id);
        last = Some(id);
    }
    rooms.is_empty() && paging.is_none_or(|set| matches_protected_cursors(set, first, last))
}

/// A page may name its own ends. Count, index and cursors for omitted rooms
/// would expose information beyond the retained decision.
fn matches_protected_cursors(
    set: &minidom::Element,
    first: Option<&str>,
    last: Option<&str>,
) -> bool {
    use crate::xep::xep0059::NS_RSM;

    if !set.attrs().is_empty() {
        return false;
    }
    let mut first_seen = false;
    let mut last_seen = false;
    for cursor in set.children() {
        let (expected, seen) = match (cursor.ns().as_str(), cursor.name()) {
            (NS_RSM, "first") => (first, &mut first_seen),
            (NS_RSM, "last") => (last, &mut last_seen),
            _ => return false,
        };
        if *seen
            || !cursor.attrs().is_empty()
            || cursor.children().next().is_some()
            || !expected.is_some_and(|expected| expected == cursor.text())
        {
            return false;
        }
        *seen = true;
    }
    true
}

impl AsRef<Stanza> for SmReplayPayload {
    fn as_ref(&self) -> &Stanza {
        &self.stanza
    }
}

impl std::ops::Deref for SmReplayPayload {
    type Target = Stanza;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

/// One row in the `sm_unacked` table — a stanza sent to a session
/// that has not yet been acknowledged.
#[derive(Debug, Clone)]
pub struct PersistedUnackedStanza {
    /// Origin claims for this exact frame; they do not grant settlement authority.
    pub provenance: super::SmFrameProvenance,
    /// Ingress obligations confirmed only after this ordered replay prefix is written.
    pub ingress_receipts: Vec<SmIngressFrameReceipt>,
    /// XEP-0198 stream-id of the owning session.
    pub stream_id: SmSessionId,
    /// Server-side outbound sequence number; ordered ascending.
    pub sequence: u32,
    /// Typed payload and the original bytes for SM retransmission.
    pub stanza: SmReplayPayload,
    /// Original receipt time at the server. Carried so that the Q6
    /// promotion path can stamp the `<delay/>` per XEP-0203 §4.1 +
    /// XEP-0198 §5 line 364 with the correct timestamp.
    pub original_receipt_at: chrono::DateTime<chrono::Utc>,
}

/// One bounded mutation of an existing detached replay queue.
///
/// Only the session's counters and replay-gap watermark are updated. The first
/// durable detach timestamp, resume window, presence and principal remain intact.
/// The new stanza is inserted before explicit evictions, so an entry immediately
/// evicted by the buffer still commits its ingress custody without a replay copy.
#[derive(Debug, Clone)]
pub struct PersistedDetachedAppend {
    pub session: PersistedSession,
    pub stanza: PersistedUnackedStanza,
    pub evicted_sequences: Vec<u32>,
    pub ingress_append: Option<PersistedIngressAppend>,
    /// Check canonical live-attempt/completion authority for a new delivery.
    pub new_delivery: bool,
}

impl PersistedDetachedAppend {
    /// Reject inconsistent queue/custody identities before any durable mutation.
    pub fn validate(&self) -> Result<(), SmPersistenceError> {
        if self.stanza.stream_id != self.session.stream_id {
            return Err(SmPersistenceError::Other(
                "detached append stanza belongs to another stream".into(),
            ));
        }
        if self.new_delivery && self.ingress_append.is_none() {
            return Err(SmPersistenceError::Other(
                "new delivery requires an ingress append".into(),
            ));
        }
        if let Some(append) = &self.ingress_append {
            if append.accepting_stream != self.session.stream_id
                || append.sequence != self.stanza.sequence
                || append.disposition != IngressCustodyDisposition::Pending
            {
                return Err(SmPersistenceError::Other(
                    "detached append custody does not match its pending queue entry".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Result of an atomic promotion-inventory append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionAppendOutcome {
    /// Payload and outbound watermark were committed together.
    Appended,
    /// The same payload is still present in the durable queue.
    AlreadyRecorded,
    /// The watermark covers this sequence and its payload was already retired.
    Retired,
}

/// A durable acknowledgement interval awaiting pending-delivery settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmAckWindow {
    pub from_exclusive: u32,
    pub to_inclusive: u32,
}

/// Durable proof that one ingress obligation allocated a queue entry.
///
/// An immutable ingress allocation and the payload whose custody it proves.
///
/// The replay queue is a bounded cache of this payload. Eviction, resume and
/// quarantine cannot destroy custody because neither owns this row's lifetime.
#[derive(Debug, Clone)]
pub struct PersistedIngressAppend {
    pub key: SmIngressAppendKey,
    pub accepting_stream: SmSessionId,
    pub sequence: u32,
    pub appended_at: chrono::DateTime<chrono::Utc>,
    pub payload: Stanza,
    pub original_receipt_at: chrono::DateTime<chrono::Utc>,
    pub disposition: IngressCustodyDisposition,
}

impl PartialEq for PersistedIngressAppend {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.accepting_stream == other.accepting_stream
            && self.sequence == other.sequence
            && self.appended_at == other.appended_at
            && self.payload.to_element() == other.payload.to_element()
            && self.original_receipt_at == other.original_receipt_at
            && self.disposition == other.disposition
    }
}

impl Eq for PersistedIngressAppend {}

/// Only positive delivery evidence can discharge durable custody. A missing
/// session or replay gap never constitutes acknowledgement or promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressCustodyDisposition {
    Pending,
    Acknowledged,
    Promoted,
    Tombstoned,
}

/// Result of a snapshot write that also claims an ingress obligation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyedSnapshotOutcome {
    /// Durable ingress completion superseded this new-delivery request. No SM
    /// queue allocation or snapshot mutation occurred and no stream owns it.
    ObligationAlreadyResolved,
    /// The snapshot and the ledger proof committed together.
    Committed,
    /// The obligation was already allocated, so nothing committed: neither the queue nor
    /// the session counters moved. Carries the stream that won the allocation.
    ObligationAlreadyAllocated { accepting_stream: SmSessionId },
}

/// Persistent storage contract for XEP-0198 SM session state.
///
/// Operations are async to allow libSQL / Postgres backends; the
/// in-memory implementation in this module is fully synchronous
/// internally but exposes the same async trait so handlers can call
/// it without conditionalizing on backend.
#[async_trait]
pub trait SmPersistenceStorage: Send + Sync {
    /// Insert or replace a session record. Called when a stream
    /// detaches and the session is moved into the resumable pool.
    /// Refreshes retain an existing source's first durable `detached_at` stamp.
    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError>;

    /// Atomically retire resume eligibility, advance the acknowledgement floor,
    /// and remove exactly the wrapping interval `(last_acked, acknowledged_h]`,
    /// discharging that interval's pending ingress allocations as acknowledged.
    /// Validate that interval against the canonical outbound count before any
    /// writes. A missing source is an error, never admission or recreation.
    /// Clustered stores check the immutable fence in this transaction and reuse
    /// supplied identity authority through commit without reacquiring its gate.
    /// Portable stores rely on the registry's claim checks and stream gate.
    async fn mark_session_nonresumable(
        &self,
        stream_id: &SmSessionId,
        acknowledged_h: u32,
        expected_fence: &SmClaimFence,
        authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError>;

    /// Advance the canonical ACK floor and journal the wrapping interval atomically,
    /// preserving resume eligibility. Requires an existing source and exact claim.
    async fn acknowledge_claimed_session(
        &self,
        stream_id: &SmSessionId,
        acknowledged_h: u32,
        expected_fence: &SmClaimFence,
        authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError>;

    /// Read outstanding ACK settlements without acquiring identity authority.
    async fn pending_acknowledgements(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<SmAckWindow>, SmPersistenceError>;

    /// Discharge exactly one durable window after its pending rows are deleted.
    /// Missing windows are idempotent; stale ownership must never discharge one.
    async fn confirm_acknowledgement(
        &self,
        stream_id: &SmSessionId,
        window: SmAckWindow,
        expected_fence: &SmClaimFence,
        authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError>;

    /// Read one durable liveness snapshot for orphaned pending-delivery cleanup.
    /// Include canonical sources, outstanding ACK settlements and, for clustered
    /// storage, all SM claims (including attached streams without a source).
    /// Reads must not acquire or renew ownership; errors must fail cleanup closed.
    async fn pending_claim_protected_streams(&self)
        -> Result<Vec<SmSessionId>, SmPersistenceError>;

    /// Promotion writes carry the captured owner/epoch through the actual
    /// transaction. Clustered stores must never replace it from a claim cache.
    async fn upsert_promotion_session(
        &self,
        session: PersistedSession,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.upsert_session(session).await
    }

    async fn store_promotion_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.store_session_atomic(session, unacked).await
    }

    async fn append_promotion_stanza_with_fence(
        &self,
        stanza: PersistedUnackedStanza,
        _expected_fence: &SmClaimFence,
    ) -> Result<PromotionAppendOutcome, SmPersistenceError> {
        self.append_promotion_stanza(stanza).await
    }

    async fn delete_promotion_session(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.delete_session(stream_id).await
    }

    async fn delete_promotion_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
        _expected_fence: &SmClaimFence,
    ) -> Result<u64, SmPersistenceError> {
        self.delete_unacked(stream_id, sequences).await
    }

    async fn record_promotion_failure_with_fence(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<u32, SmPersistenceError> {
        self.record_promotion_failure(stream_id).await
    }

    /// Look up a session by stream-id. Used on `<resume/>` to verify
    /// the previd is recognized and to rebuild the in-memory session.
    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError>;

    /// Delete a session and all its unacked stanzas. Called on
    /// successful `<resumed/>` (the session is now live again, no
    /// longer detached) and on session timeout.
    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError>;

    /// Delete a session while the caller already holds current-incarnation
    /// authority. Clustered implementations reuse this guard instead of
    /// reacquiring the writer-preferring identity gate; portable stores have
    /// no identity fence and use their ordinary atomic delete.
    async fn delete_session_with_authority(
        &self,
        stream_id: &SmSessionId,
        _authority: &CurrentNodeIdentityGuard,
    ) -> Result<(), SmPersistenceError> {
        self.delete_session(stream_id).await
    }

    /// Delete an undecodable session and its unacked queue as one durable
    /// quarantine operation. Clustered implementations bind the immutable
    /// owner/epoch context into the same transaction; portable implementations
    /// deliberately reuse their already-atomic `delete_session` path.
    async fn quarantine_session(
        &self,
        stream_id: &SmSessionId,
        _expected_fence: &SmClaimFence,
    ) -> Result<(), SmPersistenceError> {
        self.delete_session(stream_id).await
    }

    /// Append an outbound stanza to the unacked queue for the named
    /// session.
    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError>;

    /// Append a sequence to non-resumable promotion custody and advance its
    /// outbound watermark atomically. Repeating the same recorded allocation
    /// or a retired sequence is idempotent. A conflicting payload, a queue row
    /// ahead of the watermark, or a missing/resumable inventory is an error.
    /// Implementations must fence this transaction like the source SM stream.
    async fn append_promotion_stanza(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<PromotionAppendOutcome, SmPersistenceError>;

    /// Remove unacked entries up to and including `up_to_sequence`.
    ///
    /// This contract uses raw numeric `<=` comparison in both the in-memory
    /// and SQL implementations, so it is wrap-unsafe and deliberately does
    /// not use the Stream Management sequence comparator. It has no
    /// production callers; any future production caller must first make this
    /// operation wrap-aware before wiring it into an acknowledgement path.
    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError>;

    /// Remove the named unacked entries — exact `(stream_id,
    /// sequence)` primary-key matches — and return how many rows were
    /// deleted. Used by the XEP-0424/0425 tombstone scrub (issue
    /// #1145) so a retracted stanza cannot be rehydrated into a
    /// replay queue after a restart. Sequences that do not exist are
    /// ignored (idempotent).
    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError>;

    /// Read every unacked stanza for a session in sequence order.
    /// Used by `<resumed/>` to replay and by the Q6 promotion path to
    /// drain on session expiry.
    /// Delete replay entries selected by a tombstone and suppress their custody
    /// in the same transaction, including appends committed after the initial scrub.
    async fn delete_tombstoned_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError>;

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError>;

    /// Enumerate every persisted session whose `detached_at +
    /// max_resume_duration` is in the past. Used by the janitor that
    /// expires timed-out sessions and by the graceful-shutdown drain.
    /// Undecodable session rows are skipped with a warning and skipped-row
    /// count so corruption of one row cannot prevent other sessions expiring.
    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError>;

    /// Enumerate every currently-persisted session, regardless of
    /// expiry. Used by [`InMemorySmSessionRegistry`] on startup to
    /// rebuild the in-memory view from durable storage so an
    /// XEP-0198 `<resume previd='…'/>` finds sessions that detached
    /// before the most recent restart.
    /// Undecodable session rows are skipped with a warning and skipped-row
    /// count. Safety-sensitive queue scans use [`Self::list_all_session_ids`]
    /// instead so a corrupt session header cannot hide durable replay entries.
    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError>;

    /// Enumerate every persisted session ID without decoding its metadata.
    ///
    /// Tombstone snapshots and scrubs must inspect a session's durable queue
    /// even when its header cannot be decoded. Implementations whose bulk
    /// session listing skips corrupt rows MUST override this method to read
    /// IDs independently. Unreadable IDs and database failures must propagate
    /// as errors, never omissions.
    async fn list_all_session_ids(&self) -> Result<Vec<SmSessionId>, SmPersistenceError> {
        Ok(self
            .list_all_sessions()
            .await?
            .into_iter()
            .map(|session| session.stream_id)
            .collect())
    }

    /// Enumerate the persisted sessions for one EXACT full JID.
    ///
    /// Separate from [`Self::list_all_sessions`] because its caller is an
    /// eviction guard, not a startup rebuild: the #1803 resource-presence
    /// probe runs per roster-absent occupant, locally and on every cluster
    /// peer, inside a bounded fan-out budget. Answering it by loading and
    /// decoding the whole `sm_sessions` table costs
    /// `occupants x peers x stored sessions` decodes per maintenance pass.
    ///
    /// A row that matches the JID but cannot be decoded is an error, never an
    /// omission. A caller deciding whether to drop a durable obligation must
    /// read an undecodable row as "cannot tell", so implementations MUST NOT
    /// skip poison rows here even where a best-effort listing does.
    ///
    /// Default impl filters [`Self::list_all_sessions`] so in-memory backends
    /// and test fakes keep working unchanged; SQL backends override it with an
    /// exact `WHERE full_jid = ?` predicate over an index.
    async fn list_sessions_for_full_jid(
        &self,
        jid: &FullJid,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let mut rows = self.list_all_sessions().await?;
        rows.retain(|row| row.jid == *jid);
        Ok(rows)
    }

    /// Enumerate every persisted session AND its unacked queue in a
    /// single round-trip, instead of an N+1 (1 list_all_sessions + N
    /// list_unacked).
    ///
    /// **Best-effort semantics**: a session whose unacked queue
    /// fails to decode (corrupted XML, invalid timestamp, etc.) is
    /// SKIPPED with a debug log; other sessions still appear in the
    /// returned set, so a single poison-pill row cannot fail the whole
    /// scan. (Greptile/Copilot/Qodo P1 review on PR #405.)
    ///
    /// Default impl falls back to the N+1 sequence so in-memory
    /// backends keep working without implementing a JOIN; the
    /// libSQL/Postgres backend overrides with a single
    /// `SELECT … LEFT JOIN sm_unacked` query that applies the same
    /// poison-pill skip per-row.
    async fn list_all_sessions_with_unacked(
        &self,
    ) -> Result<Vec<(PersistedSession, Vec<PersistedUnackedStanza>)>, SmPersistenceError> {
        let sessions = self.list_all_sessions().await?;
        let mut out = Vec::with_capacity(sessions.len());
        for session in sessions {
            match self.list_unacked(&session.stream_id).await {
                Ok(unacked) => out.push((session, unacked)),
                Err(error) => {
                    tracing::debug!(
                        stream_id = %session.stream_id,
                        error = %error,
                        "list_all_sessions_with_unacked: skipping session whose unacked \
                         queue failed to decode (poison pill); other sessions continue"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Atomically write a session record + its complete unacked
    /// queue. Either every row commits or none does, so a panic /
    /// process crash mid-batch leaves the durable view consistent
    /// (no half-stored session whose row claims N unacked stanzas but
    /// the table only holds K < N). Implementations must treat the
    /// supplied queue as a full replacement for the stream's prior
    /// unacked rows; appending full snapshots would duplicate replay
    /// stanzas. Default impl falls back to delete + upsert + N
    /// appends without a transaction so backends that don't support
    /// nested ops keep working; the libSQL/Postgres backend overrides
    /// with a `BEGIN; … ; COMMIT;` block via `Database::begin`.
    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        self.delete_session(&stream_id).await?;
        self.upsert_session(session).await?;
        for entry in unacked {
            self.append_unacked(entry).await?;
        }
        Ok(())
    }

    /// Atomically append one stanza, remove only explicit buffer evictions, update
    /// counters/replay-gap state and optionally claim its ingress obligation.
    ///
    /// The source must already exist: this operation must never recreate a
    /// deleted session or extend its original resume window. A ledger conflict
    /// rolls back every mutation and returns the winning allocation. Clustered
    /// backends require the caller's captured fence, checked in this transaction.
    /// A missing capture returns [`SmPersistenceError::MissingClaimFence`]; a
    /// supplied fence rejected by ownership validation returns
    /// [`SmPersistenceError::NotOwner`]. Both reject without mutating durable state.
    /// Unsupported adapters fail closed rather than rewriting a full snapshot.
    async fn append_detached_stanza_atomic(
        &self,
        _append: PersistedDetachedAppend,
        _expected_fence: Option<&SmClaimFence>,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        Err(SmPersistenceError::Other(
            "incremental detached append is unsupported by this backend".into(),
        ))
    }

    /// Atomically persist a detached SM snapshot, complete unacked queue, and
    /// the authenticated principal reference that authorizes its later resume.
    ///
    /// Clustered implementations must bind this write to the same exact SM
    /// claim fence as the snapshot write. The principal is intentionally an
    /// explicit argument rather than registry state so cancellation cannot
    /// leave a separately staged authority reference behind.
    async fn store_session_atomic_with_principal(
        &self,
        principal: &AuthenticatedPrincipalRef,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError>;

    /// Return the authenticated principal reference durably paired with a
    /// detached session snapshot. Callers must resolve it against the auth
    /// authority before resuming; absence is not a local-state fallback.
    async fn get_session_principal(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<AuthenticatedPrincipalRef>, SmPersistenceError>;

    /// Atomically persist a detached snapshot, its complete unacked queue, and the ledger
    /// proof for the ingress obligation that allocated the queue's newest entry.
    ///
    /// The ledger insert is the exactly-once gate, and it is the *database constraint* that
    /// decides: a pre-check cannot serialize two callers, so both may arrive here believing
    /// the obligation is unallocated. On a uniqueness conflict an implementation MUST abort
    /// the entire write — leaving the prior queue, counters, replay-gap markers, original
    /// timestamps and resume expiry untouched — and report
    /// [`KeyedSnapshotOutcome::ObligationAlreadyAllocated`] with the winning stream. It must
    /// never catch the conflict and commit anyway, and must never report a conflict for any
    /// other constraint or backend failure: those are errors.
    ///
    /// There is deliberately no default implementation. The non-transactional
    /// delete-then-upsert-then-append fallback used by [`Self::store_session_atomic`] cannot
    /// provide this gate, and a backend that silently succeeded without the ledger write
    /// would reintroduce duplicate allocation. Backends fail closed by failing to compile.
    async fn store_session_atomic_with_ingress_append(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        append: PersistedIngressAppend,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError>;

    /// Allocate a new detached delivery only if no live attempt or durable
    /// completion owns it. Prior completion returns
    /// [`KeyedSnapshotOutcome::ObligationAlreadyResolved`] without changing the
    /// snapshot, queue, or allocation ledger.
    /// Database backends serialize this check with live claims on the canonical
    /// ingress row and retain that lock through snapshot/custody commit. Already
    /// accepted frames and detach drains use the ordinary append method instead.
    /// In-memory adapters have no live-attempt repository and delegate by default.
    async fn store_session_atomic_with_ingress_delivery(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        append: PersistedIngressAppend,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        self.store_session_atomic_with_ingress_append(session, unacked, append)
            .await
    }

    /// Atomically persist a detached snapshot, its principal, its complete unacked queue,
    /// and the ledger proof for every drained entry that discharges an ingress obligation
    /// (issue #1789: frames drained from a detaching socket before the session exists).
    ///
    /// Unlike [`Self::store_session_atomic_with_ingress_append`], a ledger conflict does
    /// **not** abort the write. Every entry already holds a sequence the connection
    /// counted, so dropping one would leave a stanza the client can never acknowledge.
    /// The conflicting proof is withheld instead — the entry stays, unkeyed — and its key
    /// is returned. An implementation MUST NOT let a conflict poison the transaction
    /// (`ON CONFLICT DO NOTHING`, not a caught constraint error), and MUST NOT withhold a
    /// proof for any other reason: those are errors.
    ///
    /// No default implementation, for the reason given on the single-append variant.
    async fn store_session_atomic_with_principal_and_ingress_appends(
        &self,
        principal: &AuthenticatedPrincipalRef,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        appends: Vec<PersistedIngressAppend>,
    ) -> Result<Vec<SmIngressAppendKey>, SmPersistenceError>;

    /// Look up durable proof for one ingress obligation, across every stream.
    ///
    /// Consulted before any session work, which is what makes resume and rebind safe: proof
    /// outlives the detached snapshot a resume deletes, and is found even when the resource
    /// is now bound to a different stream or has no detached session at all.
    async fn get_ingress_append(
        &self,
        key: &SmIngressAppendKey,
    ) -> Result<Option<PersistedIngressAppend>, SmPersistenceError>;

    /// Read all immutable allocations at a replay slot, including terminal rows.
    /// Callers also match payload and receipt time: wrapping counters alone do
    /// not identify the same historic delivery on a long-lived stream.
    async fn get_ingress_appends_for_sequence(
        &self,
        stream: &SmSessionId,
        sequence: u32,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError>;

    /// Enumerate durable custody independently of detached sessions and ingress
    /// settlement. The caller must establish current delivery ownership before
    /// recovering a row; a resumed live stream has no detached session row.
    async fn list_pending_ingress_appends(
        &self,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError>;

    /// Keyset pagination lets bounded recovery advance beyond protected rows.
    async fn list_pending_ingress_appends_after(
        &self,
        after: Option<&SmIngressAppendKey>,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError>;

    /// Discharge exactly the allocation whose acknowledgement or durable handoff
    /// the caller observed. The payload remains alongside the proof until GC.
    async fn complete_ingress_append(
        &self,
        key: &SmIngressAppendKey,
        accepting_stream: &SmSessionId,
        sequence: u32,
        disposition: IngressCustodyDisposition,
    ) -> Result<bool, SmPersistenceError>;

    /// Record an authenticated, validated XEP-0198 acknowledgement interval.
    async fn complete_ingress_appends_through(
        &self,
        stream: &SmSessionId,
        from_exclusive: u32,
        h: u32,
    ) -> Result<(), SmPersistenceError>;

    /// Suppress retained custody matching a retraction even after its replay
    /// cache has been retired. The cutoff excludes later reused wire ids.
    async fn scrub_ingress_custody(
        &self,
        target: &TombstoneTarget,
        through: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SmPersistenceError>;

    /// Atomically increment the persistent promotion-failure counter
    /// for `stream_id` and return the new value. Used by the SM-
    /// expiry janitor (issue #209 finding #14) to break runaway retry
    /// loops when Q6 promotion fails repeatedly for permanent reasons
    /// (disk full, schema corruption, etc.) — once the count crosses a
    /// threshold the caller dead-letters the durable row instead of
    /// preserving it for yet another retry.
    ///
    /// Default impl returns `Ok(0)` — in-memory backends don't track
    /// the counter durably, so the janitor's dead-letter path simply
    /// never trips for them. Production backends override with a
    /// `UPDATE … SET promotion_attempts = promotion_attempts + 1` plus
    /// a follow-up SELECT.
    async fn record_promotion_failure(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<u32, SmPersistenceError> {
        let _ = stream_id;
        Ok(0)
    }

    /// Evict any per-`stream_id` claim-fence cache entry this implementation
    /// keeps as a fencing side channel (ADR-0017 Phase 3 Slice 4's "Epoch
    /// side channel" design note; Slice 5 debt (a)).
    ///
    /// Called by `InMemorySmSessionRegistry` (`session_registry/claims.rs`)
    /// every time this node's `ClaimStore` claim for `stream_id` ends —
    /// `release_claim`/`complete_claim`/`complete_claim_if_resumable`'s
    /// terminal branches, attached terminal teardown, and
    /// `invalidate_sessions_for_jid`'s removal of a claimed session — so a
    /// cache keyed by stream_id never outlives the claim it caches the
    /// owner/epoch pair for. Default no-op: the portable
    /// (single-node, in-memory) implementation has no such cache (its
    /// `ClaimStore` is always `InProcessClaimStore`, which is cheap to call
    /// directly on every write); only the Postgres-fenced implementation
    /// (`waddle-server`'s `PostgresFencedSmPersistence`) overrides this to
    /// actually remove the cached cell, so a subsequent fenced write for the
    /// same stream_id after a fresh claim always re-derives its epoch rather
    /// than reusing one issued under a claim this node no longer holds.
    fn evict_claim_cache(&self, stream_id: &SmSessionId, expected_fence: &SmClaimFence) {
        let _ = (stream_id, expected_fence);
    }
}

/// Whether acknowledgement leaves its source resumable or retires it for promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmAcknowledgementMode {
    /// Preserve the session's existing resume lifetime.
    PreserveResumability,
    /// Disable further resumption while preserving the unacknowledged suffix.
    RetireForPromotion,
}

/// In-memory implementation suitable for tests and as the structural
/// fake that future libSQL/Postgres impls drop in for.
#[derive(Debug, Default)]
pub struct InMemorySmPersistence {
    inner: std::sync::Mutex<InMemoryState>,
}

#[derive(Debug, Default)]
struct InMemoryState {
    sessions: HashMap<SmSessionId, PersistedSession>,
    // Per-session unacked queue keyed by stream_id.
    unacked: HashMap<SmSessionId, Vec<PersistedUnackedStanza>>,
    // Non-secret authorization reference paired with the detached snapshot.
    principals: HashMap<SmSessionId, AuthenticatedPrincipalRef>,
    // Ingress append ledger, keyed by obligation and NOT by stream, so it survives the
    // snapshot deletion a resume performs and is shared across rebinds. A flat list keeps
    // the obligation key free of a `Hash` bound; only tests use this backend.
    ingress_appends: Vec<PersistedIngressAppend>,
    acknowledgement_windows: HashMap<SmSessionId, Vec<SmAckWindow>>,
}

impl InMemoryState {
    fn store_session(&mut self, mut session: PersistedSession) {
        if let Some(existing) = self.sessions.get(&session.stream_id) {
            session.detached_at = existing.detached_at;
        }
        self.sessions.insert(session.stream_id.clone(), session);
    }

    fn acknowledge(
        &mut self,
        stream_id: &SmSessionId,
        acknowledged_h: u32,
        mode: SmAcknowledgementMode,
    ) -> Result<(), SmPersistenceError> {
        let guard = self;
        let session = guard.sessions.get(stream_id).ok_or_else(|| {
            SmPersistenceError::Other("Expired SM promotion source is missing".to_owned())
        })?;
        let last_acked = session.last_acked;
        let acknowledged = acknowledged_h.wrapping_sub(last_acked);
        if acknowledged >= 0x8000_0000
            || acknowledged > session.outbound_count.wrapping_sub(last_acked)
        {
            return Err(SmPersistenceError::Other(
                "Expired SM acknowledgement exceeds canonical outbound count".to_owned(),
            ));
        }
        for append in &mut guard.ingress_appends {
            let distance = append.sequence.wrapping_sub(last_acked);
            if &append.accepting_stream == stream_id
                && distance > 0
                && distance <= acknowledged
                && append.disposition == IngressCustodyDisposition::Pending
            {
                append.disposition = IngressCustodyDisposition::Acknowledged;
            }
        }
        if let Some(queue) = guard.unacked.get_mut(stream_id) {
            queue.retain(|entry| {
                let distance = entry.sequence.wrapping_sub(last_acked);
                distance == 0 || distance > acknowledged
            });
        }
        let session = guard.sessions.get_mut(stream_id).ok_or_else(|| {
            SmPersistenceError::Other("Expired SM promotion source is missing".to_owned())
        })?;
        session.last_acked = acknowledged_h;
        if acknowledged > 0 {
            guard
                .acknowledgement_windows
                .entry(stream_id.clone())
                .or_default()
                .push(SmAckWindow {
                    from_exclusive: last_acked,
                    to_inclusive: acknowledged_h,
                });
        }
        if mode == SmAcknowledgementMode::RetireForPromotion {
            let session = guard.sessions.get_mut(stream_id).ok_or_else(|| {
                SmPersistenceError::Other("SM acknowledgement source is missing".to_owned())
            })?;
            session.max_resume_time = Some(0);
            session.max_resume_duration = Duration::ZERO;
        }
        Ok(())
    }
}

impl InMemorySmPersistence {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SmPersistenceStorage for InMemorySmPersistence {
    async fn mark_session_nonresumable(
        &self,
        stream_id: &SmSessionId,
        acknowledged_h: u32,
        _expected_fence: &SmClaimFence,
        _authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        guard.acknowledge(
            stream_id,
            acknowledged_h,
            SmAcknowledgementMode::RetireForPromotion,
        )?;
        Ok(())
    }

    async fn acknowledge_claimed_session(
        &self,
        stream_id: &SmSessionId,
        acknowledged_h: u32,
        _expected_fence: &SmClaimFence,
        _authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError> {
        self.inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
            .acknowledge(
                stream_id,
                acknowledged_h,
                SmAcknowledgementMode::PreserveResumability,
            )
    }

    async fn pending_acknowledgements(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<SmAckWindow>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard
            .acknowledgement_windows
            .get(stream_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn confirm_acknowledgement(
        &self,
        stream_id: &SmSessionId,
        window: SmAckWindow,
        _expected_fence: &SmClaimFence,
        _authority: Option<&CurrentNodeIdentityGuard>,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        if let Some(windows) = guard.acknowledgement_windows.get_mut(stream_id) {
            windows.retain(|existing| *existing != window);
            if windows.is_empty() {
                guard.acknowledgement_windows.remove(stream_id);
            }
        }
        Ok(())
    }

    async fn pending_claim_protected_streams(
        &self,
    ) -> Result<Vec<SmSessionId>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard
            .sessions
            .keys()
            .chain(guard.acknowledgement_windows.keys())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect())
    }

    async fn append_promotion_stanza(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<PromotionAppendOutcome, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let session = guard.sessions.get(&stanza.stream_id).ok_or_else(|| {
            SmPersistenceError::Other("Promotion inventory is missing".to_owned())
        })?;
        if session.max_resume_time != Some(0) || session.max_resume_duration != Duration::ZERO {
            return Err(SmPersistenceError::Other(
                "Promotion inventory is resumable".to_owned(),
            ));
        }
        let is_new = super::sequence::sequence_gt(stanza.sequence, session.outbound_count);
        let stream = stanza.stream_id.clone();
        let sequence = stanza.sequence;
        let queue = guard.unacked.entry(stream.clone()).or_default();
        if let Some(existing) = queue.iter().find(|entry| entry.sequence == sequence) {
            if is_new
                || existing.stanza.to_element() != stanza.stanza.to_element()
                || existing.original_receipt_at.timestamp_millis()
                    != stanza.original_receipt_at.timestamp_millis()
                || existing.ingress_receipts != stanza.ingress_receipts
                || existing.provenance != stanza.provenance
            {
                return Err(SmPersistenceError::PromotionSequenceConflict {
                    stream_id: stream,
                    sequence,
                });
            }
            return Ok(PromotionAppendOutcome::AlreadyRecorded);
        }
        if !is_new {
            return Ok(PromotionAppendOutcome::Retired);
        }
        queue.push(stanza);
        if let Some(session) = guard.sessions.get_mut(&stream) {
            session.outbound_count = sequence;
        }
        Ok(PromotionAppendOutcome::Appended)
    }

    async fn upsert_session(&self, session: PersistedSession) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        guard.store_session(session);
        Ok(())
    }

    async fn get_session(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(guard.sessions.get(stream_id).cloned())
    }

    async fn delete_session(&self, stream_id: &SmSessionId) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        if guard
            .acknowledgement_windows
            .get(stream_id)
            .is_some_and(|windows| !windows.is_empty())
        {
            return Err(SmPersistenceError::Other(
                "SM acknowledgement settlement is pending".to_owned(),
            ));
        }
        guard.sessions.remove(stream_id);
        guard.unacked.remove(stream_id);
        guard.principals.remove(stream_id);
        Ok(())
    }

    async fn append_unacked(
        &self,
        stanza: PersistedUnackedStanza,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        guard
            .unacked
            .entry(stanza.stream_id.clone())
            .or_default()
            .push(stanza);
        Ok(())
    }

    async fn ack_through(
        &self,
        stream_id: &SmSessionId,
        up_to_sequence: u32,
    ) -> Result<u64, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let queue = match guard.unacked.get_mut(stream_id) {
            Some(q) => q,
            None => return Ok(0),
        };
        let before = queue.len();
        queue.retain(|s| s.sequence > up_to_sequence);
        Ok((before - queue.len()) as u64)
    }

    async fn delete_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let queue = match guard.unacked.get_mut(stream_id) {
            Some(q) => q,
            None => return Ok(0),
        };
        let before = queue.len();
        queue.retain(|s| !sequences.contains(&s.sequence));
        Ok((before - queue.len()) as u64)
    }

    async fn delete_tombstoned_unacked(
        &self,
        stream_id: &SmSessionId,
        sequences: &[u32],
    ) -> Result<u64, SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let removed = if let Some(queue) = guard.unacked.get_mut(stream_id) {
            let before = queue.len();
            queue.retain(|entry| !sequences.contains(&entry.sequence));
            (before - queue.len()) as u64
        } else {
            0
        };
        for append in &mut guard.ingress_appends {
            if &append.accepting_stream == stream_id
                && sequences.contains(&append.sequence)
                && append.disposition == IngressCustodyDisposition::Pending
            {
                append.disposition = IngressCustodyDisposition::Tombstoned;
            }
        }
        Ok(removed)
    }

    async fn list_unacked(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Vec<PersistedUnackedStanza>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let mut rows = guard.unacked.get(stream_id).cloned().unwrap_or_default();
        if let Some(session) = guard.sessions.get(stream_id) {
            rows.sort_by_key(|row| row.sequence.wrapping_sub(session.last_acked));
        } else {
            // Rows without a session are orphaned; retain deterministic numeric
            // order because no acknowledgement floor remains to order by.
            rows.sort_by_key(|row| row.sequence);
        }
        Ok(rows)
    }

    async fn list_expired_sessions(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        let mut out = Vec::new();
        for session in guard.sessions.values() {
            let expires_at = session.detached_at
                + chrono::Duration::from_std(session.max_resume_duration)
                    .unwrap_or(chrono::Duration::seconds(0));
            if expires_at <= now {
                out.push(session.clone());
            }
        }
        Ok(out)
    }

    async fn list_all_sessions(&self) -> Result<Vec<PersistedSession>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|e| SmPersistenceError::Other(e.to_string()))?;
        Ok(guard.sessions.values().cloned().collect())
    }

    async fn store_session_atomic(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        let stream_id = session.stream_id.clone();
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        guard.store_session(session);
        guard.unacked.insert(stream_id, unacked);
        Ok(())
    }

    async fn append_detached_stanza_atomic(
        &self,
        append: PersistedDetachedAppend,
        _expected_fence: Option<&SmClaimFence>,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        append.validate()?;
        let stream_id = &append.session.stream_id;
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        if !guard.sessions.contains_key(stream_id) {
            return Err(SmPersistenceError::Other(
                "detached append source is missing".into(),
            ));
        }
        if let Some(proof) = &append.ingress_append {
            if let Some(existing) = guard
                .ingress_appends
                .iter()
                .find(|entry| entry.key == proof.key)
            {
                return Ok(KeyedSnapshotOutcome::ObligationAlreadyAllocated {
                    accepting_stream: existing.accepting_stream.clone(),
                });
            }
        }
        if guard.unacked.get(stream_id).is_some_and(|queue| {
            queue
                .iter()
                .any(|entry| entry.sequence == append.stanza.sequence)
        }) {
            return Err(SmPersistenceError::Other(
                "detached append sequence already exists".into(),
            ));
        }
        let session = guard
            .sessions
            .get_mut(stream_id)
            .ok_or_else(|| SmPersistenceError::Other("detached append source is missing".into()))?;
        session.inbound_count = append.session.inbound_count;
        session.outbound_count = append.session.outbound_count;
        session.last_acked = append.session.last_acked;
        session.replay_gap_through = append.session.replay_gap_through;
        let queue = guard.unacked.entry(stream_id.clone()).or_default();
        queue.push(append.stanza);
        queue.retain(|entry| !append.evicted_sequences.contains(&entry.sequence));
        if let Some(proof) = append.ingress_append {
            guard.ingress_appends.push(proof);
        }
        Ok(KeyedSnapshotOutcome::Committed)
    }

    async fn store_session_atomic_with_principal(
        &self,
        principal: &AuthenticatedPrincipalRef,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
    ) -> Result<(), SmPersistenceError> {
        self.store_session_atomic_with_principal_and_ingress_appends(
            principal,
            session,
            unacked,
            Vec::new(),
        )
        .await
        .map(drop)
    }

    async fn store_session_atomic_with_principal_and_ingress_appends(
        &self,
        principal: &AuthenticatedPrincipalRef,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        appends: Vec<PersistedIngressAppend>,
    ) -> Result<Vec<SmIngressAppendKey>, SmPersistenceError> {
        if appends
            .iter()
            .any(|append| append.disposition != IngressCustodyDisposition::Pending)
        {
            return Err(SmPersistenceError::Other(
                "new ingress custody must be pending".into(),
            ));
        }
        let stream_id = session.stream_id.clone();
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let mut withheld = Vec::new();
        for append in appends {
            let existing = guard
                .ingress_appends
                .iter()
                .position(|existing| existing.key == append.key);
            match existing {
                None => guard.ingress_appends.push(append),
                Some(_) => withheld.push(append.key),
            }
        }
        guard.store_session(session);
        guard.unacked.insert(stream_id.clone(), unacked);
        guard.principals.insert(stream_id, principal.clone());
        Ok(withheld)
    }

    async fn get_session_principal(
        &self,
        stream_id: &SmSessionId,
    ) -> Result<Option<AuthenticatedPrincipalRef>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard.principals.get(stream_id).cloned())
    }

    async fn store_session_atomic_with_ingress_append(
        &self,
        session: PersistedSession,
        unacked: Vec<PersistedUnackedStanza>,
        append: PersistedIngressAppend,
    ) -> Result<KeyedSnapshotOutcome, SmPersistenceError> {
        if append.disposition != IngressCustodyDisposition::Pending {
            return Err(SmPersistenceError::Other(
                "new ingress custody must be pending".into(),
            ));
        }
        let stream_id = session.stream_id.clone();
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        // The map insert stands in for the unique constraint: the obligation is claimed
        // here or not at all, and a loser mutates neither queue nor session.
        if let Some(index) = guard
            .ingress_appends
            .iter()
            .position(|existing| existing.key == append.key)
        {
            let existing = &guard.ingress_appends[index];
            return Ok(KeyedSnapshotOutcome::ObligationAlreadyAllocated {
                accepting_stream: existing.accepting_stream.clone(),
            });
        }
        guard.ingress_appends.push(append);
        guard.store_session(session);
        guard.unacked.insert(stream_id, unacked);
        Ok(KeyedSnapshotOutcome::Committed)
    }

    async fn get_ingress_append(
        &self,
        key: &SmIngressAppendKey,
    ) -> Result<Option<PersistedIngressAppend>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard
            .ingress_appends
            .iter()
            .find(|existing| &existing.key == key)
            .cloned())
    }
    async fn get_ingress_appends_for_sequence(
        &self,
        stream: &SmSessionId,
        sequence: u32,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        Ok(guard
            .ingress_appends
            .iter()
            .filter(|append| &append.accepting_stream == stream && append.sequence == sequence)
            .cloned()
            .collect())
    }

    async fn list_pending_ingress_appends(
        &self,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
        self.list_pending_ingress_appends_after(None, limit).await
    }

    async fn list_pending_ingress_appends_after(
        &self,
        after: Option<&SmIngressAppendKey>,
        limit: usize,
    ) -> Result<Vec<PersistedIngressAppend>, SmPersistenceError> {
        fn order(key: &SmIngressAppendKey) -> (String, i32, [u8; 32], String) {
            (
                key.message_key.to_storage().to_string(),
                key.kind.to_storage(),
                key.semantic_identity_hash,
                key.resource.to_string(),
            )
        }
        let guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let mut pending: Vec<_> = guard
            .ingress_appends
            .iter()
            .filter(|append| {
                append.disposition == IngressCustodyDisposition::Pending
                    && after.is_none_or(|after| order(&append.key) > order(after))
            })
            .cloned()
            .collect();
        pending.sort_by_key(|append| order(&append.key));
        pending.truncate(limit);
        Ok(pending)
    }

    async fn complete_ingress_append(
        &self,
        key: &SmIngressAppendKey,
        accepting_stream: &SmSessionId,
        sequence: u32,
        disposition: IngressCustodyDisposition,
    ) -> Result<bool, SmPersistenceError> {
        if disposition == IngressCustodyDisposition::Pending {
            return Err(SmPersistenceError::Other(
                "pending custody is not completion evidence".into(),
            ));
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let Some(append) = guard.ingress_appends.iter_mut().find(|append| {
            &append.key == key
                && &append.accepting_stream == accepting_stream
                && append.sequence == sequence
                && append.disposition == IngressCustodyDisposition::Pending
        }) else {
            return Ok(false);
        };
        append.disposition = disposition;
        Ok(true)
    }

    async fn complete_ingress_appends_through(
        &self,
        stream: &SmSessionId,
        from_exclusive: u32,
        h: u32,
    ) -> Result<(), SmPersistenceError> {
        let distance = h.wrapping_sub(from_exclusive);
        if distance >= 0x8000_0000 {
            return Err(SmPersistenceError::Other(
                "ambiguous acknowledgement interval".into(),
            ));
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        for append in &mut guard.ingress_appends {
            let offset = append.sequence.wrapping_sub(from_exclusive);
            if &append.accepting_stream == stream
                && offset > 0
                && offset <= distance
                && append.disposition == IngressCustodyDisposition::Pending
            {
                append.disposition = IngressCustodyDisposition::Acknowledged;
            }
        }
        Ok(())
    }

    async fn scrub_ingress_custody(
        &self,
        target: &TombstoneTarget,
        through: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SmPersistenceError> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        for append in &mut guard.ingress_appends {
            if append.disposition == IngressCustodyDisposition::Pending
                && append.original_receipt_at <= through
                && target.matches_message_element(&append.payload.to_element())
            {
                append.disposition = IngressCustodyDisposition::Tombstoned;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod protected_space_reply_storage_tests {
    use super::*;

    const REPLY: &str = "<iq xmlns='jabber:client' type='result' from='spaces.example.test' to='alice@example.test/dev' id='disco-1'><query xmlns='http://jabber.org/protocol/disco#info' node='docs'/></iq>";

    fn descriptor() -> ProtectedSpaceReplyDescriptor {
        ProtectedSpaceReplyDescriptor::new(
            "spaces.example.test".parse().expect("bare service JID"),
            "docs".to_owned(),
            "alice@example.test/dev".parse().expect("full client JID"),
            "disco-1".to_owned(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            1,
        )
        .expect("reply descriptor")
    }

    #[test]
    fn owner_affiliations_round_trip_strict_owner_payload() {
        let descriptor = descriptor().with_kind(ProtectedSpaceReplyKind::OwnerAffiliations);
        let wire = "<iq xmlns='jabber:client' type='result' from='spaces.example.test' to='alice@example.test/dev' id='disco-1'><pubsub xmlns='http://jabber.org/protocol/pubsub#owner'><affiliations node='docs'><affiliation jid='alice@example.test' affiliation='owner'/><affiliation jid='blocked@example.test' affiliation='outcast'/></affiliations></pubsub></iq>";
        let payload =
            SmReplayPayload::from_protected_space_reply(wire.to_owned(), descriptor.clone())
                .expect("owner affiliations");
        let stored = payload.to_storage().into_owned();
        assert!(SmReplayPayload::from_wire(stored.clone()).is_err());
        let restored =
            SmReplayPayload::from_storage(stored).expect("owner affiliations in storage");
        assert_eq!(restored.wire(), wire);
        assert_eq!(restored.protected_space_reply(), Some(&descriptor));

        for (current, replacement) in [
            ("pubsub#owner", "pubsub"),
            ("node='docs'", "node='other'"),
            ("affiliation='outcast'", "affiliation='none'"),
            ("blocked@example.test", "alice@example.test"),
            ("blocked@example.test", "blocked@example.test/device"),
            ("blocked@example.test", "Blocked@EXAMPLE.TEST"),
            (
                "affiliation='outcast'/>",
                "affiliation='outcast'><extra/></affiliation>",
            ),
            (
                "affiliation='outcast'/>",
                "affiliation='outcast' extra='yes'/>",
            ),
        ] {
            assert!(
                SmReplayPayload::from_protected_space_reply(
                    wire.replace(current, replacement),
                    descriptor.clone()
                )
                .is_err(),
                "accepted {replacement}"
            );
        }
        let excessive_rows = (0..201)
            .map(|index| {
                format!("<affiliation jid='member{index}@example.test' affiliation='member'/>")
            })
            .collect::<String>();
        let excessive = wire.replace(
            "<affiliation jid='blocked@example.test' affiliation='outcast'/>",
            &excessive_rows,
        );
        assert!(SmReplayPayload::from_protected_space_reply(excessive, descriptor).is_err());
    }

    #[test]
    fn typed_reply_envelopes_bind_operation_and_node() {
        let original = descriptor();
        assert_eq!(original.kind(), ProtectedSpaceReplyKind::DiscoInfo);
        assert_eq!(&original.encode()[..4], b"SPR\x01");
        let disco_items = REPLY.replace("disco#info", "disco#items");
        let pubsub_items = REPLY.replace(
            "<query xmlns='http://jabber.org/protocol/disco#info' node='docs'/>",
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='docs'/></pubsub>",
        );

        for (kind, wire, mismatched) in [
            (
                ProtectedSpaceReplyKind::DiscoItems,
                disco_items.as_str(),
                pubsub_items.as_str(),
            ),
            (
                ProtectedSpaceReplyKind::PubSubItems,
                pubsub_items.as_str(),
                disco_items.as_str(),
            ),
        ] {
            let missing = original.clone().with_kind(kind);
            assert!(ProtectedSpaceReplyDescriptor::decode(&missing.encode()).is_err());
            assert!(
                SmReplayPayload::from_protected_space_reply(wire.to_owned(), missing.clone())
                    .is_err()
            );
            let descriptor = missing
                .with_room_dependencies(Vec::new())
                .expect("explicit empty coverage");
            assert_eq!(&descriptor.encode()[..4], b"SPR\x02");
            let mut unknown = descriptor.encode();
            unknown[4] = 255;
            assert!(ProtectedSpaceReplyDescriptor::decode(&unknown).is_err());
            let reply =
                SmReplayPayload::from_protected_space_reply(wire.to_owned(), descriptor.clone())
                    .expect("matching protected reply");
            let restored = SmReplayPayload::from_storage(reply.to_storage().into_owned())
                .expect("typed envelope");
            assert_eq!(restored.wire(), wire);
            assert_eq!(restored.protected_space_reply(), Some(&descriptor));
            assert!(SmReplayPayload::from_protected_space_reply(
                mismatched.to_owned(),
                descriptor.clone()
            )
            .is_err());
            assert!(SmReplayPayload::from_protected_space_reply(
                wire.replace("node='docs'", "node='other'"),
                descriptor
            )
            .is_err());
        }
    }

    #[test]
    fn protected_content_requires_complete_unique_room_coverage() {
        use crate::stream_management::ProtectedSpaceRoomDependency;

        let room = "room@muc.example.test";
        let dependency = ProtectedSpaceRoomDependency::new(
            room.parse().expect("room JID"),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            8,
        )
        .expect("canonical room dependency");
        let disco_items = REPLY.replace("disco#info", "disco#items").replace(
            "node='docs'/>",
            &format!("node='docs'><item jid='{room}'/></query>"),
        );
        let pubsub_items = REPLY.replace(
            "<query xmlns='http://jabber.org/protocol/disco#info' node='docs'/>",
            &format!(
                "<pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='docs'><item id='{room}'><conference xmlns='urn:xmpp:bookmarks:1'/></item></items></pubsub>"
            ),
        );
        for (kind, wire) in [
            (ProtectedSpaceReplyKind::DiscoItems, disco_items.as_str()),
            (ProtectedSpaceReplyKind::PubSubItems, pubsub_items.as_str()),
        ] {
            let descriptor = descriptor().with_kind(kind);
            let empty = descriptor
                .clone()
                .with_room_dependencies(Vec::new())
                .expect("empty proof");
            assert!(SmReplayPayload::from_protected_space_reply(wire.to_owned(), empty).is_err());
            assert!(descriptor
                .clone()
                .with_room_dependencies(vec![dependency.clone(), dependency.clone()])
                .is_err());

            let full = descriptor
                .with_room_dependencies(vec![dependency.clone()])
                .expect("room proof");
            let result = SmReplayPayload::from_protected_space_reply(wire.to_owned(), full.clone())
                .expect("fully proved reply");
            let restored = SmReplayPayload::from_storage(result.to_storage().into_owned())
                .expect("retained reply");
            assert_eq!(restored.wire(), wire);
            assert_eq!(
                restored
                    .protected_space_reply()
                    .expect("descriptor")
                    .room_dependencies(),
                std::slice::from_ref(&dependency)
            );
            assert!(SmReplayPayload::from_protected_space_reply(
                wire.replace(room, "other@muc.example.test"),
                full
            )
            .is_err());
        }
    }

    #[test]
    fn protected_content_authorizes_space_images_and_only_returned_cursors() {
        use crate::xep::xep0503::SpaceImage;

        let avatar = SpaceImage::Avatar.item_id();
        let query = format!(
            "<query xmlns='http://jabber.org/protocol/disco#items' node='docs'><item jid='spaces.example.test' name='{avatar}'/><set xmlns='http://jabber.org/protocol/rsm'><first>{avatar}</first><last>{avatar}</last></set></query>"
        );
        let images = format!(
            "<pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='docs'><item id='{avatar}'><metadata xmlns='urn:xmpp:avatar:metadata'><info bytes='42' id='name' type='image/png'/></metadata></item></items></pubsub>"
        );
        for (kind, payload) in [
            (ProtectedSpaceReplyKind::DiscoItems, query),
            (ProtectedSpaceReplyKind::PubSubItems, images),
        ] {
            let descriptor = descriptor()
                .with_kind(kind)
                .with_room_dependencies(Vec::new())
                .expect("no room dependency for the Space image");
            let wire = REPLY.replace(
                "<query xmlns='http://jabber.org/protocol/disco#info' node='docs'/>",
                &payload,
            );
            assert!(
                SmReplayPayload::from_protected_space_reply(wire.clone(), descriptor.clone())
                    .is_ok()
            );
            assert!(SmReplayPayload::from_protected_space_reply(
                wire.replace(avatar, "unknown-metadata"),
                descriptor.clone()
            )
            .is_err());
            if kind == ProtectedSpaceReplyKind::DiscoItems {
                for invalid in [
                    wire.replace("<first>", "<first index='8'>"),
                    wire.replace("</set>", "<count>9</count></set>"),
                    wire.replace(&format!("<last>{avatar}"), "<last>omitted@muc.example.test"),
                ] {
                    assert!(SmReplayPayload::from_protected_space_reply(
                        invalid,
                        descriptor.clone()
                    )
                    .is_err());
                }
            } else {
                assert!(SmReplayPayload::from_protected_space_reply(
                    wire.replace("image/png", "text/plain"),
                    descriptor
                )
                .is_err());
            }
        }
    }

    #[test]
    fn subscribe_result_binds_the_full_resource_bare_account_and_chosen_subid() {
        use crate::pubsub::SubId;

        let subid = SubId::generate();
        let wire = REPLY.replace(
            "<query xmlns='http://jabber.org/protocol/disco#info' node='docs'/>",
            &format!(
                "<pubsub xmlns='http://jabber.org/protocol/pubsub'><subscription node='docs' jid='alice@example.test' subscription='subscribed' subid='{subid}'/></pubsub>"
            ),
        );
        let missing = descriptor().with_kind(ProtectedSpaceReplyKind::Subscribe);
        assert!(ProtectedSpaceReplyDescriptor::decode(&missing.encode()).is_err());
        assert!(
            SmReplayPayload::from_protected_space_reply(wire.clone(), missing.clone()).is_err()
        );
        assert!(missing
            .clone()
            .with_subscription(SubId::from_raw(""))
            .is_err());
        assert!(missing
            .clone()
            .with_subscription(SubId::from_raw("a".repeat(257)))
            .is_err());

        let descriptor = missing
            .with_subscription(subid.clone())
            .expect("exact subscription");
        let payload = SmReplayPayload::from_protected_space_reply(wire.clone(), descriptor.clone())
            .expect("matching subscribe result");
        let storage = payload.to_storage().into_owned();
        assert!(SmReplayPayload::from_wire(storage.clone()).is_err());
        let restored = SmReplayPayload::from_storage(storage).expect("subscribe envelope");
        assert_eq!(restored.protected_space_reply(), Some(&descriptor));
        assert_eq!(restored.wire(), wire);
        for (old, replacement) in [
            ("alice@example.test/dev", "alice@example.test/other"),
            ("jid='alice@example.test'", "jid='alice@example.test/dev'"),
            ("jid='alice@example.test'", "jid='bob@example.test'"),
            ("subscription='subscribed'", "subscription='pending'"),
            ("node='docs'", "node='other'"),
            ("id='disco-1'", "id='disco-2'"),
            ("spaces.example.test", "other.example.test"),
            (subid.as_str(), "another-subscription"),
        ] {
            assert!(SmReplayPayload::from_protected_space_reply(
                wire.replace(old, replacement),
                descriptor.clone()
            )
            .is_err());
        }
    }

    #[test]
    fn bookmark_event_round_trips_and_binds_each_wire_identity() {
        use crate::pubsub::SubId;
        use crate::stream_management::ProtectedSpaceRoomDependency;

        let room = "room@muc.example.test";
        let dependency = ProtectedSpaceRoomDependency::new(
            room.parse().expect("room"),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            9,
        )
        .expect("room dependency");
        let operation = uuid::Uuid::new_v4();
        let subid = SubId::generate();
        for (present, stanza_id, id_attribute, name, payload) in [
            (
                true,
                "event-1",
                " id='event-1'",
                "item",
                "<conference xmlns='urn:xmpp:bookmarks:1'/>",
            ),
            (false, "", "", "retract", ""),
        ] {
            let wire = format!(
                "<message xmlns='jabber:client' type='headline' from='spaces.example.test' to='alice@example.test/dev'{id_attribute}><event xmlns='http://jabber.org/protocol/pubsub#event'><items node='docs'><{name} id='{room}'>{payload}</{name}></items></event></message>"
            );
            let base = descriptor();
            let descriptor = ProtectedSpaceReplyDescriptor::new(
                base.service().clone(),
                base.node().to_owned(),
                base.recipient().clone(),
                stanza_id.to_owned(),
                base.lifecycle(),
                base.incarnation(),
                base.generation(),
            )
            .expect("event base")
            .with_bookmark_event(subid.clone(), operation, dependency.clone(), present)
            .expect("bookmark event proof");
            let parsed =
                SmReplayPayload::from_protected_space_reply(wire.clone(), descriptor.clone())
                    .expect("exact bookmark event");
            let stored = parsed.to_storage().into_owned();
            assert!(SmReplayPayload::from_wire(stored.clone()).is_err());
            let restored = SmReplayPayload::from_storage(stored).expect("stored event");
            assert_eq!(restored.wire(), wire);
            assert_eq!(restored.protected_space_reply(), Some(&descriptor));
            assert_eq!(descriptor.subscription_id(), Some(&subid));
            assert_eq!(
                descriptor
                    .bookmark_event()
                    .expect("mutation identity")
                    .operation(),
                operation
            );
            assert_eq!(
                descriptor
                    .bookmark_event()
                    .expect("mutation identity")
                    .present(),
                present
            );
            assert_eq!(
                descriptor.room_dependencies(),
                std::slice::from_ref(&dependency)
            );

            let mut bad_action = descriptor.encode();
            *bad_action.last_mut().expect("action byte") = 2;
            assert!(ProtectedSpaceReplyDescriptor::decode(&bad_action).is_err());
            let wrong_action = if present { "retract" } else { "item" };
            for changed in [
                wire.replace("headline", "chat"),
                wire.replace("node='docs'", "node='other'"),
                wire.replace("spaces.example.test", "other.example.test"),
                wire.replace("alice@example.test/dev", "alice@example.test/other"),
                wire.replace(room, "other@muc.example.test"),
                wire.replace(&format!("<{name} "), &format!("<{wrong_action} "))
                    .replace(&format!("</{name}>"), &format!("</{wrong_action}>")),
                wire.replace("</message>", "<body>extra data</body></message>"),
            ] {
                assert!(
                    SmReplayPayload::from_protected_space_reply(changed, descriptor.clone())
                        .is_err()
                );
            }
            let wrong_id = if present {
                wire.replace(" id='event-1'", "")
            } else {
                wire.replace(" type='headline'", " type='headline' id='unexpected'")
            };
            assert!(SmReplayPayload::from_protected_space_reply(wrong_id, descriptor).is_err());
        }
    }

    #[test]
    fn protected_reply_round_trips_with_no_legacy_wire_xml() {
        let descriptor = descriptor();
        let payload =
            SmReplayPayload::from_protected_space_reply(REPLY.to_owned(), descriptor.clone())
                .expect("protected payload");
        let stored = payload.to_storage().into_owned();
        assert!(stored.starts_with(PROTECTED_SPACE_STORAGE_PREFIX));
        assert!(!stored.contains('<'));
        assert!(SmReplayPayload::from_wire(stored.clone()).is_err());

        let restored = SmReplayPayload::from_storage(stored.clone()).expect("protected envelope");
        assert_eq!(restored.wire(), REPLY);
        assert_eq!(restored.protected_space_reply(), Some(&descriptor));
        assert_eq!(restored.to_storage(), stored);

        let ordinary = SmReplayPayload::from_storage(REPLY.to_owned()).expect("legacy XML");
        assert_eq!(ordinary.wire(), REPLY);
        assert_eq!(ordinary.protected_space_reply(), None);
        assert_eq!(ordinary.to_storage(), REPLY);
    }

    #[test]
    fn unknown_and_malformed_protected_storage_is_rejected() {
        let descriptor = descriptor();
        let encoded = STANDARD_NO_PAD.encode(descriptor.encode());
        let payload = SmReplayPayload::from_protected_space_reply(REPLY.to_owned(), descriptor)
            .expect("protected reply");
        let stored = payload.to_storage();
        for invalid in [
            stored.replacen("space:1:", "space:2:", 1),
            format!("{stored}.extra"),
            format!("{stored}="),
            format!("{PROTECTED_SPACE_STORAGE_PREFIX}{encoded}"),
            format!("{PROTECTED_SPACE_STORAGE_PREFIX}{encoded}.!"),
            format!(
                "{PROTECTED_SPACE_STORAGE_PREFIX}{encoded}.{}",
                "A".repeat(MAX_ENCODED_SPACE_REPLY + 1)
            ),
            format!(
                "{PROTECTED_SPACE_STORAGE_PREFIX}{encoded}.{}",
                STANDARD_NO_PAD.encode(REPLY.replace("node='docs'", "node='other'"))
            ),
        ] {
            assert!(SmReplayPayload::from_storage(invalid).is_err());
        }
    }

    #[test]
    fn protected_reply_identity_validates_stanza_shape_and_addresses() {
        let descriptor = descriptor();
        for (expected, wrong) in [
            ("jabber:client", "jabber:server"),
            ("type='result'", "type='get'"),
            ("disco-1", "disco-2"),
            ("spaces.example.test", "other.example.test"),
            ("alice@example.test/dev", "alice@example.test/other"),
            ("node='docs'", "node='other'"),
            ("disco#info", "disco#items"),
        ] {
            let wrong_reply = REPLY.replace(expected, wrong);
            assert!(
                SmReplayPayload::from_protected_space_reply(wrong_reply, descriptor.clone())
                    .is_err()
            );
        }
    }
}
