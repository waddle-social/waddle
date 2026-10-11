//! Frozen local projection mutations inside canonical ingress settlement.
use waddle_xmpp::ingress::IngressEffectIntent;

use super::{IngressUowError, IngressUowTransaction};

pub(crate) struct ProjectionRepository;

impl ProjectionRepository {
    /// The caller holds the canonical message lock and validates this exact
    /// intent before applying it. A receipt is written in the same transaction.
    pub(crate) async fn apply(
        tx: &mut IngressUowTransaction<'_>,
        intent: &IngressEffectIntent,
        accepted_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), IngressUowError> {
        let query = match intent {
            IngressEffectIntent::NotificationActivityPreview { owner, mutation } => {
                crate::notification_activity::NotificationActivityStore::mutation_write(
                    owner, mutation,
                )
                .ok_or(IngressUowError::EffectIntentConflict)?
            }
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => {
                crate::server::routes::interpret::preview_plan::mutation_write(
                    mutation,
                    accepted_at,
                    tx.transaction_mut().driver(),
                )
            }
            _ => return Err(IngressUowError::EffectIntentConflict),
        };
        tx.transaction_mut()
            .execute(&query.sql, query.params)
            .await?;
        Ok(())
    }
}
