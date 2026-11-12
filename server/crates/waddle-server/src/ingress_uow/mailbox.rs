//! Durable ordered mailbox repository.
//!
//! Every operation runs on the caller's [`IngressUowTransaction`], so a
//! mailbox write always sits behind the typed ingress entry and its epoch
//! proof. Nothing here opens a transaction or waits on the network.
//!
//! Lock order inside one transaction is lane, then the entry's parent (its
//! canonical message, or its room request), then entry. [`MailboxRepository::lock_lane`] and
//! [`MailboxRepository::lock_lane_for_target`] take the lane row first and
//! hand back a [`LockedMailboxLane`]; the operations that touch an entry
//! require that value. Message garbage collection takes the canonical
//! message and reaches entries through its cascade, never a lane, so it
//! cannot wait on a lane held by an appender waiting for that message.
//!
//! Lanes exist only on PostgreSQL. A single-node deployment seals a room
//! acceptance in its own transaction and writes no lane.

use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::ingress::mailbox::{
    MailboxAdmissionLimits, MailboxAdmissionRefusal, MailboxConsumptionCursor, MailboxCursorError,
    MailboxDispositionChange, MailboxDispositionError, MailboxEntryDisposition, MailboxEntryEvent,
    MailboxLaneCodecError, MailboxLaneDigest, MailboxLaneDigestCollision, MailboxLaneEncoding,
    MailboxLaneFence, MailboxLaneIdentity, MailboxLaneKey, MailboxLaneSequence,
    MailboxLaneSequenceError, MailboxOperationKey, MailboxOperationParent, MailboxOwnerDecision,
    MailboxPendingLoad, RoomRequestKey, RoomRequestKind, StaleMailboxLaneFence,
};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};

use super::repositories::CanonicalMessageRepository;
use super::room_requests::RoomRequestRepository;
use super::{IngressUowError, IngressUowTransaction};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_substrate::{
    prepared_ingress_relation, EnteredTransaction, PreparedAttestedReceiptWriter,
    PreparedIngressRelation,
};

