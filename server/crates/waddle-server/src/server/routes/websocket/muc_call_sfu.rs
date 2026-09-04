//! Side-effect glue from MUC presence-unavailable / disconnect cleanup
//! into the LiveKit SFU registry.
//!
//! Without this, a session's SFU participant outlives its XMPP
//! presence: if a user closes the tab or their stream-management
//! session expires, the room actor processes a `LeaveByRealJid` but
//! the SFU never hears about it until LiveKit's own (long) timeout.
//! Meanwhile the channel UI keeps showing the user as "in call" and
//! the previously-issued JWT remains replayable.
//!
//! The XEP-0045 leave path is the authoritative XMPP signal that a
//! user is no longer in the room; mirroring it onto
//! [`waddle_sfu::SfuService::unregister_call_participant`] keeps the
//! SFU's view of the world in lock-step with XMPP. The SFU call
//! registry is keyed by the room JID (the call-id) and the user's
//! full JID (the identity), and `unregister_call_participant` is
//! idempotent — calling it for rooms the user never had a media
//! session in is a benign no-op.

use jid::{BareJid, FullJid};
use std::collections::{BTreeMap, HashSet};
use waddle_sfu::{
    CallId, Identity, MediaCapabilities, ObservedCallSids, SessionScopedTeardown,
    SidObservationDisposition, TeardownDisposition,
};
use waddle_xmpp_core::{
    types::{Moderation, Voice},
    OccupancySessionGeneration,
};

use super::state::WebSocketState;

pub(crate) fn derive_room_voice_from_snapshot(
    room: &waddle_xmpp::muc::MucRoom,
    config: &waddle_xmpp::muc::RoomConfig,
) -> Vec<(FullJid, Voice)> {
    let moderation = Moderation::from_moderated_flag(config.moderated);
    let mut voices: Vec<(FullJid, Voice)> = room
        .occupants
        .values()
        .flat_map(|occupant| {
            let voice = occupant.role.voice(moderation);
            room.get_occupant_sessions(&occupant.nick)
                .into_iter()
                .map(move |session| (session, voice))
        })
        .collect();
    voices.sort_by_key(|voice| voice.0.to_string());
    voices
}

/// Tear down `jid`'s SFU participant in `room_jid`, if a SFU is
/// configured for this deployment.
///
/// No-op when:
/// - `LIVEKIT_*` env vars were not set (then `state.deps.protocol.sfu`
///   is `None` and there is no SFU to update),
/// - the room JID cannot be converted into a valid call-id (the
///   call-id grammar is stricter than the JID grammar, so this can
///   theoretically reject some MUC bare JIDs — those are rooms with
///   no calling capability and there is nothing on the SFU to undo
///   for them anyway).
pub(crate) fn unregister_participant_from_room_if_occupant_matches(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
    occupant: OccupancySessionGeneration,
    observed_sids: Option<&ObservedCallSids>,
) -> Option<SessionScopedTeardown> {
    let sfu = state.deps.protocol.sfu.as_ref()?;
    unregister_participant_via_sfu_if_occupant_matches(sfu, room_jid, jid, occupant, observed_sids)
}

/// SFU-handle variant of [`unregister_participant_from_room_if_occupant_matches`] for
/// callers that do not hold a `WebSocketState`.
pub(crate) fn unregister_participant_via_sfu_if_occupant_matches(
    sfu: &std::sync::Arc<dyn waddle_sfu::SfuService>,
    room_jid: &BareJid,
    jid: &FullJid,
    occupant: OccupancySessionGeneration,
    observed_sids: Option<&ObservedCallSids>,
) -> Option<SessionScopedTeardown> {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return None;
    };
    let identity = Identity::from_jid(jid.clone());
    Some(sfu.unregister_call_participant_if_occupant_matches(
        &call_id,
        &identity,
        occupant,
        observed_sids,
    ))
}

/// SFU-handle variant of [`unregister_participant_from_room_ungated`] for
/// callers that don't hold a `WebSocketState` (the admin V2 command
/// handlers receive the SFU as an explicit dependency).
pub(crate) fn unregister_participant_via_sfu_ungated(
    sfu: &std::sync::Arc<dyn waddle_sfu::SfuService>,
    room_jid: &BareJid,
    jid: &FullJid,
) {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        // Room JID couldn't round-trip into a CallId, which means it
        // could never have been used to mint a join token either —
        // there's nothing to undo.
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    let _ = sfu.unregister_call_participant(&call_id, &identity, None);
}

