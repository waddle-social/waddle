//! Transactional enqueue of `extension_job_outbox` rows (issue #1831 Phase
//! B — the generic replacement for `judgment_outbox.rs`).
use super::{IngressUowError, IngressUowTransaction};
use crate::extension_job_outbox::PendingJobInput;

/// Repository for the one write `ingress::durable::apply_durable` needs
/// against `extension_job_outbox`: enqueueing a row on the exact same
/// transaction that just wrote the archive row it accompanies. Kept in
/// `ingress_uow` — not called directly from `ingress::durable` — so the raw
/// transaction handle never leaves this module tree, matching every other
/// repository here.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExtensionJobOutboxRepository;

impl ExtensionJobOutboxRepository {
    /// Enqueue one row. Propagates any database error as a real
    /// [`IngressUowError`] so a failure here fails the whole ingress
    /// transaction — this insert and the archive write it accompanies
    /// commit or roll back together, never independently.
    pub(crate) async fn enqueue_in_tx(
        tx: &mut IngressUowTransaction<'_>,
        input: PendingJobInput,
    ) -> Result<(), IngressUowError> {
        crate::extension_job_outbox::enqueue_pending_in_tx(tx.transaction_mut(), input).await?;
        Ok(())
    }
}
