//! Direct requests: stanzas an origin hands, through a mailbox lane, to the
//! node that owns the recipient's account.
//!
//! A direct request stores what the ordered relay envelope would have
//! carried for a bare-JID or full-JID recipient: the relay payload and the
//! claim entity the origin presented as sender. The request row is the
//! parent of its mailbox entry, the way a room request is.
//!
//! Every operation runs on the caller's [`IngressUowTransaction`], so each
//! write sits behind the typed ingress entry and its epoch proof. Nothing
//! here opens a transaction or waits on the network.
//!
//! Lock order inside one transaction is lane, then request, then entry. The
//! collector takes only request rows, with `SKIP LOCKED`, and reaches
//! entries through the cascade; it never locks a lane.
//!
//! A request may name a target: the node that hosts the recipient's
//! binding and that binding. Such a request is decided by the hosting node,
//! not by the account owner.
//!
//! The table exists only on PostgreSQL, and no definer API covers it.

use std::time::Duration;

use jid::Jid;
use thiserror::Error;
use waddle_xmpp::ingress::mailbox::{DirectRequestKey, MailboxOwnerDecision};
use waddle_xmpp::ingress::MessageKey;
use waddle_xmpp::ownership::{BindingRef, Entity, NodeIdentity};

use super::{IngressUowError, IngressUowTransaction};
use crate::clustering::ordered_relay::OrderedRelayPayload;
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_substrate::{
    prepared_ingress_relation, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};

/// Largest serialized payload a row holds: the inbound frame size limit and
/// room for the relay's field names, the recipient and an append obligation.
pub const DIRECT_REQUEST_PAYLOAD_MAX_BYTES: usize =
    waddle_xmpp::protocol::frame::MAX_FRAME_SIZE + 64 * 1024;
/// How long a consumed request is kept after its origin settled it.
pub const DIRECT_REQUEST_SETTLED_RETENTION: Duration = Duration::from_secs(3600);

/// Typed refusals of the direct request repository.
#[derive(Debug, Error)]
pub enum DirectRequestError {
    #[error("direct requests require PostgreSQL")]
    PostgresRequired,
    #[error("direct requests have no definer API; direct statements are refused under it")]
    DefinerApiUnsupported,
    #[error("stored direct request row is malformed")]
    InvalidStored,
    #[error("direct request is missing")]
    RequestMissing,
    #[error("the payload is not addressed to the request's recipient")]
    RecipientMismatch,
    #[error("a room payload cannot travel as a direct request")]
    RoomPayload,
    #[error("direct request payload of {bytes} bytes is outside the storable range")]
    PayloadSize { bytes: usize },
    #[error("direct request payload cannot be serialized")]
    PayloadUnserializable,
    #[error("direct request key is already used by a different request")]
    KeyReuse,
    #[error("only the target node of a direct request decides it, except to refuse it as hosted elsewhere")]
    NotTargetNode,
    #[error("direct request already records {recorded:?}, not {attempted:?}")]
    ConflictingDecision {
        recorded: DirectDecision,
        attempted: DirectDecision,
    },
}

/// Why the deciding node rejected a direct request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectRefusal {
    /// Nothing took the stanza: the resource is not present, or the
    /// recipient pipeline delivered to nobody.
    NotDelivered,
    /// The request names a target node other than the node that worked
    /// it, which cannot take custody for the target's binding. A lane
    /// adopted by a new account owner after its claim moved is refused
    /// this way, and its origin resolves the binding again.
    HostedElsewhere,
}

/// The node a request is addressed to and the exact binding it hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectTarget {
    pub node: NodeIdentity,
    pub binding: BindingRef,
}

/// The account owner's decision on a direct request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectDecision {
    Accepted,
    Rejected(DirectRefusal),
}

impl DirectDecision {
    /// The decision as a mailbox entry records it.
    pub fn mailbox(self) -> MailboxOwnerDecision {
        match self {
            Self::Accepted => MailboxOwnerDecision::Accepted,
            Self::Rejected(_) => MailboxOwnerDecision::Rejected,
        }
    }
}

/// A direct request as its origin commits it.
#[derive(Debug, Clone)]
pub struct NewDirectRequest {
    pub key: DirectRequestKey,
    /// The lane's recipient, bare or full.
    pub recipient: Jid,
    /// The claim entity the origin presents as sender.
    pub sender_entity: Entity,
    pub payload: OrderedRelayPayload,
    /// The canonical message that owns this copy, if one does.
    pub message_key: Option<MessageKey>,
    /// The hosting node and binding, for a request that the account owner
    /// does not decide.
    pub target: Option<DirectTarget>,
}