pub(crate) fn unregister_participant_from_room_ungated(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
) {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return;
    };
    unregister_participant_via_sfu_ungated(sfu, room_jid, jid);
}

/// Push a session's XEP-0045 voice grant through an explicit SFU handle.
pub(crate) fn apply_voice_grants_via_sfu(
    sfu: &std::sync::Arc<dyn waddle_sfu::SfuService>,
    room_jid: &BareJid,
    jid: &FullJid,
    voice: Voice,
) {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return;
    };
    let identity = Identity::from_jid(jid.clone());
    sfu.update_participant_capabilities(
        &call_id,
        &identity,
        MediaCapabilities::from_muc_voice(voice),
    );
}

fn effective_voice_changes<'a>(
    removed_sessions: &[FullJid],
    voice_changes: &'a [(FullJid, Voice)],
) -> Vec<(&'a FullJid, Voice)> {
    let removed: HashSet<&FullJid> = removed_sessions.iter().collect();
    let mut fused = BTreeMap::new();
    for (session, voice) in voice_changes {
        if removed.contains(session) {
            continue;
        }
        fused.insert(session, *voice);
    }
    fused.into_iter().collect()
}

pub(crate) fn converge_moderation_deltas_via_sfu(
    sfu: Option<&std::sync::Arc<dyn waddle_sfu::SfuService>>,
    room_jid: &BareJid,
    removed_sessions: &[FullJid],
    voice_changes: &[(FullJid, Voice)],
) {
    let Some(sfu) = sfu else {
        return;
    };
    for removed in removed_sessions {
        unregister_participant_via_sfu_ungated(sfu, room_jid, removed);
    }
    for (session, voice) in effective_voice_changes(removed_sessions, voice_changes) {
        apply_voice_grants_via_sfu(sfu, room_jid, session, voice);
    }
}

/// What [`enforce_current_voice_grants`] established about one live
/// SFU participant's authorization, from THIS process's room map.
///
/// The distinction between [`Self::NoLocalRoomActor`] and every other
/// variant is load-bearing for clustering (#1594): only a LOCAL room
/// actor's answer is authoritative. An absent actor means "the room
/// may be claimed by another replica", never "not an occupant" —
/// treating absence as authorization evidence would evict legitimate
/// occupants from roughly every join that lands on a non-owning node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantEnforcement {
    /// The occupant's current voice was pushed to the SFU.
    Applied,
    /// A local (authoritative) actor answered "not an occupant"; the
    /// participant was evicted from the call.
    EvictedNonOccupant,
    /// No room actor in this process — the claim may live on another
    /// replica. No side effect was performed.
    NoLocalRoomActor,
    /// The registry or actor lookup failed transiently. No side effect
    /// was performed; retrying may succeed.
    LookupFailed,
    /// This deployment has no SFU configured; there is nothing to
    /// enforce against.
    SfuNotConfigured,
}

/// The side-effect-free half of [`enforce_current_voice_grants`]:
/// THIS process's room actor's authoritative answer about one live
/// SFU participant, with no SFU effect performed. Split out so the
/// #1594 relayed path can re-validate its room claim BETWEEN deriving
/// the answer and acting on it — binding the (possibly slow) actor
/// answer to the claim it was derived under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VoiceDerivation {
    /// A local actor answered with the occupant's current voice.
    Occupant(Voice),
    /// A local (authoritative) actor answered "not an occupant".
    NotOccupant,
    /// No room actor in this process — cannot determine anything.
    NoLocalRoomActor,
    /// The registry or actor lookup failed transiently.
    LookupFailed,
}

/// Resolve `full_jid`'s current XEP-0045 voice from THIS process's
/// room actor.
///
/// Performs no SFU side effects — the state-changing half lives in
/// [`apply_derived_enforcement`], so the #1594 relayed path can
/// re-validate its room claim between the two. The failure branches
/// DO emit their diagnostic WARN here, deliberately: both callers
/// (the same-node webhook path and the relayed owner-side executor)
/// need the same error-carrying line, and emitting it once at the
/// derivation site is what keeps log-based alerting from
/// double-counting a single transient failure.
pub(crate) async fn derive_current_voice(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) -> VoiceDerivation {
    let actor = match crate::server::routes::websocket::get_room_actor_result(state, room_jid).await
    {
        Ok(Some(actor)) => actor,
        Ok(None) => return VoiceDerivation::NoLocalRoomActor,
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                error = %error,
                "could not resolve MUC voice for a live call participant",
            );
            return VoiceDerivation::LookupFailed;
        }
    };
    match actor
        .ask(waddle_xmpp::muc::room_actor::GetOccupantVoice {
            jid: full_jid.clone(),
        })
        // Bounded for the same reason as the reconciler's identical
        // ask: a wedged room actor must not park an HTTP webhook
        // handler — or, on the relayed #1594 path, leak an owner-side
        // delegated relay task per LiveKit retry.
        .reply_timeout(waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT)
        .await
    {
        Ok(Some(voice)) => VoiceDerivation::Occupant(voice),
        Ok(None) => VoiceDerivation::NotOccupant,
        Err(error) => {
            tracing::warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                error = %error,
                "MUC voice lookup failed for a live call participant",
            );
            VoiceDerivation::LookupFailed
        }
    }
}

