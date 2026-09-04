//! Session-binding supersession fence for durable teardown intents
//! (#1608, PR #1626): an intent recording the terminate's Jingle
//! session is skipped when the live registration is bound to a
//! different session, regardless of clocks.

use super::*;

#[tokio::test]
async fn participant_intent_with_stale_session_binding_is_skipped() {
    // #1608 via the durable path (#1626 review): a Muji relay-fallback
    // Participant intent created AFTER the user's replacement
    // registration passes the timestamp fence, but its recorded
    // signaling session no longer matches the registration's binding —
    // supersession proven directly, no clock comparison needed.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-session-stale@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let stale = waddle_sfu::SessionBinding::new("muji-stale").expect("binding");
    let occupant = occupant_generation();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant, &Some(owner_session.clone())),
    )
    .await;
    // The replacement registration happens FIRST; the stale intent is
    // enqueued after it, so the registered-after-intent timestamp
    // fence alone would let it execute.
    sfu.register_call_participant_with_session(&call_id, &identity, &current, occupant);

    let intent_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::Participant {
                identity: alice.clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
            session: Some(stale),
            occupant: Some(occupant),
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    assert!(
        sfu.has_call_participant(&call_id, &identity),
        "stale-session intent must not unregister the current registration"
    );
    assert!(
        admin
            .remove_calls
            .lock()
            .expect("recording lock")
            .is_empty(),
        "stale-session intent must not fire RemoveParticipant"
    );
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&intent_id)
            .await
            .expect("find")
            .expect("stored intent")
            .status,
        CallTeardownStatus::Done
    );
}

#[tokio::test]
async fn participant_intent_with_matching_session_binding_executes() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-session-match@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let occupant = occupant_generation();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant, &Some(owner_session.clone())),
    )
    .await;
    sfu.register_call_participant_with_session(&call_id, &identity, &current, occupant);

    // The registration predates the intent, so the timestamp fence
    // does not veto; only the session gate could, and it matches.
    let _ = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call_id.clone(),
            target: TeardownTarget::Participant {
                identity: alice.clone(),
                participant_sid: None,
            },
            generation: None,
            room_sid: None,
            session: Some(current),
            occupant: Some(occupant),
        })
        .await
        .expect("enqueue");

    let summary = drain_due(&state, 8).await.expect("drain");

    assert_eq!(summary.drained, 1);
    // The executor's effect is the LiveKit-side evict; the local
    // registry entry is cleared later by the participant_left webhook.
    let removes = admin.remove_calls.lock().expect("recording lock");
    assert_eq!(
        removes.len(),
        1,
        "matching-session intent must fire RemoveParticipant"
    );
    assert_eq!(removes[0].1, identity);
}

#[tokio::test]
async fn actor_present_muji_clear_with_stale_session_is_refused() {
    // #1608 (PR #1626 review round 4): the post-terminate async clear
    // can reach the room actor AFTER the same full JID rejoined as a
    // new session. The actor-present path must honor the session gate
    // too — advertisement, SFU bookkeeping, and broadcast all belong
    // to the OLD session.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-clear-session-stale@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let stale = waddle_sfu::SessionBinding::new("muji-stale").expect("binding");
    let occupant = occupant_generation();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant, &Some(owner_session.clone())),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    sfu.register_call_participant_with_session(&call_id, &identity, &current, occupant);

    let outcome = crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
        state.as_ref(),
        &room_jid,
        &alice,
        None,
        None,
        waddle_sfu::UnboundOccupantPolicy::Keep,
        Some(&stale),
    )
    .await;

    assert!(
        matches!(
            outcome,
            crate::server::routes::muc_muji_clear::WebhookEffectOutcome::Stale
        ),
        "stale-session clear must be refused, got {outcome:?}"
    );
    assert!(
        sfu.has_call_participant(&call_id, &identity),
        "stale-session clear must not unregister the current session"
    );
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_some(),
        "stale-session clear must preserve the current advertisement"
    );
}

#[tokio::test]
async fn unbound_registration_survives_a_redriven_muji_clear_but_not_the_live_connections() {
    // #1608 (PR #1626 review round 4): the post-terminate async clear
    // can reach the room actor AFTER the same full JID rejoined as a
    // new session. The actor-present path must honor the session gate
    // too — advertisement, SFU bookkeeping, and broadcast all belong
    // to the OLD session.
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-clear-session-stale@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let occupant = occupant_generation();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant, &Some(owner_session.clone())),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    // Restored after a restart: registered without a generation.
    sfu.register_call_participant(&call_id, &identity);

    // A durable redrive of an old intent (Keep) must not touch it: it may be
    // the live replacement's registration. The joined generation is presented
    // on both calls so the policy is the only variable.
    let _ = crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
        state.as_ref(),
        &room_jid,
        &alice,
        None,
        Some(occupant),
        waddle_sfu::UnboundOccupantPolicy::Keep,
        Some(&current),
    )
    .await;
    assert!(
        sfu.has_call_participant(&call_id, &identity),
        "a redriven clear must keep an unbound registration"
    );

    // The live connection's own clear (TearDown) removes it.
    let _ = crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
        state.as_ref(),
        &room_jid,
        &alice,
        None,
        Some(occupant),
        waddle_sfu::UnboundOccupantPolicy::TearDown,
        Some(&current),
    )
    .await;
    assert!(
        !sfu.has_call_participant(&call_id, &identity),
        "the live connection's clear tears down an unbound registration"
    );
}

#[tokio::test]
async fn actor_present_muji_clear_with_matching_session_still_clears() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        Arc::clone(&admin) as Arc<_>,
    ));
    let state = state_with_executor(Arc::clone(&sfu)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-clear-session-match@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let call_id = CallId::new(room_jid.to_string()).expect("call id");
    let identity = Identity::from_jid(alice.clone());
    let current = waddle_sfu::SessionBinding::new("muji-current").expect("binding");
    let occupant = occupant_generation();

    let _ = handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (occupant, &Some(owner_session.clone())),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("muji update")
        .expect("occupant update");
    sfu.register_call_participant_with_session(&call_id, &identity, &current, occupant);

    let _ = crate::server::routes::muc_muji_clear::clear_muji_presence_for_departure(
        state.as_ref(),
        &room_jid,
        &alice,
        None,
        None,
        waddle_sfu::UnboundOccupantPolicy::Keep,
        Some(&current),
    )
    .await;

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.muji_for_session("alice", &alice).is_none(),
        "matching-session clear must remove the advertisement"
    );
}
