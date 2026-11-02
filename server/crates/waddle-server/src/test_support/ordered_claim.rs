//! Opaque test access to the dark ordered-claim lab.

use jid::BareJid;
use waddle_xmpp::pending_delivery::{PendingRowId, SmSessionId};

use crate::db::DatabasePool;
use crate::pending_delivery::database::ordered_claim::{
    self, OrderedClaimAttempt, OrderedClaimAuthority, OrderedClaimError, OrderedClaimObservation,
    OrderedClaimPhase,
};

/// A lab over one pool; it exposes no database, transaction, or SQL.
pub struct OrderedClaimLab(OrderedClaimAuthority);

/// A same-process attempt retaining its internally minted token.
#[derive(Clone)]
pub struct OrderedClaimAttemptLab(OrderedClaimAttempt);

/// A positively observed row at one instant.
#[derive(Debug)]
pub struct OrderedClaimObservationLab(OrderedClaimObservation);

impl OrderedClaimObservationLab {
    pub fn row_id(&self) -> &PendingRowId {
        self.0.row_id()
    }

    pub fn phase(&self) -> OrderedClaimPhase {
        self.0.phase()
    }
}

impl OrderedClaimLab {
    pub async fn from_pool(pool: &DatabasePool) -> Result<Self, OrderedClaimError> {
        Ok(Self(OrderedClaimAuthority::from_pool(pool).await?))
    }

    pub fn attempt(
        &self,
        recipient: BareJid,
        session: SmSessionId,
        limit: usize,
    ) -> Result<OrderedClaimAttemptLab, OrderedClaimError> {
        self.0
            .attempt(recipient, session, limit)
            .map(OrderedClaimAttemptLab)
    }

    pub async fn claim(
        &self,
        attempt: &OrderedClaimAttemptLab,
    ) -> Result<Vec<OrderedClaimObservationLab>, OrderedClaimError> {
        self.0.claim(&attempt.0).await.map(wrap)
    }

    /// Simulate a lost reply after a known successful commit.
    pub async fn claim_with_lost_commit_ack(
        &self,
        attempt: &OrderedClaimAttemptLab,
    ) -> Result<Vec<OrderedClaimObservationLab>, OrderedClaimError> {
        ordered_claim::with_lost_commit_ack(self.0.claim(&attempt.0))
            .await
            .map(wrap)
    }

    /// Change only this transaction's search path after attestation.
    pub async fn claim_with_search_path(
        &self,
        attempt: &OrderedClaimAttemptLab,
        path: String,
    ) -> Result<Vec<OrderedClaimObservationLab>, OrderedClaimError> {
        ordered_claim::with_search_path(path, self.0.claim(&attempt.0))
            .await
            .map(wrap)
    }

    /// Observe under the same test-only transaction-local path perturbation.
    pub async fn observe_with_search_path(
        &self,
        attempt: &OrderedClaimAttemptLab,
        path: String,
    ) -> Result<Vec<OrderedClaimObservationLab>, OrderedClaimError> {
        ordered_claim::with_search_path(path, self.0.observe(&attempt.0))
            .await
            .map(wrap)
    }

    pub async fn observe(
        &self,
        attempt: &OrderedClaimAttemptLab,
    ) -> Result<Vec<OrderedClaimObservationLab>, OrderedClaimError> {
        self.0.observe(&attempt.0).await.map(wrap)
    }

    pub async fn observe_row(
        &self,
        attempt: &OrderedClaimAttemptLab,
        row_id: &PendingRowId,
    ) -> Result<Option<OrderedClaimObservationLab>, OrderedClaimError> {
        self.0
            .observe_row(&attempt.0, row_id)
            .await
            .map(|row| row.map(OrderedClaimObservationLab))
    }
}

fn wrap(rows: Vec<OrderedClaimObservation>) -> Vec<OrderedClaimObservationLab> {
    rows.into_iter().map(OrderedClaimObservationLab).collect()
}
