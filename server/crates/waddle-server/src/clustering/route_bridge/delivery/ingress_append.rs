//! Optional receiver-side authority for a peer's keyed detached append.

use super::*;
use crate::ingress::append_authority::{
    check_canonical_obligation, check_stanza_binding, record_authorization_failure,
    AppendAuthorityRejection,
};
use crate::ingress::identity::IngressAppendObligationRef;
use crate::server::routes::interpret::SmIngressAppendContext;

/// Call only after authenticating the sender claim or resource registration.
/// Archive-ordered copies must retain this authority; callers reject them when
/// validation fails instead of falling back to an unkeyed append.
pub(super) async fn authorize_ingress_append(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    stanza: &Stanza,
    obligation: Option<&IngressAppendObligationRef>,
) -> Option<SmIngressAppendContext> {
    let obligation = obligation?;
    match check_authority(services, validated_sender, stanza, obligation).await {
        Ok(()) => Some(obligation.clone().into_context()),
        Err(reason) => {
            record_authorization_failure(&reason, &obligation.sender_bare);
            None
        }
    }
}

pub(super) fn requires_ordering_authority(obligation: Option<&IngressAppendObligationRef>) -> bool {
    obligation.is_some_and(|obligation| !obligation.archive_positions.is_empty())
}

async fn check_authority(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    stanza: &Stanza,
    obligation: &IngressAppendObligationRef,
) -> Result<(), AppendAuthorityRejection> {
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
    check_canonical_obligation(state.deps.app_state.db_pool.global(), stanza, obligation).await
}
