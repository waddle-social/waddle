//! Canonical room requests, their frozen responses and durable room
//! occupancy.
//!
//! A room request is a MUC join, occupant presence update, leave or relayed
//! IQ that an origin hands to the room owner through a mailbox lane. The
//! request row is the parent of its mailbox entry, the way a canonical
//! message is the parent of a room acceptance.
//!
//! Every operation runs on the caller's [`IngressUowTransaction`], so each
//! write sits behind the typed ingress entry and its epoch proof. Nothing
//! here opens a transaction or waits on the network.
//!
//! Lock order inside one transaction is lane, then request, then entry, the
//! same as lane, canonical message, entry for a room acceptance. The
//! collector takes only request rows, with `SKIP LOCKED`, and reaches entries
//! and responses through the cascade; it never locks a lane.
//!
//! The tables exist only on PostgreSQL, and no definer API covers them.

use std::time::Duration;

use jid::{BareJid, FullJid};
use thiserror::Error;
use uuid::Uuid;
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::mailbox::{MailboxOwnerDecision, RoomRequestKey, RoomRequestKind};
use waddle_xmpp::ingress::{IngressOrdinal, SmIngressId};
use waddle_xmpp::ownership::ClaimEpoch;

use waddle_xmpp::Stanza;
use waddle_xmpp_core::{Affiliation, OccupancySessionGeneration, Role};
use xmpp_parsers::presence::Presence;

use super::{IngressUowError, IngressUowTransaction};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_substrate::{
    prepared_ingress_relation, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};

/// Largest serialized request or response stanza a row holds: the inbound
/// frame size limit.
pub const ROOM_REQUEST_STANZA_MAX_BYTES: usize = waddle_xmpp::protocol::frame::MAX_FRAME_SIZE;
/// Largest serialized presence an occupancy row holds.
pub const ROOM_OCCUPANT_PRESENCE_MAX_BYTES: usize = 16 * 1024;
/// How long a consumed request is kept after its origin settled it.
pub const ROOM_REQUEST_SETTLED_RETENTION: Duration = Duration::from_secs(3600);

/// Typed refusals of the room request repositories.
#[derive(Debug, Error)]
pub enum RoomRequestError {
    #[error("room requests require PostgreSQL")]
    PostgresRequired,
    #[error("room requests have no definer API; direct statements are refused under it")]
    DefinerApiUnsupported,
    #[error("stored room request row is malformed")]
    InvalidStored,
    #[error("room request is missing")]
    RequestMissing,
    #[error("a {kind:?} request cannot carry this stanza type")]
    StanzaKindMismatch { kind: RoomRequestKind },
    #[error("room request stanza of {bytes} bytes is outside the storable range")]
    StanzaSize { bytes: usize },
    #[error("room request stanza cannot be serialized")]
    StanzaUnserializable,
    #[error("room request key is already used by a different request")]
    KeyReuse,
    #[error("room request already records {recorded:?}, not {attempted:?}")]
    ConflictingDecision {
        recorded: MailboxOwnerDecision,
        attempted: MailboxOwnerDecision,
    },
    #[error("room request decision was latched by another transaction")]
    DecisionNotLatchedHere,
    #[error("frozen response has more stanzas than a request can hold")]
    TooManyResponses,
    #[error("groupchat sender has no durable occupancy in the room")]
    SenderNotSeated,
}

/// The authenticated origin of a room request: the principal reference and,
/// for a stream with stream management, its stream and ordinal. These are
/// the values a bound canonical message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomRequestSource {
    pub principal: AuthenticatedPrincipalRef,
    pub sm_occurrence: Option<(SmIngressId, IngressOrdinal)>,
}

/// A room request as its origin commits it.
#[derive(Debug, Clone)]
pub struct NewRoomRequest {
    pub key: RoomRequestKey,
    pub kind: RoomRequestKind,
    /// Bare JID of the room the request is for. For a Muji IQ this is the
    /// room the signalling names, which is not the stanza's `to`.
    pub room: BareJid,
    /// The requesting resource.
    pub occupant: FullJid,
    /// Generation of the connection that sent the request.
    pub generation: OccupancySessionGeneration,
    pub source: RoomRequestSource,
    /// `Stanza::Iq` for an IQ request and `Stanza::Presence` for every other
    /// kind.
    pub stanza: Stanza,
}

/// One stored room request.
#[derive(Debug, Clone)]
pub struct StoredRoomRequest {
    pub key: RoomRequestKey,
    pub kind: RoomRequestKind,
    pub room: BareJid,
    pub occupant: FullJid,
    pub generation: OccupancySessionGeneration,
    pub source: RoomRequestSource,
    pub stanza: Stanza,
    /// `None` while the room owner has not decided.
    pub decision: Option<MailboxOwnerDecision>,
    /// Whether the origin has handed the response to the requester.
    pub settled: bool,
}

