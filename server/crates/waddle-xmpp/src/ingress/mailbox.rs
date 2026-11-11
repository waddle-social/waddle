//! Pure value types for the durable ordered mailbox (#1986, #2118, #1948).
//!
//! Nothing here performs I/O, owns a schema or is wired into a runtime path.
//! The types fix the vocabulary the durable lane, its allocator and its
//! acceptance and consumption transactions share:
//!
//! - a stable logical lane identity with a canonical, versioned encoding and a
//!   versioned digest for indexing, with claim epochs and node incarnation
//!   carried beside the identity as fence values;
//! - a gap-free, nonwrapping lane sequence and its exclusive consumption
//!   cursor;
//! - the owner-independent operation key of a remote room acceptance;
//! - the closed disposition state machine of a mailbox entry;
//! - a pure admission check for pending-entry caps.

use std::fmt;

use jid::{BareJid, FullJid};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::ingress::MessageKey;
use crate::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity, ENTITY_ID_MAX_LEN};
use crate::pending_delivery::SmSessionId;

/// Ordered lane within one room's traffic.
///
/// XEP-0045 presence and messages share one lane because a join must precede
/// the occupant's messages. Muji signaling orders only against itself, so it
/// has its own lane and cannot stall, or be stalled by, room traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxRoomLane {
    MucStanza,
    MujiSignaling,
}

/// Logical origin of a lane: the party whose submissions the lane orders.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MailboxLaneOrigin {
    SmSession(SmSessionId),
    Entity(Entity),
}

/// Logical recipient of a lane. Bare-JID, full-JID and room recipients are
/// distinct lanes even when their JIDs coincide.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MailboxLaneRecipient {
    BareJid(BareJid),
    FullJid(FullJid),
    Room {
        room: BareJid,
        lane: MailboxRoomLane,
    },
}

/// Stable logical identity of one ordered lane.
///
/// Ownership fences are deliberately absent: a successor owner resumes the
/// same lane and sequence space, and proves its authority with a
/// [`MailboxLaneFence`] at each mutation instead.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailboxLaneIdentity {
    pub origin: MailboxLaneOrigin,
    pub recipient: MailboxLaneRecipient,
}

/// Authority a mutation of a lane must hold. Carried beside the identity and
/// compared at the mutation boundary; never an input to the lane's encoding
/// or digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxLaneFence {
    /// Claim epoch of the lane's origin.
    pub origin_epoch: ClaimEpoch,
    /// Claim epoch of the lane's recipient.
    pub target_epoch: ClaimEpoch,
    /// Node and incarnation holding the recipient claim.
    pub target_owner: NodeIdentity,
}

/// A mutation presented a fence other than the one currently on file.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("mailbox lane fence is stale")]
pub struct StaleMailboxLaneFence;

impl MailboxLaneFence {
    /// Accept `presented` only when it is exactly this current fence. A newer
    /// or older epoch, or another node incarnation, is refused alike: the
    /// caller must re-read its claim rather than guess an ordering.
    pub fn verify(&self, presented: &Self) -> Result<(), StaleMailboxLaneFence> {
        if self == presented {
            Ok(())
        } else {
            Err(StaleMailboxLaneFence)
        }
    }
}

/// Version of the canonical lane-identity encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxLaneEncodingVersion {
    V1,
}

impl MailboxLaneEncodingVersion {
    pub const CURRENT: Self = Self::V1;

    pub fn to_storage(self) -> u8 {
        match self {
            Self::V1 => 1,
        }
    }

    pub fn from_storage(value: u8) -> Result<Self, MailboxLaneCodecError> {
        match value {
            1 => Ok(Self::V1),
            _ => Err(MailboxLaneCodecError::UnsupportedEncodingVersion { value }),
        }
    }
}

/// Version of the lane digest derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxLaneDigestVersion {
    V1,
}

impl MailboxLaneDigestVersion {
    pub const CURRENT: Self = Self::V1;

    pub fn to_storage(self) -> u8 {
        match self {
            Self::V1 => 1,
        }
    }

    pub fn from_storage(value: u8) -> Result<Self, MailboxLaneCodecError> {
        match value {
            1 => Ok(Self::V1),
            _ => Err(MailboxLaneCodecError::UnsupportedDigestVersion { value }),
        }
    }
}

/// Failure to decode a stored lane identity or digest.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MailboxLaneCodecError {
    #[error("unsupported mailbox lane encoding version: {value}")]
    UnsupportedEncodingVersion { value: u8 },
    #[error("unsupported mailbox lane digest version: {value}")]
    UnsupportedDigestVersion { value: u8 },
    #[error("mailbox lane encoding ends before its declared content")]
    Truncated,
    #[error("mailbox lane encoding has {count} bytes after its content")]
    TrailingBytes { count: usize },
    #[error("unknown mailbox lane {field} discriminator: {value}")]
    UnknownDiscriminator { field: LaneField, value: u8 },
    #[error("mailbox lane {field} is not valid UTF-8")]
    InvalidUtf8 { field: LaneField },
    #[error("mailbox lane {field} is not a valid value of its type")]
    InvalidValue { field: LaneField },
    #[error("mailbox lane {field} of {len} bytes exceeds its bound")]
    TooLong { field: LaneField, len: usize },
    /// The bytes decode to an identity whose canonical encoding differs, so
    /// accepting them would give one lane two encodings and two digests.
    #[error("mailbox lane encoding is not canonical")]
    NonCanonical,
    #[error("mailbox lane digest must be {expected} bytes, got {len}")]
    InvalidDigestLength { expected: usize, len: usize },
}

/// Field of the lane encoding a codec error refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneField {
    Origin,
    OriginEntityType,
    OriginId,
    Recipient,
    RecipientJid,
    RoomLane,
}

impl fmt::Display for LaneField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Origin => "origin",
            Self::OriginEntityType => "origin entity type",
            Self::OriginId => "origin id",
            Self::Recipient => "recipient",
            Self::RecipientJid => "recipient JID",
            Self::RoomLane => "room lane",
        })
    }
}