/// The side-effecting half of [`enforce_current_voice_grants`]: act
/// on an already-derived answer. Side effects happen only on an
/// authoritative derivation: a seated occupant gets their
/// voice-derived grants pushed, a confirmed non-occupant is evicted,
/// and every other derivation performs nothing.
pub(crate) fn apply_derived_enforcement(
    sfu: &std::sync::Arc<dyn waddle_sfu::SfuService>,
    room_jid: &BareJid,
    full_jid: &FullJid,
    derivation: VoiceDerivation,
) -> GrantEnforcement {
    match derivation {
        VoiceDerivation::Occupant(voice) => {
            tracing::debug!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                "re-asserting SFU media grants from current MUC voice",
            );
            apply_voice_grants_via_sfu(sfu, room_jid, full_jid, voice);
            GrantEnforcement::Applied
        }
        VoiceDerivation::NotOccupant => {
            // A LOCAL room actor answered, so it owns this room and its
            // occupant set is authoritative: this participant is in the
            // SFU room while not being an occupant of the MUC.
            // Occupancy is the precondition for call participation, so
            // this is a stale-token join and must end. (Contrast the
            // absent-actor case, which proves nothing.)
            tracing::warn!(
                room = %room_jid,
                user = %full_jid.to_bare(),
                "LiveKit participant is not a MUC occupant; evicting from the call",
            );
            unregister_participant_via_sfu_ungated(sfu, room_jid, full_jid);
            GrantEnforcement::EvictedNonOccupant
        }
        VoiceDerivation::NoLocalRoomActor => GrantEnforcement::NoLocalRoomActor,
        VoiceDerivation::LookupFailed => GrantEnforcement::LookupFailed,
    }
}

/// Re-derive `full_jid`'s XEP-0045 voice from THIS process's room
/// actor and converge their live SFU media grants with it — the
/// shared enforcement core behind both the same-node
/// `participant_joined` webhook path and the owner side of the #1594
/// cross-node relay. Keeping both on one core is deliberate: the
/// relayed path must not be able to diverge from the local one (it
/// composes [`derive_current_voice`] and [`apply_derived_enforcement`]
/// with a claim re-check in between).
pub(crate) async fn enforce_current_voice_grants(
    state: &WebSocketState,
    room_jid: &BareJid,
    full_jid: &FullJid,
) -> GrantEnforcement {
    let Some(sfu) = state.deps.protocol.sfu.as_ref() else {
        return GrantEnforcement::SfuNotConfigured;
    };
    let derivation = derive_current_voice(state, room_jid, full_jid).await;
    apply_derived_enforcement(sfu, room_jid, full_jid, derivation)
}

/// Push every occupant's current XEP-0045 voice to the SFU after a
/// room-configuration change flipped `moderated`.
///
/// Flipping moderation re-decides voice for every seated visitor
/// without changing any role, and nothing else in the config path
/// touches an occupant, so without this the SFU keeps the pre-flip
/// grants: a visitor who just lost text voice would keep publishing.
///
/// Deliberately NOT filtered by the process-local participant registry.
/// Filtering would skip a visitor LiveKit still holds but this node has
/// lost track of (a reconnect after `participant_left`, reconciliation,
/// actor migration, or registration by another node), leaving them
/// publishing — the same fail-open that
/// [`waddle_sfu::SfuService::update_participant_capabilities`] is
/// documented to avoid. LiveKit maps an unknown participant to
/// `not_found`, which the admin client treats as success, so
/// over-pushing costs only a few no-op requests.
pub(crate) async fn converge_room_voice_after_moderation_flip(
    sfu: Option<&std::sync::Arc<dyn waddle_sfu::SfuService>>,
    actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room_jid: &BareJid,
    voices_from_recovered_snapshot: Option<&[(FullJid, Voice)]>,
) {
    let Some(sfu) = sfu else {
        return;
    };
    if let Some(voices) = voices_from_recovered_snapshot {
        for (session, voice) in voices {
            apply_voice_grants_via_sfu(sfu, room_jid, session, *voice);
        }
        return;
    }
    let voices = match actor
        .ask(waddle_xmpp::muc::room_actor::OccupantVoices)
        // Bounded: a wedged room actor must not park the config-change
        // handler that is waiting on this convergence.
        .reply_timeout(waddle_xmpp::muc::ROOM_REGISTRY_REPLY_TIMEOUT)
        .await
    {
        Ok(voices) => voices,
        Err(error) => {
            // Security-relevant convergence: a silent skip leaves
            // occupants publishing against the new moderation policy.
            tracing::warn!(
                room = %room_jid,
                error = ?error,
                "could not read occupant voices after a moderation flip; \
                 live media grants now lag the room configuration",
            );
            return;
        }
    };
    for (session, voice) in voices {
        apply_voice_grants_via_sfu(sfu, room_jid, &session, voice);
    }
}

