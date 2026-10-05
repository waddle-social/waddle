//! Durable policy resolution of remaining work, without claiming socket delivery.
use super::{EffectReceiptRepository, IngressUowError, IngressUowTransaction};
use crate::{db::DatabaseDriver, ingress::EffectReceiptKey};
use waddle_xmpp::ingress::MessageKey;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicyDiscardReason {
    RecipientBlocked,
    StorageHintForbidsHandoff,
}

impl PolicyDiscardReason {
    fn as_storage(self) -> &'static str {
        match self {
            Self::RecipientBlocked => "recipient_blocked",
            Self::StorageHintForbidsHandoff => "storage_hint_forbids_handoff",
        }
    }
}

impl EffectReceiptRepository {
    /// Caller holds canonical authority. An existing receipt always wins:
    /// later policy cannot relabel an earlier completed/custodied obligation.
    pub(crate) async fn record_policy_discard(
        tx: &mut IngressUowTransaction<'_>,
        key: MessageKey,
        receipt: &EffectReceiptKey,
        reason: PolicyDiscardReason,
    ) -> Result<(), IngressUowError> {
        let sql = if tx.transaction_mut().driver() == DatabaseDriver::Postgres {
            "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash, policy_discard_reason) SELECT message_key, kind, semantic_identity_hash, ? FROM ingress_effect_intents WHERE message_key = ?::uuid AND kind = ? AND semantic_identity_hash = ? ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING"
        } else {
            "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash, policy_discard_reason) SELECT message_key, kind, semantic_identity_hash, ? FROM ingress_effect_intents WHERE message_key = ? AND kind = ? AND semantic_identity_hash = ? ON CONFLICT (message_key, kind, semantic_identity_hash) DO NOTHING"
        };
        tx.transaction_mut()
            .execute(
                sql,
                crate::db_params![
                    reason.as_storage(),
                    key.to_storage().to_string(),
                    receipt.kind.to_storage(),
                    receipt.semantic_identity_hash.to_vec(),
                ],
            )
            .await?;
        Ok(())
    }
}

/// Ownership incarnation observed before probing resource absence.
pub(crate) enum PolicyOwnerProof {
    SingleNode,
    #[cfg(feature = "clustering")]
    Clustered {
        owner: waddle_xmpp::ownership::NodeIdentity,
        epoch: waddle_xmpp::ownership::ClaimEpoch,
    },
}

pub(crate) async fn capture_local_policy_owner(
    uow: &super::IngressUnitOfWork,
    resource: &jid::FullJid,
) -> Result<Option<PolicyOwnerProof>, IngressUowError> {
    #[cfg(feature = "clustering")]
    if let super::IngressFencing::Clustered(identity) = uow.fencing() {
        let owner = identity.current();
        let mut tx = uow
            .begin_with_timeouts(
                std::time::Duration::from_millis(100),
                std::time::Duration::from_millis(250),
            )
            .await?;
        let Some(_guard) = identity.guard_if_current(&owner).await else {
            return Ok(None);
        };
        let entity = format!(
            "{}:{}",
            waddle_xmpp::ownership::EntityType::UserActor.as_db_str(),
            resource.to_bare()
        );
        let mut rows = tx.transaction_mut().query(
            "SELECT c.claim_epoch FROM clustering_claims c WHERE c.entity = ? AND c.node_id = ? AND c.node_epoch = ? AND EXISTS (SELECT 1 FROM clustering_nodes n WHERE n.node_id = c.node_id AND n.node_epoch = c.node_epoch AND NOT n.expired)",
            crate::db_params![entity, owner.node_id.clone(), owner.node_epoch.clone()],
        ).await?;
        let epoch = rows
            .next()
            .await?
            .map(|row| row.get::<i64>(0))
            .transpose()?;
        drop(rows);
        tx.commit().await?;
        return Ok(epoch.map(|epoch| PolicyOwnerProof::Clustered {
            owner,
            epoch: waddle_xmpp::ownership::ClaimEpoch(epoch),
        }));
    }
    let _ = (uow, resource);
    Ok(Some(PolicyOwnerProof::SingleNode))
}

pub(crate) async fn assert_local_policy_owner(
    tx: &mut IngressUowTransaction<'_>,
    resource: &jid::FullJid,
    proof: &PolicyOwnerProof,
) -> Result<bool, IngressUowError> {
    #[cfg(feature = "clustering")]
    if let (
        super::IngressFencing::Clustered(identity),
        PolicyOwnerProof::Clustered { owner, epoch },
    ) = (tx.fencing(), proof)
    {
        let identity = identity.clone();
        let Some(guard) = identity.guard_if_current(owner).await else {
            return Ok(false);
        };
        let entity = format!(
            "{}:{}",
            waddle_xmpp::ownership::EntityType::UserActor.as_db_str(),
            resource.to_bare()
        );
        let mut rows = tx.transaction_mut().query(
            "SELECT 1 FROM clustering_claims c WHERE c.entity = ? AND c.node_id = ? AND c.node_epoch = ? AND c.claim_epoch = ? AND EXISTS (SELECT 1 FROM clustering_nodes n WHERE n.node_id = c.node_id AND n.node_epoch = c.node_epoch AND NOT n.expired) FOR SHARE",
            crate::db_params![entity, owner.node_id.clone(), owner.node_epoch.clone(), epoch.0],
        ).await?;
        let local = rows.next().await?.is_some();
        drop(rows);
        if local {
            tx.retain_authority(guard);
        }
        return Ok(local);
    }
    let _ = resource;
    Ok(matches!(
        (tx.fencing(), proof),
        (
            super::IngressFencing::SingleNode,
            PolicyOwnerProof::SingleNode
        )
    ))
}