/// Result of committing a request at its origin.
#[derive(Debug, Clone)]
pub enum RoomRequestInsert {
    Inserted,
    /// The key already names this same request, from an earlier attempt.
    Existing(Box<StoredRoomRequest>),
}

/// Proof that the current transaction moved a request's decision from
/// undecided. Only this value lets the frozen responses be written, so a
/// response is never stored without its decision.
#[derive(Debug)]
pub struct LatchedRoomDecision {
    key: RoomRequestKey,
    decision: MailboxOwnerDecision,
    transaction_identity: Uuid,
}

impl LatchedRoomDecision {
    pub fn key(&self) -> RoomRequestKey {
        self.key
    }

    pub fn decision(&self) -> MailboxOwnerDecision {
        self.decision
    }
}

/// Result of latching a decision.
#[derive(Debug)]
pub enum RoomDecisionLatch {
    /// The decision was recorded now; responses may be frozen with it.
    Applied(LatchedRoomDecision),
    /// The request already records this decision and its responses.
    Unchanged(MailboxOwnerDecision),
}

fn kind_to_storage(kind: RoomRequestKind) -> i32 {
    match kind {
        RoomRequestKind::Join => 1,
        RoomRequestKind::Presence => 2,
        RoomRequestKind::Leave => 3,
        RoomRequestKind::Iq => 4,
    }
}

fn kind_from_storage(value: i32) -> Result<RoomRequestKind, RoomRequestError> {
    match value {
        1 => Ok(RoomRequestKind::Join),
        2 => Ok(RoomRequestKind::Presence),
        3 => Ok(RoomRequestKind::Leave),
        4 => Ok(RoomRequestKind::Iq),
        _ => Err(RoomRequestError::InvalidStored),
    }
}

fn decision_to_storage(decision: MailboxOwnerDecision) -> i32 {
    match decision {
        MailboxOwnerDecision::Accepted => 1,
        MailboxOwnerDecision::Rejected => 2,
    }
}

fn decision_from_storage(value: i32) -> Result<Option<MailboxOwnerDecision>, RoomRequestError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(MailboxOwnerDecision::Accepted)),
        2 => Ok(Some(MailboxOwnerDecision::Rejected)),
        _ => Err(RoomRequestError::InvalidStored),
    }
}

fn role_to_storage(role: Role) -> i32 {
    match role {
        Role::None => 0,
        Role::Visitor => 1,
        Role::Participant => 2,
        Role::Moderator => 3,
    }
}

fn role_from_storage(value: i32) -> Option<Role> {
    match value {
        0 => Some(Role::None),
        1 => Some(Role::Visitor),
        2 => Some(Role::Participant),
        3 => Some(Role::Moderator),
        _ => None,
    }
}

fn affiliation_to_storage(affiliation: Affiliation) -> i32 {
    match affiliation {
        Affiliation::Outcast => 0,
        Affiliation::None => 1,
        Affiliation::Member => 2,
        Affiliation::Admin => 3,
        Affiliation::Owner => 4,
    }
}

fn affiliation_from_storage(value: i32) -> Option<Affiliation> {
    match value {
        0 => Some(Affiliation::Outcast),
        1 => Some(Affiliation::None),
        2 => Some(Affiliation::Member),
        3 => Some(Affiliation::Admin),
        4 => Some(Affiliation::Owner),
        _ => None,
    }
}

/// Serialize a stanza for a request or response row.
fn encode_stanza(stanza: &Stanza) -> Result<Vec<u8>, RoomRequestError> {
    let text = waddle_xmpp::parser::element_to_string(&stanza.to_element())
        .map_err(|_| RoomRequestError::StanzaUnserializable)?;
    let bytes = text.into_bytes();
    if bytes.is_empty() || bytes.len() > ROOM_REQUEST_STANZA_MAX_BYTES {
        return Err(RoomRequestError::StanzaSize { bytes: bytes.len() });
    }
    Ok(bytes)
}

/// Parse a stored stanza back into its typed form.
fn decode_stanza(bytes: &[u8]) -> Result<Stanza, RoomRequestError> {
    let text = std::str::from_utf8(bytes).map_err(|_| RoomRequestError::InvalidStored)?;
    let element: minidom::Element = text.parse().map_err(|_| RoomRequestError::InvalidStored)?;
    match element.name() {
        "message" => xmpp_parsers::message::Message::try_from(element)
            .map(Stanza::Message)
            .map_err(|_| RoomRequestError::InvalidStored),
        "presence" => Presence::try_from(element)
            .map(Stanza::Presence)
            .map_err(|_| RoomRequestError::InvalidStored),
        "iq" => xmpp_parsers::iq::Iq::try_from(element)
            .map(|iq| Stanza::Iq(Box::new(iq)))
            .map_err(|_| RoomRequestError::InvalidStored),
        _ => Err(RoomRequestError::InvalidStored),
    }
}

