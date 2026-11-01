use std::sync::Arc;

use jid::{BareJid, FullJid};
use waddle_sfu::{
    CallId, Identity, MediaCapabilities, ObservedCallSids, ParticipantSid, RoomSid, SessionBinding,
    SfuService, SidObservationDisposition,
};
use waddle_xmpp::muc::room_actor::UpsertMujiPresence;
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};

use super::{
    create_test_server_owner_session, drain_due, fixture_config, get_room_actor,
    handle_muc_join_with_occupancy_session, occupant_generation, snapshot_room,
    state_with_executor, CallTeardownIntent, CallTeardownStatus, RecordingAdmin, TeardownTarget,
};

#[tokio::test]
async fn queued_untagged_old_sid_after_takeover_preserves_unobserved_successor() {
    let admin = Arc::new(RecordingAdmin::default());
    let sfu = Arc::new(waddle_sfu::LiveKitSfu::with_admin(
        fixture_config(),
        admin.clone() as Arc<_>,
    ));
    let state = state_with_executor(sfu.clone()).await;
    let owner = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "queued-old-sid@muc.example.com".parse().expect("room JID");
    let alice: FullJid = "alice@example.com/web".parse().expect("JID");
    let call = CallId::try_new(room_jid.to_string()).expect("call");
    let identity = Identity::from_jid(alice.clone());
    let old_generation = occupant_generation();
    let successor = occupant_generation();
    let publish = |generation, name: &str| {
        sfu.issue_join_token_with_session(
            &call,
            &identity,
            MediaCapabilities::direct_call_peer(),
            &SessionBinding::try_new(name).expect("session"),
            generation,
            sfu.participant_registration_expectation(&call, &identity)
                .expect("expectation"),
        )
        .expect("mint")
        .expect("publish")
    };
    let old_token = publish(old_generation, "old-session");
    let room_sid = RoomSid::try_new("RM_shared").expect("room SID");
    let old_sid = ParticipantSid::try_new("PA_old").expect("old SID");
    let mut old_join = ObservedCallSids::new(Some(room_sid.clone()), Some(old_sid.clone()));
    old_join.media_generation = Some(old_generation);
    assert_eq!(
        sfu.register_call_participant_observed(&call, &identity, &old_join),
        SidObservationDisposition::Applied
    );
    let token = publish(successor, "successor-session");
    assert!(sfu.is_revoked(&old_token.jti));
    handle_muc_join_with_occupancy_session(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        (successor, Some(&owner)),
    )
    .await;
    get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("actor")
        .ask(UpsertMujiPresence {
            sender_jid: alice.clone(),
            occupant: None,
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("Muji update")
        .expect("occupant update");

    // This real durable row is created after g2 registration, defeating the
    // timestamp supersession shortcut. It retains SID-old but lost the tag.
    let store = &state.deps.protocol.call_teardown_outbox;
    let id = store
        .enqueue(CallTeardownIntent {
            call_id: call.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room_jid.clone(),
                departed: alice.clone(),
                participant_sid: Some(old_sid),
            },
            generation: None,
            occupant: None,
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::Keep,
            room_sid: Some(room_sid.clone()),
            session: None,
        })
        .await
        .expect("queued old departure");
    let queued = store.find(&id).await.expect("find").expect("row");
    assert!(
        queued.created_at_ms
            >= sfu
                .participant_registered_at(&call, &identity)
                .expect("registration time")
                .timestamp_millis()
    );
    let drained = drain_due(state.as_ref(), 8).await.expect("real drain");
    assert_eq!(drained.drained, 1);
    assert_eq!(
        store.find(&id).await.expect("find").expect("row").status,
        CallTeardownStatus::Done
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &identity),
        Some(successor)
    );
    assert!(!sfu.is_revoked(&token.jti));
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .muji_for_session("alice", &alice)
        .is_some());

    let mut joined = ObservedCallSids::new(
        Some(room_sid),
        Some(ParticipantSid::try_new("PA_successor").expect("new SID")),
    );
    joined.media_generation = Some(successor);
    assert_eq!(
        sfu.register_call_participant_observed(&call, &identity, &joined),
        SidObservationDisposition::Applied
    );
    tokio::task::yield_now().await;
    assert_eq!(
        sfu.participant_occupant_session(&call, &identity),
        Some(successor)
    );
    assert!(!sfu.is_revoked(&token.jti));
    assert!(
        admin.remove_calls.lock().expect("admin calls").is_empty(),
        "the later successor join must not trigger a delayed revocation eject"
    );
}