/// Typed refusals of the mailbox repository.
#[derive(Debug, Error)]
pub enum MailboxError {
    #[error("the durable mailbox requires PostgreSQL")]
    PostgresRequired,
    #[error("the durable mailbox has no definer API; direct statements are refused under it")]
    DefinerApiUnsupported,
    #[error(transparent)]
    DigestCollision(#[from] MailboxLaneDigestCollision),
    #[error(transparent)]
    Codec(#[from] MailboxLaneCodecError),
    #[error(transparent)]
    Sequence(#[from] MailboxLaneSequenceError),
    #[error(transparent)]
    Cursor(#[from] MailboxCursorError),
    #[error(transparent)]
    Disposition(#[from] MailboxDispositionError),
    #[error(transparent)]
    StaleFence(#[from] StaleMailboxLaneFence),
    #[error(transparent)]
    Admission(#[from] MailboxAdmissionRefusal),
    #[error(transparent)]
    Cutover(#[from] MailboxCutoverRefusal),
    #[error("stored mailbox row is malformed")]
    InvalidStored,
    #[error("mailbox lane is missing")]
    LaneMissing,
    #[error("mailbox entry is missing")]
    EntryMissing,
    #[error("mailbox entry belongs to another lane than the one locked")]
    LaneMismatch,
    #[error("mailbox entry's canonical message or room request is missing")]
    ParentMissing,
    #[error("mailbox entry is dead-lettered and awaits operator repair")]
    DeadLettered,
    #[error("mailbox entry is not dead-lettered")]
    NotDeadLettered,
    #[error("mailbox entry already carries an owner decision")]
    NotPending,
    #[error("mailbox lane counter changed under its row lock")]
    CounterConflict,
}

/// Mailbox rows that fit neither side of the cutover to mailbox acceptance.
///
/// Below the mailbox epoch no commit appends or decides an entry, so an
/// entry that still awaits its owner or its origin can never be finished.
/// At any epoch an entry belongs to a canonical message that is bound to its
/// source and has an open or latched owner decision; an entry without them
/// was not written by the mailbox's own commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "mailbox state fits neither the relay nor the mailbox era at protocol epoch {live:?}: \
     {unfinished_below_epoch} unfinished entries below the mailbox epoch, \
     {without_owner_decision} entries whose message has no source binding or owner decision"
)]
pub struct MailboxCutoverRefusal {
    pub live: waddle_xmpp::ingress::ProtocolEpoch,
    pub unfinished_below_epoch: u64,
    pub without_owner_decision: u64,
}

/// Storage identity of one lane row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MailboxLaneId(Uuid);

impl MailboxLaneId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    fn to_storage(self) -> String {
        self.0.to_string()
    }

    fn from_storage(value: &str) -> Result<Self, MailboxError> {
        value
            .parse()
            .map(Self)
            .map_err(|_| MailboxError::InvalidStored)
    }
}

/// Why an entry was parked at its lane head for an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxDeadLetterReason {
    /// The bounded retry budget was spent without an owner decision.
    RetriesExhausted,
    /// The canonical evidence the entry refers to cannot be decoded or
    /// contradicts the entry.
    CanonicalEvidenceInvalid,
    /// An operator parked the entry.
    OperatorHold,
}

impl MailboxDeadLetterReason {
    fn to_storage(self) -> i32 {
        match self {
            Self::RetriesExhausted => 1,
            Self::CanonicalEvidenceInvalid => 2,
            Self::OperatorHold => 3,
        }
    }

    fn from_storage(value: i32) -> Result<Self, MailboxError> {
        match value {
            1 => Ok(Self::RetriesExhausted),
            2 => Ok(Self::CanonicalEvidenceInvalid),
            3 => Ok(Self::OperatorHold),
            _ => Err(MailboxError::InvalidStored),
        }
    }
}

/// One stored mailbox entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredMailboxEntry {
    pub lane: MailboxLaneId,
    pub sequence: MailboxLaneSequence,
    pub operation: MailboxOperationKey,
    pub disposition: MailboxEntryDisposition,
    pub attempts: u32,
    pub dead_letter: Option<MailboxDeadLetterReason>,
}

/// Result of an append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxAppendOutcome {
    /// A new entry took the next sequence of the locked lane.
    Appended(StoredMailboxEntry),
    /// The operation already has an entry, possibly on another lane. Nothing
    /// was allocated and the entry was not moved.
    Existing(StoredMailboxEntry),
}

impl MailboxAppendOutcome {
    pub fn entry(&self) -> &StoredMailboxEntry {
        match self {
            Self::Appended(entry) | Self::Existing(entry) => entry,
        }
    }
}

/// A lane row locked `FOR UPDATE` by the current transaction, with the
/// counter, cursor and fence read under that lock.
///
/// The value is only produced by the lane-locking operations and is consumed
/// by reference, so an entry write cannot precede its lane lock.
#[derive(Debug)]
pub struct LockedMailboxLane {
    id: MailboxLaneId,
    key: MailboxLaneKey,
    last_sequence: MailboxLaneSequence,
    cursor: MailboxConsumptionCursor,
    fence: MailboxLaneFence,
    transaction_identity: Uuid,
}

impl LockedMailboxLane {
    pub fn id(&self) -> MailboxLaneId {
        self.id
    }

    pub fn key(&self) -> &MailboxLaneKey {
        &self.key
    }

    pub fn last_sequence(&self) -> MailboxLaneSequence {
        self.last_sequence
    }

    pub fn cursor(&self) -> MailboxConsumptionCursor {
        self.cursor
    }

    pub fn fence(&self) -> &MailboxLaneFence {
        &self.fence
    }

    /// Entries appended and not yet dispositioned by the lane's target.
    pub fn pending(&self) -> u64 {
        self.last_sequence
            .get()
            .saturating_sub(self.cursor.last_dispositioned().get())
    }
}

/// The target side of a lane fence: the claim epoch of the lane's recipient
/// and the node incarnation holding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxTargetFence {
    pub target_epoch: ClaimEpoch,
    pub target_owner: NodeIdentity,
}

const LANE_COLUMNS: &str = "lane_id::text, digest_version::int, digest, encoding, \
    last_sequence::text, last_dispositioned::text, origin_epoch, target_epoch, \
    target_node_id, target_node_epoch";
const ENTRY_COLUMNS: &str = "lane_id::text, sequence::text, operation_kind::int, \
    message_key::text, disposition::int, attempt_count, dead_letter_reason::int, \
    request_key::text";

const OPERATION_ROOM_ACCEPTANCE: i32 = 1;
const OPERATION_ROOM_JOIN: i32 = 2;
const OPERATION_ROOM_PRESENCE: i32 = 3;
const OPERATION_ROOM_LEAVE: i32 = 4;
const OPERATION_ROOM_IQ: i32 = 5;

fn operation_kind(operation: &MailboxOperationKey) -> i32 {
    match operation {
        MailboxOperationKey::RoomAcceptance(_) => OPERATION_ROOM_ACCEPTANCE,
        MailboxOperationKey::RoomJoin(_) => OPERATION_ROOM_JOIN,
        MailboxOperationKey::RoomPresence(_) => OPERATION_ROOM_PRESENCE,
        MailboxOperationKey::RoomLeave(_) => OPERATION_ROOM_LEAVE,
        MailboxOperationKey::RoomIq(_) => OPERATION_ROOM_IQ,
    }
}

/// The parent column an operation's entry is found by, with the key stored
/// in it. Exactly one parent column of an entry is set.
fn parent_column(operation: &MailboxOperationKey) -> (&'static str, String) {
    match operation.parent() {
        MailboxOperationParent::Message(key) => ("message_key", key.to_storage().to_string()),
        MailboxOperationParent::Request(key) => ("request_key", key.to_storage().to_string()),
    }
}

fn disposition_to_storage(disposition: MailboxEntryDisposition) -> i32 {
    match disposition {
        MailboxEntryDisposition::Pending => 0,
        MailboxEntryDisposition::OwnerAccepted => 1,
        MailboxEntryDisposition::OwnerRejected => 2,
        MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Accepted) => 3,
        MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Rejected) => 4,
    }
}

fn disposition_from_storage(value: i32) -> Result<MailboxEntryDisposition, MailboxError> {
    match value {
        0 => Ok(MailboxEntryDisposition::Pending),
        1 => Ok(MailboxEntryDisposition::OwnerAccepted),
        2 => Ok(MailboxEntryDisposition::OwnerRejected),
        3 => Ok(MailboxEntryDisposition::Consumed(
            MailboxOwnerDecision::Accepted,
        )),
        4 => Ok(MailboxEntryDisposition::Consumed(
            MailboxOwnerDecision::Rejected,
        )),
        _ => Err(MailboxError::InvalidStored),
    }
}

/// Schema-qualified relation names for one transaction.
struct Relations {
    lanes: String,
    entries: String,
}

fn relations(
    tx: &Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<Relations, MailboxError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(MailboxError::PostgresRequired);
    }
    if prepared.is_some_and(PreparedAttestedReceiptWriter::uses_definer_api_candidate) {
        return Err(MailboxError::DefinerApiUnsupported);
    }
    Ok(Relations {
        lanes: prepared_ingress_relation(prepared, PreparedIngressRelation::MailboxLanes),
        entries: prepared_ingress_relation(prepared, PreparedIngressRelation::MailboxEntries),
    })
}