fn stanza_fits_kind(kind: RoomRequestKind, stanza: &Stanza) -> bool {
    match kind {
        RoomRequestKind::Join | RoomRequestKind::Presence | RoomRequestKind::Leave => {
            matches!(stanza, Stanza::Presence(_))
        }
        RoomRequestKind::Iq => matches!(stanza, Stanza::Iq(_)),
    }
}

/// Serialize an occupant's presence for its occupancy row. A presence above
/// the row's bound is stored without its extension payloads, and one that
/// still does not fit is not stored.
fn encode_occupant_presence(presence: &Presence) -> Option<Vec<u8>> {
    let serialize = |presence: &Presence| {
        waddle_xmpp::parser::element_to_string(&Stanza::Presence(presence.clone()).to_element())
            .ok()
            .map(String::into_bytes)
            .filter(|bytes| !bytes.is_empty() && bytes.len() <= ROOM_OCCUPANT_PRESENCE_MAX_BYTES)
    };
    serialize(presence).or_else(|| {
        let mut bare = presence.clone();
        bare.payloads.clear();
        serialize(&bare)
    })
}

/// Schema-qualified relation names for one transaction.
struct Relations {
    requests: String,
    responses: String,
    occupants: String,
    entries: String,
}

fn relations(
    tx: &Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<Relations, RoomRequestError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(RoomRequestError::PostgresRequired);
    }
    if prepared.is_some_and(PreparedAttestedReceiptWriter::uses_definer_api_candidate) {
        return Err(RoomRequestError::DefinerApiUnsupported);
    }
    Ok(Relations {
        requests: prepared_ingress_relation(prepared, PreparedIngressRelation::RoomRequests),
        responses: prepared_ingress_relation(prepared, PreparedIngressRelation::RoomResponses),
        occupants: prepared_ingress_relation(prepared, PreparedIngressRelation::RoomOccupants),
        entries: prepared_ingress_relation(prepared, PreparedIngressRelation::MailboxEntries),
    })
}

const REQUEST_COLUMNS: &str = "request_key::text, request_kind::int, room_jid, occupant_jid, \
    occupancy_generation, source_bare_jid, source_auth_context_id::text, \
    source_auth_context_version::text, source_auth_epoch::text, source_sm_ingress_id::text, \
    source_ingress_ordinal::text, stanza, owner_decision::int, (settled_at IS NOT NULL)";

fn decode_request(row: &crate::db::Row) -> Result<StoredRoomRequest, IngressUowError> {
    let invalid = || RoomRequestError::InvalidStored;
    let key: String = row.get(0)?;
    let kind: i32 = row.get(1)?;
    let room: String = row.get(2)?;
    let occupant: String = row.get(3)?;
    let generation: String = row.get(4)?;
    let source_bare: String = row.get(5)?;
    let context: String = row.get(6)?;
    let context_version: String = row.get(7)?;
    let auth_epoch: String = row.get(8)?;
    let sm: Option<String> = row.get(9)?;
    let ordinal: Option<String> = row.get(10)?;
    let stanza: Vec<u8> = row.get(11)?;
    let decision: i32 = row.get(12)?;
    let settled: bool = row.get(13)?;
    let kind = kind_from_storage(kind)?;
    let stanza = decode_stanza(&stanza)?;
    if !stanza_fits_kind(kind, &stanza) {
        return Err(invalid().into());
    }
    let context_version: u64 = context_version.parse().map_err(|_| invalid())?;
    let auth_epoch: u64 = auth_epoch.parse().map_err(|_| invalid())?;
    if context_version == 0 || auth_epoch == 0 {
        return Err(invalid().into());
    }
    let principal = AuthenticatedPrincipalRef::new(
        source_bare.parse().map_err(|_| invalid())?,
        waddle_xmpp::auth::AuthContextId::new(context.parse().map_err(|_| invalid())?),
        waddle_xmpp::auth::AuthContextVersion::new(context_version),
        waddle_xmpp::auth::PrincipalAuthEpoch::new(auth_epoch),
    );
    let sm_occurrence = match (sm, ordinal) {
        (None, None) => None,
        (Some(sm), Some(ordinal)) => Some((
            SmIngressId::from_storage(sm.parse().map_err(|_| invalid())?),
            IngressOrdinal::from_storage(ordinal.parse().map_err(|_| invalid())?)
                .map_err(|_| invalid())?,
        )),
        _ => return Err(invalid().into()),
    };
    Ok(StoredRoomRequest {
        key: RoomRequestKey::from_storage(key.parse().map_err(|_| invalid())?),
        kind,
        room: room.parse().map_err(|_| invalid())?,
        occupant: occupant.parse().map_err(|_| invalid())?,
        generation: generation.parse().map_err(|_| invalid())?,
        source: RoomRequestSource {
            principal,
            sm_occurrence,
        },
        stanza,
        decision: decision_from_storage(decision)?,
        settled,
    })
}