/// Mirror a members-only enforcement sweep (XEP-0045 status 322) onto
/// the SFU: an ejection ends room membership, so it must end call
/// participation, and a surviving occupant who lost voice must lose
/// publish rights.
pub(crate) fn converge_members_only_sweep_via_sfu(
    sfu: Option<&std::sync::Arc<dyn waddle_sfu::SfuService>>,
    room_jid: &BareJid,
    applied: &waddle_xmpp::muc::room_actor::AdminItemsApplied,
) {
    converge_moderation_deltas_via_sfu(
        sfu,
        room_jid,
        &applied.removed_by_moderation,
        &applied.voice_changes,
    );
}

/// Local-only teardown variant for the LiveKit webhook bridge. The
/// SFU's `participant_left` event is the acknowledgement that
/// LiveKit already removed the participant on its side — invoking
/// the full `unregister_participant_from_room` here would loop a
/// redundant `RemoveParticipant` admin call back to LiveKit (which
/// returns `not_found`, mapped to success, but the round-trip is
/// wasted and amplifies the race with quick rejoins). This helper
/// runs only the bookkeeping side.
pub(crate) fn note_participant_left_from_webhook(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
) -> Option<TeardownDisposition> {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return None;
    };
    note_participant_left_by_call_id(state, &call_id, jid, observed_sids)
}

/// Session-scoped variant (#1608): the local bookkeeping applies only
/// when the registration's binding accepts `session` — atomic in the
/// registry, so a signaling-driven cleanup racing a same-identity
/// rebind cannot remove the NEW session. `session = None` keeps the
/// membership-scoped semantics.
pub(crate) fn note_participant_left_for_session(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
    session: Option<&waddle_sfu::SessionBinding>,
) -> Option<waddle_sfu::SessionScopedTeardown> {
    let Ok(call_id) = CallId::new(room_jid.to_string()) else {
        return None;
    };
    let sfu = state.deps.protocol.sfu.as_ref()?;
    let identity = Identity::from_jid(jid.clone());
    Some(sfu.note_participant_left_if_session_matches(&call_id, &identity, observed_sids, session))
}

/// Non-destructively validate and learn webhook SIDs before an async
/// MUC actor cleanup. Keeping membership intact until the actor step
/// succeeds preserves `room_finished`'s survivor recovery path when a
/// transient actor failure asks LiveKit to retry.
pub(crate) fn observe_participant_sids_from_webhook(
    state: &WebSocketState,
    room_jid: &BareJid,
    jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
) -> Option<SidObservationDisposition> {
    let call_id = CallId::new(room_jid.to_string()).ok()?;
    let sfu = state.deps.protocol.sfu.as_ref()?;
    let identity = Identity::from_jid(jid.clone());
    Some(sfu.observe_call_participant_sids(
        &call_id,
        &identity,
        observed_sids,
        waddle_sfu::SidObservationDirection::Leave,
    ))
}