const ORIGIN_SM_SESSION: u8 = 1;
const ORIGIN_ENTITY: u8 = 2;
const ENTITY_USER_ACTOR: u8 = 1;
const ENTITY_ROOM_ACTOR: u8 = 2;
const ENTITY_SM_SESSION: u8 = 3;
const RECIPIENT_BARE_JID: u8 = 1;
const RECIPIENT_FULL_JID: u8 = 2;
const RECIPIENT_ROOM: u8 = 3;
const ROOM_LANE_MUC_STANZA: u8 = 1;
const ROOM_LANE_MUJI_SIGNALING: u8 = 2;

/// Canonical encoding of a [`MailboxLaneIdentity`]: a version byte, then
/// every type discriminator and every length-delimited value in a fixed
/// order. Equal identities have equal encodings and distinct identities have
/// distinct ones, so byte equality is identity equality.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MailboxLaneEncoding(Vec<u8>);

impl fmt::Debug for MailboxLaneEncoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MailboxLaneEncoding({} bytes)", self.0.len())
    }
}

impl MailboxLaneEncoding {
    /// Bytes for the storage boundary.
    pub fn to_storage(&self) -> &[u8] {
        &self.0
    }

    /// Validate stored bytes by decoding them. Only bytes that decode to an
    /// identity and re-encode identically are accepted.
    pub fn from_storage(bytes: &[u8]) -> Result<Self, MailboxLaneCodecError> {
        MailboxLaneIdentity::decode_bytes(bytes).map(|identity| identity.encode())
    }

    /// The identity these bytes denote.
    pub fn decode(&self) -> Result<MailboxLaneIdentity, MailboxLaneCodecError> {
        MailboxLaneIdentity::decode_bytes(&self.0)
    }

    /// Index digest of this encoding under the current digest version.
    pub fn digest(&self) -> MailboxLaneDigest {
        let mut hash = Sha256::new();
        hash.update(b"waddle.ingress.mailbox_lane.v1");
        hash.update((self.0.len() as u64).to_be_bytes());
        hash.update(&self.0);
        MailboxLaneDigest {
            version: MailboxLaneDigestVersion::V1,
            bytes: hash.finalize().into(),
        }
    }
}

fn put_value(out: &mut Vec<u8>, value: &str) {
    // A `usize` length always fits the eight-byte prefix.
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Result<u8, MailboxLaneCodecError> {
        let (&first, rest) = self
            .rest
            .split_first()
            .ok_or(MailboxLaneCodecError::Truncated)?;
        self.rest = rest;
        Ok(first)
    }

    fn value(&mut self, field: LaneField) -> Result<&'a str, MailboxLaneCodecError> {
        let (prefix, rest) = self
            .rest
            .split_first_chunk::<8>()
            .ok_or(MailboxLaneCodecError::Truncated)?;
        let len = usize::try_from(u64::from_be_bytes(*prefix))
            .map_err(|_| MailboxLaneCodecError::Truncated)?;
        let (value, rest) = rest
            .split_at_checked(len)
            .ok_or(MailboxLaneCodecError::Truncated)?;
        self.rest = rest;
        std::str::from_utf8(value).map_err(|_| MailboxLaneCodecError::InvalidUtf8 { field })
    }
}

impl MailboxLaneIdentity {
    /// Canonical encoding under the current version.
    pub fn encode(&self) -> MailboxLaneEncoding {
        let mut out = vec![MailboxLaneEncodingVersion::CURRENT.to_storage()];
        match &self.origin {
            MailboxLaneOrigin::SmSession(session) => {
                out.push(ORIGIN_SM_SESSION);
                put_value(&mut out, session.as_str());
            }
            MailboxLaneOrigin::Entity(entity) => {
                out.push(ORIGIN_ENTITY);
                out.push(match entity.entity_type {
                    EntityType::UserActor => ENTITY_USER_ACTOR,
                    EntityType::RoomActor => ENTITY_ROOM_ACTOR,
                    EntityType::SmSession => ENTITY_SM_SESSION,
                });
                put_value(&mut out, &entity.id);
            }
        }
        match &self.recipient {
            MailboxLaneRecipient::BareJid(jid) => {
                out.push(RECIPIENT_BARE_JID);
                put_value(&mut out, jid.as_str());
            }
            MailboxLaneRecipient::FullJid(jid) => {
                out.push(RECIPIENT_FULL_JID);
                put_value(&mut out, jid.as_str());
            }
            MailboxLaneRecipient::Room { room, lane } => {
                out.push(RECIPIENT_ROOM);
                put_value(&mut out, room.as_str());
                out.push(match lane {
                    MailboxRoomLane::MucStanza => ROOM_LANE_MUC_STANZA,
                    MailboxRoomLane::MujiSignaling => ROOM_LANE_MUJI_SIGNALING,
                });
            }
        }
        MailboxLaneEncoding(out)
    }