/// Typed, transaction-taking access to canonical room requests.
#[derive(Debug, Default, Clone, Copy)]
pub struct RoomRequestRepository;

impl RoomRequestRepository {
    /// Commit a request at its origin, undecided.
    ///
    /// A key that already names the same request (kind, room, occupant and
    /// generation) returns the stored row, so every retry of one attempt
    /// reads one request. A key that names a different request is refused.
    pub async fn insert(
        transaction: &mut IngressUowTransaction<'_>,
        request: &NewRoomRequest,
    ) -> Result<RoomRequestInsert, IngressUowError> {
        if !stanza_fits_kind(request.kind, &request.stanza) {
            return Err(RoomRequestError::StanzaKindMismatch { kind: request.kind }.into());
        }
        let stanza = encode_stanza(&request.stanza)?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let principal = &request.source.principal;
        let inserted = tx
            .execute(
                &format!(
                    "INSERT INTO {} (request_key, request_kind, room_jid, occupant_jid, \
                     occupancy_generation, source_binding_version, source_bare_jid, \
                     source_auth_context_id, source_auth_context_version, source_auth_epoch, \
                     source_sm_ingress_id, source_ingress_ordinal, stanza) \
                     VALUES (?::uuid, ?::smallint, ?, ?, ?, 1::smallint, ?, ?::uuid, ?::numeric, \
                     ?::numeric, ?::uuid, ?::numeric, ?) \
                     ON CONFLICT (request_key) DO NOTHING",
                    relations.requests
                ),
                crate::db_params![
                    request.key.to_storage().to_string(),
                    kind_to_storage(request.kind),
                    request.room.to_string(),
                    request.occupant.to_string(),
                    request.generation.to_string(),
                    principal.bare_jid().to_string(),
                    principal.auth_context_id().as_uuid().to_string(),
                    principal.auth_context_version().get().to_string(),
                    principal.auth_epoch().get().to_string(),
                    request
                        .source
                        .sm_occurrence
                        .map(|(id, _)| id.to_storage().to_string()),
                    request
                        .source
                        .sm_occurrence
                        .map(|(_, ordinal)| ordinal.to_storage().to_string()),
                    stanza,
                ],
            )
            .await?;
        if inserted == 1 {
            return Ok(RoomRequestInsert::Inserted);
        }
        let existing = Self::read(transaction, request.key)
            .await?
            .ok_or(RoomRequestError::RequestMissing)?;
        if existing.kind != request.kind
            || existing.room != request.room
            || existing.occupant != request.occupant
            || existing.generation != request.generation
            || existing.source.principal != request.source.principal
        {
            return Err(RoomRequestError::KeyReuse.into());
        }
        Ok(RoomRequestInsert::Existing(Box::new(existing)))
    }

    /// Read a request without locking it.
    pub async fn read(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
    ) -> Result<Option<StoredRoomRequest>, IngressUowError> {
        Self::select(transaction, key, false).await
    }