/// A stored direct request.
#[derive(Debug, Clone)]
pub struct StoredDirectRequest {
    pub key: DirectRequestKey,
    pub recipient: Jid,
    pub sender_entity: Entity,
    pub payload: OrderedRelayPayload,
    pub message_key: Option<MessageKey>,
    pub target: Option<DirectTarget>,
    /// `None` while the deciding node has not decided.
    pub decision: Option<DirectDecision>,
    pub settled: bool,
}

/// The recipient a payload names; `None` for a room payload.
fn payload_recipient(payload: &OrderedRelayPayload) -> Option<&Jid> {
    match payload {
        OrderedRelayPayload::ProcessedDirectMessage { recipient, .. }
        | OrderedRelayPayload::Message { recipient, .. }
        | OrderedRelayPayload::Iq { recipient, .. }
        | OrderedRelayPayload::Presence { recipient, .. } => Some(recipient),
        OrderedRelayPayload::MucProxy { .. } => None,
    }
}

fn encode_payload(
    recipient: &Jid,
    payload: &OrderedRelayPayload,
) -> Result<Vec<u8>, DirectRequestError> {
    match payload_recipient(payload) {
        None => return Err(DirectRequestError::RoomPayload),
        Some(named) if named != recipient => return Err(DirectRequestError::RecipientMismatch),
        Some(_) => {}
    }
    let bytes =
        serde_json::to_vec(payload).map_err(|_| DirectRequestError::PayloadUnserializable)?;
    if bytes.is_empty() || bytes.len() > DIRECT_REQUEST_PAYLOAD_MAX_BYTES {
        return Err(DirectRequestError::PayloadSize { bytes: bytes.len() });
    }
    Ok(bytes)
}

fn decision_to_storage(decision: DirectDecision) -> (i32, Option<i32>) {
    match decision {
        DirectDecision::Accepted => (1, None),
        DirectDecision::Rejected(DirectRefusal::NotDelivered) => (2, Some(1)),
        DirectDecision::Rejected(DirectRefusal::HostedElsewhere) => (2, Some(2)),
    }
}

fn decision_from_storage(
    decision: i32,
    refusal: Option<i32>,
) -> Result<Option<DirectDecision>, DirectRequestError> {
    match (decision, refusal) {
        (0, None) => Ok(None),
        (1, None) => Ok(Some(DirectDecision::Accepted)),
        (2, Some(1)) => Ok(Some(DirectDecision::Rejected(DirectRefusal::NotDelivered))),
        (2, Some(2)) => Ok(Some(DirectDecision::Rejected(
            DirectRefusal::HostedElsewhere,
        ))),
        _ => Err(DirectRequestError::InvalidStored),
    }
}

/// Schema-qualified relation names for one transaction.
struct Relations {
    requests: String,
    entries: String,
}

fn relations(
    tx: &Transaction<'_>,
    prepared: Option<&PreparedAttestedReceiptWriter>,
) -> Result<Relations, DirectRequestError> {
    if tx.driver() != DatabaseDriver::Postgres {
        return Err(DirectRequestError::PostgresRequired);
    }
    if prepared.is_some_and(PreparedAttestedReceiptWriter::uses_definer_api_candidate) {
        return Err(DirectRequestError::DefinerApiUnsupported);
    }
    Ok(Relations {
        requests: prepared_ingress_relation(prepared, PreparedIngressRelation::DirectRequests),
        entries: prepared_ingress_relation(prepared, PreparedIngressRelation::MailboxEntries),
    })
}

const REQUEST_COLUMNS: &str = "request_key::text, recipient_jid, sender_entity, payload, \
    message_key::text, owner_decision::int, refusal::int, (settled_at IS NOT NULL), \
    target_node_id, target_node_epoch, target_binding";

/// A target as its three stored columns: node id, node epoch, binding.
struct StoredTarget {
    node_id: Option<String>,
    node_epoch: Option<String>,
    binding: Option<String>,
}

fn encode_target(target: Option<&DirectTarget>) -> Result<StoredTarget, DirectRequestError> {
    let Some(target) = target else {
        return Ok(StoredTarget {
            node_id: None,
            node_epoch: None,
            binding: None,
        });
    };
    let binding = serde_json::to_string(&target.binding)
        .map_err(|_| DirectRequestError::PayloadUnserializable)?;
    Ok(StoredTarget {
        node_id: Some(target.node.node_id.clone()),
        node_epoch: Some(target.node.node_epoch.clone()),
        binding: Some(binding),
    })
}

fn decode_target(
    node_id: Option<String>,
    node_epoch: Option<String>,
    binding: Option<String>,
) -> Result<Option<DirectTarget>, DirectRequestError> {
    match (node_id, node_epoch, binding) {
        (None, None, None) => Ok(None),
        (Some(node_id), Some(node_epoch), Some(binding)) => Ok(Some(DirectTarget {
            node: NodeIdentity::new(node_id, node_epoch),
            binding: serde_json::from_str(&binding)
                .map_err(|_| DirectRequestError::InvalidStored)?,
        })),
        _ => Err(DirectRequestError::InvalidStored),
    }
}