fn decode_entry(row: &crate::db::Row) -> Result<StoredMailboxEntry, IngressUowError> {
    let lane: String = row.get(0)?;
    let sequence: String = row.get(1)?;
    let kind: i32 = row.get(2)?;
    let message_key: Option<String> = row.get(3)?;
    let disposition: i32 = row.get(4)?;
    let attempts: i32 = row.get(5)?;
    let dead_letter: Option<i32> = row.get(6)?;
    let request_key: Option<String> = row.get(7)?;
    let parse = |value: String| {
        value
            .parse::<Uuid>()
            .map_err(|_| MailboxError::InvalidStored)
    };
    let request_kind = match kind {
        OPERATION_ROOM_ACCEPTANCE => None,
        OPERATION_ROOM_JOIN => Some(RoomRequestKind::Join),
        OPERATION_ROOM_PRESENCE => Some(RoomRequestKind::Presence),
        OPERATION_ROOM_LEAVE => Some(RoomRequestKind::Leave),
        OPERATION_ROOM_IQ => Some(RoomRequestKind::Iq),
        _ => return Err(MailboxError::InvalidStored.into()),
    };
    // The operation kind decides which parent the entry has; a row with the
    // other parent, both or neither is malformed.
    let operation = match (request_kind, message_key, request_key) {
        (None, Some(message_key), None) => {
            MailboxOperationKey::RoomAcceptance(MessageKey::from_storage(parse(message_key)?))
        }
        (Some(request_kind), None, Some(request_key)) => MailboxOperationKey::for_request(
            request_kind,
            RoomRequestKey::from_storage(parse(request_key)?),
        ),
        _ => return Err(MailboxError::InvalidStored.into()),
    };
    Ok(StoredMailboxEntry {
        lane: MailboxLaneId::from_storage(&lane)?,
        sequence: MailboxLaneSequence::from_storage(&sequence).map_err(MailboxError::from)?,
        operation,
        disposition: disposition_from_storage(disposition)?,
        attempts: u32::try_from(attempts).map_err(|_| MailboxError::InvalidStored)?,
        dead_letter: dead_letter
            .map(MailboxDeadLetterReason::from_storage)
            .transpose()?,
    })
}

fn decode_lane(
    row: &crate::db::Row,
    transaction_identity: Uuid,
) -> Result<LockedMailboxLane, IngressUowError> {
    let id: String = row.get(0)?;
    let digest_version: i32 = row.get(1)?;
    let digest: Vec<u8> = row.get(2)?;
    let encoding: Vec<u8> = row.get(3)?;
    let last_sequence: String = row.get(4)?;
    let last_dispositioned: String = row.get(5)?;
    let origin_epoch: i64 = row.get(6)?;
    let target_epoch: i64 = row.get(7)?;
    let target_node_id: String = row.get(8)?;
    let target_node_epoch: String = row.get(9)?;
    let digest_version = u8::try_from(digest_version).map_err(|_| MailboxError::InvalidStored)?;
    let key = MailboxLaneKey::from_storage(
        MailboxLaneDigest::from_storage(digest_version, &digest).map_err(MailboxError::from)?,
        MailboxLaneEncoding::from_storage(&encoding).map_err(MailboxError::from)?,
    );
    let last_sequence =
        MailboxLaneSequence::from_storage(&last_sequence).map_err(MailboxError::from)?;
    let cursor = MailboxConsumptionCursor::from_storage(
        MailboxLaneSequence::from_storage(&last_dispositioned).map_err(MailboxError::from)?,
    );
    if cursor.last_dispositioned() > last_sequence {
        return Err(MailboxError::InvalidStored.into());
    }
    Ok(LockedMailboxLane {
        id: MailboxLaneId::from_storage(&id)?,
        key,
        last_sequence,
        cursor,
        fence: MailboxLaneFence {
            origin_epoch: ClaimEpoch(origin_epoch),
            target_epoch: ClaimEpoch(target_epoch),
            target_owner: NodeIdentity::new(target_node_id, target_node_epoch),
        },
        transaction_identity,
    })
}

/// How a presented fence relates to the one on file.
enum FenceOrder {
    Current,
    Supersedes,
}

/// Claim epochs come from one database sequence, so a later claim always
/// carries a larger epoch. A presentation that matches the stored fence is
/// current; one whose epochs are nowhere smaller and somewhere larger comes
/// from a later claim and replaces it; anything else is from a claim that has
/// already been superseded.
fn fence_order(
    stored: &MailboxLaneFence,
    presented: &MailboxLaneFence,
) -> Result<FenceOrder, StaleMailboxLaneFence> {
    if stored.verify(presented).is_ok() {
        return Ok(FenceOrder::Current);
    }
    let not_older = presented.origin_epoch >= stored.origin_epoch
        && presented.target_epoch >= stored.target_epoch;
    let newer = presented.origin_epoch > stored.origin_epoch
        || presented.target_epoch > stored.target_epoch;
    // An equal target epoch names the same claim, so its holder cannot differ.
    let same_claim_other_node = presented.target_epoch == stored.target_epoch
        && presented.target_owner != stored.target_owner;
    if not_older && newer && !same_claim_other_node {
        Ok(FenceOrder::Supersedes)
    } else {
        Err(StaleMailboxLaneFence)
    }
}

/// Typed, transaction-taking access to mailbox lanes and entries.
#[derive(Debug, Default, Clone, Copy)]
pub struct MailboxRepository;