    /// Lock a request `FOR UPDATE` and read it. The caller holds the
    /// request's lane.
    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
    ) -> Result<Option<StoredRoomRequest>, IngressUowError> {
        Self::select(transaction, key, true).await
    }

    /// Lock a request row as the parent of a mailbox entry operation,
    /// without decoding it. `false` when the request is gone.
    pub(super) async fn lock_parent(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT 1 FROM {} WHERE request_key = ?::uuid FOR UPDATE",
                    relations.requests
                ),
                crate::db_params![key.to_storage().to_string()],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    async fn select(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
        for_update: bool,
    ) -> Result<Option<StoredRoomRequest>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let suffix = if for_update { " FOR UPDATE" } else { "" };
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {REQUEST_COLUMNS} FROM {} WHERE request_key = ?::uuid{suffix}",
                    relations.requests
                ),
                crate::db_params![key.to_storage().to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        decode_request(&row).map(Some)
    }

    /// Record the room owner's decision. It moves once, from undecided: a
    /// repeated decision changes nothing and the opposite one is refused.
    ///
    /// The caller holds the request's lane, has asserted its room claim, and
    /// records the same decision on the request's mailbox entry in this
    /// transaction.
    pub async fn latch_decision(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
        decision: MailboxOwnerDecision,
    ) -> Result<RoomDecisionLatch, IngressUowError> {
        let transaction_identity = transaction.identity();
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET owner_decision = ?::smallint \
                     WHERE request_key = ?::uuid AND owner_decision = 0",
                    relations.requests
                ),
                crate::db_params![decision_to_storage(decision), key.to_storage().to_string()],
            )
            .await?;
        if updated == 1 {
            return Ok(RoomDecisionLatch::Applied(LatchedRoomDecision {
                key,
                decision,
                transaction_identity,
            }));
        }
        let recorded = Self::read(transaction, key)
            .await?
            .ok_or(RoomRequestError::RequestMissing)?
            .decision
            .ok_or(RoomRequestError::InvalidStored)?;
        if recorded == decision {
            Ok(RoomDecisionLatch::Unchanged(recorded))
        } else {
            Err(RoomRequestError::ConflictingDecision {
                recorded,
                attempted: decision,
            }
            .into())
        }
    }

    /// Mark a decided request settled: its origin has consumed the entry and
    /// is handing the response to the requester. `true` when this call set
    /// it; a request that is undecided or already settled is left alone.
    pub async fn settle(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET settled_at = now() \
                     WHERE request_key = ?::uuid AND owner_decision <> 0 AND settled_at IS NULL",
                    relations.requests
                ),
                crate::db_params![key.to_storage().to_string()],
            )
            .await?;
        Ok(updated == 1)
    }

    /// Delete requests whose entry is consumed and that were settled at
    /// least `settled_for` ago, at most `limit`, oldest settlement first.
    /// Returns how many were deleted.
    ///
    /// The entry and the responses leave through the cascade. An undecided
    /// or unsettled request is never selected. Rows another transaction
    /// holds are skipped, and no lane is locked.
    pub async fn collect_settled(
        transaction: &mut IngressUowTransaction<'_>,
        settled_for: Duration,
        limit: u32,
    ) -> Result<u64, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let settled_for_ms = i64::try_from(settled_for.as_millis()).unwrap_or(i64::MAX);
        let deleted = tx
            .execute(
                &format!(
                    "DELETE FROM {requests} WHERE request_key IN ( \
                       SELECT r.request_key FROM {requests} r \
                       WHERE r.settled_at IS NOT NULL \
                         AND r.settled_at <= now() - (?::bigint * interval '1 millisecond') \
                         AND EXISTS (SELECT 1 FROM {entries} e \
                           WHERE e.request_key = r.request_key AND e.disposition >= 3) \
                       ORDER BY r.settled_at LIMIT ? FOR UPDATE OF r SKIP LOCKED)",
                    requests = relations.requests,
                    entries = relations.entries,
                ),
                crate::db_params![settled_for_ms, i64::from(limit)],
            )
            .await?;
        Ok(deleted)
    }

    /// Requests whose owner decided at least `older_than` ago by their
    /// creation time and whose origin has not settled them. These are the
    /// decisions no live connection will consume; they are reported, never
    /// consumed on the requester's behalf. The read takes no lock.
    pub async fn count_unsettled_decisions(
        transaction: &mut IngressUowTransaction<'_>,
        older_than: Duration,
    ) -> Result<u64, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let older_than_ms = i64::try_from(older_than.as_millis()).unwrap_or(i64::MAX);
        let mut rows = tx
            .query(
                &format!(
                    "SELECT count(*) FROM {} WHERE settled_at IS NULL AND owner_decision <> 0 \
                     AND created_at <= now() - (?::bigint * interval '1 millisecond')",
                    relations.requests
                ),
                crate::db_params![older_than_ms],
            )
            .await?;
        let row = rows.next().await?.ok_or(RoomRequestError::InvalidStored)?;
        let count: i64 = row.get(0)?;
        Ok(u64::try_from(count).map_err(|_| RoomRequestError::InvalidStored)?)
    }
}

/// Most stanzas one frozen response holds. A join's response is the room's
/// occupants, the self-presence, history and the subject.
pub const ROOM_RESPONSE_MAX_STANZAS: usize = 4096;

/// Typed, transaction-taking access to frozen responses.
#[derive(Debug, Default, Clone, Copy)]
pub struct RoomResponseRepository;

impl RoomResponseRepository {
    /// Store the response to a request in the order the requester must
    /// receive it. `latched` proves that this transaction recorded the
    /// decision, so the response and the decision commit together.
    pub async fn freeze(
        transaction: &mut IngressUowTransaction<'_>,
        latched: &LatchedRoomDecision,
        responses: &[Stanza],
    ) -> Result<(), IngressUowError> {
        if latched.transaction_identity != transaction.identity() {
            return Err(RoomRequestError::DecisionNotLatchedHere.into());
        }
        if responses.len() > ROOM_RESPONSE_MAX_STANZAS {
            return Err(RoomRequestError::TooManyResponses.into());
        }
        let encoded = responses
            .iter()
            .map(encode_stanza)
            .collect::<Result<Vec<_>, _>>()?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        for (position, stanza) in encoded.into_iter().enumerate() {
            let position =
                i64::try_from(position).map_err(|_| RoomRequestError::TooManyResponses)?;
            tx.execute(
                &format!(
                    "INSERT INTO {} (request_key, position, stanza) VALUES (?::uuid, ?, ?)",
                    relations.responses
                ),
                crate::db_params![latched.key.to_storage().to_string(), position, stanza],
            )
            .await?;
        }
        Ok(())
    }

