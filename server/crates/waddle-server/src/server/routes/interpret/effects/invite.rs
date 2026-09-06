//! Frozen invitation fan-out and its delivery-failure compensation.
use super::super::Deps;
use crate::server::routes::websocket::{
    handlers::message::{
        group_dm_invite::GroupDmMembershipMutation,
        muc_invite::{self, MucMembershipMutation},
    },
    muc_invites::{self, OutstandingInvite},
};
use jid::{BareJid, FullJid};
use waddle_xmpp::{pending_delivery::PendingRow, Stanza};
use xmpp_parsers::message::Message;

#[derive(Clone, Debug)]
pub enum InviteDeliveryFailure {
    RestoreLedger(OutstandingInvite),
    RemoveLedger(OutstandingInvite),
    RollbackMucMembership(Box<MucMembershipMutation>),
    RollbackMuc {
        grant: Box<MucMembershipMutation>,
        invite: OutstandingInvite,
    },
    RollbackGroupDm {
        grant: Box<GroupDmMembershipMutation>,
        invite: OutstandingInvite,
    },
}

#[derive(Clone, Debug)]
pub struct MucUserRoute {
    pub route_identity: Option<waddle_xmpp::ingress::EffectMessageIdentity>,
    pub recipient: BareJid,
    pub resources: Vec<FullJid>,
    pub message: Box<Message>,
    pub fallback: PendingRow,
    pub failure: Option<Box<InviteDeliveryFailure>>,
}

#[derive(Clone, Debug)]
pub enum MucUserDeliveryProof {
    Delivered {
        resources: Vec<FullJid>,
    },
    Queued {
        row_id: waddle_xmpp::pending_delivery::PendingRowId,
    },
}

pub(crate) async fn execute(route: MucUserRoute, deps: &Deps<'_>) -> super::EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return super::EffectOutcome::Unavailable;
    };
    let mut accepted = Vec::new();
    for resource in &route.resources {
        if deps
            .connection_registry
            .send_to(resource, Stanza::Message(*route.message.clone()))
            .await
            .is_sent()
        {
            accepted.push(resource.clone());
        }
    }
    let proof = if accepted.is_empty() {
        let row_id = route.fallback.id.clone();
        let result = state
            .deps
            .protocol
            .pending_delivery_storage
            .insert(route.fallback)
            .await;
        let error = match result {
            Ok(waddle_xmpp::pending_delivery::InsertOutcome::Inserted) => None,
            Ok(waddle_xmpp::pending_delivery::InsertOutcome::QuotaExceeded) => {
                Some(muc_invite::MucUserDeliveryError::QuotaExceeded)
            }
            Err(error) => Some(muc_invite::MucUserDeliveryError::Storage(error)),
        };
        if let Some(error) = error {
            if let Some(failure) = route.failure {
                compensate(*failure, deps).await;
            }
            return super::EffectOutcome::MucUserDelivery(Err(error));
        }
        deps.capture_intent(waddle_xmpp::ingress::IngressEffectIntent::PendingDelivery {
            mutation: waddle_xmpp::ingress::PendingDeliveryMutation::Transient {
                recipient: route.recipient.clone(),
                row_id: row_id.clone(),
            },
        });
        MucUserDeliveryProof::Queued { row_id }
    } else {
        MucUserDeliveryProof::Delivered {
            resources: accepted.clone(),
        }
    };
    if let Some(capture) = deps.ingress_effect_capture.as_ref() {
        accepted.sort();
        accepted.dedup();
        capture.record_intent(waddle_xmpp::ingress::IngressEffectIntent::RouteDirect {
            recipient: route.recipient,
            fanout: accepted,
            route_identity: route
                .route_identity
                .unwrap_or_else(|| capture.next_route_identity()),
        });
    }
    super::EffectOutcome::MucUserDelivery(Ok(proof))
}

pub(crate) async fn compensate(failure: InviteDeliveryFailure, deps: &Deps<'_>) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    match failure {
        InviteDeliveryFailure::RestoreLedger(invite) => {
            if let Err(error) = muc_invites::record_invite(
                state.deps.app_state.db_pool.global_actor().clone(),
                &invite,
            )
            .await
            {
                tracing::warn!(%error, "Failed to restore invitation after delivery failure");
            }
        }
        InviteDeliveryFailure::RemoveLedger(invite) => {
            if let Err(error) = muc_invites::claim_invite(
                state.deps.app_state.db_pool.global_actor().clone(),
                &invite,
            )
            .await
            {
                tracing::warn!(%error, "Failed to remove undeliverable invitation");
            }
        }
        InviteDeliveryFailure::RollbackMucMembership(grant) => {
            muc_invite::rollback_muc_membership(&grant, deps).await;
        }
        InviteDeliveryFailure::RollbackMuc { grant, invite } => {
            if let Err(error) = muc_invites::claim_invite(
                state.deps.app_state.db_pool.global_actor().clone(),
                &invite,
            )
            .await
            {
                tracing::warn!(%error, "Failed to remove undeliverable invitation");
            }
            muc_invite::rollback_muc_membership(&grant, deps).await;
        }
        InviteDeliveryFailure::RollbackGroupDm { grant, .. } => {
            crate::server::routes::websocket::handlers::message::group_dm_invite::rollback_group_dm_membership(&grant, deps).await;
        }
    }
}
