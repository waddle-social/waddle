use super::delivery::receiver::room_entity;
use super::*;

/// Bound on each receiver-side claim-store read in
/// [`OrderedRelayDeliveryBridge::reassert_media_grants_local`]. The
/// executor runs in a delegated relay task that outlives the asker's
/// webhook timeout, so a stalled control-plane pool would otherwise
/// keep one pending task alive per LiveKit retry. On elapse the
/// executor answers `Unavailable` — a retry signal, never an
/// authorization conclusion.
pub(super) const REASSERT_CLAIM_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Owner-side outcome of a #1594 cross-node media-grant re-assert,
/// executed against THIS node's room map. The relay actor maps these
/// onto [`super::relay::RelayReassertMediaGrantsReply`] — kept as a
/// separate enum so the bridge (like `ResumeStealBridge`'s
/// `LocalForcedDetachOutcome`) does not depend on relay wire types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalMediaGrantReassertion {
    /// A local room actor answered with the occupant's voice and the
    /// grants were pushed to the SFU.
    Applied,
    /// A local room actor answered authoritatively that the
    /// participant is not an occupant; they were evicted from the call.
    EvictedNonOccupant,
    /// No room actor lives in this process — the asker's claim read is
    /// stale or the actor is mid-(re)spawn. No side effect performed.
    NoLocalRoomActor,
    /// This node cannot execute the re-assert right now (services or
    /// `WebSocketState` unavailable, or the local lookup flaked). No
    /// authorization conclusion may be drawn from this.
    Unavailable,
}

impl OrderedRelayDeliveryBridge {
    /// Execute a relayed `participant_joined` grant re-assert on this
    /// node — the receiving side of #1594. Terminal by design: this
    /// never re-relays, so a stale claim read on the asker cannot
    /// bounce a webhook around the cluster; a `NoLocalRoomActor`
    /// answer sends the asker back to LiveKit's retry (which
    /// re-resolves the claim).
    ///
    /// Reuses the exact same enforcement core as the same-node webhook
    /// path (`muc_call_sfu::enforce_current_voice_grants`), so relayed
    /// and local enforcement cannot diverge.
    pub async fn reassert_media_grants_local(
        &self,
        room_jid: &jid::BareJid,
        participant: &jid::FullJid,
    ) -> LocalMediaGrantReassertion {
        use crate::server::routes::websocket::muc_call_sfu::{
            apply_derived_enforcement, derive_current_voice, GrantEnforcement, VoiceDerivation,
        };
        let Some(services) = self.services.get().cloned() else {
            return LocalMediaGrantReassertion::Unavailable;
        };
        let Some(state) = services.web_socket_state.upgrade() else {
            return LocalMediaGrantReassertion::Unavailable;
        };
        // Receiver-side claim gate, mirroring what `validate_claims`
        // does for ordered payloads: the asker's claim read can be
        // stale, and a lingering post-demote actor on this node must
        // not answer authoritatively — evicting from a superseded
        // occupant set is the #1593 breaker class. Execute only while
        // this node still owns the claim WITH a fresh lease (an
        // expired lease means another node may already be stealing
        // it — the same fresh-and-mine predicate every other receiver
        // gate in this module applies); otherwise answer
        // `NoLocalRoomActor` so the asker's retry re-resolves the
        // owner.
        let me = services.node_identity.current();
        let gate_epoch = match tokio::time::timeout(
            REASSERT_CLAIM_READ_TIMEOUT,
            services.claim_store.current_claim(&room_entity(room_jid)),
        )
        .await
        {
            Ok(Ok(Some(snapshot))) if snapshot.owner_lease_fresh && snapshot.owner == me => {
                snapshot.claim_epoch
            }
            Ok(Ok(_)) => return LocalMediaGrantReassertion::NoLocalRoomActor,
            Ok(Err(_)) | Err(_) => return LocalMediaGrantReassertion::Unavailable,
        };
        // Derive first, act second: the room-actor ask can take up to
        // its reply timeout, during which the claim can be stolen and
        // the local actor's occupant set superseded. Re-checking the
        // claim (same epoch, still fresh-and-mine) between the answer
        // and the SFU effect fences the answer to the claim it was
        // derived under — the SFU push/evict happens only while this
        // node's ownership is continuously observable.
        let derivation = derive_current_voice(state.as_ref(), room_jid, participant).await;
        match derivation {
            VoiceDerivation::NoLocalRoomActor => {
                return LocalMediaGrantReassertion::NoLocalRoomActor
            }
            VoiceDerivation::LookupFailed => return LocalMediaGrantReassertion::Unavailable,
            VoiceDerivation::Occupant(_) | VoiceDerivation::NotOccupant => {}
        }
        match tokio::time::timeout(
            REASSERT_CLAIM_READ_TIMEOUT,
            services.claim_store.current_claim(&room_entity(room_jid)),
        )
        .await
        {
            Ok(Ok(Some(snapshot)))
                if snapshot.owner_lease_fresh
                    && snapshot.owner == me
                    && snapshot.claim_epoch == gate_epoch => {}
            Ok(Ok(_)) => return LocalMediaGrantReassertion::NoLocalRoomActor,
            Ok(Err(_)) | Err(_) => return LocalMediaGrantReassertion::Unavailable,
        }
        let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
            // An SFU-less node cannot push grants — "cannot execute
            // here", never an authorization conclusion.
            return LocalMediaGrantReassertion::Unavailable;
        };
        match apply_derived_enforcement(sfu, room_jid, participant, derivation) {
            GrantEnforcement::Applied => LocalMediaGrantReassertion::Applied,
            GrantEnforcement::EvictedNonOccupant => LocalMediaGrantReassertion::EvictedNonOccupant,
            // Unreachable: both non-authoritative derivations returned
            // above, and `apply_derived_enforcement` maps the two
            // authoritative ones exhaustively to the two arms matched
            // here.
            GrantEnforcement::NoLocalRoomActor
            | GrantEnforcement::LookupFailed
            | GrantEnforcement::SfuNotConfigured => LocalMediaGrantReassertion::Unavailable,
        }
    }
}
