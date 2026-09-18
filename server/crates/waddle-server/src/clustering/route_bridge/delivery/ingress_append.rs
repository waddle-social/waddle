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
    /// Not a failure: the recipient is not detached, so no keyed append is possible.
    RecipientNotDetached,
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
            // Never reaches the counter; classified for exhaustiveness only.
            Self::RecipientNotDetached => IngressAppendAuthorizationFailure::Indeterminate,
        }
    }
}

/// Call only after authenticating the sender claim or resource registration.
/// Failure removes the optional deduplication key, never delivery availability.
pub(super) async fn authorize_ingress_append(
    services: &OrderedRelayDeliveryServices,
    validated_sender: &Entity,
    target: &jid::FullJid,
    stanza: &Stanza,
    obligation: Option<&IngressAppendObligationRef>,
) -> Option<SmIngressAppendContext> {
    let obligation = obligation?;
    match check_authority(services, validated_sender, target, stanza, obligation).await {
        Ok(()) => Some(obligation.clone().into_context()),
        // An unused key is an ordinary outcome, not a degraded one: nothing is
        // logged or counted, so the failure counter keeps meaning "a key that
        // should have applied did not".
        Err(Rejection::RecipientNotDetached) => None,
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
    target: &jid::FullJid,
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
    // The key is only ever consulted by a detached append, so a live recipient
    // would pay for a canonical read whose result is discarded. Skip it: this
    // is not an authorization failure and must not be counted as one. A
    // resource that detaches between here and the append degrades to an
    // unkeyed append, which is the same already-documented fallback as a failed
    // authorization -- never a dropped delivery.
    if !target_is_detached(services, target).await {
        return Err(Rejection::RecipientNotDetached);
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

/// Whether this resource currently holds a detached session, read from the
/// in-memory registry only. A miss means no keyed append can happen for it.
async fn target_is_detached(
    services: &OrderedRelayDeliveryServices,
    target: &jid::FullJid,
) -> bool {
    services
        .sm_session_registry
        .detached_resources_for_user(&target.to_bare())
        .await
        .is_ok_and(|resources| resources.iter().any(|resource| resource == target))
}