    /// Lookup key: the digest for the index plus the full encoding every
    /// resolution compares.
    pub fn key(&self) -> MailboxLaneKey {
        let encoding = self.encode();
        MailboxLaneKey {
            digest: encoding.digest(),
            encoding,
        }
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, MailboxLaneCodecError> {
        let mut reader = Reader { rest: bytes };
        match MailboxLaneEncodingVersion::from_storage(reader.byte()?)? {
            MailboxLaneEncodingVersion::V1 => {}
        }
        let origin = match reader.byte()? {
            ORIGIN_SM_SESSION => {
                let id = reader.value(LaneField::OriginId)?;
                MailboxLaneOrigin::SmSession(SmSessionId::try_from_wire(id).map_err(|error| {
                    MailboxLaneCodecError::TooLong {
                        field: LaneField::OriginId,
                        len: error.len,
                    }
                })?)
            }
            ORIGIN_ENTITY => {
                let entity_type = match reader.byte()? {
                    ENTITY_USER_ACTOR => EntityType::UserActor,
                    ENTITY_ROOM_ACTOR => EntityType::RoomActor,
                    ENTITY_SM_SESSION => EntityType::SmSession,
                    value => {
                        return Err(MailboxLaneCodecError::UnknownDiscriminator {
                            field: LaneField::OriginEntityType,
                            value,
                        })
                    }
                };
                let id = reader.value(LaneField::OriginId)?;
                if id.len() > ENTITY_ID_MAX_LEN {
                    return Err(MailboxLaneCodecError::TooLong {
                        field: LaneField::OriginId,
                        len: id.len(),
                    });
                }
                MailboxLaneOrigin::Entity(Entity::new(entity_type, id))
            }
            value => {
                return Err(MailboxLaneCodecError::UnknownDiscriminator {
                    field: LaneField::Origin,
                    value,
                })
            }
        };
        let invalid_jid = |_| MailboxLaneCodecError::InvalidValue {
            field: LaneField::RecipientJid,
        };
        let recipient = match reader.byte()? {
            RECIPIENT_BARE_JID => MailboxLaneRecipient::BareJid(
                BareJid::new(reader.value(LaneField::RecipientJid)?).map_err(invalid_jid)?,
            ),
            RECIPIENT_FULL_JID => MailboxLaneRecipient::FullJid(
                FullJid::new(reader.value(LaneField::RecipientJid)?).map_err(invalid_jid)?,
            ),
            RECIPIENT_ROOM => {
                let room =
                    BareJid::new(reader.value(LaneField::RecipientJid)?).map_err(invalid_jid)?;
                let lane = match reader.byte()? {
                    ROOM_LANE_MUC_STANZA => MailboxRoomLane::MucStanza,
                    ROOM_LANE_MUJI_SIGNALING => MailboxRoomLane::MujiSignaling,
                    value => {
                        return Err(MailboxLaneCodecError::UnknownDiscriminator {
                            field: LaneField::RoomLane,
                            value,
                        })
                    }
                };
                MailboxLaneRecipient::Room { room, lane }
            }
            value => {
                return Err(MailboxLaneCodecError::UnknownDiscriminator {
                    field: LaneField::Recipient,
                    value,
                })
            }
        };
        if !reader.rest.is_empty() {
            return Err(MailboxLaneCodecError::TrailingBytes {
                count: reader.rest.len(),
            });
        }
        let identity = Self { origin, recipient };
        // JID parsing normalizes, so differently spelled bytes can decode to
        // one identity; only the spelling `encode` produces is accepted.
        if identity.encode().0 != bytes {
            return Err(MailboxLaneCodecError::NonCanonical);
        }
        Ok(identity)
    }
}

/// Bounded digest of a lane's canonical encoding, for an index that cannot
/// hold the full identity. Never proof of identity on its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct MailboxLaneDigest {
    version: MailboxLaneDigestVersion,
    bytes: [u8; Self::LEN],
}

impl fmt::Debug for MailboxLaneDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MailboxLaneDigest({:?}, ..)", self.version)
    }
}

impl MailboxLaneDigest {
    pub const LEN: usize = 32;

    pub fn version(&self) -> MailboxLaneDigestVersion {
        self.version
    }

    pub fn to_storage(&self) -> &[u8; Self::LEN] {
        &self.bytes
    }

    pub fn from_storage(version: u8, bytes: &[u8]) -> Result<Self, MailboxLaneCodecError> {
        let version = MailboxLaneDigestVersion::from_storage(version)?;
        let bytes = bytes
            .try_into()
            .map_err(|_| MailboxLaneCodecError::InvalidDigestLength {
                expected: Self::LEN,
                len: bytes.len(),
            })?;
        Ok(Self { version, bytes })
    }
}

/// A lane's index digest together with the full encoding it was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MailboxLaneKey {
    digest: MailboxLaneDigest,
    encoding: MailboxLaneEncoding,
}

/// Two different lane identities share one digest. Treating them as one lane
/// would merge their sequence spaces, so resolution fails closed.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("mailbox lane digest collision between two different lane identities")]
pub struct MailboxLaneDigestCollision;

impl MailboxLaneKey {
    pub fn digest(&self) -> &MailboxLaneDigest {
        &self.digest
    }

    pub fn encoding(&self) -> &MailboxLaneEncoding {
        &self.encoding
    }

    /// Rebuild a key from a stored row. The digest is taken as stored, not
    /// recomputed: [`Self::resolves_to`] is what detects a row whose digest
    /// matches a lookup while its identity does not.
    pub fn from_storage(digest: MailboxLaneDigest, encoding: MailboxLaneEncoding) -> Self {
        Self { digest, encoding }
    }

    /// Whether `stored`, a row found for this key, is this lane.
    ///
    /// Different digests are different lanes. Equal digests are the same lane
    /// only when the full encodings are equal too; equal digests over
    /// different encodings are a collision, reported rather than merged.
    pub fn resolves_to(&self, stored: &Self) -> Result<bool, MailboxLaneDigestCollision> {
        if self.digest != stored.digest {
            return Ok(false);
        }
        if self.encoding == stored.encoding {
            Ok(true)
        } else {
            Err(MailboxLaneDigestCollision)
        }
    }
}

/// Position in a lane's gap-free sequence space.
///
/// [`Self::ZERO`] is the counter of a lane that has allocated nothing and
/// the cursor of a lane with nothing dispositioned; entries occupy `1`
/// through `u64::MAX`. The space never wraps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxLaneSequence(u64);

/// Failure at the lane-sequence boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MailboxLaneSequenceError {
    /// The lane has allocated `u64::MAX`; it can hold no further entry.
    #[error("mailbox lane sequence space is exhausted")]
    Exhausted,
    /// Stored text is not the canonical decimal form of a `u64`.
    #[error("mailbox lane sequence is not a canonical unsigned 64-bit decimal")]
    InvalidDecimal,
}

impl MailboxLaneSequence {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next sequence, or [`MailboxLaneSequenceError::Exhausted`] after
    /// `u64::MAX`.
    pub fn successor(self) -> Result<Self, MailboxLaneSequenceError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(MailboxLaneSequenceError::Exhausted)
    }

    /// Decimal text for a `NUMERIC(20)` column, which holds the whole `u64`
    /// range that a signed 64-bit column cannot.
    pub fn to_storage(self) -> String {
        self.0.to_string()
    }

    /// Parse the decimal text of a `NUMERIC(20)` column. Only the canonical
    /// form is accepted: digits alone, no sign, no leading zero, within
    /// `u64`.
    pub fn from_storage(value: &str) -> Result<Self, MailboxLaneSequenceError> {
        let canonical = !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && (value == "0" || !value.starts_with('0'));
        if !canonical {
            return Err(MailboxLaneSequenceError::InvalidDecimal);
        }
        value
            .parse()
            .map(Self)
            .map_err(|_| MailboxLaneSequenceError::InvalidDecimal)
    }
}

