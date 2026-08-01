use super::*;

#[derive(Debug, Clone)]
pub(crate) enum OrderedRelayMucProxyOutcome {
    Delivered(Vec<Stanza>),
    Unavailable,
    Dropped,
    MaybeCommitted,
    JoinMaybeCommitted,
}

/// Typed routing decision for a MUC proxy attempt (#1249). The old
/// `Option<OrderedRelayMucProxyOutcome>` API collapsed six distinct
/// "no relay attempted" conditions into one `None`, forcing the
/// disconnect-cleanup caller to treat the benign "room claim is locally
/// owned" case (the local room loop handles the leave moments later)
/// exactly like the harmful "origin `UserActor` claim held by another
/// node" case (which recurs whenever the disconnecting user has a
/// second device on another node and previously ghosted the occupant
/// forever). Callers that only need the legacy semantics keep using
/// [`OrderedRelayDeliveryBridge::try_proxy_muc_remote`]; the cleanup
/// path consumes this decision directly.
#[derive(Debug, Clone)]
pub(crate) enum MucProxyRouteDecision {
    /// An ordered-relay send was attempted; the payload is its result.
    Attempted(OrderedRelayMucProxyOutcome),
    /// The room claim is owned by THIS node — the local room path is
    /// authoritative and handles the stanza. Benign for cleanup: the
    /// local `LeaveByRealJid` loop converges the occupancy.
    LocalRoom,
    /// Definitive: no claim row exists for the room, so no node holds a
    /// live `RoomActor` (occupancy is in-memory on the claim owner).
    /// There is no remote occupancy left to clean up.
    RoomUnclaimed,
    /// The room claim could not be used right now: the claim lookup
    /// errored, the owner's lease is stale (owner crash / renewal lag),
    /// or the bridge services are not wired. Retryable.
    RoomClaimUnavailable,
    /// The origin/sender claim needed to sequence the relay is not
    /// usable from this node (typically: the origin `UserActor` claim
    /// is held by the node hosting the user's other device). Retryable;
    /// disconnect cleanup avoids this case up-front by preferring the
    /// remote-resource origin when the socket was registered against a
    /// foreign `UserActor` owner.
    OriginUnavailable,
}

impl MucProxyRouteDecision {
    /// Legacy adapter: `Some(outcome)` iff a relay send was attempted;
    /// `None` means "keep the existing local path" (all non-attempt
    /// variants), exactly matching the pre-#1249 `Option` contract.
    pub(super) fn into_attempted(self) -> Option<OrderedRelayMucProxyOutcome> {
        match self {
            MucProxyRouteDecision::Attempted(outcome) => Some(outcome),
            MucProxyRouteDecision::LocalRoom
            | MucProxyRouteDecision::RoomUnclaimed
            | MucProxyRouteDecision::RoomClaimUnavailable
            | MucProxyRouteDecision::OriginUnavailable => None,
        }
    }
}