fn decode_request(row: &crate::db::Row) -> Result<StoredDirectRequest, IngressUowError> {
    let invalid = || DirectRequestError::InvalidStored;
    let key: String = row.get(0)?;
    let recipient: String = row.get(1)?;
    let sender_entity: String = row.get(2)?;
    let payload: Vec<u8> = row.get(3)?;
    let message_key: Option<String> = row.get(4)?;
    let decision: i32 = row.get(5)?;
    let refusal: Option<i32> = row.get(6)?;
    let settled: bool = row.get(7)?;
    let target_node_id: Option<String> = row.get(8)?;
    let target_node_epoch: Option<String> = row.get(9)?;
    let target_binding: Option<String> = row.get(10)?;
    let recipient: Jid = recipient.parse().map_err(|_| invalid())?;
    let payload: OrderedRelayPayload = serde_json::from_slice(&payload).map_err(|_| invalid())?;
    if payload_recipient(&payload) != Some(&recipient) {
        return Err(invalid().into());
    }
    let message_key = match message_key {
        None => None,
        Some(key) => Some(MessageKey::from_storage(
            key.parse().map_err(|_| invalid())?,
        )),
    };
    Ok(StoredDirectRequest {
        key: DirectRequestKey::from_storage(key.parse().map_err(|_| invalid())?),
        recipient,
        sender_entity: serde_json::from_str(&sender_entity).map_err(|_| invalid())?,
        payload,
        message_key,
        target: decode_target(target_node_id, target_node_epoch, target_binding)?,
        decision: decision_from_storage(decision, refusal)?,
        settled,
    })
}

/// Typed, transaction-taking access to direct requests.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectRequestRepository;

impl DirectRequestRepository {
    /// Insert a request, or find the one already stored under its key.
    /// `true` when this call inserted it. A stored request with another
    /// recipient, sender, canonical message or target is a reused key and
    /// is refused.
    pub async fn insert(
        transaction: &mut IngressUowTransaction<'_>,
        request: &NewDirectRequest,
    ) -> Result<bool, IngressUowError> {
        let payload = encode_payload(&request.recipient, &request.payload)?;
        let sender_entity = serde_json::to_string(&request.sender_entity)
            .map_err(|_| DirectRequestError::PayloadUnserializable)?;
        let target = encode_target(request.target.as_ref())?;
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let inserted = tx
            .execute(
                &format!(
                    "INSERT INTO {} (request_key, recipient_jid, sender_entity, payload, \
                     message_key, target_node_id, target_node_epoch, target_binding) \
                     VALUES (?::uuid, ?, ?, ?, ?::uuid, ?, ?, ?) \
                     ON CONFLICT (request_key) DO NOTHING",
                    relations.requests
                ),
                crate::db_params![
                    request.key.to_storage().to_string(),
                    request.recipient.to_string(),
                    sender_entity,
                    payload,
                    request.message_key.map(|key| key.to_storage().to_string()),
                    target.node_id,
                    target.node_epoch,
                    target.binding,
                ],
            )
            .await?;
        if inserted == 1 {
            return Ok(true);
        }
        let existing = Self::read(transaction, request.key)
            .await?
            .ok_or(DirectRequestError::RequestMissing)?;
        if existing.recipient != request.recipient
            || existing.sender_entity != request.sender_entity
            || existing.message_key != request.message_key
            || existing.target != request.target
        {
            return Err(DirectRequestError::KeyReuse.into());
        }
        Ok(false)
    }

    /// Read a request without locking it.
    pub async fn read(
        transaction: &mut IngressUowTransaction<'_>,
        key: DirectRequestKey,
    ) -> Result<Option<StoredDirectRequest>, IngressUowError> {
        Self::select(transaction, key, false).await
    }

    /// Lock a request `FOR UPDATE` and read it. The caller holds the
    /// request's lane.
    pub async fn lock(
        transaction: &mut IngressUowTransaction<'_>,
        key: DirectRequestKey,
    ) -> Result<Option<StoredDirectRequest>, IngressUowError> {
        Self::select(transaction, key, true).await
    }