/// Exclusive consumption cursor: the last sequence of a lane that has been
/// dispositioned. Work to read starts strictly after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxConsumptionCursor(MailboxLaneSequence);

/// A cursor move that would skip or revisit a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MailboxCursorError {
    #[error("cursor at {cursor:?} cannot move back or stay at {requested:?}")]
    NotAhead {
        cursor: MailboxLaneSequence,
        requested: MailboxLaneSequence,
    },
    #[error("cursor at {cursor:?} cannot skip to {requested:?}")]
    Gap {
        cursor: MailboxLaneSequence,
        requested: MailboxLaneSequence,
    },
}

impl MailboxConsumptionCursor {
    /// Cursor of a lane with nothing dispositioned.
    pub const START: Self = Self(MailboxLaneSequence::ZERO);

    pub const fn from_storage(last_dispositioned: MailboxLaneSequence) -> Self {
        Self(last_dispositioned)
    }

    pub const fn last_dispositioned(self) -> MailboxLaneSequence {
        self.0
    }

    /// Whether `sequence` lies after the cursor and is therefore still owed.
    pub fn is_pending(self, sequence: MailboxLaneSequence) -> bool {
        sequence > self.0
    }

    /// Move to `next`, which must be the cursor's immediate successor.
    pub fn advance_to(self, next: MailboxLaneSequence) -> Result<Self, MailboxCursorError> {
        if next <= self.0 {
            return Err(MailboxCursorError::NotAhead {
                cursor: self.0,
                requested: next,
            });
        }
        if self.0.successor() != Ok(next) {
            return Err(MailboxCursorError::Gap {
                cursor: self.0,
                requested: next,
            });
        }
        Ok(Self(next))
    }
}

/// Identity of the operation a mailbox entry carries, known before any commit
/// and independent of the lane and of whichever node owns the target.
///
/// A dispatch effect key or delivery key names one owner-specific attempt and
/// changes when the room moves; it is provenance of an attempt, never an
/// input here. Every attempt at one room acceptance carries the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxOperationKey {
    /// The room owner's acceptance of one canonical message.
    RoomAcceptance(MessageKey),
}

impl MailboxOperationKey {
    /// The canonical message this operation is about.
    pub fn message_key(&self) -> MessageKey {
        match self {
            Self::RoomAcceptance(message_key) => *message_key,
        }
    }
}

/// The room owner's decision on a pending operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxOwnerDecision {
    Accepted,
    Rejected,
}

/// Disposition of one mailbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxEntryDisposition {
    /// Appended; the owner has not decided.
    Pending,
    /// The owner durably accepted the operation.
    OwnerAccepted,
    /// The owner durably rejected the operation.
    OwnerRejected,
    /// The decided outcome was consumed. Terminal; keeps the decision so a
    /// later lookup still answers which outcome it was.
    Consumed(MailboxOwnerDecision),
}

/// An attempted change to a mailbox entry's disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxEntryEvent {
    /// The owner records its decision.
    OwnerDecided(MailboxOwnerDecision),
    /// The decided outcome is consumed.
    Consumed,
}

/// Result of applying a legal event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxDispositionChange {
    /// The entry moved to this disposition.
    Applied(MailboxEntryDisposition),
    /// The event repeats what the entry already records; nothing changes.
    Unchanged(MailboxEntryDisposition),
}

impl MailboxDispositionChange {
    /// The entry's disposition after the event.
    pub fn disposition(self) -> MailboxEntryDisposition {
        match self {
            Self::Applied(disposition) | Self::Unchanged(disposition) => disposition,
        }
    }
}

/// An event the entry's current disposition forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MailboxDispositionError {
    /// The entry already records the opposite owner decision.
    #[error("mailbox entry already records {recorded:?}, not {attempted:?}")]
    ConflictingDecision {
        recorded: MailboxOwnerDecision,
        attempted: MailboxOwnerDecision,
    },
    /// Nothing can be consumed before the owner decides.
    #[error("mailbox entry has no owner decision to consume")]
    Undecided,
}

impl MailboxEntryDisposition {
    /// The owner decision this disposition records, if any.
    pub fn decision(self) -> Option<MailboxOwnerDecision> {
        match self {
            Self::Pending => None,
            Self::OwnerAccepted => Some(MailboxOwnerDecision::Accepted),
            Self::OwnerRejected => Some(MailboxOwnerDecision::Rejected),
            Self::Consumed(decision) => Some(decision),
        }
    }

    /// Whether no event can change this disposition.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Consumed(_))
    }

    /// Apply `event`. Total over every disposition and event: the result is
    /// a change, a no-op for a repeated event, or a typed refusal.
    pub fn apply(
        self,
        event: MailboxEntryEvent,
    ) -> Result<MailboxDispositionChange, MailboxDispositionError> {
        use MailboxDispositionChange::{Applied, Unchanged};
        match (self.decision(), event) {
            (None, MailboxEntryEvent::OwnerDecided(decision)) => Ok(Applied(match decision {
                MailboxOwnerDecision::Accepted => Self::OwnerAccepted,
                MailboxOwnerDecision::Rejected => Self::OwnerRejected,
            })),
            (None, MailboxEntryEvent::Consumed) => Err(MailboxDispositionError::Undecided),
            (Some(recorded), MailboxEntryEvent::OwnerDecided(attempted)) => {
                if recorded == attempted {
                    Ok(Unchanged(self))
                } else {
                    Err(MailboxDispositionError::ConflictingDecision {
                        recorded,
                        attempted,
                    })
                }
            }
            (Some(decision), MailboxEntryEvent::Consumed) => {
                if self.is_terminal() {
                    Ok(Unchanged(self))
                } else {
                    Ok(Applied(Self::Consumed(decision)))
                }
            }
        }
    }
}

