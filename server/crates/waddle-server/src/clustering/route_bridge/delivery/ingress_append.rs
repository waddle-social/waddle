//! Optional receiver-side authority for a peer's keyed detached append.

use super::*;
use crate::ingress::append_authority::{
    check_canonical_sender, check_stanza_binding, record_degraded_to_unkeyed,
    AppendAuthorityRejection,
};
use crate::ingress::identity::IngressAppendObligationRef;
use crate::server::routes::interpret::SmIngressAppendContext;

/// Call only after authenticating the sender claim or resource registration.
/// Failure removes the optional deduplication key, never delivery availability.
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
            record_degraded_to_unkeyed(&reason, &obligation.sender_bare);
            None
        }
    }
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
    check_canonical_sender(
        state.deps.app_state.db_pool.global(),
        obligation.message_key,
        &obligation.sender_bare,
    )
    .await
}
