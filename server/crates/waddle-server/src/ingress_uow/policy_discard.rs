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