impl MailboxRepository {
    /// Resolve the lane for `identity`, creating its row when absent, and
    /// lock it `FOR UPDATE`.
    ///
    /// `presented` is the fence the caller has proven against its claims in
    /// this transaction. A lane that records an older fence adopts it; a
    /// lane that already records a newer one refuses the caller as stale.
    /// Two identities that share a digest fail closed.
    pub async fn lock_lane(
        transaction: &mut IngressUowTransaction<'_>,
        identity: &MailboxLaneIdentity,
        presented: &MailboxLaneFence,
    ) -> Result<LockedMailboxLane, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let key = identity.key();
        let digest_version = i32::from(key.digest().version().to_storage());
        let encoding_version = i32::from(
            waddle_xmpp::ingress::mailbox::MailboxLaneEncodingVersion::CURRENT.to_storage(),
        );
        tx.execute(
            &format!(
                "INSERT INTO {} (lane_id, digest_version, digest, encoding_version, encoding, \
                 origin_epoch, target_epoch, target_node_id, target_node_epoch) \
                 VALUES (?::uuid, ?::smallint, ?, ?::smallint, ?, ?, ?, ?, ?) \
                 ON CONFLICT (digest_version, digest) DO NOTHING",
                relations.lanes
            ),
            crate::db_params![
                Uuid::new_v4().to_string(),
                digest_version,
                key.digest().to_storage().to_vec(),
                encoding_version,
                key.encoding().to_storage().to_vec(),
                presented.origin_epoch.0,
                presented.target_epoch.0,
                presented.target_owner.node_id.clone(),
                presented.target_owner.node_epoch.clone(),
            ],
        )
        .await?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {LANE_COLUMNS} FROM {} \
                     WHERE digest_version = ?::smallint AND digest = ? FOR UPDATE",
                    relations.lanes
                ),
                crate::db_params![digest_version, key.digest().to_storage().to_vec()],
            )
            .await?;
        let row = rows.next().await?.ok_or(MailboxError::LaneMissing)?;
        let mut lane = decode_lane(&row, transaction_identity)?;
        drop(rows);
        if !key.resolves_to(&lane.key).map_err(MailboxError::from)? {
            return Err(MailboxError::InvalidStored.into());
        }
        match fence_order(&lane.fence, presented).map_err(MailboxError::from)? {
            FenceOrder::Current => {}
            FenceOrder::Supersedes => {
                Self::write_fence(tx, &relations, lane.id, presented).await?;
                lane.fence = presented.clone();
            }
        }
        Ok(lane)
    }

    /// Lock an existing lane as its target. The caller has proven the
    /// recipient claim in this transaction; a lane that records an older
    /// target adopts it and one that records a newer target refuses it.
    pub async fn lock_lane_for_target(
        transaction: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        presented: &MailboxTargetFence,
    ) -> Result<LockedMailboxLane, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut locked = Self::lock_by_id(tx, &relations, lane, transaction_identity, false)
            .await?
            .ok_or(MailboxError::LaneMissing)?;
        let presented = MailboxLaneFence {
            origin_epoch: locked.fence.origin_epoch,
            target_epoch: presented.target_epoch,
            target_owner: presented.target_owner.clone(),
        };
        match fence_order(&locked.fence, &presented).map_err(MailboxError::from)? {
            FenceOrder::Current => {}
            FenceOrder::Supersedes => {
                Self::write_fence(tx, &relations, locked.id, &presented).await?;
                locked.fence = presented;
            }
        }
        Ok(locked)
    }

    /// Lock an existing lane as its origin, which must present the origin
    /// claim epoch on file.
    pub async fn lock_lane_for_origin(
        transaction: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        origin_epoch: ClaimEpoch,
    ) -> Result<LockedMailboxLane, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut locked = Self::lock_by_id(tx, &relations, lane, transaction_identity, false)
            .await?
            .ok_or(MailboxError::LaneMissing)?;
        if origin_epoch < locked.fence.origin_epoch {
            return Err(MailboxError::StaleFence(StaleMailboxLaneFence).into());
        }
        if origin_epoch > locked.fence.origin_epoch {
            let presented = MailboxLaneFence {
                origin_epoch,
                ..locked.fence.clone()
            };
            Self::write_fence(tx, &relations, locked.id, &presented).await?;
            locked.fence = presented;
        }
        Ok(locked)
    }

    /// Lock a lane a poller selected, or return `None` when another worker
    /// holds it or its backlog was drained meanwhile. A lane whose recorded
    /// target is not `target` is left alone. Claiming a lane confers no
    /// authority over its room: the owner's commit still asserts its claim.
    pub async fn claim_backlogged_lane(
        transaction: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        target: &NodeIdentity,
    ) -> Result<Option<LockedMailboxLane>, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let Some(locked) =
            Self::lock_by_id(tx, &relations, lane, transaction_identity, true).await?
        else {
            return Ok(None);
        };
        if locked.pending() == 0 || locked.fence.target_owner != *target {
            return Ok(None);
        }
        Ok(Some(locked))
    }

    async fn lock_by_id(
        tx: &mut EnteredTransaction<'_>,
        relations: &Relations,
        lane: MailboxLaneId,
        transaction_identity: Uuid,
        skip_locked: bool,
    ) -> Result<Option<LockedMailboxLane>, IngressUowError> {
        let suffix = if skip_locked {
            "FOR UPDATE SKIP LOCKED"
        } else {
            "FOR UPDATE"
        };
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {LANE_COLUMNS} FROM {} WHERE lane_id = ?::uuid {suffix}",
                    relations.lanes
                ),
                crate::db_params![lane.to_storage()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        decode_lane(&row, transaction_identity).map(Some)
    }

    async fn write_fence(
        tx: &mut EnteredTransaction<'_>,
        relations: &Relations,
        lane: MailboxLaneId,
        fence: &MailboxLaneFence,
    ) -> Result<(), IngressUowError> {
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET origin_epoch = ?, target_epoch = ?, target_node_id = ?, \
                     target_node_epoch = ? WHERE lane_id = ?::uuid",
                    relations.lanes
                ),
                crate::db_params![
                    fence.origin_epoch.0,
                    fence.target_epoch.0,
                    fence.target_owner.node_id.clone(),
                    fence.target_owner.node_epoch.clone(),
                    lane.to_storage(),
                ],
            )
            .await?;
        if updated != 1 {
            return Err(MailboxError::LaneMissing.into());
        }
        Ok(())
    }

    fn assert_locked_here(
        transaction: &IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
    ) -> Result<(), MailboxError> {
        if lane.transaction_identity == transaction.identity() {
            Ok(())
        } else {
            Err(MailboxError::LaneMissing)
        }
    }

    /// Read the entry of `operation` without locking it. This is the
    /// authoritative lookup after a lost reply: the operation key is unique
    /// across lanes, so the answer does not depend on which owner or lane a
    /// retry was planned for.
    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        operation: MailboxOperationKey,
    ) -> Result<Option<StoredMailboxEntry>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        Self::read_entry(tx, &relations, operation, false).await
    }

    async fn read_entry(
        tx: &mut EnteredTransaction<'_>,
        relations: &Relations,
        operation: MailboxOperationKey,
        for_update: bool,
    ) -> Result<Option<StoredMailboxEntry>, IngressUowError> {
        let suffix = if for_update { " FOR UPDATE" } else { "" };
        let (column, parent) = parent_column(&operation);
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {ENTRY_COLUMNS} FROM {} \
                     WHERE {column} = ?::uuid AND operation_kind = ?::smallint{suffix}",
                    relations.entries
                ),
                crate::db_params![parent, operation_kind(&operation)],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        decode_entry(&row).map(Some)
    }

    /// Pending entries of the node the lane targets, summed over its
    /// backlogged lanes. The read takes no lock, so it bounds admission
    /// without serializing unrelated lanes.
    pub async fn pending_load(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
    ) -> Result<MailboxPendingLoad, IngressUowError> {
        Self::assert_locked_here(transaction, lane)?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT COALESCE(SUM(last_sequence - last_dispositioned), 0)::text FROM {} \
                     WHERE target_node_id = ? AND last_dispositioned < last_sequence",
                    relations.lanes
                ),
                crate::db_params![lane.fence.target_owner.node_id.clone()],
            )
            .await?;
        let row = rows.next().await?.ok_or(MailboxError::InvalidStored)?;
        let node_pending: String = row.get(0)?;
        // The sum of many lanes can exceed one lane's range; saturate rather
        // than treat an over-full node as malformed.
        let node_pending = node_pending.parse::<u64>().unwrap_or(u64::MAX);
        Ok(MailboxPendingLoad {
            lane_pending: lane.pending(),
            node_pending,
        })
    }

    /// Lock the durable parent of `operation`: its canonical message or its
    /// room request. `false` when the parent is gone.
    async fn lock_parent(
        transaction: &mut IngressUowTransaction<'_>,
        operation: MailboxOperationKey,
    ) -> Result<bool, IngressUowError> {
        match operation.parent() {
            MailboxOperationParent::Message(key) => {
                CanonicalMessageRepository::lock(transaction, key).await
            }
            MailboxOperationParent::Request(key) => {
                RoomRequestRepository::lock_parent(transaction, key).await
            }
        }
    }

    /// Append `operation` to the locked lane.
    ///
    /// The operation's parent (its canonical message or room request) is
    /// locked before the operation is looked up, so two appenders holding
    /// different lanes serialize on the parent and at most one allocates. An operation that already has an entry returns it
    /// unchanged, on whichever lane it was first appended. Admission is
    /// checked before the counter moves; a refused append changes nothing.
    pub async fn append(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &mut LockedMailboxLane,
        operation: MailboxOperationKey,
        limits: &MailboxAdmissionLimits,
    ) -> Result<MailboxAppendOutcome, IngressUowError> {
        Self::assert_locked_here(transaction, lane)?;
        if !Self::lock_parent(transaction, operation).await? {
            return Err(MailboxError::ParentMissing.into());
        }
        let load = Self::pending_load(transaction, lane).await?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        if let Some(existing) = Self::read_entry(tx, &relations, operation, false).await? {
            return Ok(MailboxAppendOutcome::Existing(existing));
        }
        limits.admit(load).map_err(MailboxError::from)?;
        let next = lane.last_sequence.successor().map_err(MailboxError::from)?;
        let moved = tx
            .execute(
                &format!(
                    "UPDATE {} SET last_sequence = ?::numeric \
                     WHERE lane_id = ?::uuid AND last_sequence = ?::numeric",
                    relations.lanes
                ),
                crate::db_params![
                    next.to_storage(),
                    lane.id.to_storage(),
                    lane.last_sequence.to_storage(),
                ],
            )
            .await?;
        if moved != 1 {
            return Err(MailboxError::CounterConflict.into());
        }
        let (column, parent) = parent_column(&operation);
        tx.execute(
            &format!(
                "INSERT INTO {} (lane_id, sequence, operation_kind, {column}) \
                 VALUES (?::uuid, ?::numeric, ?::smallint, ?::uuid)",
                relations.entries
            ),
            crate::db_params![
                lane.id.to_storage(),
                next.to_storage(),
                operation_kind(&operation),
                parent,
            ],
        )
        .await?;
        lane.last_sequence = next;
        Ok(MailboxAppendOutcome::Appended(StoredMailboxEntry {
            lane: lane.id,
            sequence: next,
            operation,
            disposition: MailboxEntryDisposition::Pending,
            attempts: 0,
            dead_letter: None,
        }))
    }

    /// Lock the entry of `operation` on the locked lane, after its parent.
    async fn lock_entry(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        operation: MailboxOperationKey,
    ) -> Result<StoredMailboxEntry, IngressUowError> {
        Self::assert_locked_here(transaction, lane)?;
        if !Self::lock_parent(transaction, operation).await? {
            return Err(MailboxError::ParentMissing.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let entry = Self::read_entry(tx, &relations, operation, true)
            .await?
            .ok_or(MailboxError::EntryMissing)?;
        if entry.lane != lane.id {
            return Err(MailboxError::LaneMismatch.into());
        }
        Ok(entry)
    }

    async fn write_disposition(
        transaction: &mut IngressUowTransaction<'_>,
        entry: &StoredMailboxEntry,
        next: MailboxEntryDisposition,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET disposition = ?::smallint \
                     WHERE lane_id = ?::uuid AND sequence = ?::numeric \
                       AND disposition = ?::smallint",
                    relations.entries
                ),
                crate::db_params![
                    disposition_to_storage(next),
                    entry.lane.to_storage(),
                    entry.sequence.to_storage(),
                    disposition_to_storage(entry.disposition),
                ],
            )
            .await?;
        if updated != 1 {
            return Err(MailboxError::EntryMissing.into());
        }
        Ok(())
    }

    /// Record the owner's decision on the entry at the head of the locked
    /// lane and move the lane's cursor past it, in the caller's transaction.
    ///
    /// The caller commits the canonical evidence of the decision (the sealed
    /// manifest or the owner rejection) in the same transaction. A repeated
    /// decision changes nothing; the opposite decision, an entry that is not
    /// the lane head, and a dead-lettered entry are refused.
    pub async fn decide(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &mut LockedMailboxLane,
        operation: MailboxOperationKey,
        decision: MailboxOwnerDecision,
    ) -> Result<MailboxDispositionChange, IngressUowError> {
        let entry = Self::lock_entry(transaction, lane, operation).await?;
        let change = entry
            .disposition
            .apply(MailboxEntryEvent::OwnerDecided(decision))
            .map_err(MailboxError::from)?;
        let MailboxDispositionChange::Applied(next) = change else {
            return Ok(change);
        };
        if entry.dead_letter.is_some() {
            return Err(MailboxError::DeadLettered.into());
        }
        let cursor = lane
            .cursor
            .advance_to(entry.sequence)
            .map_err(MailboxError::from)?;
        Self::write_disposition(transaction, &entry, next).await?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let moved = tx
            .execute(
                &format!(
                    "UPDATE {} SET last_dispositioned = ?::numeric \
                     WHERE lane_id = ?::uuid AND last_dispositioned = ?::numeric",
                    relations.lanes
                ),
                crate::db_params![
                    cursor.last_dispositioned().to_storage(),
                    lane.id.to_storage(),
                    lane.cursor.last_dispositioned().to_storage(),
                ],
            )
            .await?;
        if moved != 1 {
            return Err(MailboxError::CounterConflict.into());
        }
        lane.cursor = cursor;
        Ok(change)
    }

    /// Mark a decided entry consumed by its origin. The decision stays
    /// readable on the terminal entry.
    pub async fn consume(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        operation: MailboxOperationKey,
    ) -> Result<MailboxDispositionChange, IngressUowError> {
        let entry = Self::lock_entry(transaction, lane, operation).await?;
        let change = entry
            .disposition
            .apply(MailboxEntryEvent::Consumed)
            .map_err(MailboxError::from)?;
        if let MailboxDispositionChange::Applied(next) = change {
            Self::write_disposition(transaction, &entry, next).await?;
        }
        Ok(change)
    }

    /// Lanes that owe `target` a disposition, in lane-id order after `after`,
    /// at most `limit`. The read takes no lock and confers no authority: a
    /// worker must still claim each lane with
    /// [`Self::claim_backlogged_lane`].
    pub async fn scan_backlog(
        transaction: &mut IngressUowTransaction<'_>,
        target: &NodeIdentity,
        after: Option<MailboxLaneId>,
        limit: u32,
    ) -> Result<Vec<MailboxLaneId>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT lane_id::text FROM {} \
                     WHERE target_node_id = ? AND target_node_epoch = ? \
                       AND last_dispositioned < last_sequence \
                       AND (?::uuid IS NULL OR lane_id > ?::uuid) \
                     ORDER BY lane_id LIMIT ?",
                    relations.lanes
                ),
                crate::db_params![
                    target.node_id.clone(),
                    target.node_epoch.clone(),
                    after.map(MailboxLaneId::to_storage),
                    after.map(MailboxLaneId::to_storage),
                    i64::from(limit),
                ],
            )
            .await?;
        let mut lanes = Vec::new();
        while let Some(row) = rows.next().await? {
            let lane: String = row.get(0)?;
            lanes.push(MailboxLaneId::from_storage(&lane)?);
        }
        Ok(lanes)
    }

    /// Check that the mailbox rows on file can be explained by the cutover
    /// rule, before a node serves.
    ///
    /// The rule: a message committed below the mailbox epoch was dispatched
    /// through the ordered relay and acknowledged on the origin's commit; it
    /// has no entry, and its unreceipted dispatch belongs to ingress
    /// recovery. A message committed at the mailbox epoch or later has its
    /// source binding, an owner decision and one entry. Nothing is adopted
    /// from one era into the other and no decision is derived for a
    /// relay-era message. State that fits neither side is refused, never
    /// repaired by guessing.
    pub async fn verify_cutover_state(
        transaction: &mut IngressUowTransaction<'_>,
    ) -> Result<(), IngressUowError> {
        let live = transaction.protocol_epoch();
        let below_epoch = live < crate::ingress_substrate::mailbox_acceptance_epoch();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let messages = prepared_ingress_relation(prepared, PreparedIngressRelation::Messages);
        let mut rows = tx
            .query(
                &format!(
                    "SELECT \
                       (SELECT count(*) FROM {entries} WHERE disposition < 3), \
                       (SELECT count(*) FROM {entries} e JOIN {messages} m \
                          ON m.message_key = e.message_key \
                         WHERE m.source_binding_version IS NULL OR m.owner_decision IS NULL)",
                    entries = relations.entries,
                ),
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(MailboxError::InvalidStored)?;
        let unfinished: i64 = row.get(0)?;
        let without_owner_decision: i64 = row.get(1)?;
        drop(rows);
        let unfinished_below_epoch = if below_epoch { unfinished } else { 0 };
        if unfinished_below_epoch == 0 && without_owner_decision == 0 {
            return Ok(());
        }
        Err(MailboxError::Cutover(MailboxCutoverRefusal {
            live,
            unfinished_below_epoch: u64::try_from(unfinished_below_epoch)
                .map_err(|_| MailboxError::InvalidStored)?,
            without_owner_decision: u64::try_from(without_owner_decision)
                .map_err(|_| MailboxError::InvalidStored)?,
        })
        .into())
    }

    /// Backlogged lanes whose recorded target is not `target`, in lane-id
    /// order after `after`, at most `limit`, each with its lane key.
    ///
    /// A lane records the owner incarnation its origin last proved. When the
    /// recipient's claim moves, the successor finds the lane here, proves
    /// the claim and adopts the lane with
    /// [`Self::adopt_backlogged_lane`]. The read takes no lock and confers
    /// no authority.
    pub async fn scan_foreign_backlog(
        transaction: &mut IngressUowTransaction<'_>,
        target: &NodeIdentity,
        after: Option<MailboxLaneId>,
        limit: u32,
    ) -> Result<Vec<(MailboxLaneId, MailboxLaneKey)>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT lane_id::text, digest_version::int, digest, encoding FROM {} \
                     WHERE last_dispositioned < last_sequence \
                       AND NOT (target_node_id = ? AND target_node_epoch = ?) \
                       AND (?::uuid IS NULL OR lane_id > ?::uuid) \
                     ORDER BY lane_id LIMIT ?",
                    relations.lanes
                ),
                crate::db_params![
                    target.node_id.clone(),
                    target.node_epoch.clone(),
                    after.map(MailboxLaneId::to_storage),
                    after.map(MailboxLaneId::to_storage),
                    i64::from(limit),
                ],
            )
            .await?;
        let mut lanes = Vec::new();
        while let Some(row) = rows.next().await? {
            let lane: String = row.get(0)?;
            let digest_version: i32 = row.get(1)?;
            let digest: Vec<u8> = row.get(2)?;
            let encoding: Vec<u8> = row.get(3)?;
            let digest_version =
                u8::try_from(digest_version).map_err(|_| MailboxError::InvalidStored)?;
            lanes.push((
                MailboxLaneId::from_storage(&lane)?,
                MailboxLaneKey::from_storage(
                    MailboxLaneDigest::from_storage(digest_version, &digest)
                        .map_err(MailboxError::from)?,
                    MailboxLaneEncoding::from_storage(&encoding).map_err(MailboxError::from)?,
                ),
            ));
        }
        Ok(lanes)
    }

    /// Move a backlogged lane to the target claim `presented`, which the
    /// caller has read from the claims table in this transaction.
    ///
    /// `true` when the lane now records `presented`. A lane another worker
    /// holds, a drained lane, and a lane whose recorded target claim is not
    /// older than `presented` are left as they are. Adoption confers no
    /// authority over the recipient: the owner's commit still asserts its
    /// claim before it decides an entry.
    pub async fn adopt_backlogged_lane(
        transaction: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
        presented: &MailboxTargetFence,
    ) -> Result<bool, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let Some(locked) =
            Self::lock_by_id(tx, &relations, lane, transaction_identity, true).await?
        else {
            return Ok(false);
        };
        if locked.pending() == 0 {
            return Ok(false);
        }
        let presented = MailboxLaneFence {
            origin_epoch: locked.fence.origin_epoch,
            target_epoch: presented.target_epoch,
            target_owner: presented.target_owner.clone(),
        };
        match fence_order(&locked.fence, &presented) {
            Ok(FenceOrder::Current) => Ok(true),
            Ok(FenceOrder::Supersedes) => {
                Self::write_fence(tx, &relations, locked.id, &presented).await?;
                Ok(true)
            }
            Err(StaleMailboxLaneFence) => Ok(false),
        }
    }

    /// Decided room acceptances their origin has not consumed, appended at
    /// least `older_than` ago, in message-key order after `after`, at most
    /// `limit`. The read takes no lock.
    ///
    /// Entries of room requests are not returned: a decided request is
    /// consumed only by the connection that sent it, because consuming it
    /// hands over its response.
    pub async fn scan_unconsumed_decisions(
        transaction: &mut IngressUowTransaction<'_>,
        older_than: std::time::Duration,
        after: Option<MessageKey>,
        limit: u32,
    ) -> Result<Vec<StoredMailboxEntry>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let older_than_ms = i64::try_from(older_than.as_millis()).unwrap_or(i64::MAX);
        let after = after.map(|key| key.to_storage().to_string());
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {ENTRY_COLUMNS} FROM {} \
                     WHERE disposition IN (1, 2) AND message_key IS NOT NULL \
                       AND appended_at <= now() - (?::bigint * interval '1 millisecond') \
                       AND (?::uuid IS NULL OR message_key > ?::uuid) \
                     ORDER BY message_key LIMIT ?",
                    relations.entries
                ),
                crate::db_params![older_than_ms, after.clone(), after, i64::from(limit)],
            )
            .await?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            entries.push(decode_entry(&row)?);
        }
        Ok(entries)
    }

    /// Lock the lane of a decided entry for the sweep that consumes it on
    /// behalf of an origin that is gone. `None` when another transaction
    /// holds the lane. No fence is presented: consuming a decided entry
    /// changes neither the decision nor the lane's cursor.
    pub async fn claim_lane_for_sweep(
        transaction: &mut IngressUowTransaction<'_>,
        lane: MailboxLaneId,
    ) -> Result<Option<LockedMailboxLane>, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        Self::lock_by_id(tx, &relations, lane, transaction_identity, true).await
    }

    /// Count one failed attempt at the pending head entry and defer its next
    /// attempt by `retry_after`. Returns the attempts made so far.
    pub async fn record_attempt(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        operation: MailboxOperationKey,
        retry_after: std::time::Duration,
    ) -> Result<u32, IngressUowError> {
        let entry = Self::lock_entry(transaction, lane, operation).await?;
        if entry.disposition != MailboxEntryDisposition::Pending {
            return Ok(entry.attempts);
        }
        let attempts = entry.attempts.saturating_add(1);
        let retry_after_ms = i64::try_from(retry_after.as_millis()).unwrap_or(i64::MAX);
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET attempt_count = ?, \
                     next_attempt_at = now() + (? * interval '1 millisecond') \
                     WHERE lane_id = ?::uuid AND sequence = ?::numeric AND disposition = 0",
                    relations.entries
                ),
                crate::db_params![
                    i64::from(attempts),
                    retry_after_ms,
                    entry.lane.to_storage(),
                    entry.sequence.to_storage(),
                ],
            )
            .await?;
        if updated != 1 {
            return Err(MailboxError::EntryMissing.into());
        }
        Ok(attempts)
    }

    /// Whether the pending entry's deferred next attempt is still in the
    /// future by the database clock.
    pub async fn attempt_deferred(
        transaction: &mut IngressUowTransaction<'_>,
        entry: &StoredMailboxEntry,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT 1 FROM {} WHERE lane_id = ?::uuid AND sequence = ?::numeric \
                     AND next_attempt_at IS NOT NULL AND next_attempt_at > now()",
                    relations.entries
                ),
                crate::db_params![entry.lane.to_storage(), entry.sequence.to_storage()],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Park the pending head entry for an operator. The entry stays pending
    /// and the lane's cursor does not move, so nothing behind it is
    /// dispositioned until [`Self::repair_dead_letter`].
    pub async fn dead_letter(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        operation: MailboxOperationKey,
        reason: MailboxDeadLetterReason,
    ) -> Result<(), IngressUowError> {
        let entry = Self::lock_entry(transaction, lane, operation).await?;
        if entry.dead_letter.is_some() {
            return Ok(());
        }
        if entry.disposition != MailboxEntryDisposition::Pending {
            return Err(MailboxError::NotPending.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET dead_lettered_at = now(), dead_letter_reason = ?::smallint \
                     WHERE lane_id = ?::uuid AND sequence = ?::numeric \
                       AND disposition = 0 AND dead_lettered_at IS NULL",
                    relations.entries
                ),
                crate::db_params![
                    reason.to_storage(),
                    entry.lane.to_storage(),
                    entry.sequence.to_storage(),
                ],
            )
            .await?;
        if updated != 1 {
            return Err(MailboxError::EntryMissing.into());
        }
        Ok(())
    }

    /// Operator repair: return a dead-lettered entry to ordinary pending
    /// work with a fresh retry budget.
    pub async fn repair_dead_letter(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        operation: MailboxOperationKey,
    ) -> Result<(), IngressUowError> {
        let entry = Self::lock_entry(transaction, lane, operation).await?;
        if entry.dead_letter.is_none() {
            return Err(MailboxError::NotDeadLettered.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET dead_lettered_at = NULL, dead_letter_reason = NULL, \
                     attempt_count = 0, next_attempt_at = NULL \
                     WHERE lane_id = ?::uuid AND sequence = ?::numeric \
                       AND dead_lettered_at IS NOT NULL",
                    relations.entries
                ),
                crate::db_params![entry.lane.to_storage(), entry.sequence.to_storage()],
            )
            .await?;
        if updated != 1 {
            return Err(MailboxError::EntryMissing.into());
        }
        Ok(())
    }

    /// Record the top-level `xml:lang` of the request a pending entry
    /// carries. The canonical message does not retain it.
    pub async fn record_request_language(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        entry: &StoredMailboxEntry,
        language: &str,
    ) -> Result<(), IngressUowError> {
        Self::assert_locked_here(transaction, lane)?;
        if entry.lane != lane.id {
            return Err(MailboxError::LaneMismatch.into());
        }
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        tx.execute(
            &format!(
                "UPDATE {} SET request_language = ? \
                 WHERE lane_id = ?::uuid AND sequence = ?::numeric \
                   AND disposition = 0 AND request_language IS NULL",
                relations.entries
            ),
            crate::db_params![
                language.to_owned(),
                entry.lane.to_storage(),
                entry.sequence.to_storage(),
            ],
        )
        .await?;
        Ok(())
    }

    /// The top-level `xml:lang` recorded for an entry's request, if any.
    pub async fn request_language(
        transaction: &mut IngressUowTransaction<'_>,
        entry: &StoredMailboxEntry,
    ) -> Result<Option<String>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT request_language FROM {} \
                     WHERE lane_id = ?::uuid AND sequence = ?::numeric",
                    relations.entries
                ),
                crate::db_params![entry.lane.to_storage(), entry.sequence.to_storage()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(MailboxError::EntryMissing.into());
        };
        Ok(row.get(0)?)
    }

    /// Entries of the locked lane strictly after its cursor, in sequence
    /// order, at most `limit`.
    pub async fn read_after_cursor(
        transaction: &mut IngressUowTransaction<'_>,
        lane: &LockedMailboxLane,
        limit: u32,
    ) -> Result<Vec<StoredMailboxEntry>, IngressUowError> {
        Self::assert_locked_here(transaction, lane)?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {ENTRY_COLUMNS} FROM {} \
                     WHERE lane_id = ?::uuid AND sequence > ?::numeric \
                     ORDER BY sequence LIMIT ?",
                    relations.entries
                ),
                crate::db_params![
                    lane.id.to_storage(),
                    lane.cursor.last_dispositioned().to_storage(),
                    i64::from(limit),
                ],
            )
            .await?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next().await? {
            entries.push(decode_entry(&row)?);
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fence(origin: i64, target: i64, node: &str) -> MailboxLaneFence {
        MailboxLaneFence {
            origin_epoch: ClaimEpoch(origin),
            target_epoch: ClaimEpoch(target),
            target_owner: NodeIdentity::new(node, "incarnation"),
        }
    }

    #[test]
    fn equal_fence_is_current_and_a_later_claim_supersedes() {
        assert!(matches!(
            fence_order(&fence(3, 5, "a"), &fence(3, 5, "a")),
            Ok(FenceOrder::Current)
        ));
        assert!(matches!(
            fence_order(&fence(3, 5, "a"), &fence(3, 8, "b")),
            Ok(FenceOrder::Supersedes)
        ));
        assert!(matches!(
            fence_order(&fence(3, 5, "a"), &fence(4, 5, "a")),
            Ok(FenceOrder::Supersedes)
        ));
    }

    #[test]
    fn older_or_contradictory_fence_is_stale() {
        assert!(fence_order(&fence(3, 5, "a"), &fence(3, 4, "a")).is_err());
        assert!(fence_order(&fence(3, 5, "a"), &fence(2, 9, "b")).is_err());
        // The same target claim cannot be held by another node.
        assert!(fence_order(&fence(3, 5, "a"), &fence(4, 5, "b")).is_err());
    }

    #[test]
    fn stored_dispositions_round_trip() {
        for disposition in [
            MailboxEntryDisposition::Pending,
            MailboxEntryDisposition::OwnerAccepted,
            MailboxEntryDisposition::OwnerRejected,
            MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Accepted),
            MailboxEntryDisposition::Consumed(MailboxOwnerDecision::Rejected),
        ] {
            assert_eq!(
                disposition_from_storage(disposition_to_storage(disposition)).ok(),
                Some(disposition)
            );
        }
        assert!(disposition_from_storage(5).is_err());
    }
}