    /// The frozen response to a request, in order.
    pub async fn read(
        transaction: &mut IngressUowTransaction<'_>,
        key: RoomRequestKey,
    ) -> Result<Vec<Stanza>, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT stanza FROM {} WHERE request_key = ?::uuid ORDER BY position",
                    relations.responses
                ),
                crate::db_params![key.to_storage().to_string()],
            )
            .await?;
        let mut responses = Vec::new();
        while let Some(row) = rows.next().await? {
            let stanza: Vec<u8> = row.get(0)?;
            responses.push(decode_stanza(&stanza)?);
        }
        Ok(responses)
    }
}

/// One occupying resource of a room as the room owner records it.
#[derive(Debug, Clone)]
pub struct RoomOccupantRecord {
    pub room: BareJid,
    pub occupant: FullJid,
    pub nick: String,
    pub role: Role,
    pub affiliation: Affiliation,
    /// Generation of the connection that joined.
    pub generation: OccupancySessionGeneration,
    /// The join that created the row; `None` once that request is collected.
    pub join_request: Option<RoomRequestKey>,
    /// The occupant's last available presence as the room broadcasts it.
    pub presence: Option<Presence>,
    /// Room claim epoch under which the row was last written.
    pub room_epoch: ClaimEpoch,
}

/// What the occupancy table holds for one resource of a room.
#[derive(Debug, Clone)]
pub enum RoomOccupantLookup {
    Absent,
    Seated(Box<RoomOccupantRecord>),
    /// A row exists and cannot be parsed. The occupant is not seated.
    Unparseable,
}

/// The occupancy a room's new owner recovers.
#[derive(Debug, Clone, Default)]
pub struct RoomOccupancyLoad {
    pub seated: Vec<RoomOccupantRecord>,
    /// Rows that could not be parsed and are therefore not seated.
    pub unparseable: u64,
}

const OCCUPANT_COLUMNS: &str = "room_jid, occupant_jid, nick, role::int, affiliation::int, \
    occupancy_generation, join_request::text, presence, room_epoch";

/// Decode an occupancy row; `None` when any field is not a valid value of
/// its type.
fn decode_occupant(row: &crate::db::Row) -> Result<Option<RoomOccupantRecord>, IngressUowError> {
    let room: String = row.get(0)?;
    let occupant: String = row.get(1)?;
    let nick: String = row.get(2)?;
    let role: i32 = row.get(3)?;
    let affiliation: i32 = row.get(4)?;
    let generation: String = row.get(5)?;
    let join_request: Option<String> = row.get(6)?;
    let presence: Option<Vec<u8>> = row.get(7)?;
    let room_epoch: i64 = row.get(8)?;
    let parsed = (|| {
        let presence = match presence {
            None => None,
            Some(bytes) => match decode_stanza(&bytes).ok()? {
                Stanza::Presence(presence) => Some(presence),
                Stanza::Message(_) | Stanza::Iq(_) => return None,
            },
        };
        let join_request = match join_request {
            None => None,
            Some(key) => Some(RoomRequestKey::from_storage(key.parse().ok()?)),
        };
        Some(RoomOccupantRecord {
            room: room.parse().ok()?,
            occupant: occupant.parse().ok()?,
            nick: (!nick.is_empty()).then_some(nick)?,
            role: role_from_storage(role)?,
            affiliation: affiliation_from_storage(affiliation)?,
            generation: generation.parse().ok()?,
            join_request,
            presence,
            room_epoch: ClaimEpoch(room_epoch),
        })
    })();
    Ok(parsed)
}

/// Typed, transaction-taking access to durable room occupancy. Only the room
/// owner writes it: its room actor records every seat it grants or removes
/// under the room claim, and the transaction that decides a room request
/// completes the requester's row.
#[derive(Debug, Default, Clone, Copy)]
pub struct RoomOccupantRepository;