    /// Lock a request row as the parent of a mailbox entry operation,
    /// without decoding it. `false` when the request is gone.
    pub(super) async fn lock_parent(
        transaction: &mut IngressUowTransaction<'_>,
        key: DirectRequestKey,
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
        key: DirectRequestKey,
        for_update: bool,
    ) -> Result<Option<StoredDirectRequest>, IngressUowError> {
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

    /// Record the deciding node's decision. It moves once, from undecided:
    /// the recorded decision is returned, and a different one is refused.
    ///
    /// The caller holds the request's lane as its target, has proven its
    /// authority to decide (the account's claim for a request without a
    /// target, the request's target node otherwise), and records the same
    /// decision on the request's mailbox entry in this transaction.
    pub async fn latch_decision(
        transaction: &mut IngressUowTransaction<'_>,
        key: DirectRequestKey,
        decision: DirectDecision,
    ) -> Result<DirectDecision, IngressUowError> {
        let (stored_decision, stored_refusal) = decision_to_storage(decision);
        let (tx, prepared) = transaction.prepared_parts();
        let relations = relations(tx, prepared)?;
        let updated = tx
            .execute(
                &format!(
                    "UPDATE {} SET owner_decision = ?::smallint, refusal = ?::smallint \
                     WHERE request_key = ?::uuid AND owner_decision = 0",
                    relations.requests
                ),
                crate::db_params![
                    stored_decision,
                    stored_refusal,
                    key.to_storage().to_string()
                ],
            )
            .await?;
        if updated == 1 {
            return Ok(decision);
        }
        let recorded = Self::read(transaction, key)
            .await?
            .ok_or(DirectRequestError::RequestMissing)?
            .decision
            .ok_or(DirectRequestError::InvalidStored)?;
        if recorded == decision {
            Ok(recorded)
        } else {
            Err(DirectRequestError::ConflictingDecision {
                recorded,
                attempted: decision,
            }
            .into())
        }
    }

    /// Mark a decided request settled: its origin has consumed the entry.
    /// `true` when this call set it.
    pub async fn settle(
        transaction: &mut IngressUowTransaction<'_>,
        key: DirectRequestKey,
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
    /// Returns how many were deleted. An undecided or unsettled request is
    /// never selected, rows another transaction holds are skipped, and no
    /// lane is locked.
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
                           WHERE e.direct_key = r.request_key AND e.disposition >= 3) \
                       ORDER BY r.settled_at LIMIT ? FOR UPDATE OF r SKIP LOCKED)",
                    requests = relations.requests,
                    entries = relations.entries,
                ),
                crate::db_params![settled_for_ms, i64::from(limit)],
            )
            .await?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clustering::codec::RemoteStanza;

    fn presence_payload(recipient: &str) -> OrderedRelayPayload {
        OrderedRelayPayload::Presence {
            recipient: recipient.parse().expect("recipient"),
            stanza: RemoteStanza(waddle_xmpp::Stanza::Presence(
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None),
            )),
        }
    }

    #[test]
    fn decision_codes_round_trip_and_reject_unknown_values() {
        for decision in [
            DirectDecision::Accepted,
            DirectDecision::Rejected(DirectRefusal::NotDelivered),
            DirectDecision::Rejected(DirectRefusal::HostedElsewhere),
        ] {
            let (stored, refusal) = decision_to_storage(decision);
            assert_eq!(
                decision_from_storage(stored, refusal).ok(),
                Some(Some(decision))
            );
        }
        assert_eq!(decision_from_storage(0, None).ok(), Some(None));
        assert!(decision_from_storage(0, Some(1)).is_err());
        assert!(decision_from_storage(1, Some(1)).is_err());
        assert!(decision_from_storage(2, None).is_err());
        assert!(decision_from_storage(2, Some(3)).is_err());
    }

    #[test]
    fn a_target_is_stored_whole_or_not_at_all() {
        use waddle_xmpp::ownership::{ConnectionRef, NodeLeaseIdentity};
        let node = NodeIdentity::new("node-b", "epoch-b");
        let binding = BindingRef::fresh(ConnectionRef::fresh(
            NodeLeaseIdentity::from_node_identity(&node),
        ));
        let target = DirectTarget {
            node: node.clone(),
            binding,
        };
        let stored = encode_target(Some(&target)).expect("encodes");
        assert_eq!(
            decode_target(
                stored.node_id.clone(),
                stored.node_epoch.clone(),
                stored.binding.clone()
            )
            .ok(),
            Some(Some(target))
        );
        assert_eq!(decode_target(None, None, None).ok(), Some(None));
        assert!(decode_target(stored.node_id.clone(), None, stored.binding.clone()).is_err());
        assert!(decode_target(stored.node_id, stored.node_epoch, Some("{}".to_owned())).is_err());
    }

    #[test]
    fn payload_must_name_the_requests_recipient() {
        let recipient: Jid = "juliet@example.com/balcony".parse().expect("recipient");
        assert!(
            encode_payload(&recipient, &presence_payload("juliet@example.com/balcony")).is_ok()
        );
        assert!(matches!(
            encode_payload(&recipient, &presence_payload("juliet@example.com/orchard")),
            Err(DirectRequestError::RecipientMismatch)
        ));
    }
}