/// Raw-`CallId` variant of [`note_participant_left_from_webhook`] for
/// call ids that are NOT MUC room JIDs (#1128): 1:1 scoped ids
/// (`<initiator-bare>::<sid>`) deliberately fail the `BareJid` parse,
/// but their SFU registry entries and un-revoked JTIs still must be
/// cleaned when LiveKit reports the participant gone — otherwise a
/// crashed 1:1 peer lingers until reconciliation.
pub(crate) fn note_participant_left_by_call_id(
    state: &WebSocketState,
    call_id: &CallId,
    jid: &FullJid,
    observed_sids: Option<&ObservedCallSids>,
) -> Option<TeardownDisposition> {
    let sfu = state.deps.protocol.sfu.as_ref()?;
    let identity = Identity::from_jid(jid.clone());
    Some(sfu.note_participant_left(call_id, &identity, observed_sids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::tests::{
        create_test_websocket_state, create_test_websocket_state_with_sfu, RecordingSfu,
    };
    use std::sync::Arc;
    use waddle_xmpp::{muc::Occupant, Affiliation, Role};

    #[tokio::test]
    async fn unregister_is_no_op_when_no_sfu_is_configured() {
        // Pre-LiveKit deployments: protocol.sfu is None. The cleanup
        // path must remain a no-op so legacy installs aren't broken
        // by the teardown hook.
        let state = create_test_websocket_state().await;
        assert!(state.deps.protocol.sfu.is_none());
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        // Should not panic, not write anywhere.
        unregister_participant_from_room_ungated(&state, &room, &alice);
    }

    #[tokio::test]
    async fn unregister_records_call_id_and_identity_on_the_sfu() {
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();

        unregister_participant_from_room_ungated(&state, &room, &alice);

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 1, "exactly one teardown call expected");
        let (call_id, identity) = &recorded[0];
        assert_eq!(call_id.as_str(), "room@muc.example.com");
        assert_eq!(identity.as_livekit_identity(), "alice@example.com/web");
    }

    #[tokio::test]
    async fn unregister_is_idempotent_across_repeated_calls() {
        // Disconnect cleanup and graceful presence-unavailable may
        // both fire for the same `(room, jid)` if a tab close races
        // with an explicit leave. SfuService::unregister is
        // documented as idempotent — verify the helper passes
        // through the second call rather than short-circuiting,
        // since silently skipping would mask bugs in the SFU layer.
        let recorder = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
        let room: BareJid = "room@muc.example.com".parse().unwrap();
        let alice: FullJid = "alice@example.com/web".parse().unwrap();

        unregister_participant_from_room_ungated(&state, &room, &alice);
        unregister_participant_from_room_ungated(&state, &room, &alice);

        let recorded = recorder.snapshot();
        assert_eq!(recorded.len(), 2, "both teardown calls reach the SFU");
        assert_eq!(recorded[0], recorded[1]);
    }

    #[test]
    fn derive_room_voice_from_snapshot_reflects_current_moderation_setting() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let mut room = waddle_xmpp::muc::MucRoom::new(
            room_jid,
            "waddle-id".to_string(),
            "channel-id".to_string(),
            waddle_xmpp::muc::RoomConfig::default(),
        );
        room.add_occupant(Occupant {
            real_jid: "alice@example.com/web".parse().unwrap(),
            nick: "alice".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
        room.add_occupant(Occupant {
            real_jid: "bob@example.com/phone".parse().unwrap(),
            nick: "bob".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });

        let moderate = waddle_xmpp::muc::RoomConfig {
            moderated: true,
            ..waddle_xmpp::muc::RoomConfig::default()
        };
        let voices = derive_room_voice_from_snapshot(&room, &moderate);

        assert_eq!(voices.len(), 2);
        assert_eq!(
            voices[0],
            (
                "alice@example.com/web".parse().unwrap(),
                Role::Participant.voice(Moderation::from_moderated_flag(true))
            )
        );
        assert_eq!(
            voices[1],
            (
                "bob@example.com/phone".parse().unwrap(),
                Role::Participant.voice(Moderation::from_moderated_flag(true))
            )
        );
    }

    #[test]
    fn effective_voice_changes_last_write_wins_per_session() {
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        let bob: FullJid = "bob@example.com/web".parse().unwrap();
        let voice_changes = [
            (alice.clone(), Voice::Muted),
            (bob.clone(), Voice::Muted),
            (alice.clone(), Voice::Voiced),
        ];

        let fused = effective_voice_changes(&[], &voice_changes);

        assert_eq!(
            fused,
            vec![(&alice, Voice::Voiced), (&bob, Voice::Muted)],
            "the last moderation delta per session must win"
        );
    }

    #[test]
    fn effective_voice_changes_omit_removed_sessions() {
        let alice: FullJid = "alice@example.com/web".parse().unwrap();
        let bob: FullJid = "bob@example.com/web".parse().unwrap();
        let voice_changes = [(alice.clone(), Voice::Voiced), (bob.clone(), Voice::Muted)];

        let fused = effective_voice_changes(std::slice::from_ref(&alice), &voice_changes);

        assert_eq!(
            fused,
            vec![(&bob, Voice::Muted)],
            "removed sessions must never be re-granted voice in the same moderation batch"
        );
    }
}