impl RoomOccupantRepository {
    /// Record a seat the room actor granted, replacing whatever row the full
    /// JID had. A row of the same connection generation keeps its presence
    /// and its join request; a row of another generation loses both, because
    /// they belong to the connection it replaces.
    pub async fn record_seat(
        transaction: &mut IngressUowTransaction<'_>,
        occupant: &RoomOccupantRecord,
    ) -> Result<(), IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        tx.execute(
            &format!(
                "INSERT INTO {occupants} AS seat (room_jid, occupant_jid, nick, role, \
                 affiliation, occupancy_generation, room_epoch) \
                 VALUES (?, ?, ?, ?::smallint, ?::smallint, ?, ?) \
                 ON CONFLICT (room_jid, occupant_jid) DO UPDATE SET nick = EXCLUDED.nick, \
                 role = EXCLUDED.role, affiliation = EXCLUDED.affiliation, \
                 join_request = CASE WHEN seat.occupancy_generation = \
                   EXCLUDED.occupancy_generation THEN seat.join_request END, \
                 presence = CASE WHEN seat.occupancy_generation = \
                   EXCLUDED.occupancy_generation THEN seat.presence END, \
                 occupancy_generation = EXCLUDED.occupancy_generation, \
                 room_epoch = EXCLUDED.room_epoch",
                occupants = relations.occupants
            ),
            crate::db_params![
                occupant.room.to_string(),
                occupant.occupant.to_string(),
                occupant.nick.clone(),
                role_to_storage(occupant.role),
                affiliation_to_storage(occupant.affiliation),
                occupant.generation.to_string(),
                occupant.room_epoch.0,
            ],
        )
        .await?;
        Ok(())
    }

    /// Complete the row of an accepted join with the request that created
    /// it and the presence the room broadcast for it. The room actor
    /// recorded the seat before the join's projection committed; this
    /// writes nothing when that row is gone or belongs to another
    /// connection generation, so an occupant removed since is not seated
    /// again. `true` when the row was completed.
    pub async fn confirm_join(
        transaction: &mut IngressUowTransaction<'_>,
        occupant: &RoomOccupantRecord,
    ) -> Result<bool, IngressUowError> {
        let presence = occupant
            .presence
            .as_ref()
            .and_then(encode_occupant_presence);
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET nick = ?, role = ?::smallint, affiliation = ?::smallint, \
                     join_request = ?::uuid, presence = ?, room_epoch = ? \
                     WHERE room_jid = ? AND occupant_jid = ? AND occupancy_generation = ?",
                    relations.occupants
                ),
                crate::db_params![
                    occupant.nick.clone(),
                    role_to_storage(occupant.role),
                    affiliation_to_storage(occupant.affiliation),
                    occupant
                        .join_request
                        .map(|key| key.to_storage().to_string()),
                    presence,
                    occupant.room_epoch.0,
                    occupant.room.to_string(),
                    occupant.occupant.to_string(),
                    occupant.generation.to_string(),
                ],
            )
            .await?;
        Ok(updated == 1)
    }

    /// Rewrite the presence, nick, role and affiliation of the row a
    /// presence update was accepted for. `false` when the full JID has no
    /// row of `generation`, which makes the update stale.
    pub async fn update_presence(
        transaction: &mut IngressUowTransaction<'_>,
        occupant: &RoomOccupantRecord,
    ) -> Result<bool, IngressUowError> {
        let presence = occupant
            .presence
            .as_ref()
            .and_then(encode_occupant_presence);
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET nick = ?, role = ?::smallint, affiliation = ?::smallint, \
                     presence = ?, room_epoch = ? \
                     WHERE room_jid = ? AND occupant_jid = ? AND occupancy_generation = ?",
                    relations.occupants
                ),
                crate::db_params![
                    occupant.nick.clone(),
                    role_to_storage(occupant.role),
                    affiliation_to_storage(occupant.affiliation),
                    presence,
                    occupant.room_epoch.0,
                    occupant.room.to_string(),
                    occupant.occupant.to_string(),
                    occupant.generation.to_string(),
                ],
            )
            .await?;
        Ok(updated == 1)
    }

    /// Remove the row of a departed occupant. With a generation only that
    /// connection's row is removed, so a leave from an earlier connection
    /// cannot unseat a later one; without one (the owner's own removal of a
    /// ghost, a kick or a ban) the full JID's row is removed whatever its
    /// generation. `true` when a row was deleted.
    pub async fn remove(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
        occupant: &FullJid,
        generation: Option<OccupancySessionGeneration>,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let generation = generation.map(|generation| generation.to_string());
        let deleted = tx
            .execute(
                &format!(
                    "DELETE FROM {} WHERE room_jid = ? AND occupant_jid = ? \
                     AND (?::text IS NULL OR occupancy_generation = ?::text)",
                    relations.occupants
                ),
                crate::db_params![
                    room.to_string(),
                    occupant.to_string(),
                    generation.clone(),
                    generation,
                ],
            )
            .await?;
        Ok(deleted == 1)
    }

    /// The row of one resource in a room, without a lock.
    pub async fn lookup(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
        occupant: &FullJid,
    ) -> Result<RoomOccupantLookup, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {OCCUPANT_COLUMNS} FROM {} WHERE room_jid = ? AND occupant_jid = ?",
                    relations.occupants
                ),
                crate::db_params![room.to_string(), occupant.to_string()],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(RoomOccupantLookup::Absent);
        };
        Ok(match decode_occupant(&row)? {
            Some(record) => RoomOccupantLookup::Seated(Box::new(record)),
            None => RoomOccupantLookup::Unparseable,
        })
    }

    /// Whether any resource of `sender` occupies `room`. The owner's
    /// groupchat commit refuses a sender for which this is false.
    pub async fn bare_jid_occupies(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
        sender: &BareJid,
    ) -> Result<bool, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        // A full JID is its bare JID, a slash and the resource; `left` and
        // `length` count characters, so the prefix test is exact.
        let prefix = format!("{sender}/");
        let mut rows = tx
            .query(
                &format!(
                    "SELECT 1 FROM {} WHERE room_jid = ? \
                     AND left(occupant_jid, length(?::text)) = ?::text LIMIT 1",
                    relations.occupants
                ),
                crate::db_params![room.to_string(), prefix.clone(), prefix],
            )
            .await?;
        Ok(rows.next().await?.is_some())
    }

    /// Every row of `room`, for the node that has taken the room's claim.
    /// A row that cannot be parsed is counted and not seated.
    pub async fn load_room(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
    ) -> Result<RoomOccupancyLoad, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let mut rows = tx
            .query(
                &format!(
                    "SELECT {OCCUPANT_COLUMNS} FROM {} WHERE room_jid = ? ORDER BY occupant_jid",
                    relations.occupants
                ),
                crate::db_params![room.to_string()],
            )
            .await?;
        let mut load = RoomOccupancyLoad::default();
        while let Some(row) = rows.next().await? {
            match decode_occupant(&row)? {
                Some(record) => load.seated.push(record),
                None => load.unparseable += 1,
            }
        }
        Ok(load)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_codes_round_trip_and_reject_unknown_values() {
        for kind in [
            RoomRequestKind::Join,
            RoomRequestKind::Presence,
            RoomRequestKind::Leave,
            RoomRequestKind::Iq,
        ] {
            assert_eq!(kind_from_storage(kind_to_storage(kind)).ok(), Some(kind));
        }
        assert!(kind_from_storage(0).is_err());
        assert!(kind_from_storage(5).is_err());
        for role in [
            Role::None,
            Role::Visitor,
            Role::Participant,
            Role::Moderator,
        ] {
            assert_eq!(role_from_storage(role_to_storage(role)), Some(role));
        }
        assert_eq!(role_from_storage(4), None);
        for affiliation in [
            Affiliation::Outcast,
            Affiliation::None,
            Affiliation::Member,
            Affiliation::Admin,
            Affiliation::Owner,
        ] {
            assert_eq!(
                affiliation_from_storage(affiliation_to_storage(affiliation)),
                Some(affiliation)
            );
        }
        assert_eq!(affiliation_from_storage(5), None);
        assert_eq!(decision_from_storage(0).ok(), Some(None));
        for decision in [
            MailboxOwnerDecision::Accepted,
            MailboxOwnerDecision::Rejected,
        ] {
            assert_eq!(
                decision_from_storage(decision_to_storage(decision)).ok(),
                Some(Some(decision))
            );
        }
        assert!(decision_from_storage(3).is_err());
    }

    #[test]
    fn stanza_type_must_fit_the_request_kind() {
        let presence = Stanza::Presence(Presence::new(xmpp_parsers::presence::Type::None));
        let iq = Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::from_get(
            "id",
            xmpp_parsers::ping::Ping,
        )));
        for kind in [
            RoomRequestKind::Join,
            RoomRequestKind::Presence,
            RoomRequestKind::Leave,
        ] {
            assert!(stanza_fits_kind(kind, &presence));
            assert!(!stanza_fits_kind(kind, &iq));
        }
        assert!(stanza_fits_kind(RoomRequestKind::Iq, &iq));
        assert!(!stanza_fits_kind(RoomRequestKind::Iq, &presence));
    }

    #[test]
    fn stored_stanza_round_trips_and_malformed_bytes_are_refused() {
        let mut presence = Presence::new(xmpp_parsers::presence::Type::None);
        presence.set_status(String::new(), "here".to_owned());
        let bytes = encode_stanza(&Stanza::Presence(presence)).expect("encode");
        assert!(matches!(decode_stanza(&bytes), Ok(Stanza::Presence(_))));
        assert!(decode_stanza(b"<not-a-stanza xmlns='jabber:client'/>").is_err());
        assert!(decode_stanza(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn oversized_presence_is_stored_without_its_payloads() {
        let mut presence = Presence::new(xmpp_parsers::presence::Type::None);
        let filler = "x".repeat(ROOM_OCCUPANT_PRESENCE_MAX_BYTES);
        presence.payloads.push(
            minidom::Element::builder("blob", "urn:waddle:test:blob")
                .append(filler)
                .build(),
        );
        let stored = encode_occupant_presence(&presence).expect("stripped presence fits");
        assert!(stored.len() <= ROOM_OCCUPANT_PRESENCE_MAX_BYTES);
        let Ok(Stanza::Presence(decoded)) = decode_stanza(&stored) else {
            panic!("stored presence must decode");
        };
        assert!(decoded.payloads.is_empty());
    }
}
