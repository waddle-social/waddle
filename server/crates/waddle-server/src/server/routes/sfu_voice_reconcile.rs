//! Periodic re-assertion of XEP-0045 voice onto LiveKit media grants.
//!
//! Split out of `livekit_webhook` because it is not webhook ingestion:
//! the webhook is the fast path for enforcing a stale token, and this is
//! the guarantee that the enforcement happens at all regardless of which
//! replica received that webhook.

use tracing::warn;
use waddle_sfu::{CallId, SfuReconciler};

use super::websocket::WebSocketState;

/// Reply budget for the room-actor asks this pass makes. Bounded so a
/// single wedged room actor cannot stall the pass and starve every later
/// room of voice-grant convergence. Shares the room-registry budget
/// rather than inventing a second number.
const ROOM_ACTOR_ASK_TIMEOUT: std::time::Duration = waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT;

/// Re-derive and push the current XEP-0045 voice of every occupant of
/// every MUC room whose actor lives on THIS node.
///
/// This is the backstop that makes stale-token enforcement independent
/// of which replica received a `participant_joined` webhook. The webhook
/// path is the fast path, and it asks LiveKit to retry when it lands on
/// a node that cannot resolve the room — but retries are finite and all
/// of them could miss the owner. Because a room actor is claimed by
/// exactly one node, iterating locally-claimed rooms covers every room
/// exactly once across the cluster, with no cross-node request and no
/// new relay protocol.
///
/// Whether a room has a call to converge is decided by asking the SFU
/// itself (`SfuReconciler::live_participants`), NOT by
/// `SfuService::participants_for_call`. The latter is the calling
/// process's registry, and the whole point of this pass is that the node
/// claiming a room is frequently NOT the node that registered the
/// participant — filtering on it would skip exactly the joins this
/// backstop exists to cover, which is the same fail-open the
/// `update_participant_capabilities` contract warns about.
///
/// Cost per pass is one `ListParticipants` per claimed room that has
/// occupants, and an `UpdateParticipant` only for occupants LiveKit
/// actually reports as connected. Both are idempotent.
pub(super) async fn reconcile_voice_grants(state: &WebSocketState, reconciler: &dyn SfuReconciler) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    // Bounded like the per-room asks below. `reconcile_once` is awaited
    // inside the interval loop, so an unbounded wait here would stall
    // every future pass on this node — including the pre-existing ghost
    // sweep, not just voice convergence.
    let rooms = match state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::LocalRoomJids)
        .reply_timeout(ROOM_ACTOR_ASK_TIMEOUT)
        .await
    {
        Ok(rooms) => rooms,
        Err(error) => {
            warn!(error = ?error, "could not list locally-claimed rooms for voice reconciliation");
            return;
        }
    };
    for room_jid in rooms {
        let Ok(call_id) = CallId::new(room_jid.to_string()) else {
            continue;
        };
        let Ok(Some(actor)) =
            crate::server::routes::websocket::get_room_actor_result(state, &room_jid).await
        else {
            continue;
        };
        // A room with no occupants cannot have a legitimate call
        // participant, and this is a cheap local actor ask — it keeps
        // idle rooms from costing an HTTP round-trip every pass. Bounded
        // so one wedged room actor cannot stall the whole pass and
        // starve every later room of convergence.
        let voices = match actor
            .ask(waddle_xmpp::muc::room_actor::OccupantVoices)
            .reply_timeout(ROOM_ACTOR_ASK_TIMEOUT)
            .await
        {
            Ok(voices) => voices,
            Err(error) => {
                // Not silent: this room's stale-token backstop did not
                // run this pass.
                warn!(
                    room = %room_jid,
                    error = ?error,
                    "occupant-voice lookup failed during reconciliation; \
                     voice-grant convergence skipped for this room this pass",
                );
                continue;
            }
        };
        if voices.is_empty() {
            continue;
        }
        // `None` means the SFU could not be reached, so absence is
        // unconfirmed — treating it as "nobody is connected" would let a
        // LiveKit outage silently disable this backstop. The impl warns.
        let Some(live) = reconciler.live_participants(&call_id).await else {
            continue;
        };
        if live.is_empty() {
            continue;
        }
        let RoomVoiceReconciliation { converge, evict } = plan_room_reconciliation(&voices, &live);
        for (session, voice) in converge {
            crate::server::routes::websocket::muc_call_sfu::apply_voice_grants_via_sfu(
                sfu, &room_jid, &session, voice,
            );
        }
        for session in evict {
            // Occupancy is the precondition for call participation, and
            // this node owns the room, so its occupant set is
            // authoritative: a participant LiveKit reports as connected
            // who is no longer an occupant holds a token for a room they
            // left. The webhook path evicts these when it happens to
            // reach the owning node; without this the backstop would
            // converge grants but leave a former occupant publishing —
            // and listening — indefinitely.
            warn!(
                room = %room_jid,
                user = %session.to_bare(),
                "LiveKit participant is no longer a MUC occupant; evicting from the call",
            );
            crate::server::routes::websocket::muc_call_sfu::unregister_participant_via_sfu(
                sfu, &room_jid, &session,
            );
        }
    }
}

