//! Offline replay uses the same durable predecessor authority as live delivery.

use std::time::Duration;

use jid::FullJid;
use waddle_xmpp::pending_delivery::{storage::PendingStorageError, PendingRow};

use crate::{
    ingress_uow::{ArchiveDispatchRepository, DispatchReadiness, IngressUowError},
    pending_delivery::{PendingDispatchGate, PendingDispatchReadiness},
};

#[async_trait::async_trait]
impl PendingDispatchGate for super::IngressAuthority {
    async fn check_turn(
        &self,
        row: &PendingRow,
        resource: &FullJid,
        stream: Option<&waddle_xmpp::pending_delivery::SmSessionId>,
    ) -> Result<PendingDispatchReadiness, PendingStorageError> {
        if resource.to_bare() != row.recipient {
            return Err(PendingStorageError::ArchiveOrderingUnavailable);
        }
        let probe = async {
            let mut tx = self
                .uow
                .begin_with_timeouts(Duration::from_millis(100), Duration::from_millis(250))
                .await?;
            let readiness =
                ArchiveDispatchRepository::readiness_pending(&mut tx, resource, &row.id, stream)
                    .await?;
            tx.commit().await?;
            Ok::<_, IngressUowError>(readiness)
        }
        .await;
        match probe {
            Ok(DispatchReadiness::Ready | DispatchReadiness::Completed) => {
                Ok(PendingDispatchReadiness::Ready)
            }
            Ok(DispatchReadiness::Blocked(_)) => Ok(PendingDispatchReadiness::Deferred),
            Err(error) => {
                tracing::warn!(%error, "pending archive dispatch ordering unavailable");
                Err(PendingStorageError::ArchiveOrderingUnavailable)
            }
        }
    }
}