/// Pending-entry caps an append is admitted against. The values are the
/// caller's policy; none is implied here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxAdmissionLimits {
    /// Most pending entries one lane may hold.
    pub lane_pending: u64,
    /// Most pending entries one node may hold across its lanes.
    pub node_pending: u64,
}

/// Pending entries counted before the append being admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxPendingLoad {
    pub lane_pending: u64,
    pub node_pending: u64,
}

/// Why an append was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MailboxAdmissionRefusal {
    #[error("mailbox lane holds {pending} pending entries, at its cap of {cap}")]
    LaneFull { pending: u64, cap: u64 },
    #[error("node holds {pending} pending mailbox entries, at its cap of {cap}")]
    NodeFull { pending: u64, cap: u64 },
}

impl MailboxAdmissionLimits {
    /// Admit one more pending entry under `load`, or say which cap refuses
    /// it. The lane cap is reported first when both are reached, because it
    /// names the narrower backlog a caller can act on.
    pub fn admit(&self, load: MailboxPendingLoad) -> Result<(), MailboxAdmissionRefusal> {
        if load.lane_pending >= self.lane_pending {
            return Err(MailboxAdmissionRefusal::LaneFull {
                pending: load.lane_pending,
                cap: self.lane_pending,
            });
        }
        if load.node_pending >= self.node_pending {
            return Err(MailboxAdmissionRefusal::NodeFull {
                pending: load.node_pending,
                cap: self.node_pending,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{any, prop_assert, prop_assert_eq, prop_oneof, proptest, Just};
    use proptest::strategy::Strategy;

    use super::{
        BareJid, ClaimEpoch, Entity, EntityType, FullJid, LaneField, MailboxAdmissionLimits,
        MailboxAdmissionRefusal, MailboxConsumptionCursor, MailboxCursorError,
        MailboxDispositionChange, MailboxDispositionError, MailboxEntryDisposition,
        MailboxEntryEvent, MailboxLaneCodecError, MailboxLaneDigest, MailboxLaneDigestCollision,
        MailboxLaneEncoding, MailboxLaneFence, MailboxLaneIdentity, MailboxLaneKey,
        MailboxLaneOrigin, MailboxLaneRecipient, MailboxLaneSequence, MailboxLaneSequenceError,
        MailboxOperationKey, MailboxOwnerDecision, MailboxPendingLoad, MailboxRoomLane, MessageKey,
        NodeIdentity, SmSessionId, StaleMailboxLaneFence,
    };

    fn bare_jid() -> impl Strategy<Value = BareJid> {
        ("[a-z0-9]{1,12}", "[a-z]{1,12}").prop_map(|(local, domain)| {
            BareJid::new(&format!("{local}@{domain}.example")).expect("generated bare JID")
        })
    }

    fn full_jid() -> impl Strategy<Value = FullJid> {
        (bare_jid(), "[a-zA-Z0-9]{1,12}").prop_map(|(bare, resource)| {
            bare.with_resource_str(&resource)
                .expect("generated full JID")
        })
    }

    fn origin() -> impl Strategy<Value = MailboxLaneOrigin> {
        let entity_type = prop_oneof![
            Just(EntityType::UserActor),
            Just(EntityType::RoomActor),
            Just(EntityType::SmSession),
        ];
        prop_oneof![
            "[a-zA-Z0-9-]{0,40}".prop_map(|id| MailboxLaneOrigin::SmSession(SmSessionId::new(id))),
            (entity_type, "[a-z0-9@./-]{0,40}").prop_map(|(entity_type, id)| {
                MailboxLaneOrigin::Entity(Entity::new(entity_type, id))
            }),
        ]
    }

    fn recipient() -> impl Strategy<Value = MailboxLaneRecipient> {
        let lane = prop_oneof![
            Just(MailboxRoomLane::MucStanza),
            Just(MailboxRoomLane::MujiSignaling),
        ];
        prop_oneof![
            bare_jid().prop_map(MailboxLaneRecipient::BareJid),
            full_jid().prop_map(MailboxLaneRecipient::FullJid),
            (bare_jid(), lane).prop_map(|(room, lane)| MailboxLaneRecipient::Room { room, lane }),
        ]
    }

    fn identity() -> impl Strategy<Value = MailboxLaneIdentity> {
        (origin(), recipient())
            .prop_map(|(origin, recipient)| MailboxLaneIdentity { origin, recipient })
    }

    fn disposition() -> impl Strategy<Value = MailboxEntryDisposition> {
        prop_oneof![
            Just(MailboxEntryDisposition::Pending),
            Just(MailboxEntryDisposition::OwnerAccepted),
            Just(MailboxEntryDisposition::OwnerRejected),
            Just(MailboxEntryDisposition::Consumed(
                MailboxOwnerDecision::Accepted
            )),
            Just(MailboxEntryDisposition::Consumed(
                MailboxOwnerDecision::Rejected
            )),
        ]
    }

    fn event() -> impl Strategy<Value = MailboxEntryEvent> {
        prop_oneof![
            Just(MailboxEntryEvent::OwnerDecided(
                MailboxOwnerDecision::Accepted
            )),
            Just(MailboxEntryEvent::OwnerDecided(
                MailboxOwnerDecision::Rejected
            )),
            Just(MailboxEntryEvent::Consumed),
        ]
    }

    const DISPOSITIONS: [MailboxEntryDisposition; 5] = [
        MailboxEntryDisposition::Pending,
        MailboxEntryDisposition::OwnerAccepted,
        MailboxEntryDisposition::OwnerRejected,
        MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Accepted),
        MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Rejected),
    ];

    const EVENTS: [MailboxEntryEvent; 3] = [
        MailboxEntryEvent::OwnerDecided(MailboxOwnerDecision::Accepted),
        MailboxEntryEvent::OwnerDecided(MailboxOwnerDecision::Rejected),
        MailboxEntryEvent::Consumed,
    ];

    fn room_identity() -> MailboxLaneIdentity {
        MailboxLaneIdentity {
            origin: MailboxLaneOrigin::SmSession(SmSessionId::new("stream-1")),
            recipient: MailboxLaneRecipient::Room {
                room: BareJid::new("room@muc.example").expect("room JID"),
                lane: MailboxRoomLane::MucStanza,
            },
        }
    }

    proptest! {
        #[test]
        fn mailbox_lane_encoding_round_trips(identity in identity()) {
            let encoding = identity.encode();
            prop_assert_eq!(encoding.decode(), Ok(identity.clone()));
            let restored = MailboxLaneEncoding::from_storage(encoding.to_storage());
            prop_assert_eq!(restored.as_ref(), Ok(&encoding));
            prop_assert_eq!(*identity.key().digest(), encoding.digest());
        }

        #[test]
        fn mailbox_lane_encoding_separates_distinct_identities(
            left in identity(),
            right in identity(),
        ) {
            prop_assert_eq!(left == right, left.encode() == right.encode());
            prop_assert_eq!(left.key().resolves_to(&right.key()), Ok(left == right));
        }

        #[test]
        fn mailbox_lane_decoding_rejects_other_versions(
            identity in identity(),
            version in any::<u8>(),
        ) {
            let mut bytes = identity.encode().to_storage().to_vec();
            bytes[0] = version;
            let decoded = MailboxLaneEncoding::from_storage(&bytes);
            if version == 1 {
                prop_assert!(decoded.is_ok());
            } else {
                prop_assert_eq!(
                    decoded,
                    Err(MailboxLaneCodecError::UnsupportedEncodingVersion { value: version })
                );
            }
        }

        #[test]
        fn mailbox_lane_decoding_never_panics_and_accepts_only_canonical_bytes(
            bytes in proptest::collection::vec(any::<u8>(), 0..96),
        ) {
            if let Ok(encoding) = MailboxLaneEncoding::from_storage(&bytes) {
                prop_assert_eq!(encoding.to_storage(), bytes.as_slice());
            }
        }

        #[test]
        fn mailbox_lane_decoding_rejects_truncation_and_trailing_bytes(
            identity in identity(),
            cut in any::<proptest::sample::Index>(),
            extra in proptest::collection::vec(any::<u8>(), 1..8),
        ) {
            let bytes = identity.encode().to_storage().to_vec();
            let truncated = &bytes[..cut.index(bytes.len())];
            prop_assert!(MailboxLaneEncoding::from_storage(truncated).is_err());
            let mut extended = bytes;
            extended.extend_from_slice(&extra);
            prop_assert!(MailboxLaneEncoding::from_storage(&extended).is_err());
        }

        #[test]
        fn mailbox_lane_digest_collision_fails_closed(left in identity(), right in identity()) {
            let lookup = left.key();
            let stored = MailboxLaneKey::from_storage(*lookup.digest(), right.encode());
            if left == right {
                prop_assert_eq!(lookup.resolves_to(&stored), Ok(true));
            } else {
                prop_assert_eq!(lookup.resolves_to(&stored), Err(MailboxLaneDigestCollision));
            }
        }

        #[test]
        fn mailbox_sequence_successor_is_checked(value in any::<u64>()) {
            let sequence = MailboxLaneSequence::new(value);
            match sequence.successor() {
                Ok(next) => {
                    prop_assert!(value < u64::MAX);
                    prop_assert_eq!(next.get(), value + 1);
                    prop_assert!(next > sequence);
                }
                Err(error) => {
                    prop_assert_eq!(value, u64::MAX);
                    prop_assert_eq!(error, MailboxLaneSequenceError::Exhausted);
                }
            }
        }

        #[test]
        fn mailbox_sequence_decimal_round_trips(value in any::<u64>()) {
            let sequence = MailboxLaneSequence::new(value);
            let decimal = sequence.to_storage();
            prop_assert!(decimal.len() <= 20);
            prop_assert_eq!(MailboxLaneSequence::from_storage(&decimal), Ok(sequence));
        }

        #[test]
        fn mailbox_sequence_decimal_accepts_only_canonical_u64(text in "[0-9+ -]{0,24}") {
            let parsed = MailboxLaneSequence::from_storage(&text);
            match text.parse::<u64>() {
                Ok(value) if value.to_string() == text => {
                    prop_assert_eq!(parsed, Ok(MailboxLaneSequence::new(value)));
                }
                _ => prop_assert_eq!(parsed, Err(MailboxLaneSequenceError::InvalidDecimal)),
            }
        }

        #[test]
        fn mailbox_disposition_transitions_are_closed(
            start in disposition(),
            events in proptest::collection::vec(event(), 0..12),
        ) {
            let mut current = start;
            for event in events {
                let before = current;
                match current.apply(event) {
                    Ok(MailboxDispositionChange::Applied(next)) => {
                        prop_assert!(!before.is_terminal());
                        prop_assert!(next != before);
                        if let Some(decision) = before.decision() {
                            prop_assert_eq!(next.decision(), Some(decision));
                        }
                        current = next;
                    }
                    Ok(MailboxDispositionChange::Unchanged(same)) => {
                        prop_assert_eq!(same, before);
                    }
                    Err(_) => {}
                }
                // A decision, once recorded, is never replaced or erased.
                if let Some(decision) = before.decision() {
                    prop_assert_eq!(current.decision(), Some(decision));
                }
            }
        }

        #[test]
        fn mailbox_disposition_events_are_idempotent(start in disposition(), event in event()) {
            if let Ok(change) = start.apply(event) {
                let settled = change.disposition();
                prop_assert_eq!(
                    settled.apply(event),
                    Ok(MailboxDispositionChange::Unchanged(settled))
                );
            }
        }

        #[test]
        fn mailbox_cursor_advances_only_to_its_successor(
            cursor in any::<u64>(),
            requested in any::<u64>(),
        ) {
            let start = MailboxConsumptionCursor::from_storage(MailboxLaneSequence::new(cursor));
            let requested = MailboxLaneSequence::new(requested);
            match start.advance_to(requested) {
                Ok(next) => {
                    prop_assert_eq!(Ok(requested), start.last_dispositioned().successor());
                    prop_assert_eq!(next.last_dispositioned(), requested);
                    prop_assert!(next > start);
                    prop_assert!(!next.is_pending(requested));
                }
                Err(MailboxCursorError::NotAhead { .. }) => {
                    prop_assert!(requested <= start.last_dispositioned());
                }
                Err(MailboxCursorError::Gap { .. }) => {
                    prop_assert!(requested > start.last_dispositioned());
                    prop_assert!(Ok(requested) != start.last_dispositioned().successor());
                }
            }
        }

        #[test]
        fn mailbox_cursor_is_monotonic_over_any_requests(
            start in 0u64..1000,
            requests in proptest::collection::vec(0u64..1024, 0..64),
        ) {
            let mut cursor =
                MailboxConsumptionCursor::from_storage(MailboxLaneSequence::new(start));
            for requested in requests {
                let before = cursor;
                if let Ok(next) = cursor.advance_to(MailboxLaneSequence::new(requested)) {
                    cursor = next;
                }
                prop_assert!(cursor >= before);
                prop_assert!(
                    cursor.last_dispositioned().get() - before.last_dispositioned().get() <= 1
                );
            }
        }

        #[test]
        fn mailbox_admission_matches_its_caps(
            lane_cap in any::<u64>(),
            node_cap in any::<u64>(),
            lane_pending in any::<u64>(),
            node_pending in any::<u64>(),
        ) {
            let limits = MailboxAdmissionLimits {
                lane_pending: lane_cap,
                node_pending: node_cap,
            };
            let load = MailboxPendingLoad { lane_pending, node_pending };
            match limits.admit(load) {
                Ok(()) => prop_assert!(lane_pending < lane_cap && node_pending < node_cap),
                Err(MailboxAdmissionRefusal::LaneFull { pending, cap }) => {
                    prop_assert_eq!((pending, cap), (lane_pending, lane_cap));
                    prop_assert!(lane_pending >= lane_cap);
                }
                Err(MailboxAdmissionRefusal::NodeFull { pending, cap }) => {
                    prop_assert_eq!((pending, cap), (node_pending, node_cap));
                    prop_assert!(lane_pending < lane_cap && node_pending >= node_cap);
                }
            }
        }
    }

    #[test]
    fn mailbox_lane_identity_distinguishes_recipient_kinds_and_room_lanes() {
        let jid = BareJid::new("room@muc.example").expect("JID");
        let origin = MailboxLaneOrigin::SmSession(SmSessionId::new("stream-1"));
        let recipients = [
            MailboxLaneRecipient::BareJid(jid.clone()),
            MailboxLaneRecipient::Room {
                room: jid.clone(),
                lane: MailboxRoomLane::MucStanza,
            },
            MailboxLaneRecipient::Room {
                room: jid,
                lane: MailboxRoomLane::MujiSignaling,
            },
        ];
        let digests: std::collections::HashSet<MailboxLaneDigest> = recipients
            .iter()
            .map(|recipient| {
                *MailboxLaneIdentity {
                    origin: origin.clone(),
                    recipient: recipient.clone(),
                }
                .key()
                .digest()
            })
            .collect();
        assert_eq!(digests.len(), recipients.len());

        // An SM session and an SM-session entity with the same id are
        // different origins.
        let entity_origin = MailboxLaneIdentity {
            origin: MailboxLaneOrigin::Entity(Entity::new(EntityType::SmSession, "stream-1")),
            ..room_identity()
        };
        assert_ne!(entity_origin.encode(), room_identity().encode());
    }

    #[test]
    fn mailbox_lane_fence_is_not_part_of_the_identity() {
        let fence = MailboxLaneFence {
            origin_epoch: ClaimEpoch(3),
            target_epoch: ClaimEpoch(7),
            target_owner: NodeIdentity::new("node-b", "incarnation-1"),
        };
        let successor = MailboxLaneFence {
            target_epoch: ClaimEpoch(8),
            target_owner: NodeIdentity::new("node-c", "incarnation-1"),
            ..fence.clone()
        };
        let restarted = MailboxLaneFence {
            target_owner: NodeIdentity::new("node-b", "incarnation-2"),
            ..fence.clone()
        };
        assert_eq!(fence.verify(&fence.clone()), Ok(()));
        assert_eq!(fence.verify(&successor), Err(StaleMailboxLaneFence));
        assert_eq!(fence.verify(&restarted), Err(StaleMailboxLaneFence));
        assert_eq!(successor.verify(&fence), Err(StaleMailboxLaneFence));
        // The lane a successor resumes is the lane its predecessor wrote.
        assert_eq!(room_identity().key(), room_identity().key());
    }

    #[test]
    fn mailbox_lane_decoding_rejects_unknown_discriminators_and_noncanonical_jids() {
        let canonical = room_identity().encode().to_storage().to_vec();
        let mut unknown_origin = canonical.clone();
        unknown_origin[1] = 9;
        assert_eq!(
            MailboxLaneEncoding::from_storage(&unknown_origin),
            Err(MailboxLaneCodecError::UnknownDiscriminator {
                field: LaneField::Origin,
                value: 9
            })
        );
        let mut unknown_lane = canonical.clone();
        *unknown_lane.last_mut().expect("lane byte") = 9;
        assert_eq!(
            MailboxLaneEncoding::from_storage(&unknown_lane),
            Err(MailboxLaneCodecError::UnknownDiscriminator {
                field: LaneField::RoomLane,
                value: 9
            })
        );
        // An upper-case localpart parses to the same JID, so it would be a
        // second spelling of the same lane.
        let position = canonical
            .windows(4)
            .position(|window| window == b"room")
            .expect("room localpart");
        let mut respelled = canonical;
        respelled[position] = b'R';
        assert_eq!(
            MailboxLaneEncoding::from_storage(&respelled),
            Err(MailboxLaneCodecError::NonCanonical)
        );
        assert_eq!(
            MailboxLaneEncoding::from_storage(&[]),
            Err(MailboxLaneCodecError::Truncated)
        );
    }

    #[test]
    fn mailbox_lane_digest_storage_rejects_unknown_versions_and_lengths() {
        let digest = room_identity().encode().digest();
        assert_eq!(
            MailboxLaneDigest::from_storage(digest.version().to_storage(), digest.to_storage()),
            Ok(digest)
        );
        assert_eq!(
            MailboxLaneDigest::from_storage(2, digest.to_storage()),
            Err(MailboxLaneCodecError::UnsupportedDigestVersion { value: 2 })
        );
        assert_eq!(
            MailboxLaneDigest::from_storage(1, &digest.to_storage()[..31]),
            Err(MailboxLaneCodecError::InvalidDigestLength {
                expected: 32,
                len: 31
            })
        );
    }

    #[test]
    fn mailbox_sequence_bounds() {
        assert_eq!(MailboxLaneSequence::ZERO.to_storage(), "0");
        assert_eq!(
            MailboxLaneSequence::from_storage("0"),
            Ok(MailboxLaneSequence::ZERO)
        );
        assert_eq!(
            MailboxLaneSequence::MAX.to_storage(),
            "18446744073709551615"
        );
        assert_eq!(
            MailboxLaneSequence::from_storage("18446744073709551615"),
            Ok(MailboxLaneSequence::MAX)
        );
        assert_eq!(
            MailboxLaneSequence::ZERO.successor(),
            Ok(MailboxLaneSequence::new(1))
        );
        assert_eq!(
            MailboxLaneSequence::MAX.successor(),
            Err(MailboxLaneSequenceError::Exhausted)
        );
        for invalid in [
            "",
            "00",
            "01",
            "+1",
            "-1",
            " 1",
            "1.0",
            "18446744073709551616",
        ] {
            assert_eq!(
                MailboxLaneSequence::from_storage(invalid),
                Err(MailboxLaneSequenceError::InvalidDecimal),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn mailbox_disposition_table_is_total() {
        use MailboxDispositionChange::{Applied, Unchanged};
        use MailboxEntryDisposition::{Consumed, OwnerAccepted, OwnerRejected};
        use MailboxOwnerDecision::{Accepted, Rejected};
        let conflict = |recorded, attempted| {
            Err(MailboxDispositionError::ConflictingDecision {
                recorded,
                attempted,
            })
        };
        let expected = [
            [
                Ok(Applied(OwnerAccepted)),
                Ok(Applied(OwnerRejected)),
                Err(MailboxDispositionError::Undecided),
            ],
            [
                Ok(Unchanged(OwnerAccepted)),
                conflict(Accepted, Rejected),
                Ok(Applied(Consumed(Accepted))),
            ],
            [
                conflict(Rejected, Accepted),
                Ok(Unchanged(OwnerRejected)),
                Ok(Applied(Consumed(Rejected))),
            ],
            [
                Ok(Unchanged(Consumed(Accepted))),
                conflict(Accepted, Rejected),
                Ok(Unchanged(Consumed(Accepted))),
            ],
            [
                conflict(Rejected, Accepted),
                Ok(Unchanged(Consumed(Rejected))),
                Ok(Unchanged(Consumed(Rejected))),
            ],
        ];
        for (disposition, row) in DISPOSITIONS.into_iter().zip(expected) {
            for (event, outcome) in EVENTS.into_iter().zip(row) {
                assert_eq!(
                    disposition.apply(event),
                    outcome,
                    "{disposition:?} {event:?}"
                );
            }
        }
        assert!(DISPOSITIONS
            .into_iter()
            .filter(|disposition| disposition.is_terminal())
            .all(|terminal| EVENTS
                .into_iter()
                .all(|event| !matches!(terminal.apply(event), Ok(Applied(_))))));
    }

    #[test]
    fn mailbox_cursor_bounds() {
        let start = MailboxConsumptionCursor::START;
        assert!(start.is_pending(MailboxLaneSequence::new(1)));
        assert!(!start.is_pending(MailboxLaneSequence::ZERO));
        let first = start
            .advance_to(MailboxLaneSequence::new(1))
            .expect("first entry");
        assert_eq!(
            first.advance_to(MailboxLaneSequence::new(1)),
            Err(MailboxCursorError::NotAhead {
                cursor: MailboxLaneSequence::new(1),
                requested: MailboxLaneSequence::new(1),
            })
        );
        assert_eq!(
            first.advance_to(MailboxLaneSequence::new(3)),
            Err(MailboxCursorError::Gap {
                cursor: MailboxLaneSequence::new(1),
                requested: MailboxLaneSequence::new(3),
            })
        );
        let last = MailboxConsumptionCursor::from_storage(MailboxLaneSequence::MAX);
        assert_eq!(
            last.advance_to(MailboxLaneSequence::MAX),
            Err(MailboxCursorError::NotAhead {
                cursor: MailboxLaneSequence::MAX,
                requested: MailboxLaneSequence::MAX,
            })
        );
    }

    #[test]
    fn mailbox_operation_key_is_the_message_not_the_attempt() {
        let message = MessageKey::new();
        let operation = MailboxOperationKey::RoomAcceptance(message);
        assert_eq!(operation, MailboxOperationKey::RoomAcceptance(message));
        assert_eq!(operation.message_key(), message);
        assert_ne!(
            operation,
            MailboxOperationKey::RoomAcceptance(MessageKey::new())
        );
    }

    #[test]
    fn mailbox_admission_reports_the_lane_cap_first() {
        let limits = MailboxAdmissionLimits {
            lane_pending: 2,
            node_pending: 3,
        };
        let admit = |lane_pending, node_pending| {
            limits.admit(MailboxPendingLoad {
                lane_pending,
                node_pending,
            })
        };
        assert_eq!(admit(1, 2), Ok(()));
        assert_eq!(
            admit(2, 3),
            Err(MailboxAdmissionRefusal::LaneFull { pending: 2, cap: 2 })
        );
        assert_eq!(
            admit(1, 3),
            Err(MailboxAdmissionRefusal::NodeFull { pending: 3, cap: 3 })
        );
        let closed = MailboxAdmissionLimits {
            lane_pending: 0,
            node_pending: 0,
        };
        assert!(closed
            .admit(MailboxPendingLoad {
                lane_pending: 0,
                node_pending: 0
            })
            .is_err());
    }
}
