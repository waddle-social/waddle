//! Optional receiver-side authority for a peer's keyed detached append.

use super::*;
use crate::ingress::identity::IngressAppendObligationRef;
use crate::server::routes::interpret::SmIngressAppendContext;
use waddle_xmpp::telemetry::attributes::IngressAppendAuthorizationFailure;

const AUTHORIZATION_READ_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug)]
enum Rejection {
    IneligibleKind,
    NotMessage,
    SenderClaimMismatch,
    StanzaSenderMismatch,
    ServicesUnavailable,
    CanonicalReadFailed,
    CanonicalReadTimedOut,
    CanonicalSenderMissing,
    CanonicalSenderMismatch,
}

impl Rejection {
    /// Whether the identity was provably unusable, or merely undecidable here.
    ///
    /// The two degrade identically — delivery never depends on this check — but
    /// they mean different things operationally: `Unauthorized` is a statement
    /// about the peer, `Indeterminate` is a statement about this node's own
    /// ability to read canonical state, and only the latter silently widens the
    /// duplicate window while it persists.
    fn failure_class(&self) -> IngressAppendAuthorizationFailure {
        match self {
            Self::IneligibleKind
            | Self::NotMessage
            | Self::SenderClaimMismatch
            | Self::StanzaSenderMismatch
            | Self::CanonicalSenderMissing
            | Self::CanonicalSenderMismatch => IngressAppendAuthorizationFailure::Unauthorized,
            Self::ServicesUnavailable | Self::CanonicalReadFailed | Self::CanonicalReadTimedOut => {
                IngressAppendAuthorizationFailure::Indeterminate
            }
        }
    }
}

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
            tracing::warn!(
                ?reason,
                sender = %obligation.sender_bare,
                "relay append identity unauthorized; continuing with unkeyed delivery"
            );
            waddle_xmpp::counter_add!(
                "waddle.clustering.ingress_append.authorization_failed",
                "{obligation}",
                "Relayed ingress append identities degraded to unkeyed delivery -- \
                 `indeterminate` means this node could not read canonical state and the \
                 cross-node duplicate window is open for as long as it persists.",
                1,
                reason.failure_class(),
            );
            None
        }
    }
}

async fn check_authority(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    stanza: &Stanza,
    obligation: &IngressAppendObligationRef,
) -> Result<(), Rejection> {
    if !obligation.kind_is_append_eligible() {
        return Err(Rejection::IneligibleKind);
    }
    let Stanza::Message(message) = stanza else {
        return Err(Rejection::NotMessage);
    };
    if obligation.sender_bare.as_str() != validated_sender.id {
        return Err(Rejection::SenderClaimMismatch);
    }
    if message.from.as_ref().map(jid::Jid::to_bare).as_ref() != Some(&obligation.sender_bare) {
        return Err(Rejection::StanzaSenderMismatch);
    }
    let state = services
        .web_socket_state
        .upgrade()
        .ok_or(Rejection::ServicesUnavailable)?;
    let sender = tokio::time::timeout(
        AUTHORIZATION_READ_TIMEOUT,
        crate::ingress_substrate::canonical_sender_pooled(
            state.deps.app_state.db_pool.global(),
            obligation.message_key,
        ),
    )
    .await
    .map_err(|_| Rejection::CanonicalReadTimedOut)?
    .map_err(|_| Rejection::CanonicalReadFailed)?
    .ok_or(Rejection::CanonicalSenderMissing)?;
    if sender != obligation.sender_bare {
        return Err(Rejection::CanonicalSenderMismatch);
    }
    Ok(())
}
