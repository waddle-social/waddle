//! Optional receiver-side authority for a peer's keyed detached append.

use super::*;
use crate::ingress::append_authority::{
    check_stanza_binding, record_authorization_failure, AppendAuthorityRejection,
};
use crate::ingress::identity::IngressAppendObligationRef;
use crate::server::routes::interpret::SmIngressAppendContext;

/// Call only after authenticating the sender claim or resource registration.
/// Every keyed copy must retain this authority; callers reject it when
/// validation fails instead of falling back to an unkeyed append.
///
/// Only the synchronous checks run here. The canonical-row read is deferred to
/// the first consumer that trusts the context (#1790): the ordered local-copy
/// boundary, the detached append, and a frame bound for a registered remote
/// socket verify it; a forwarding hop never reads, because its receiver does.
pub(super) fn authorize_ingress_append(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    stanza: &Stanza,
    obligation: Option<&IngressAppendObligationRef>,
) -> Option<SmIngressAppendContext> {
    let obligation = obligation?;
    match check_authority(services, validated_sender, stanza, obligation) {
        Ok(db) => Some(obligation.clone().into_deferred_context(db)),
        Err(reason) => {
            record_authorization_failure(&reason, &obligation.sender_bare);
            None
        }
    }
}

pub(super) fn requires_ingress_authority(obligation: Option<&IngressAppendObligationRef>) -> bool {
    obligation.is_some()
}

fn check_authority(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    stanza: &Stanza,
    obligation: &IngressAppendObligationRef,
) -> Result<crate::db::Database, AppendAuthorityRejection> {
    if obligation.sender_bare.as_str() != validated_sender.id {
        return Err(AppendAuthorityRejection::SenderClaimMismatch);
    }
    let state = services
        .web_socket_state
        .upgrade()
        .ok_or(AppendAuthorityRejection::ServicesUnavailable)?;
    check_stanza_binding(
        stanza,
        &obligation.sender_bare,
        obligation.receipt.kind.to_storage(),
    )?;
    Ok(state.deps.app_state.db_pool.global().clone())
}
