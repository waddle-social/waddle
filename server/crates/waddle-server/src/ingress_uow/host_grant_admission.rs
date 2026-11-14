//! Host-grant admission inside an ingress acceptance transaction.
//!
//! The scope's guard is taken here, in the acceptance transaction itself and
//! after its ownership claims, and it is held until that transaction ends.
//! Nothing but PostgreSQL is consulted.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use jid::BareJid;

use super::{IngressUowError, IngressUowTransaction};
use crate::db::DatabaseDriver;
use crate::ingress_substrate::{
    prepared_ingress_relation, PreparedAttestedReceiptWriter, PreparedIngressRelation,
};
use crate::permissions::host_grant::{
    admit_room_acceptance, managing_scope, AdmissionRelations, ManagingScope, RoomAcceptance,
    RoomAdmission, RoomManagement,
};

/// Repository for the host-grant policy state an acceptance reads.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct HostGrantAdmissionRepository;

impl HostGrantAdmissionRepository {
    /// The enrolled scope that manages `room`, if any, looked up by the
    /// room's canonical address.
    ///
    /// SQLite holds no canonical grant store, so no room is managed there. A
    /// PostgreSQL schema that was built without the migration catalog holds
    /// no host-grant relation and therefore no enrolled scope either; an
    /// attested transaction runs on a schema whose ledger was verified, so
    /// only an unattested one looks, and stops looking once it found them.
    pub(crate) async fn managing_scope(
        transaction: &mut IngressUowTransaction<'_>,
        room: &BareJid,
    ) -> Result<RoomManagement, IngressUowError> {
        let present = Arc::clone(&transaction.host_grant_relations);
        let (tx, prepared) = transaction.prepared_parts();
        if tx.driver() != DatabaseDriver::Postgres {
            return Ok(RoomManagement::Unmanaged);
        }
        if prepared.is_none() && !present.load(Ordering::Acquire) {
            let mut rows = tx
                .query(
                    "SELECT pg_catalog.to_regclass('host_grant_rooms') IS NOT NULL",
                    (),
                )
                .await?;
            let found = match rows.next().await? {
                Some(row) => row.get::<bool>(0)?,
                None => false,
            };
            drop(rows);
            if !found {
                return Ok(RoomManagement::Unmanaged);
            }
            present.store(true, Ordering::Release);
        }
        Ok(managing_scope(tx, &relations(prepared), room).await?)
    }

    /// Decide the acceptance of a managed room from the canonical policy
    /// state and keep the scope's guard, shared, until `transaction` ends.
    pub(crate) async fn admit_room_acceptance(
        transaction: &mut IngressUowTransaction<'_>,
        scope: &ManagingScope,
        acceptance: &RoomAcceptance<'_>,
    ) -> Result<RoomAdmission, IngressUowError> {
        let (tx, prepared) = transaction.prepared_parts();
        Ok(admit_room_acceptance(tx, &relations(prepared), scope, acceptance).await?)
    }
}

/// The host-grant relations as this transaction names them.
fn relations(prepared: Option<&PreparedAttestedReceiptWriter>) -> AdmissionRelations {
    let relation = |relation| prepared_ingress_relation(prepared, relation);
    AdmissionRelations {
        rooms: relation(PreparedIngressRelation::HostGrantRooms),
        scopes: relation(PreparedIngressRelation::HostGrantScopes),
        grants: relation(PreparedIngressRelation::HostGrants),
        authority: relation(PreparedIngressRelation::HostGrantAuthority),
    }
}
