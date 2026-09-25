//! Transactional enqueue of `message_judgment_outbox` rows (#1831 Phase 2).
use super::{IngressUowError, IngressUowTransaction};
use crate::message_judgment_outbox::PendingJudgmentInput;

/// Repository for the one write `ingress::durable::apply_durable` needs
/// against `message_judgment_outbox`: enqueueing a row on the exact same
/// transaction that just wrote the archive row it accompanies.
///
/// Kept in `ingress_uow` — not called directly from `ingress::durable` — so
/// the raw transaction handle
/// ([`IngressUowTransaction::transaction_mut`]) never leaves this module
/// tree, matching every other repository here (`MamArchiveRepository`,
/// `InboxRepository`, ...).
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageJudgmentOutboxRepository;

impl MessageJudgmentOutboxRepository {
    /// Enqueue one row. Propagates any database error as a real
    /// [`IngressUowError`] (via `#[from]` on
    /// [`crate::message_judgment_outbox::MessageJudgmentOutboxError`]) so a
    /// failure here fails the whole ingress transaction — this insert and
    /// the archive write it accompanies commit or roll back together,
    /// never independently.
    pub(crate) async fn enqueue_in_tx(
        tx: &mut IngressUowTransaction<'_>,
        input: PendingJudgmentInput,
    ) -> Result<(), IngressUowError> {
        crate::message_judgment_outbox::enqueue_pending_in_tx(tx.transaction_mut(), input).await?;
        Ok(())
    }
}