/// What one room's reconciliation pass should do to the SFU.
#[derive(Debug, Default, PartialEq)]
struct RoomVoiceReconciliation {
    /// Connected occupants whose grants should be re-asserted.
    converge: Vec<(jid::FullJid, waddle_xmpp_core::types::Voice)>,
    /// Connected participants who are no longer occupants at all.
    evict: Vec<jid::FullJid>,
}

/// Decide a room's reconciliation from the authoritative occupant set
/// and the SFU's authoritative participant set.
///
/// Pure so the decision is testable without actors or HTTP. NOTE the
/// caller only reaches this with a NON-EMPTY occupant set: a room actor
/// freshly claimed by this node has no occupants until clients rejoin,
/// and treating that as "everyone is a former occupant" would evict a
/// whole live call. The empty-occupant guard at the call site is
/// therefore load-bearing, not an optimisation.
fn plan_room_reconciliation(
    voices: &[(jid::FullJid, waddle_xmpp_core::types::Voice)],
    live: &[waddle_sfu::Identity],
) -> RoomVoiceReconciliation {
    let mut plan = RoomVoiceReconciliation::default();
    for identity in live {
        let session = identity.as_jid();
        match voices.iter().find(|(occupant, _)| occupant == session) {
            Some((_, voice)) => plan.converge.push((session.clone(), *voice)),
            None => plan.evict.push(session.clone()),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_sfu::Identity;
    use waddle_xmpp_core::types::Voice;

    fn jid(value: &str) -> jid::FullJid {
        value.parse().expect("full jid")
    }

    /// Only participants the SFU actually reports are touched, each with
    /// the voice the room says they have.
    #[test]
    fn connected_occupants_are_converged_with_their_current_voice() {
        let voices = vec![
            (jid("alice@example.com/web"), Voice::Muted),
            (jid("bob@example.com/web"), Voice::Voiced),
            // Not connected to the SFU: nothing to converge.
            (jid("carol@example.com/web"), Voice::Voiced),
        ];
        let live = vec![
            Identity::from_jid(jid("alice@example.com/web")),
            Identity::from_jid(jid("bob@example.com/web")),
        ];

        let plan = plan_room_reconciliation(&voices, &live);

        assert_eq!(
            plan.converge,
            vec![
                (jid("alice@example.com/web"), Voice::Muted),
                (jid("bob@example.com/web"), Voice::Voiced),
            ]
        );
        assert!(plan.evict.is_empty());
    }

    /// A participant the SFU reports who is no longer an occupant holds a
    /// token for a room they left; occupancy is the precondition for call
    /// participation, so they are evicted rather than left publishing.
    #[test]
    fn connected_non_occupants_are_evicted() {
        let voices = vec![(jid("alice@example.com/web"), Voice::Voiced)];
        let live = vec![
            Identity::from_jid(jid("alice@example.com/web")),
            Identity::from_jid(jid("mallory@example.com/web")),
        ];

        let plan = plan_room_reconciliation(&voices, &live);

        assert_eq!(
            plan.converge,
            vec![(jid("alice@example.com/web"), Voice::Voiced)]
        );
        assert_eq!(plan.evict, vec![jid("mallory@example.com/web")]);
    }

    /// Multi-resource: each session is decided independently, so one
    /// resource leaving does not disturb another that is still joined.
    #[test]
    fn sessions_of_one_user_are_decided_independently() {
        let voices = vec![(jid("alice@example.com/web"), Voice::Muted)];
        let live = vec![
            Identity::from_jid(jid("alice@example.com/web")),
            Identity::from_jid(jid("alice@example.com/phone")),
        ];

        let plan = plan_room_reconciliation(&voices, &live);

        assert_eq!(
            plan.converge,
            vec![(jid("alice@example.com/web"), Voice::Muted)]
        );
        assert_eq!(plan.evict, vec![jid("alice@example.com/phone")]);
    }
}
