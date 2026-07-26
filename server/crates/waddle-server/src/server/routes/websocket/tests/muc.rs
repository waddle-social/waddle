use super::*;
use crate::permissions::CheckPermission;
use std::io;
use std::sync::{Arc, Mutex};
use tracing::Instrument as _;

#[derive(Clone, Default)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("capture buffer lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn captured_logs(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
        .expect("captured logs are valid UTF-8")
}

fn captured_admission_denial_log(buffer: &Arc<Mutex<Vec<u8>>>) -> String {
    let logs = captured_logs(buffer);
    logs.lines()
        .find(|line| line.contains("MUC join admission denied"))
        .unwrap_or_else(|| panic!("MUC join admission denial log not found in:\n{logs}"))
        .to_string()
}

fn expected_local_room_fence(
    room_jid: &BareJid,
) -> waddle_xmpp::muc::durable::RoomClaimFenceContext {
    expected_room_fence(
        room_jid,
        waddle_xmpp::ownership::NodeIdentity::local(),
        waddle_xmpp::ownership::ClaimEpoch(0),
    )
}

fn expected_room_fence(
    room_jid: &BareJid,
    owner: waddle_xmpp::ownership::NodeIdentity,
    epoch: waddle_xmpp::ownership::ClaimEpoch,
) -> waddle_xmpp::muc::durable::RoomClaimFenceContext {
    waddle_xmpp::muc::durable::RoomClaimFenceContext::new(
        waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::RoomActor,
            room_jid.to_string(),
        ),
        owner,
        epoch,
    )
}

fn validate_local_room_fence(
    room_jid: &BareJid,
    fence: &waddle_xmpp::muc::durable::RoomClaimFenceContext,
) -> Result<(), waddle_xmpp::XmppError> {
    if fence == &expected_local_room_fence(room_jid) {
        Ok(())
    } else {
        Err(waddle_xmpp::XmppError::internal(
            "test store received an unexpected room claim fence",
        ))
    }
}

#[tokio::test]
async fn muc_stale_leave_does_not_remove_current_resource() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let current_jid: FullJid = "alice@example.com/current".parse().expect("current jid");
    let stale_jid: FullJid = "alice@example.com/stale".parse().expect("stale jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &current_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    let responses = handle_muc_leave(state.as_ref(), &room_jid, &stale_jid, "alice", None).await;

    assert_eq!(responses.len(), 1);
    let response = Element::from_str(&responses[0]).expect("leave response XML");
    assert_eq!(response.name(), "presence");
    assert_eq!(response.attr("type"), Some("unavailable"));

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&current_jid), Some("alice"));
    assert!(room.find_nick_by_real_jid(&stale_jid).is_none());
    assert_eq!(room.occupant_count(), 1);
}

/// #1108: after the dormancy janitor's guarded destroy evicts a room,
/// a join must transparently respawn it through the registry — the
/// "room not registered; dropping" failure mode must not exist for a
/// joining occupant.
#[tokio::test]
async fn muc_join_after_guarded_dormancy_eviction_respawns_room() {
    use waddle_xmpp::muc::room_actor::{IsDormant, SealGuard};
    use waddle_xmpp::muc::room_registry_actor::{CreateRoom, DestroyRoomIfInactive, RoomCount};

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "evicted-then-joined@muc.example.com"
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    // Seed a persistent, dormant room (like topology seeding does),
    // then evict it exactly as the janitor would.
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "w".to_string(),
            channel_id: "c".to_string(),
            config: waddle_xmpp::muc::RoomConfig::default(),
        })
        .await
        .expect("create room");
    let probe = actor.ask(IsDormant).await.expect("probe");
    assert!(probe.dormant);
    let destroyed: bool = state
        .deps
        .protocol
        .room_registry
        .ask(DestroyRoomIfInactive {
            room_jid: room_jid.clone(),
            expected_occupancy_revision: probe.occupancy_revision,
            guard: SealGuard::Dormant,
        })
        .await
        .expect("guarded destroy");
    assert!(destroyed, "dormant room evicted");

    // The join after eviction must succeed against a respawned room.
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
    assert_eq!(self_presence.name(), "presence");
    assert_ne!(
        self_presence.attr("type"),
        Some("error"),
        "join after eviction must not error: {responses:?}"
    );
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&sender_jid), Some("alice"));
    assert_eq!(
        state
            .deps
            .protocol
            .room_registry
            .ask(RoomCount)
            .await
            .expect("count"),
        1,
        "the registry holds the respawned room"
    );
}

#[tokio::test]
async fn muc_join_maps_registry_ownership_deferral_to_retryable_resource_constraint() {
    use waddle_xmpp::muc::room_registry_actor::ReservePendingReclaimedRoom;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "ownership-reconciling@muc.example.com"
        .parse()
        .expect("room JID");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender JID");

    state
        .deps
        .protocol
        .room_registry
        .ask(ReservePendingReclaimedRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("reserve reclaimed-room reconciliation");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    assert_eq!(responses.len(), 1, "one retryable error presence");
    let presence = Element::from_str(&responses[0]).expect("error presence XML");
    assert_eq!(presence.attr("type"), Some("error"));
    let error = presence
        .get_child("error", waddle_xmpp::parser::ns::JABBER_CLIENT)
        .expect("typed stanza error");
    assert_eq!(error.attr("type"), Some("wait"));
    assert!(
        error
            .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas",)
            .is_some(),
        "ownership reconciliation must remain retryable: {responses:?}"
    );
}

#[tokio::test]
async fn ordinary_join_coalesces_with_restoring_room_without_create_permission() {
    use std::sync::Arc;

    use waddle_xmpp::muc::durable::{
        DurableRoomState, MucDurableFuture, MucDurableStore, RoomClaimFenceContext,
    };
    use waddle_xmpp::muc::room_registry_actor::WireClusteringClaims;
    use waddle_xmpp::ownership::{
        ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    struct BlockingExistingRoomStore {
        started: Arc<tokio::sync::Notify>,
        allow: Arc<tokio::sync::Notify>,
        snapshot: DurableRoomState,
    }

    impl MucDurableStore for BlockingExistingRoomStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let validation = validate_local_room_fence(room_jid, fence);
            let started = Arc::clone(&self.started);
            let allow = Arc::clone(&self.allow);
            let snapshot = self.snapshot.clone();
            Box::pin(async move {
                validation?;
                started.notify_one();
                allow.notified().await;
                Ok(Some(snapshot))
            })
        }

        fn save_config_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a waddle_xmpp::muc::RoomConfig,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = validate_local_room_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn save_subject_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = validate_local_room_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn save_affiliation_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = validate_local_room_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn delete_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let validation = validate_local_room_fence(room_jid, fence);
            Box::pin(async move { validation })
        }

        fn check_fenced_fanout<'a>(&'a self, _room_jid: &'a BareJid) -> MucDurableFuture<'a, bool> {
            Box::pin(async { Ok(true) })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let matches = fence == &expected_local_room_fence(room_jid);
            Box::pin(async move { Ok(matches) })
        }
    }

    let state = create_test_websocket_state().await;
    let creator_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let ordinary_session = create_test_session(state.as_ref(), "bob").await;
    let admin_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "restoring-existing@muc.example.com"
        .parse()
        .expect("room JID");
    let durable_owner: BareJid = "carol@example.com".parse().expect("owner JID");
    let started = Arc::new(tokio::sync::Notify::new());
    let allow = Arc::new(tokio::sync::Notify::new());
    let store = Arc::new(BlockingExistingRoomStore {
        started: Arc::clone(&started),
        allow: Arc::clone(&allow),
        snapshot: DurableRoomState {
            waddle_id: "restored-waddle".to_string(),
            channel_id: "restored-channel".to_string(),
            config: waddle_xmpp::muc::RoomConfig {
                name: "Restored room".to_string(),
                persistent: true,
                members_only: false,
                ..Default::default()
            },
            subject: None,
            affiliations: vec![waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                durable_owner.clone(),
                Affiliation::Owner,
            )],
        },
    });
    state
        .deps
        .protocol
        .room_registry
        .ask(WireClusteringClaims {
            claim_store: Arc::new(InProcessClaimStore::new()) as Arc<dyn ClaimStore>,
            node_identity: SharedNodeIdentity::new(NodeIdentity::local()),
            durable_store: Some(store as Arc<dyn MucDurableStore>),
            rollout_backoff: None,
        })
        .await
        .expect("wire blocking durable store");

    let creator_state = Arc::clone(&state);
    let creator_room = room_jid.clone();
    let creator_jid: FullJid = "alice@example.com/web".parse().expect("creator JID");
    let creator_join = tokio::spawn(async move {
        handle_muc_join(
            creator_state.as_ref(),
            "example.com",
            &creator_room,
            &creator_jid,
            "alice",
            None,
            &Some(creator_session),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("durable restore started");

    let ordinary_state = Arc::clone(&state);
    let ordinary_room = room_jid.clone();
    let ordinary_jid: FullJid = "bob@example.com/web".parse().expect("ordinary JID");
    let ordinary_join = tokio::spawn(async move {
        handle_muc_join(
            ordinary_state.as_ref(),
            "example.com",
            &ordinary_room,
            &ordinary_jid,
            "bob",
            None,
            &Some(ordinary_session),
        )
        .await
    });
    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "restore-admin")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "owner",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let admin_state = Arc::clone(&state);
    let admin_jid: FullJid = "carol@example.com/web".parse().expect("admin JID");
    let admin_ready = ready_phase(&admin_jid);
    let admin_lookup = tokio::spawn(async move {
        handle_iq(
            &admin_iq,
            "example.com",
            "muc.example.com",
            admin_state.as_ref(),
            &Some(admin_session),
            &admin_ready,
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    assert!(
        !admin_lookup.is_finished(),
        "admin IQ must coalesce behind the pending durable restore"
    );
    allow.notify_one();

    let creator_responses = creator_join.await.expect("creator join task");
    let ordinary_responses = ordinary_join.await.expect("ordinary join task");
    let admin_responses = admin_lookup.await.expect("admin lookup task");
    assert_eq!(
        admin_responses.len(),
        1,
        "admin response: {admin_responses:?}"
    );
    assert!(
        admin_responses[0].contains("type='result'")
            && !admin_responses[0].contains("item-not-found"),
        "a restoring room must not appear absent to MUC admin IQ: {admin_responses:?}"
    );
    assert!(
        ordinary_responses
            .iter()
            .all(|response| !response.contains("not-allowed")),
        "a restoring existing room must not run create-room authorization: {ordinary_responses:?}"
    );
    for responses in [&creator_responses, &ordinary_responses] {
        let self_presence = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .find(|presence| {
                presence
                    .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                    .is_some_and(|payload| {
                        payload.children().any(|child| {
                            child.name() == "status" && child.attr("code") == Some("110")
                        })
                    })
            })
            .unwrap_or_else(|| panic!("self presence in responses: {responses:?}"));
        let payload = self_presence
            .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
            .expect("MUC user payload");
        assert!(
            payload
                .children()
                .all(|child| child.name() != "status" || child.attr("code") != Some("201")),
            "restoring an existing room must not emit creator status 201: {responses:?}"
        );
        assert_ne!(
            payload
                .get_child("item", waddle_xmpp::muc::presence::NS_MUC_USER)
                .and_then(|item| item.attr("affiliation")),
            Some("owner"),
            "the first post-restore joiner must not receive CreatorOwner"
        );
    }
    let snapshot = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(snapshot.get_affiliation(&durable_owner), Affiliation::Owner);
}

/// #1107 / XEP-0045 §7.6: a full JID already in the room under nick A
/// joining as nick B gets `<error type='cancel'><not-acceptable/>`
/// on the wire (nicknames are locked to identity) and no second
/// occupancy is created.
#[tokio::test]
async fn muc_join_under_second_nick_returns_not_acceptable() {
    // The denial increments waddle.muc.admission.denied, so the metrics
    // test lock must be held for the export window.
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "no-second-nick@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice-two",
        None,
        &Some(session),
    )
    .await;
    assert_eq!(responses.len(), 1, "one error presence: {responses:?}");
    let error_presence = Element::from_str(&responses[0]).expect("error presence xml");
    assert_eq!(error_presence.name(), "presence");
    assert_eq!(error_presence.attr("type"), Some("error"));
    let error = error_presence
        .get_child("error", "jabber:client")
        .expect("typed error element");
    assert_eq!(error.attr("type"), Some("cancel"));
    assert!(
        error
            .get_child("not-acceptable", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "XEP-0045 §7.6 locked-nickname refusal uses <not-acceptable/>: {responses:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.occupant_count(), 1, "no ghost occupancy");
    assert_eq!(room.find_nick_by_real_jid(&sender_jid), Some("alice"));

    // #1440: the locked-nick refusal is a counted join denial.
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "not-acceptable"),
                ("deny_reason", "nick_locked")
            ]
        ),
        Some(1)
    );
}

/// #1111 / XEP-0045 §7.2.9: joining a room that has reached its maximum
/// number of occupants returns `<presence type='error'>` with
/// `<error type='wait'><service-unavailable/></error>` from the
/// requested room-nick to the joiner — never an empty reply that
/// leaves the client stalled waiting for self-presence.
#[tokio::test]
async fn muc_join_full_room_returns_service_unavailable() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let carol_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "full-room@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol: FullJid = "carol@example.com/web".parse().expect("carol jid");

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .room
        .config;
    config.max_occupants = 1;
    room_actor
        .ask(UpdateConfig { config })
        .await
        .expect("room config update");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "exactly one error presence (never an empty reply): {responses:?}"
    );
    let error_presence = Element::from_str(&responses[0]).expect("error presence xml");
    assert_eq!(error_presence.name(), "presence");
    assert_eq!(error_presence.attr("type"), Some("error"));
    assert_eq!(
        error_presence.attr("from"),
        Some(format!("{room_jid}/bob").as_str()),
        "XEP-0045 §7.2.9: the error comes from the requested room-nick: {responses:?}"
    );
    assert_eq!(
        error_presence.attr("to"),
        Some(bob.to_string().as_str()),
        "the error is addressed to the joining full JID: {responses:?}"
    );
    assert!(
        error_presence
            .get_child("x", xmpp_parsers::ns::MUC)
            .is_some(),
        "join-failure presence echoes <x xmlns='http://jabber.org/protocol/muc'/>: {responses:?}"
    );
    let error = error_presence
        .get_child("error", waddle_xmpp::parser::ns::JABBER_CLIENT)
        .expect("typed error element");
    assert_eq!(
        error.attr("type"),
        Some("wait"),
        "XEP-0045 §7.2.9 max-users refusal uses error type='wait': {responses:?}"
    );
    assert_eq!(
        error.attr("by"),
        Some(room_jid.to_string().as_str()),
        "the erroring entity is the room bare JID: {responses:?}"
    );
    assert!(
        error
            .get_child("service-unavailable", waddle_xmpp::parser::ns::STANZAS)
            .is_some(),
        "XEP-0045 §7.2.9 max-users refusal uses <service-unavailable/>: {responses:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.occupant_count(), 1, "the full room admits no one");
    assert_eq!(room.find_nick_by_real_jid(&bob), None);

    room_actor
        .ask(ChangeAffiliation {
            jid: carol.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("carol admin affiliation");
    let admin_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol,
        "carol",
        None,
        &Some(carol_session),
    )
    .await;
    let admin_self = admin_responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|presence| {
            presence
                .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
                .is_some_and(|x| {
                    x.children()
                        .any(|child| child.name() == "status" && child.attr("code") == Some("110"))
                })
        })
        .expect("admin self-presence response");
    assert_ne!(
        admin_self.attr("type"),
        Some("error"),
        "XEP-0045 §7.2.9 requires owner/admin affiliations to be admitted beyond a full room: {admin_responses:?}"
    );
    let admin_item = admin_self
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .and_then(|x| x.get_child("item", waddle_xmpp::muc::presence::NS_MUC_USER))
        .expect("admin muc item");
    assert_eq!(admin_item.attr("affiliation"), Some("admin"));

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        room.occupant_count(),
        2,
        "privileged overflow join should admit the admin beyond max_occupants"
    );
    assert_eq!(room.find_nick_by_real_jid(&carol), Some("carol"));
}

/// #1134 / XEP-0045 §10.1.1: only the room creator receives Owner. The
/// second user to join an unmanaged room must not be granted Owner —
/// the created-bit comes from the registry, not from call-site
/// inference.
#[tokio::test]
async fn second_joiner_of_unmanaged_room_is_not_owner() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_server_owner_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "creator-owner@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");

    let alice_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let alice_presence = Element::from_str(&alice_responses[0]).expect("alice presence");
    let muc_user_ns = waddle_xmpp::muc::presence::NS_MUC_USER;
    let alice_item = alice_presence
        .get_child("x", muc_user_ns)
        .and_then(|x| x.get_child("item", muc_user_ns))
        .expect("alice muc item");
    assert_eq!(
        alice_item.attr("affiliation"),
        Some("owner"),
        "the creator gets Owner (XEP-0045 §10.1.1)"
    );
    let alice_status_codes: Vec<&str> = alice_presence
        .get_child("x", muc_user_ns)
        .expect("alice muc user payload")
        .children()
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        alice_status_codes.contains(&"110"),
        "the creator's reflected presence is self-presence: {alice_responses:?}"
    );
    assert!(
        alice_status_codes.contains(&"201"),
        "XEP-0045 §10.1.1 requires status 201 on the creator's self-presence: {alice_responses:?}"
    );

    let bob_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    let bob_self = bob_responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|el| {
            el.name() == "presence"
                && el.get_child("x", muc_user_ns).is_some_and(|x| {
                    x.children()
                        .any(|c| c.name() == "status" && c.attr("code") == Some("110"))
                })
        })
        .expect("bob self presence");
    let bob_item = bob_self
        .get_child("x", muc_user_ns)
        .and_then(|x| x.get_child("item", muc_user_ns))
        .expect("bob muc item");
    assert_ne!(
        bob_item.attr("affiliation"),
        Some("owner"),
        "a later joiner is not the creator and must not be Owner (#1134)"
    );
    let bob_status_codes: Vec<&str> = bob_self
        .get_child("x", muc_user_ns)
        .expect("bob muc user payload")
        .children()
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        !bob_status_codes.contains(&"201"),
        "status 201 is only for the creating join: {bob_responses:?}"
    );
}

#[tokio::test]
async fn muc_join_responses_use_client_namespace() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    assert_eq!(responses.len(), 2);

    let self_presence = Element::from_str(&responses[0]).expect("self presence xml");
    assert_eq!(self_presence.name(), "presence");
    assert_eq!(self_presence.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("jid"), Some("alice@example.com/web"));
    assert_eq!(item.attr("affiliation"), Some("owner"));
    assert_eq!(item.attr("role"), Some("moderator"));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("100")));
    assert!(user_x
        .children()
        .any(|child| child.name() == "status" && child.attr("code") == Some("110")));
    assert!(
        self_presence
            .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
            .is_some(),
        "self-presence must carry XEP-0421 occupant-id"
    );

    let subject_message = Element::from_str(&responses[1]).expect("subject xml");
    assert_eq!(subject_message.name(), "message");
    assert_eq!(subject_message.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    assert_eq!(subject_message.attr("type"), Some("groupchat"));
}

#[tokio::test]
async fn xep_0045_join_replay_exposes_existing_occupant_real_jids() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "icepuma").await;
    let room_jid: BareJid = "mentions@muc.example.com".parse().expect("room jid");

    let occupants = [
        ("icepuma", "icepuma@example.com/web"),
        ("randax", "randax@example.com/desktop"),
        ("rawkode", "rawkode@example.com/mobile"),
    ];

    for (index, (nick, full_jid)) in occupants.iter().enumerate() {
        let sender_jid: FullJid = full_jid.parse().expect("occupant jid");
        let session = if index == 0 {
            Some(owner_session.clone())
        } else {
            None
        };
        let _ = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &sender_jid,
            nick,
            None,
            &session,
        )
        .await;
    }

    let joiner: FullJid = "witness@example.com/browser".parse().expect("joiner jid");
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner,
        "witness",
        None,
        &None,
    )
    .await;

    for (nick, full_jid) in occupants {
        let from = format!("{room_jid}/{nick}");
        let replay = responses
            .iter()
            .filter_map(|xml| Element::from_str(xml).ok())
            .find(|element| {
                element.name() == "presence"
                    && element.attr("from") == Some(from.as_str())
                    && element.attr("to") == Some(joiner.as_str())
            })
            .unwrap_or_else(|| panic!("missing replay presence for {nick}: {responses:?}"));
        let user_x = replay
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .expect("muc user payload");
        let item = user_x
            .get_child("item", "http://jabber.org/protocol/muc#user")
            .expect("muc user item");

        assert_eq!(item.attr("jid"), Some(full_jid));
        // XEP-0045 registrar (#1265 item 4): status 100 rides only on
        // the joiner's initial SELF-presence, never on occupant replays.
        assert!(
            !user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
            "occupant replay must not carry status 100 for {nick}"
        );
        assert!(
            !user_x
                .children()
                .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
            "replay presence for another occupant must not be self-presence"
        );
        assert!(
            replay
                .get_child("occupant-id", waddle_xmpp::xep::xep0421::NS_OCCUPANT_ID)
                .is_some(),
            "replay presence for {nick} must carry XEP-0421 occupant-id"
        );
    }
}

#[tokio::test]
async fn managed_members_only_join_requires_explicit_channel_member_affiliation() {
    // #1440: every join denial now increments
    // `waddle.muc.admission.denied`, so tests that produce one must hold
    // the metrics test lock; otherwise their samples leak into a
    // concurrently asserting test's export window.
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
    let room_jid: BareJid = "private-space@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "private-space".to_string(),
            name: "Private Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "private-space"),
                Relation::new("parent"),
                Subject::userset(crate::permissions::SubjectType::Space, "team", ""),
            ),
        })
        .await
        .expect("channel parent tuple");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "team"),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("space member tuple");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "open-space".to_string(),
            name: "Open Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("open channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "open-space"),
                Relation::new("parent"),
                Subject::userset(crate::permissions::SubjectType::Space, "team", ""),
            ),
        })
        .await
        .expect("open channel parent tuple");
    let open_room_jid: BareJid = "open-space@muc.example.com".parse().expect("open room jid");
    let open_join = handle_muc_join(
        state.as_ref(),
        "example.com",
        &open_room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;
    assert!(
        open_join
            .first()
            .is_some_and(|frame| frame.contains("affiliation='none'")
                || frame.contains(r#"affiliation="none""#)),
        "inherited read access must not masquerade as persistent MUC membership in open rooms: {open_join:?}"
    );

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;
    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "members-only MUC admission must not treat inherited read access as membership: {denied:?}"
    );
    assert!(
        get_room_actor(state.as_ref(), &room_jid).await.is_none(),
        "denied members-only join must not create the room actor"
    );

    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "private-space"),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("channel member tuple");

    let admitted = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session),
    )
    .await;
    assert!(
        admitted
            .first()
            .is_some_and(|frame| frame.contains("affiliation='member'")
                || frame.contains(r#"affiliation="member""#)),
        "explicit channel member affiliation must admit the members-only join: {admitted:?}"
    );
}

/// #1315: a managed members-only join by an unaffiliated user is
/// rejected with `<registration-required/>`, and that denial must now
/// be visible — the `waddle.muc.admission.denied` counter records it,
/// keyed by the stanza error condition, through the metric-reader seam.
#[tokio::test(flavor = "current_thread")]
async fn managed_registration_required_denial_increments_admission_counter() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let state = create_test_websocket_state().await;
    let session = crate::auth::Session::new("alice@example.com", "alice", "alice");
    let room_jid: BareJid = "locked-space@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "locked-space".to_string(),
            name: "Locked Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    // Match the production dispatch span declaration so the denial helper
    // records the allowlisted condition without classifying policy as failure.
    let dispatch_span =
        tracing::info_span!("xmpp.stanza.dispatch", condition = tracing::field::Empty);
    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session),
    )
    .instrument(dispatch_span)
    .await;
    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "unaffiliated managed members-only join must be registration-required: {denied:?}"
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "registration-required"),
                ("deny_reason", "membership_required"),
            ]
        ),
        Some(1),
        "the registration-required admission denial must increment the counter exactly once"
    );
    // Assert on the RECORDED span field, not the exported span: this
    // join's scope fans out actor asks, and the dispatch span only exports
    // once every kameo child (`actor.handle_message` / `actor.lifecycle`,
    // parented under the caller's span) closes — a straggling or
    // spawned-inside-the-scope actor can hold it open past this assertion
    // point indefinitely (#1479). Field records happen synchronously on
    // the denial call path, so the observer sees them deterministically.
    // The record→OTel-attribute export fidelity, and the Unset-vs-Error
    // status split, stay pinned by `managed_internal_admission_failure_
    // exports_error_dispatch_span` below and the frame-backstop span
    // tests, whose narrower scopes export deterministically. (A denial
    // never calls `mark_span_error` unless the condition is
    // internal-server-error, which the condition assertion excludes.)
    assert_eq!(
        spans.recorded_field("xmpp.stanza.dispatch", "condition"),
        Some("registration-required".to_string()),
        "the dispatch span must carry the allowlisted stanza condition"
    );
    assert!(
        get_room_actor(state.as_ref(), &room_jid).await.is_none(),
        "denied members-only join must not create the room actor"
    );
}

/// #1315: an unauthenticated managed-channel join is rejected with
/// `<not-authorized/>`; the denial must be counted and logged at INFO
/// without exposing the joining resource in the denial event's user field.
#[tokio::test(flavor = "current_thread")]
async fn managed_not_authorized_denial_emits_admission_telemetry() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CaptureWriter(buffer.clone()))
            .finish(),
    );
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "private-space@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "private-space".to_string(),
            name: "Private Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &None,
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("not-authorized"),
        "unauthenticated managed-channel join must be not-authorized: {denied:?}"
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "not-authorized"),
                ("deny_reason", "session_missing"),
            ]
        ),
        Some(1),
        "the not-authorized admission denial must increment the counter exactly once"
    );
    assert_eq!(
        metrics.metric_unit("waddle.muc.admission.denied"),
        Some("1".to_string()),
        "the admission denial counter must use the dimensionless unit"
    );

    let denial_log = captured_admission_denial_log(&buffer);
    assert!(denial_log.contains("\"level\":\"INFO\""), "{denial_log}");
    assert!(
        denial_log.contains("\"room\":\"private-space@muc.example.com\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"user\":\"alice@example.com\""),
        "{denial_log}"
    );
    let all_logs = captured_logs(&buffer);
    assert!(!all_logs.contains("alice@example.com/web"), "{all_logs}");
    assert!(
        denial_log.contains("\"condition\":\"not-authorized\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"resolver_outcome\":\"session-missing\""),
        "{denial_log}"
    );
}

/// #1315: malformed authenticated-session identity is an internal
/// managed-channel admission failure. It must emit the same condition-only
/// counter and bare-JID INFO fields as policy denials.
#[tokio::test(flavor = "current_thread")]
async fn managed_internal_server_error_denial_emits_admission_telemetry() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CaptureWriter(buffer.clone()))
            .finish(),
    );
    let state = create_test_websocket_state().await;
    let malformed_session = crate::auth::Session::new("not a jid", "alice", "alice");
    let room_jid: BareJid = "resolver-failure@muc.example.com"
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "resolver-failure".to_string(),
            name: "Resolver Failure".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(malformed_session),
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("internal-server-error"),
        "malformed managed-channel identity must fail closed: {denied:?}"
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "internal-server-error"),
                ("deny_reason", "session_identity_malformed"),
            ]
        ),
        Some(1),
        "the internal-server-error admission denial must increment the counter exactly once"
    );

    let denial_log = captured_admission_denial_log(&buffer);
    assert!(denial_log.contains("\"level\":\"INFO\""), "{denial_log}");
    assert!(
        denial_log.contains("\"room\":\"resolver-failure@muc.example.com\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"user\":\"alice@example.com\""),
        "{denial_log}"
    );
    let all_logs = captured_logs(&buffer);
    assert!(!all_logs.contains("alice@example.com/web"), "{all_logs}");
    assert!(
        denial_log.contains("\"condition\":\"internal-server-error\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"resolver_outcome\":\"session-jid-malformed\""),
        "{denial_log}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn managed_internal_admission_failure_exports_error_dispatch_span() {
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let spans = waddle_xmpp::telemetry::test_support::acquire_spans();
    let state = create_test_websocket_state().await;
    let malformed_session = crate::auth::Session::new("not a jid", "alice", "alice");
    let room_jid: BareJid = "resolver-span-failure@muc.example.com"
        .parse()
        .expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "resolver-span-failure".to_string(),
            name: "Resolver Span Failure".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    // This malformed authenticated identity is an internal resolver failure,
    // not a policy denial, so the same condition attribute carries ERROR status.
    let dispatch_span =
        tracing::info_span!("xmpp.stanza.dispatch", condition = tracing::field::Empty);
    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(malformed_session),
    )
    .instrument(dispatch_span)
    .await;

    assert!(
        denied
            .first()
            .is_some_and(|frame| frame.contains("internal-server-error")),
        "malformed identity must fail closed: {denied:?}"
    );

    // The span assertions below have failed in CI twice while the frame
    // assertion above passed, and have never reproduced locally
    // (including under CI's exact `--workspace --all-features --lib
    // --tests` nextest command). Take ONE snapshot and assert against
    // it: `attribute_of`/`status_of` each force-flush and re-read, so
    // asserting through them after building a debug string would compare
    // a different snapshot than the one printed. Dumping the snapshot
    // lets the next occurrence distinguish the candidates instead of
    // just reporting `None` — no dispatch span exported at all (held
    // open by a stray span clone), one present without the attribute
    // (the stamp landed on another span), or several same-named spans
    // where only a later one carries it.
    let dispatch_spans: Vec<_> = spans
        .exported()
        .into_iter()
        .filter(|span| span.name == "xmpp.stanza.dispatch")
        .collect();
    let rendered = dispatch_spans
        .iter()
        .map(|span| {
            let attributes: Vec<String> = span
                .attributes
                .iter()
                .map(|attribute| format!("{}={}", attribute.key.as_str(), attribute.value))
                .collect();
            format!(
                "{{status={:?} attrs=[{}]}}",
                span.status,
                attributes.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");

    let condition = dispatch_spans.iter().find_map(|span| {
        span.attributes
            .iter()
            .find(|attribute| attribute.key.as_str() == "condition")
            .map(|attribute| attribute.value.to_string())
    });
    assert_eq!(
        condition,
        Some("internal-server-error".to_string()),
        "exported xmpp.stanza.dispatch spans: [{rendered}]",
    );
    assert!(
        dispatch_spans
            .iter()
            .any(|span| matches!(span.status, opentelemetry::trace::Status::Error { .. })),
        "exported xmpp.stanza.dispatch spans: [{rendered}]",
    );
}

/// #1440: a managed-channel lookup failure bounces the join with a
/// wait-type error, so it must be visible like any other join denial —
/// counted under its condition AND its refusal site, and logged once.
#[tokio::test(flavor = "current_thread")]
async fn managed_channel_lookup_failure_counts_as_a_wait_denial() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::INFO)
            .with_writer(CaptureWriter(buffer.clone()))
            .finish(),
    );
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "lookup-failure@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    state.deps.app_state.db_pool.global_actor().kill();
    tokio::task::yield_now().await;

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &None,
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(denied[0].contains("internal-server-error"), "{denied:?}");
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "internal-server-error"),
                ("deny_reason", "managed_channel_lookup"),
            ]
        ),
        Some(1),
        "a wait-type lookup-failure denial must be counted under its refusal site"
    );

    let denial_log = captured_admission_denial_log(&buffer);
    assert!(
        denial_log.contains("\"resolver_outcome\":\"managed-channel-lookup-error\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"deny_reason\":\"managed_channel_lookup\""),
        "{denial_log}"
    );
    assert!(
        denial_log.contains("\"managed_channel\":false"),
        "{denial_log}"
    );
}

/// #1315: a channel-banned (outcast) joiner is rejected with
/// `<forbidden/>` per XEP-0045 §7.2.8, even in an open room. The
/// denial must be counted under the `forbidden` condition through the
/// metric-reader seam.
#[tokio::test]
async fn managed_forbidden_ban_denial_increments_admission_counter() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let session = crate::auth::Session::new("mallory@example.com", "mallory", "mallory");
    let room_jid: BareJid = "guarded-space@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "mallory@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "guarded-space".to_string(),
            name: "Guarded Space".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("channel upsert");

    // An explicit channel-level ban makes the resolver report Outcast,
    // which XEP-0045 §7.2.8 maps to <forbidden/> even in an open room.
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "guarded-space"),
                Relation::new("outcast"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("channel outcast tuple");

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "mallory",
        None,
        &Some(session),
    )
    .await;
    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("forbidden"),
        "a channel-banned managed joiner must be forbidden: {denied:?}"
    );
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[("condition", "forbidden"), ("deny_reason", "channel_ban")]
        ),
        Some(1),
        "the forbidden (ban) admission denial must increment the counter exactly once"
    );
}

#[tokio::test]
async fn managed_public_channel_allows_deployment_member_without_channel_tuple() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "project".to_string(),
            name: "Project".to_string(),
            description: None,
            channel_type: "text".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("server member tuple");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "alice",
        None,
        &Some(session),
    )
    .await;

    let presence =
        Element::from_str(responses.first().expect("self-presence")).expect("presence XML");
    assert_eq!(presence.name(), "presence");
    assert_ne!(
        presence.attr("type"),
        Some("error"),
        "public/open managed channel must admit deployment members: {responses:?}"
    );
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("affiliation"), Some("none"));
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "successful join must complete with XEP-0045 self-presence 110: {responses:?}"
    );
}

#[tokio::test]
async fn managed_join_uses_live_actor_members_only_config_over_stale_channel_row() {
    // #1440: every join denial now increments
    // `waddle.muc.admission.denied`, so tests that produce one must hold
    // the metrics test lock; otherwise their samples leak into a
    // concurrently asserting test's export window.
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "chat@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "bob@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "chat".to_string(),
            name: "Chat".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: true,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("channel upsert");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Server, DEPLOYMENT_SERVER_ID),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("server member tuple");

    let actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "chat".to_string(),
    )
    .await
    .expect("room actor")
    .actor_ref;

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "bob",
        None,
        &Some(session),
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "live actor members-only config must override stale open channel row: {denied:?}"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert!(snapshot.find_nick_by_real_jid(&sender_jid).is_none());
}

/// Review F3: on the members-only path a `Resolver(None)` verdict makes
/// the handler return registration-required BEFORE any actor message,
/// so a stale resolver-derived Member inside a LIVE room actor (written
/// before the revocation) was never cleared — the revoked user stayed
/// on the room's affiliation list (admin queries, XEP-0045 §7.x member
/// lists) until eviction. The rejection must best-effort sync
/// `Affiliation::None` into the existing actor.
#[tokio::test]
async fn rejected_members_only_join_clears_stale_resolver_affiliation_in_live_actor() {
    // #1440: every join denial now increments
    // `waddle.muc.admission.denied`, so tests that produce one must hold
    // the metrics test lock; otherwise their samples leak into a
    // concurrently asserting test's export window.
    let _metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "revoked@muc.example.com".parse().expect("room jid");
    let sender_jid: FullJid = "bob@example.com/web".parse().expect("sender jid");

    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: "revoked".to_string(),
            name: "Revoked".to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only: true,
            public_room: false,
        },
    )
    .await
    .expect("channel upsert");

    let actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        RoomConfig {
            members_only: true,
            ..RoomConfig::default()
        },
        "space".to_string(),
        "revoked".to_string(),
    )
    .await
    .expect("room actor")
    .actor_ref;

    // Stale resolver-derived Member from before the revocation.
    let seed_revision = snapshot_room(state.as_ref(), &room_jid)
        .await
        .admission_revision;
    let seeded = actor
        .ask(waddle_xmpp::muc::room_actor::SyncResolverAffiliation {
            jid: sender_jid.to_bare(),
            affiliation: waddle_xmpp::Affiliation::Member,
            expected_admission_revision: seed_revision,
        })
        .await
        .expect("seed stale resolver-derived member");
    assert_eq!(
        seeded,
        waddle_xmpp::muc::room_actor::ResolverAffiliationSyncOutcome::Applied {
            admission_revision: snapshot_room(state.as_ref(), &room_jid)
                .await
                .admission_revision,
        },
        "seeding the stale member must apply"
    );

    // No permission tuples exist for bob: the resolver reports None.
    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender_jid,
        "bob",
        None,
        &Some(session),
    )
    .await;
    assert_eq!(denied.len(), 1, "denied join: {denied:?}");
    assert!(
        denied[0].contains("registration-required"),
        "revoked user's members-only join must be refused: {denied:?}"
    );

    let room = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let room = snapshot_room(state.as_ref(), &room_jid).await.room;
            if room.get_affiliation(&sender_jid.to_bare()) == waddle_xmpp::Affiliation::None {
                break room;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached resolver repair must complete within the test deadline");
    assert_eq!(
        room.get_affiliation(&sender_jid.to_bare()),
        waddle_xmpp::Affiliation::None,
        "the rejection must clear the stale resolver-derived Member from the live actor"
    );
}

#[tokio::test]
async fn standard_members_only_join_rejects_unaffiliated_user() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "standard-private@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = actor.ask(GetConfig).await.expect("config");
    config.members_only = true;
    actor
        .ask(UpdateConfig { config })
        .await
        .expect("members-only config");

    let denied = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &None,
    )
    .await;

    assert_eq!(denied.len(), 1);
    assert!(
        denied[0].contains("registration-required"),
        "standard members-only MUC admission must reject unaffiliated users: {denied:?}"
    );
    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.occupant_count(), 1);
    assert!(snapshot.find_nick_by_real_jid(&bob_jid).is_none());
}

#[tokio::test]
async fn xep_0045_section_7_2_15_join_replay_serializes_full_subject_envelope() {
    // Boundary test for the WebSocket join wiring (Copilot review,
    // PR #319). Pre-populates `MucRoom.subject` with a multi-language
    // SubjectState via the production `SetSubject` actor message,
    // then drives a fresh join through `handle_muc_join` and asserts
    // the serialized subject message carries every conformance
    // element: `from='room/setter_nick'`, every persisted
    // `<subject xml:lang='...'>`, the XEP-0203 `<delay/>` from the
    // room JID, and the XEP-0421 `<occupant-id/>`.
    use chrono::TimeZone;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let setter_session = create_test_server_owner_session(state.as_ref(), "setter").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let setter_jid: FullJid = "setter@example.com/web".parse().expect("setter jid");
    let joiner_jid: FullJid = "alice@example.com/web".parse().expect("joiner jid");

    // Bootstrap the room actor by joining the setter (first joiner
    // becomes Owner → Moderator), then seed the subject state with
    // a multi-language `texts` map matching what a real §8.1
    // dispatch would produce.
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &setter_jid,
        "setter-nick",
        None,
        &Some(setter_session),
    )
    .await;
    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let texts = waddle_xmpp::muc::RoomSubjectTexts::from_iter([
        (String::new(), "Default subject".to_string()),
        ("en".to_string(), "English subject".to_string()),
    ]);
    let set_at = chrono::Utc.with_ymd_and_hms(2026, 5, 2, 12, 0, 0).unwrap();
    room_actor
        .ask(SetSubject {
            texts,
            setter: setter_jid.to_bare(),
            setter_nick: "setter-nick".to_string(),
            set_at,
        })
        .await
        .expect("SetSubject succeeds");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &joiner_jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // 1 existing-occupant presence (setter) + self-presence + subject = 3.
    assert_eq!(
        responses.len(),
        3,
        "join responses: existing-occupants + self-presence + subject"
    );

    let subject_msg =
        Element::from_str(responses.last().expect("subject is last")).expect("subject xml");
    assert_eq!(subject_msg.name(), "message");
    assert_eq!(subject_msg.attr("type"), Some("groupchat"));
    assert_eq!(
        subject_msg.attr("from"),
        Some("channel@muc.example.com/setter-nick"),
        "§7.2.15 nick-form `from` for set room"
    );

    let subject_children: Vec<&Element> = subject_msg
        .children()
        .filter(|c| c.name() == "subject")
        .collect();
    assert_eq!(
        subject_children.len(),
        2,
        "every persisted xml:lang variant round-trips into the join replay"
    );
    // minidom 0.18 keys attributes by `(Namespace, NcName)` — `xml:lang`
    // lives in the XML namespace, not the default. Use `attr_ns` to read it.
    let lang_of = |c: &Element| {
        c.attr_ns(&minidom::rxml::Namespace::XML, "lang")
            .map(str::to_string)
    };
    let default_subject = subject_children
        .iter()
        .find(|c| lang_of(c).as_deref().map(|v| v.is_empty()).unwrap_or(true))
        .expect("default-language subject present");
    assert_eq!(default_subject.text(), "Default subject");
    let en_subject = subject_children
        .iter()
        .find(|c| lang_of(c).as_deref() == Some("en"))
        .expect("xml:lang=en subject present");
    assert_eq!(en_subject.text(), "English subject");

    let delay = subject_msg
        .get_child("delay", "urn:xmpp:delay")
        .expect("XEP-0203 <delay/> stamped per §7.2.15 SHOULD");
    assert_eq!(
        delay.attr("from"),
        Some("channel@muc.example.com"),
        "§7.2.15 conditional MUST: delay's `from` is the room JID"
    );
    assert!(
        delay.attr("stamp").is_some_and(|s| !s.is_empty()),
        "delay stamp present and non-empty"
    );

    let occupant_id = subject_msg
        .get_child("occupant-id", "urn:xmpp:occupant-id:0")
        .expect("XEP-0421 <occupant-id/> stamped on set-subject replay");
    assert!(
        occupant_id.attr("id").is_some_and(|s| !s.is_empty()),
        "occupant-id `id` attribute present"
    );

    assert!(
        subject_msg.children().all(|c| c.name() != "body"),
        "subject message MUST have no <body/>"
    );
}

#[test]
fn test_parse_room_jid_valid() {
    let jid: jid::BareJid = "channel456@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "channel456");
}

#[test]
fn test_parse_room_jid_fallback() {
    let jid: jid::BareJid = "singlename@muc.example.com".parse().unwrap();
    let (waddle, channel) = parse_room_jid_context(&jid);
    assert_eq!(waddle, "space");
    assert_eq!(channel, "singlename");
}

#[tokio::test]
async fn native_muc_admin_rejects_mixed_role_and_affiliation_set() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-mixed@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "admin-mixed")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "alice")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("bad-request"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(snapshot.occupant_count(), 1);
    assert_ne!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Outcast
    );
}

#[tokio::test]
async fn native_muc_admin_last_owner_demotion_returns_conflict() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-last-owner@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-last-owner",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("conflict"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Owner
    );
}

#[tokio::test]
async fn native_muc_admin_admin_banning_owner_returns_not_allowed() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-owner-ban@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_bare,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("second owner affiliation");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-owner-ban",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("not-allowed"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Owner
    );
    assert_eq!(snapshot.find_nick_by_real_jid(&alice_jid), Some("alice"));
}

#[tokio::test]
async fn native_muc_admin_admin_cannot_grant_admin_affiliation() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-list-owner-only@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_bare.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("carol member affiliation");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-grant-admin",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "admin",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::Member);
}

#[tokio::test]
async fn native_muc_admin_admin_cannot_kick_another_admin_role() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let carol_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "admin-role-admin-target@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_jid: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("bob admin affiliation");
    actor
        .ask(ChangeAffiliation {
            jid: carol_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("carol admin affiliation");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol_jid,
        "carol",
        None,
        &Some(carol_session),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-admin-target",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "carol")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("not-allowed"));
    // XEP-0045 §8.4/§9.7: the denial returns <not-allowed/> "along
    // with the offending item(s)" — the error IQ echoes the muc#admin
    // query with the item the sender tried to apply.
    {
        let error_iq = Element::from_str(&responses[0]).expect("error IQ XML");
        let echoed = error_iq
            .get_child("query", waddle_xmpp::muc::NS_MUC_ADMIN)
            .expect("denial echoes the muc#admin query");
        let item = echoed
            .get_child("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .expect("denial echoes the offending item");
        assert_eq!(item.attr("nick"), Some("carol"));
        assert_eq!(item.attr("role"), Some("none"));
    }

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    let carol = snapshot.get_occupant("carol").expect("carol occupant");
    assert_eq!(carol.role, waddle_xmpp::Role::Moderator);
    assert_eq!(snapshot.find_nick_by_real_jid(&carol_jid), Some("carol"));
}

#[tokio::test]
async fn native_muc_admin_membership_grant_allows_optional_nick() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-member-nick@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-member-nick",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "thirdwitch")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session.clone()),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::Member);
}

#[tokio::test]
async fn native_muc_admin_get_role_list_accepts_role_without_nick() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-role-list@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-list",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='result'"));
    assert!(responses[0].contains("nick='bob'"));
    assert!(responses[0].contains("role='moderator'"));
    assert!(responses[0].contains("affiliation='none'"));
    assert!(responses[0].contains("jid='bob@example.com/web'"));
}

#[tokio::test]
async fn native_muc_admin_moderator_cannot_retrieve_affiliation_list() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-affiliation-list-authz@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let bob_ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let role_list_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-role-list",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let role_responses = handle_iq(
        &role_list_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &bob_ready,
    )
    .await;
    assert_eq!(role_responses.len(), 1, "role response: {role_responses:?}");
    assert!(role_responses[0].contains("type='result'"));
    assert!(role_responses[0].contains("nick='bob'"));

    let affiliation_list_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-affiliation-list",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "owner",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let affiliation_responses = handle_iq(
        &affiliation_list_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &bob_ready,
    )
    .await;
    assert_eq!(
        affiliation_responses.len(),
        1,
        "affiliation response: {affiliation_responses:?}"
    );
    assert!(affiliation_responses[0].contains("type='error'"));
    assert!(
        affiliation_responses[0].contains("forbidden"),
        "XEP-0045 §9.5/§9.8 affiliation-list retrieval is owner/admin-affiliation only: {affiliation_responses:?}"
    );
}

#[tokio::test]
async fn native_muc_admin_affiliation_can_retrieve_affiliation_list() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-affiliation-list-read@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let bob_ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: Some(bob_jid.to_bare()),
                nick: None,
                affiliation: Some(Affiliation::Admin),
                role: None,
                reason: None,
            }],
        })
        .await
        .expect("owner grants admin affiliation");

    let affiliation_list_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-affiliation-list-read",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "owner",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let responses = handle_iq(
        &affiliation_list_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &bob_ready,
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "admin affiliation response: {responses:?}"
    );
    assert!(responses[0].contains("type='result'"));
    assert!(
        responses[0].contains("affiliation='owner'"),
        "admin affiliations may retrieve owner/admin lists in non-anonymous rooms: {responses:?}"
    );
}

#[tokio::test]
async fn native_muc_admin_role_set_allows_jid_echo_without_affiliation() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "admin-role-jid-echo@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-role-jid-echo",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                bob_jid.to_bare().to_string(),
                            )
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "visitor")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    // §8.2 ordering (#1265 item 6): IQ result first, moderator's own
    // broadcast copy second.
    assert_eq!(responses.len(), 2, "admin response: {responses:?}");
    assert!(responses[1].contains("<presence"), "{responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    let bob = snapshot.get_occupant("bob").expect("bob occupant");
    assert_eq!(bob.role, waddle_xmpp::Role::Visitor);
}

#[tokio::test]
async fn native_muc_admin_role_moderator_cannot_grant_moderator_role() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let carol_session = create_test_session(state.as_ref(), "carol").await;
    let room_jid: BareJid = "admin-role-moderator-grant@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_jid: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol_jid,
        "carol",
        None,
        &Some(carol_session),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-grant-role",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "carol")
                            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    let carol = snapshot.get_occupant("carol").expect("carol occupant");
    assert_eq!(carol.role, waddle_xmpp::Role::Participant);
}

#[tokio::test]
async fn native_muc_admin_role_moderator_cannot_write_affiliations_durably() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let room_jid: BareJid = "moderator-durable@muc.example.com"
        .parse()
        .expect("room jid");
    let channel_id = waddle_xmpp::parse_managed_room_jid(&room_jid).expect("managed channel id");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let carol_bare: BareJid = "carol@example.com".parse().expect("carol jid");
    let ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;

    let actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    actor
        .ask(ApplyAdminItems {
            sender_jid: alice_jid.clone(),
            sender_affiliation: Affiliation::Owner,
            sender_role: waddle_xmpp::Role::Moderator,
            items: vec![waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: Some("bob".to_string()),
                affiliation: None,
                role: Some(waddle_xmpp::Role::Moderator),
                reason: None,
            }],
        })
        .await
        .expect("owner grants temporary moderator role");

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "moderator-member-write",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                carol_bare.to_string(),
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = actor.ask(GetSnapshot).await.expect("snapshot").room;
    assert_eq!(snapshot.get_affiliation(&carol_bare), Affiliation::None);

    let durable_membership = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(carol_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .expect("permission check");
    assert!(
        !durable_membership.allowed,
        "role-only moderators must not persist channel membership"
    );
}

#[tokio::test]
async fn native_muc_admin_self_ban_returns_conflict() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-self-ban@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-self-ban",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "alice@example.com",
                            )
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "outcast",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("conflict"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_ne!(
        snapshot.get_affiliation(&alice_jid.to_bare()),
        Affiliation::Outcast
    );
    assert_eq!(snapshot.occupant_count(), 1);
}

#[tokio::test]
async fn native_muc_admin_incomplete_affiliation_item_returns_bad_request() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "admin-incomplete@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let admin_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "admin-incomplete",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("jid").to_owned(),
                                "bob@example.com",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &admin_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "admin response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("bad-request"));
}

#[tokio::test]
async fn standard_muc_owner_config_persists_room_and_enforces_nonanonymous_defaults() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "project@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_roomname",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Project Room")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_persistentroom",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-submit")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "project")
        .await
        .expect("channel lookup")
        .expect("persisted channel");
    assert_eq!(channel.name, "Project Room");

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert!(snapshot.config.persistent);

    let disco = disco_items_iq_frame("muc-items", "muc.example.com", None);
    let responses = handle_iq(
        &disco,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    assert!(responses[0].contains("project@muc.example.com"));
}

#[tokio::test]
async fn standard_muc_owner_config_broadcasts_config_change_status_codes() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;

    let room_jid: BareJid = "config-broadcast@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let ready = ready_phase(&alice_jid);

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob_jid.clone(), bob_tx);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_roomname",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Config Broadcast")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_enablelogging",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "owner-config-broadcast",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session.clone()),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let broadcast = bob_rx
        .try_recv()
        .expect("Bob receives XEP-0045 config-change notification");
    let xml = stanza_to_xml(&broadcast.stanza);
    let message = Element::from_str(&xml).expect("config-change message XML");
    assert_eq!(message.name(), "message");
    assert_eq!(message.attr("from"), Some(room_jid.to_string().as_str()));
    assert_eq!(message.attr("to"), Some(bob_jid.to_string().as_str()));
    assert_eq!(message.attr("type"), Some("groupchat"));
    let user_x = message
        .get_child("x", waddle_xmpp::muc::presence::NS_MUC_USER)
        .expect("muc#user payload");
    let status_codes: Vec<&str> = user_x
        .children()
        .filter(|child| child.name() == "status")
        .filter_map(|status| status.attr("code"))
        .collect();
    assert!(
        status_codes.contains(&"171"),
        "XEP-0045 §10.2.1 logging disabled status missing: {xml}"
    );
    assert!(
        status_codes.contains(&"104"),
        "XEP-0045 §10.2.1 non-privacy config status missing: {xml}"
    );
    assert_eq!(status_codes, vec!["171", "104"]);

    let owner_get = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "owner-config-broadcast-get",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );
    let get_responses = handle_iq(
        &owner_get,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert_eq!(
        get_responses.len(),
        1,
        "owner get response: {get_responses:?}"
    );
    let owner_result = Element::from_str(&get_responses[0]).expect("owner get result XML");
    let form = owner_result
        .get_child("query", waddle_xmpp::muc::NS_MUC_OWNER)
        .and_then(|query| query.get_child("x", waddle_xmpp::muc::DATA_FORMS_NS))
        .expect("owner config form");
    let form_vars: Vec<&str> = form
        .children()
        .filter(|field| field.name() == "field" && field.ns() == waddle_xmpp::muc::DATA_FORMS_NS)
        .filter_map(|field| field.attr("var"))
        .collect();
    assert_eq!(
        form_vars,
        vec![
            "FORM_TYPE",
            "muc#roomconfig_roomname",
            "muc#roomconfig_roomdesc",
            "muc#roomconfig_membersonly",
            "muc#roomconfig_publicroom",
            "muc#roomconfig_moderatedroom",
            "muc#roomconfig_maxusers",
            "muc#roomconfig_enablelogging",
            "muc#roomconfig_changesubject",
            waddle_xmpp::xep::FIELD_FORUM_MODE,
            "muc#roomconfig_allowpm",
            waddle_xmpp::muc::owner::FIELD_PIN_PERMISSION,
        ]
    );
}

#[tokio::test]
async fn standard_muc_owner_get_returns_config_without_persisting_room() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;

    let room_jid: BareJid = "config-get@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let ready = ready_phase(&alice_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-get")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
    assert!(responses[0].contains("type='result'"));
    assert!(responses[0].contains("muc#roomconfig_roomname"));

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel = crate::server::xmpp_state::get_xmpp_channel(actor, "config-get")
        .await
        .expect("channel lookup");
    assert!(channel.is_none());
}

#[tokio::test]
async fn standard_muc_owner_config_rejects_non_owner() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;

    let room_jid: BareJid = "locked@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    let bob_ready = ready_phase(&bob_jid);

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_roomname",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("Hacked")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_iq = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "owner-submit")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session.clone()),
        &bob_ready,
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert_ne!(snapshot.config.name, "Hacked");

    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    room_actor
        .ask(ChangeAffiliation {
            jid: bob_jid.to_bare(),
            affiliation: Affiliation::Admin,
        })
        .await
        .expect("set admin affiliation");

    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob_session),
        &bob_ready,
    )
    .await;
    assert_eq!(
        responses.len(),
        1,
        "admin owner config response: {responses:?}"
    );
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("forbidden"));
}

#[tokio::test]
async fn room_disco_info_advertises_parent_space_metadata_for_linked_channel() {
    let state = create_test_websocket_state().await;
    let space_db = state.deps.app_state.db_pool.global();
    let conn = space_db.guard().await.expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)",
            crate::db_params!["linked", "Linked", "Linked channel description"],
        )
        .await
        .expect("insert channel");
    drop(conn);
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "linked".to_string(),
        name: "Linked".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "team", &item, None, false)
        .await
        .expect("publish bookmark");

    let query = disco_info_iq_frame("room-info", "linked@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_nonanonymous"));
    assert!(response.contains("urn:xmpp:spaces:0"));
    assert!(response.contains("var='parent'"));
    assert!(response.contains("xmpp:spaces.example.com?;node=team"));
    assert!(response.contains("http://jabber.org/protocol/muc#roominfo"));
    assert!(response.contains("muc#roomconfig_pubsub"));
    assert!(response.contains("muc#roominfo_description"));
    assert!(response.contains("Linked channel description"));
    assert!(
        !response.contains("urn:xmpp:muc-activity"),
        "room disco must not advertise XEP-0502 without truthful messages/hour data: {response}"
    );
    assert!(
        !response.contains("message-activity"),
        "room disco must not emit the XEP-0502 roominfo field without truthful data: {response}"
    );
}

#[tokio::test]
async fn active_room_disco_preserves_managed_announcement_channel_type() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("persistent connection");
    conn.execute(
            "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'announcement', 0, 0, 0, 1)",
            crate::db_params!["announcements", "Announcements", "Owner-posted announcements"],
        )
        .await
        .expect("insert announcement channel");
    drop(conn);

    let room_jid: BareJid = "announcements@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(session.clone()),
    )
    .await;

    let query = disco_info_iq_frame("announcement-info", "announcements@muc.example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("room disco response");
    assert!(response.contains("muc_moderated"), "response: {response}");
    assert!(response.contains("muc_open"), "response: {response}");
    assert!(
        !response.contains("muc_membersonly"),
        "response: {response}"
    );
    assert!(response.contains("muc_public"), "response: {response}");
    assert!(
        response.contains("waddle#channel_type"),
        "response: {response}"
    );
    assert!(response.contains("announcement"), "response: {response}");
    assert!(
        !response.contains("<value>text</value>"),
        "announcement room must not be reported as text: {response}"
    );
}

#[tokio::test]
async fn muc_owner_config_cannot_unmoderate_managed_announcement_channel() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("persistent connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'announcement', 0, 0, 0, 1)",
        crate::db_params![
            "ops-announcements",
            "Ops Announcements",
            "Owner-posted operational updates"
        ],
    )
    .await
    .expect("insert announcement channel");
    drop(conn);

    let room_jid: BareJid = "ops-announcements@muc.example.com"
        .parse()
        .expect("room jid");
    let alice_jid: FullJid = format!("{}@example.com/web", alice_session.xmpp_localpart)
        .parse()
        .expect("alice jid");
    let bob_jid: FullJid = format!("{}@example.com/web", bob_session.xmpp_localpart)
        .parse()
        .expect("bob jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let submit_form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("var").to_owned(),
                    "muc#roomconfig_moderatedroom",
                )
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append("0")
                        .build(),
                )
                .build(),
        )
        .build();
    let owner_set = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "announcement-owner-submit",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(submit_form)
                    .build(),
            )
            .build(),
    );

    let responses = handle_iq(
        &owner_set,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session.clone()),
        &ready_phase(&alice_jid),
    )
    .await;
    assert_eq!(responses.len(), 1, "owner config response: {responses:?}");
    assert!(responses[0].contains("type='result'"));

    let snapshot = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor")
        .ask(GetSnapshot)
        .await
        .expect("snapshot")
        .room;
    assert!(
        snapshot.config.moderated,
        "announcement owner config must keep the live room moderated"
    );
    assert!(
        !snapshot.config.forum,
        "announcement owner config must not drift into forum mode"
    );

    let owner_get = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "announcement-owner-get",
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER).build())
            .build(),
    );
    let responses = handle_iq(
        &owner_get,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready_phase(&alice_jid),
    )
    .await;
    assert_eq!(responses.len(), 1, "owner get response: {responses:?}");
    let response =
        Element::from_str(responses.first().expect("owner get response")).expect("owner get XML");
    let form = response
        .get_child("query", waddle_xmpp::muc::NS_MUC_OWNER)
        .and_then(|query| query.get_child("x", waddle_xmpp::muc::DATA_FORMS_NS))
        .expect("owner config form");
    let moderated_field = form
        .children()
        .find(|field| {
            field.name() == "field"
                && field.ns() == waddle_xmpp::muc::DATA_FORMS_NS
                && field.attr("var") == Some("muc#roomconfig_moderatedroom")
        })
        .expect("moderated field");
    let moderated_value = moderated_field
        .get_child("value", waddle_xmpp::muc::DATA_FORMS_NS)
        .map(|value| value.texts().collect::<String>())
        .expect("moderated value");
    assert_eq!(
        moderated_value, "1",
        "owner config GET must project managed announcements as moderated"
    );

    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session.clone()),
    )
    .await;
    let bob = snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .get_occupant("bob")
        .expect("bob occupant")
        .clone();
    assert_eq!(bob.role, waddle_xmpp::Role::Visitor);

    let message = format!(
        "<message xmlns='jabber:client' to='{room_jid}' type='groupchat' id='bob-announcement-post'>\
            <body>not an owner</body>\
        </message>"
    );
    let responses = handle_message_for_test(
        state.as_ref(),
        &bob_jid,
        Some(&bob_session),
        parse_message_for_test(&message),
    )
    .await;
    assert_eq!(responses.len(), 1, "message response: {responses:?}");
    assert!(
        responses[0].contains("type='error'") && responses[0].contains("forbidden"),
        "announcement visitor post must be rejected: {}",
        responses[0]
    );
}

#[tokio::test]
async fn managed_space_bookmark_join_repairs_missing_parent_tuple() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'text', 0, 0, 0, 1)",
        crate::db_params!["legacy", "Legacy", "Legacy channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "alpha")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "legacy".to_string(),
        name: "Legacy".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "alpha", &item, None, false)
        .await
        .expect("publish bookmark");

    let viewer = create_test_session(state.as_ref(), "viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "alpha"),
                Relation::new("member"),
                Subject::user(&viewer.user_jid),
            ),
        })
        .await
        .expect("space member tuple");
    let allowed_before = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&viewer.user_jid),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, "legacy"),
        })
        .await
        .expect("permission actor")
        .allowed;
    assert!(
        !allowed_before,
        "test setup should start without the channel parent tuple"
    );

    let room_jid: BareJid = "legacy@muc.example.com".parse().expect("room jid");
    let viewer_jid: FullJid = format!("{}@example.com/web", viewer.xmpp_localpart)
        .parse()
        .expect("viewer jid");
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &viewer_jid,
        "viewer",
        None,
        &Some(viewer.clone()),
    )
    .await;

    let presence = Element::from_str(responses.first().expect("self-presence response"))
        .expect("presence XML");
    assert_eq!(presence.name(), "presence");
    assert_ne!(
        presence.attr("type"),
        Some("error"),
        "managed Space bookmark join should not be rejected: {responses:?}"
    );
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(
        item.attr("affiliation"),
        Some("none"),
        "repaired Space read access must not become persistent MUC membership: {responses:?}"
    );
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "successful join must complete with XEP-0045 self-presence 110: {responses:?}"
    );

    let allowed_after = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(&viewer.user_jid),
            permission: Permission::Read,
            object: Object::new(ObjectType::Channel, "legacy"),
        })
        .await
        .expect("permission actor")
        .allowed;
    assert!(
        allowed_after,
        "join should repair the missing channel parent tuple"
    );
}

#[tokio::test]
async fn managed_space_bookmark_join_repairs_only_projected_parent_tuple() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, members_only, public_room) VALUES (?, ?, ?, 'text', 0, 0, 1, 0)",
        crate::db_params!["projected", "Projected", "Projected channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    for node in ["alpha", "beta"] {
        state
            .deps
            .protocol
            .pubsub_storage
            .get_or_create_node(&spaces_jid, node)
            .await
            .expect("space node");
    }
    let channel = waddle_xmpp::ChannelInfo {
        id: "projected".to_string(),
        name: "Projected".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    for node in ["alpha", "beta"] {
        state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(&spaces_jid, node, &item, None, false)
            .await
            .expect("publish bookmark");
    }

    let room_jid: BareJid = "projected@muc.example.com".parse().expect("room jid");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: room_jid.clone(),
            space_jid: format!("beta@{}", spaces_jid.domain())
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("beta"),
            created_at: 0,
        })
        .await
        .expect("channel-space link");

    let stale_viewer = create_test_session(state.as_ref(), "stale-viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "alpha"),
                Relation::new("member"),
                Subject::user(&stale_viewer.user_jid),
            ),
        })
        .await
        .expect("stale space member tuple");
    let current_viewer = create_test_session(state.as_ref(), "current-viewer").await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, "beta"),
                Relation::new("member"),
                Subject::user(&current_viewer.user_jid),
            ),
        })
        .await
        .expect("current space member tuple");

    let stale_jid: FullJid = format!("{}@example.com/web", stale_viewer.xmpp_localpart)
        .parse()
        .expect("stale viewer jid");
    let stale_responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &stale_jid,
        "stale-viewer",
        None,
        &Some(stale_viewer.clone()),
    )
    .await;
    let stale_presence = Element::from_str(stale_responses.first().expect("stale join response"))
        .expect("presence XML");
    assert_eq!(
        stale_presence.attr("type"),
        Some("error"),
        "stale Space member must remain denied: {stale_responses:?}"
    );
    assert!(
        !state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject: Subject::user(&stale_viewer.user_jid),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, "projected"),
            })
            .await
            .expect("permission actor")
            .allowed,
        "join must not restore the stale alpha parent tuple"
    );
    assert!(
        state
            .deps
            .app_state
            .permission_actor
            .ask(CheckPermission {
                subject: Subject::user(&current_viewer.user_jid),
                permission: Permission::Read,
                object: Object::new(ObjectType::Channel, "projected"),
            })
            .await
            .expect("permission actor")
            .allowed,
        "join should repair only the projected beta parent tuple"
    );
}

#[tokio::test]
async fn muc_self_rejoin_does_not_emit_ghost_presence() {
    // Same user joins the same nick twice from different resources —
    // the second join must NOT include a presence for the old resource
    // in the response (which used to be seen as a "ghost" occupant and
    // broke self-presence detection on the client).
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "rejoin-channel@muc.example.com".parse().expect("room");
    let first: FullJid = "alice@example.com/tab-1".parse().expect("first");
    let second: FullJid = "alice@example.com/tab-2".parse().expect("second");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &first,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &second,
        "alice",
        None,
        &None,
    )
    .await;

    // Count presences emitted to the joiner that came from room/alice.
    // Only the self-presence (status 110) should be there — no "ghost".
    let alice_presences_for_joiner = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .filter(|el| el.name() == "presence")
        .filter(|el| el.attr("from") == Some(&format!("{room_jid}/alice")))
        .filter(|el| el.attr("to") == Some(&second.to_string()))
        .count();
    assert_eq!(
        alice_presences_for_joiner, 1,
        "self-rejoin must produce exactly one self-presence, not a ghost + self pair"
    );

    // And the one presence we got must carry status 110.
    let self_presence = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|el| {
            el.name() == "presence"
                && el.attr("from") == Some(&format!("{room_jid}/alice"))
                && el.attr("to") == Some(&second.to_string())
        })
        .expect("self-presence must be present");
    let user_x = self_presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    assert!(
        user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("110")),
        "status 110 must be present on self-rejoin"
    );
}

#[tokio::test]
async fn muc_join_broadcast_includes_real_occupant_jid() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "public-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), alice_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let broadcast = alice_rx.try_recv().expect("bob join broadcast to alice");
    let broadcast_xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&broadcast_xml).expect("broadcast presence XML");
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");

    let expected_from = format!("{room_jid}/bob");
    let expected_to = alice.to_string();
    assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
    assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
    assert_eq!(item.attr("jid"), Some("bob@example.com/phone"));
    assert_eq!(item.attr("affiliation"), Some("none"));
    assert_eq!(item.attr("role"), Some("participant"));
    // XEP-0045 registrar (#1265 item 4): status 100 belongs to the
    // joiner's self-presence only, not to broadcasts to other occupants.
    assert!(
        !user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
        "join broadcast to others must not carry status 100: {broadcast_xml}"
    );
}

/// #1440: refusing to create a room for an unprivileged joiner was the
/// one join denial with zero server-side telemetry — pin that it now
/// counts and keeps its XEP-0045 `<not-allowed/>` cancel frame.
#[tokio::test]
async fn muc_room_creation_denial_emits_admission_telemetry() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "no-create-rights@muc.example.com".parse().expect("room");
    let sender: FullJid = "mallory@example.com/web".parse().expect("sender");

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &sender,
        "mallory",
        None,
        &None,
    )
    .await;

    assert_eq!(responses.len(), 1, "one error presence: {responses:?}");
    let el = Element::from_str(&responses[0]).expect("valid XML");
    assert_eq!(el.attr("type"), Some("error"));
    let err = el
        .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
        .expect("error element");
    assert_eq!(err.attr("type"), Some("cancel"));
    assert!(err
        .get_child("not-allowed", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[
                ("condition", "not-allowed"),
                ("deny_reason", "room_creation_not_permitted")
            ]
        ),
        Some(1)
    );
}

#[tokio::test]
async fn muc_nick_collision_returns_conflict_presence() {
    // Two different users try to hold the same nick — second gets a
    // <presence type='error'/> with <conflict/>, and room state for
    // the incumbent is untouched. The denial counts, so the metrics
    // test lock must be held for the export window.
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "conflict-channel@muc.example.com".parse().expect("room");
    let alice: FullJid = "alice@example.com/desktop".parse().expect("alice");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "dino",
        None,
        &Some(owner_session),
    )
    .await;
    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "dino",
        None,
        &None,
    )
    .await;

    assert_eq!(responses.len(), 1, "exactly one error presence");
    let el = Element::from_str(&responses[0]).expect("valid XML");
    assert_eq!(el.name(), "presence");
    assert_eq!(el.attr("type"), Some("error"));
    let bob_str = bob.to_string();
    assert_eq!(el.attr("to"), Some(bob_str.as_str()));
    let err = el
        .get_child("error", waddle_xmpp::ns::JABBER_CLIENT)
        .expect("error element");
    assert_eq!(err.attr("type"), Some("cancel"));
    assert!(err
        .get_child("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    // Alice still owns the nick.
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&alice), Some("dino"));
    assert!(room.find_nick_by_real_jid(&bob).is_none());
    assert_eq!(room.occupant_count(), 1);

    // #1440: the nick-collision refusal is a counted join denial even
    // though its conflict frame is built outside the choke point.
    assert_eq!(
        metrics.counter_sum(
            "waddle.muc.admission.denied",
            &[("condition", "conflict"), ("deny_reason", "nick_conflict")]
        ),
        Some(1)
    );
}

#[tokio::test]
async fn cleanup_muc_presence_broadcasts_unavailable_to_remaining_occupants() {
    // Regression for the "1 in call" ghost: when a user's connection
    // drops uncleanly (tab close, SM-expiry, panic-shed), the cleanup
    // path used to remove them from the room actor but never tell
    // the remaining occupants. Other clients then kept the leaver's
    // nick in their `$mucCallParticipants[room]` indefinitely, so
    // the "N in call" chip stayed lit with nobody actually present.
    //
    // The fix routes both the explicit-leave path AND the unclean
    // disconnect path through `broadcast_muc_leave_to_remaining`, so
    // remaining occupants receive a `<presence type='unavailable'/>`
    // either way. This test pins that contract.
    use super::cleanup::cleanup_muc_presence_for_jid;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "ghost-call-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    // Register Bob's outbound channel so we can capture the broadcast
    // his client would receive.
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    // Both Alice and Bob join the MUC.
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    // Drain the join broadcast (Alice's room → Bob's mailbox is the
    // self-presence on join; Bob's room → Alice's mailbox would be
    // the cross-broadcast Alice would receive, but we registered only
    // Bob so we just need to drain his existing traffic).
    while bob_rx.try_recv().is_ok() {}

    // Simulate Alice's unclean disconnect — same entry point the SM
    // janitor and `cleanup_connection_shutdown` reach.
    cleanup_muc_presence_for_jid(state.as_ref(), &alice).await;

    // The room actor must have evicted Alice's occupant slot.
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "Alice's occupant slot must be cleared on unclean disconnect",
    );
    assert_eq!(room.find_nick_by_real_jid(&bob), Some("bob"));

    // The fix: Bob receives an `unavailable` presence from
    // `room/alice` so his client can drop Alice from any
    // call-participants set keyed by nick.
    let broadcast = bob_rx
        .try_recv()
        .expect("Bob must receive Alice's unavailable broadcast on her unclean disconnect");
    let xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&xml).expect("broadcast presence XML");
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("type"), Some("unavailable"));
    let expected_from = format!("{room_jid}/alice");
    let expected_to = bob.to_string();
    assert_eq!(presence.attr("from"), Some(expected_from.as_str()));
    assert_eq!(presence.attr("to"), Some(expected_to.as_str()));
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("jid"), Some(alice.to_string().as_str()));
    // XEP-0045 registrar (#1265 item 4): leave broadcasts are not an
    // "entering a room" context — no status 100.
    assert!(
        !user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
        "leave broadcast must not carry status 100: {xml}"
    );
}

fn active_muji() -> waddle_xmpp::xep::xep0272::Muji {
    use waddle_xmpp::xep::xep0167::MediaKind;
    use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
    Muji::with_contents(vec![MujiContent::new(
        "audio",
        Creator::Initiator,
        MediaKind::Audio,
    )])
}

fn preparing_muji() -> waddle_xmpp::xep::xep0272::Muji {
    waddle_xmpp::xep::xep0272::Muji::preparing()
}

fn empty_muji() -> waddle_xmpp::xep::xep0272::Muji {
    waddle_xmpp::xep::xep0272::Muji::default()
}

#[derive(Default)]
struct RecordingSfu {
    calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
    note_calls: std::sync::Mutex<Vec<(waddle_sfu::CallId, waddle_sfu::Identity)>>,
    update_calls: std::sync::Mutex<
        Vec<(
            waddle_sfu::CallId,
            waddle_sfu::Identity,
            waddle_sfu::MediaCapabilities,
        )>,
    >,
}

impl RecordingSfu {
    fn snapshot(&self) -> Vec<(waddle_sfu::CallId, waddle_sfu::Identity)> {
        self.calls.lock().expect("recording lock").clone()
    }
}

impl waddle_sfu::SfuService for RecordingSfu {
    fn issue_join_token(
        &self,
        _: &waddle_sfu::CallId,
        _: &waddle_sfu::Identity,
        _: waddle_sfu::MediaCapabilities,
    ) -> Result<waddle_sfu::JoinToken, waddle_sfu::SfuError> {
        unimplemented!("not exercised by this test")
    }

    fn issue_turn_credentials(
        &self,
        _: &waddle_sfu::Identity,
    ) -> Result<waddle_sfu::TurnCredential, waddle_sfu::SfuError> {
        unimplemented!("not exercised by this test")
    }

    fn register_call_participant(&self, _: &waddle_sfu::CallId, _: &waddle_sfu::Identity) {}

    fn has_call_participant(&self, _: &waddle_sfu::CallId, _: &waddle_sfu::Identity) -> bool {
        false
    }

    fn unregister_call_participant(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
    ) -> waddle_sfu::CallState {
        self.calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
        waddle_sfu::CallState::Ended
    }

    fn update_participant_capabilities(
        &self,
        call_id: &waddle_sfu::CallId,
        identity: &waddle_sfu::Identity,
        capabilities: waddle_sfu::MediaCapabilities,
    ) {
        self.update_calls.lock().expect("recording lock").push((
            call_id.clone(),
            identity.clone(),
            capabilities,
        ));
    }

    fn note_participant_left(&self, call_id: &waddle_sfu::CallId, identity: &waddle_sfu::Identity) {
        // Recorded into `note_calls`, NOT `calls`: the trait splits
        // admin-evict from bookkeeping-only dispatch, and tests need
        // to distinguish which path was taken.
        self.note_calls
            .lock()
            .expect("recording lock")
            .push((call_id.clone(), identity.clone()));
    }

    fn is_revoked(&self, _: &waddle_sfu::Jti) -> bool {
        false
    }

    fn ws_url(&self) -> &waddle_sfu::WebsocketUrl {
        unimplemented!("not exercised by this test")
    }

    fn turn_host(&self) -> &waddle_sfu::TurnHost {
        unimplemented!("not exercised by this test")
    }

    fn webhook_secret(&self) -> &waddle_sfu::ApiSecret {
        unimplemented!("not exercised by this test")
    }

    fn participants_for_call(&self, _: &waddle_sfu::CallId) -> Vec<waddle_sfu::Identity> {
        Vec::new()
    }
}

async fn state_with_recording_sfu(
    recorder: std::sync::Arc<RecordingSfu>,
) -> std::sync::Arc<WebSocketState> {
    let base = create_test_websocket_state().await;
    let mut state = match std::sync::Arc::try_unwrap(base) {
        Ok(state) => state,
        Err(_) => panic!("test websocket state should be uniquely owned"),
    };
    let sfu: std::sync::Arc<dyn waddle_sfu::SfuService> = recorder;
    state.deps.protocol.sfu = Some(sfu);
    std::sync::Arc::new(state)
}

fn muc_user_item_jid(element: &Element) -> Option<String> {
    element
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .and_then(|x| x.get_child("item", "http://jabber.org/protocol/muc#user"))
        .and_then(|item| item.attr("jid"))
        .map(ToOwned::to_owned)
}

fn muc_presence_to(room_jid: &BareJid, nick: &str) -> xmpp_parsers::presence::Presence {
    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.to = Some(
        room_jid
            .clone()
            .with_resource_str(nick)
            .expect("valid room nick")
            .into(),
    );
    presence
}

fn muc_join_presence_to(room_jid: &BareJid, nick: &str) -> xmpp_parsers::presence::Presence {
    let mut presence = muc_presence_to(room_jid, nick);
    presence
        .payloads
        .push(Element::builder("x", waddle_xmpp::muc::presence::NS_MUC).build());
    presence
}

#[tokio::test]
async fn typed_muc_presence_is_not_join_or_update_activity() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "typed-presence@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice, false);
    let mut probe = muc_join_presence_to(&room_jid, "alice");
    probe.type_ = xmpp_parsers::presence::Type::Probe;

    let responses = handlers::presence::handle_presence(
        probe,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        responses.is_empty(),
        "typed MUC presence must not be accepted as join/update activity"
    );
    assert!(
        bob_rx.try_recv().is_err(),
        "typed MUC presence must not be broadcast to room occupants"
    );
}

#[tokio::test]
async fn available_presence_without_muji_clears_existing_muji_state() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-clear-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let plain_available = muc_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        plain_available,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &None,
        None,
    )
    .await;
    let recorded = recorder.snapshot();
    assert_eq!(
        recorded.len(),
        1,
        "Muji clear must unregister the sender from the SFU"
    );
    assert_eq!(recorded[0].0.as_str(), "muji-clear-channel@muc.example.com");
    assert_eq!(recorded[0].1.as_livekit_identity(), "alice@example.com/web");

    assert_eq!(responses.len(), 1, "sender receives reflected clear");
    let self_clear = Element::from_str(&responses[0]).expect("self clear XML");
    assert_eq!(
        self_clear.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert!(
        self_clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "self reflection must omit <muji/> after clear"
    );

    let broadcast = bob_rx
        .try_recv()
        .expect("Bob must receive canonical non-Muji presence");
    let xml = stanza_to_xml(&broadcast.stanza);
    let presence = Element::from_str(&xml).expect("broadcast presence XML");
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("type"), None);
    assert_eq!(
        presence.attr("from"),
        Some(format!("{room_jid}/alice").as_str())
    );
    assert_eq!(presence.attr("to"), Some(bob.to_string().as_str()));
    let user_x = presence
        .get_child("x", "http://jabber.org/protocol/muc#user")
        .expect("muc user payload");
    let item = user_x
        .get_child("item", "http://jabber.org/protocol/muc#user")
        .expect("muc user item");
    assert_eq!(item.attr("jid"), Some(alice.to_string().as_str()));
    // XEP-0045 registrar (#1265 item 4): presence-update reflections are
    // not an "entering a room" context — no status 100.
    assert!(
        !user_x
            .children()
            .any(|child| child.name() == "status" && child.attr("code") == Some("100")),
        "Muji-clear broadcast must not carry status 100: {xml}"
    );
    assert!(
        presence
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "clear broadcast must not carry <muji/>"
    );

    let carol: FullJid = "carol@example.com/web".parse().expect("carol jid");
    let replay = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &carol,
        "carol",
        None,
        &None,
    )
    .await;
    let alice_replay = replay
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| element.attr("from") == Some(format!("{room_jid}/alice").as_str()))
        .expect("carol receives alice replay");
    assert!(
        alice_replay
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "late join replay must not include stale Muji"
    );
}

#[tokio::test]
async fn empty_muji_presence_unregisters_the_sfu_participant() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "empty-muji-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;

    let mut empty = muc_presence_to(&room_jid, "alice");
    empty.payloads.push(empty_muji().to_element());
    let responses = handlers::presence::handle_presence(
        empty,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let recorded = recorder.snapshot();
    assert_eq!(
        recorded.len(),
        1,
        "empty <muji/> leave marker must unregister the sender from the SFU"
    );
    assert_eq!(recorded[0].0.as_str(), "empty-muji-clear@muc.example.com");
    assert_eq!(recorded[0].1.as_livekit_identity(), "alice@example.com/web");
    let self_clear = Element::from_str(&responses[0]).expect("self clear XML");
    assert!(
        self_clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "empty <muji/> must reflect as a no-Muji leave marker"
    );
}

#[tokio::test]
async fn empty_muji_presence_ends_the_active_call_thread() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "presence-clear-ended@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;
    // Anchor the call thread in the inbox under the same thread id the
    // active-call registration carries, so the ended summary can be
    // correlated back to this exact thread's row.
    let thread_id = "call-thread-uuid";
    state
        .deps
        .protocol
        .inbox_storage
        .upsert(
            &alice.to_bare(),
            waddle_xmpp::inbox::InboxEntry::new(
                room_jid.clone(),
                waddle_xmpp::inbox::ConversationKind::MucRoom,
                "anchor-stanza",
                1_700_000_000,
            )
            .with_thread(thread_id)
            .with_call_thread(
                waddle_xmpp::xep::CallThreadKind::Muc,
                waddle_xmpp::xep::CallThreadMedia {
                    audio: true,
                    video: false,
                },
            ),
            true,
        )
        .await
        .expect("seed call-thread anchor inbox row");

    state.deps.protocol.call_threads.insert(
        room_jid.clone(),
        crate::server::routes::websocket::ActiveCallThread {
            anchor_origin_id: "anchor-origin-id".to_owned(),
            initiator: alice.to_bare(),
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            started: chrono::Utc::now() - chrono::Duration::minutes(5),
            thread_id: thread_id.to_owned(),
        },
    );

    let mut empty = muc_presence_to(&room_jid, "alice");
    empty.payloads.push(empty_muji().to_element());
    let _ = handlers::presence::handle_presence(
        empty,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        !state.deps.protocol.call_threads.contains_key(&room_jid),
        "XMPP-native Muji clear must consume the active call thread when the SFU participant set is empty"
    );

    // The ended summary must be persisted onto the thread's inbox row so
    // the durable threads projection reflects the call ending without a
    // MAM replay.
    let anchor_row = state
        .deps
        .protocol
        .inbox_storage
        .list_threads(&alice.to_bare(), &room_jid)
        .await
        .expect("list room threads")
        .into_iter()
        .find(|entry| entry.thread_id.as_deref() == Some(thread_id))
        .expect("anchor thread row persists");
    assert!(
        anchor_row.call_ended_at.is_some(),
        "ending the active call must stamp call_ended_at onto the thread inbox row"
    );
    assert!(
        anchor_row
            .call_duration
            .as_ref()
            .is_some_and(|duration| duration.as_str().starts_with("PT")),
        "ending the active call must stamp an ISO-8601 call_duration onto the thread inbox row: {:?}",
        anchor_row.call_duration
    );
    assert_eq!(
        anchor_row.call_thread_kind,
        Some(waddle_xmpp::xep::CallThreadKind::Muc),
        "the anchor kind must survive the ended UPDATE"
    );
}

// Per #918: the whole call-thread lifecycle is gated on a configured SFU
// (`muc_update.rs`: anchor built only when `active_call_started &&
// sfu.is_some()`). Without an SFU the `active_call_started` flag from
// client-driven Muji presence must NOT register a call-thread anchor,
// because the end path that would consume it is itself SFU-gated. These
// two tests pin both sides of that gate.
#[tokio::test]
async fn active_muji_presence_without_sfu_does_not_register_call_thread_anchor() {
    // `create_test_websocket_state` builds state with `sfu: None`, so the
    // call-thread gate is closed.
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "no-sfu-anchor@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        !state.deps.protocol.call_threads.contains_key(&room_jid),
        "an active <muji/> must not register a call-thread anchor when no SFU is configured"
    );
}

#[tokio::test]
async fn active_muji_presence_with_sfu_registers_call_thread_anchor() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "with-sfu-anchor@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);

    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    assert!(
        state.deps.protocol.call_threads.contains_key(&room_jid),
        "an active <muji/> must register a call-thread anchor when an SFU is configured"
    );
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_existing_muji_state() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let bob_phase = waddle_xmpp::protocol::ConnectionPhase::ready(bob.clone(), false);
    let mut active = muc_presence_to(&room_jid, "bob");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let resumed_autojoin = muc_join_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let bob_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/bob").as_str())
                && element.attr("to") == Some(alice.to_string().as_str())
        })
        .expect("resumed autojoin receives bob replay");
    assert!(
        bob_replay
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_some(),
        "resumed MUC autojoin must replay existing active-call Muji state"
    );
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_own_muji_without_plain_broadcast() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-own-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), alice_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let bob_phase = waddle_xmpp::protocol::ConnectionPhase::ready(bob.clone(), false);
    let mut active = muc_presence_to(&room_jid, "bob");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let resumed_autojoin = muc_join_presence_to(&room_jid, "bob");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &bob_phase,
        &None,
        None,
    )
    .await;

    let self_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/bob").as_str())
                && element.attr("to") == Some(bob.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("resumed active participant receives own Muji replay");
    assert_eq!(
        muc_user_item_jid(&self_replay).as_deref(),
        Some(bob.to_string().as_str())
    );

    while let Ok(outbound) = alice_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        let presence = Element::from_str(&xml).expect("broadcast presence XML");
        if presence.attr("from") == Some(format!("{room_jid}/bob").as_str()) {
            assert!(
                presence
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some(),
                "same-session resumed autojoin must not broadcast a plain no-Muji update for the active participant"
            );
        }
    }
}

#[tokio::test]
async fn resumed_muc_join_presence_replays_muji_when_room_is_full() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "muji-resume-full-room@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let room_actor = get_room_actor(state.as_ref(), &room_jid)
        .await
        .expect("room actor");
    let mut config = room_actor
        .ask(GetSnapshot)
        .await
        .expect("room snapshot")
        .room
        .config;
    config.max_occupants = 1;
    room_actor
        .ask(UpdateConfig { config })
        .await
        .expect("room config update");

    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;

    let resumed_autojoin = muc_join_presence_to(&room_jid, "alice");
    let responses = handlers::presence::handle_presence(
        resumed_autojoin,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let self_replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(alice.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("full-room resumed autojoin receives own Muji replay");
    assert_eq!(
        muc_user_item_jid(&self_replay).as_deref(),
        Some(alice.to_string().as_str())
    );
}

#[tokio::test]
async fn same_nick_late_join_replays_existing_muji_with_exact_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-muji-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;

    let sibling_muji_presence = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(mobile.to_string().as_str())
                && muc_user_item_jid(element).as_deref() == Some(desktop.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("mobile receives sibling Muji presence");

    assert!(
        sibling_muji_presence
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_some(),
        "same-nick sibling join must replay the existing Muji advertisement under the sibling JID"
    );
}

#[tokio::test]
async fn same_nick_late_join_replays_preparing_only_muji_with_exact_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-preparing-replay@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &Some(owner_session),
        None,
    )
    .await;

    let responses = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;

    let replay = responses
        .iter()
        .filter_map(|xml| Element::from_str(xml).ok())
        .find(|element| {
            element.attr("from") == Some(format!("{room_jid}/alice").as_str())
                && element.attr("to") == Some(bob.to_string().as_str())
                && muc_user_item_jid(element).as_deref() == Some(mobile.to_string().as_str())
                && element
                    .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                    .is_some()
        })
        .expect("bob receives mobile's preparing replay");

    let muji = replay
        .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
        .and_then(|element| waddle_xmpp::xep::xep0272::Muji::try_from(element).ok())
        .expect("Muji parses");
    assert!(muji.preparing);
    assert!(!muji.is_active());
}

#[tokio::test]
async fn same_nick_plain_presence_preserves_sibling_preparing_owner() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-preparing-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let plain_available = muc_presence_to(&room_jid, "alice");
    let _ = handlers::presence::handle_presence(
        plain_available,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &None,
        None,
    )
    .await;

    let mut saw_desktop_preparing = false;
    while let Ok(outbound) = bob_rx.try_recv() {
        let xml = stanza_to_xml(&outbound.stanza);
        let presence = Element::from_str(&xml).expect("presence XML");
        if muc_user_item_jid(&presence).as_deref() == Some(desktop.to_string().as_str())
            && presence
                .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                .is_some()
        {
            saw_desktop_preparing = true;
        }
    }

    assert!(
        saw_desktop_preparing,
        "plain presence from one same-nick resource must preserve a sibling's preparing state under the sibling JID"
    );
}

#[tokio::test]
async fn same_nick_originator_leave_broadcasts_muji_clear() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-muji-clear@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (mobile_tx, mut mobile_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(mobile.clone(), mobile_tx);
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while mobile_rx.try_recv().is_ok() {}
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while mobile_rx.try_recv().is_ok() {}
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_muc_leave(state.as_ref(), &room_jid, &desktop, "alice", None).await;

    for (recipient, rx) in [(&mobile, &mut mobile_rx), (&bob, &mut bob_rx)] {
        let broadcast = rx
            .try_recv()
            .unwrap_or_else(|_| panic!("{recipient} must receive Muji clear"));
        let xml = stanza_to_xml(&broadcast.stanza);
        let presence = Element::from_str(&xml).expect("clear presence XML");
        assert_eq!(presence.name(), "presence");
        assert_eq!(presence.attr("type"), None);
        assert_eq!(
            presence.attr("from"),
            Some(format!("{room_jid}/alice").as_str())
        );
        assert_eq!(presence.attr("to"), Some(recipient.to_string().as_str()));
        assert!(
            presence
                .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
                .is_none(),
            "partial-resource clear broadcast must not carry <muji/>"
        );
    }

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.find_nick_by_real_jid(&mobile), Some("alice"));
    assert!(room.find_nick_by_real_jid(&desktop).is_none());
    assert!(room.muji_for_nick("alice").is_none());
}

#[tokio::test]
async fn same_nick_active_leave_broadcasts_departed_clear_before_preparing_sibling() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "same-nick-active-preparing-leave@muc.example.com"
        .parse()
        .expect("room jid");
    let desktop: FullJid = "alice@example.com/desktop".parse().expect("desktop jid");
    let mobile: FullJid = "alice@example.com/mobile".parse().expect("mobile jid");
    let bob: FullJid = "bob@example.com/desktop".parse().expect("bob jid");

    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob.clone(), bob_tx);

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &desktop,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &mobile,
        "alice",
        None,
        &None,
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let desktop_phase = waddle_xmpp::protocol::ConnectionPhase::ready(desktop.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &desktop_phase,
        &Some(owner_session.clone()),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let mobile_phase = waddle_xmpp::protocol::ConnectionPhase::ready(mobile.clone(), false);
    let mut preparing = muc_presence_to(&room_jid, "alice");
    preparing.payloads.push(preparing_muji().to_element());
    let _ = handlers::presence::handle_presence(
        preparing,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &mobile_phase,
        &Some(owner_session),
        None,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_muc_leave(state.as_ref(), &room_jid, &desktop, "alice", None).await;

    let clear_xml = stanza_to_xml(
        &bob_rx
            .try_recv()
            .expect("Bob receives departed desktop clear")
            .stanza,
    );
    let clear = Element::from_str(&clear_xml).expect("desktop clear presence XML");
    assert_eq!(
        muc_user_item_jid(&clear).as_deref(),
        Some(desktop.to_string().as_str())
    );
    assert!(
        clear
            .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
            .is_none(),
        "departed active resource must be explicitly cleared before sibling replay"
    );

    let preparing_xml = stanza_to_xml(
        &bob_rx
            .try_recv()
            .expect("Bob receives remaining mobile preparing replay")
            .stanza,
    );
    let preparing = Element::from_str(&preparing_xml).expect("mobile preparing presence XML");
    assert_eq!(
        muc_user_item_jid(&preparing).as_deref(),
        Some(mobile.to_string().as_str())
    );
    let muji = preparing
        .get_child("muji", waddle_xmpp::xep::xep0272::NS_MUJI)
        .and_then(|element| waddle_xmpp::xep::xep0272::Muji::try_from(element).ok())
        .expect("remaining sibling Muji parses");
    assert!(muji.preparing);
    assert!(!muji.is_active());
    assert!(
        bob_rx.try_recv().is_err(),
        "only departed clear plus remaining sibling replay should be broadcast"
    );
}

/// XEP-0045 slice-2b ingest: a successful `handle_muc_join` MUST
/// record `(sender_bare, room)` activity in the
/// `notification_activity` projection so the T1 XEP-0513 `<active/>`
/// evaluator can admit ActiveChannelMention pushes for the joiner.
/// Pass-2 review caught this as a regression — the writer existed but
/// nothing in production called it. The test guards the wire-in.
#[tokio::test]
async fn handle_muc_join_records_notification_activity_for_sender() {
    use crate::notification_activity::NotificationActivityReader;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");

    // Pre-condition: no activity row yet for (alice, room).
    let before = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read pre-join");
    assert!(
        before.is_none(),
        "projection MUST start empty for fresh user/room pair; got {before:?}",
    );

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        Some(crate::notification_activity::NotificationPresenceShow::Away),
        &Some(owner_session),
    )
    .await;

    let after = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-join")
        .expect("activity row MUST exist after MUC join");
    assert!(
        after.last_active_at_ms > 0,
        "MUC join MUST bump last_active_at_ms; got {}",
        after.last_active_at_ms,
    );
    assert_eq!(
        after.presence_show,
        Some(crate::notification_activity::NotificationPresenceShow::Away),
        "join MUST persist the typed `<show/>` token; got {:?}",
        after.presence_show,
    );
}

/// XEP-0045 slice-2b ingest: a `handle_muc_leave` MUST bump
/// `(sender_bare, room)` activity AND clear the persisted
/// `<show/>` (an explicit unavailable has no available presence).
#[tokio::test]
async fn handle_muc_leave_records_notification_activity_and_clears_show() {
    use crate::notification_activity::NotificationActivityReader;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "channel@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        Some(crate::notification_activity::NotificationPresenceShow::Chat),
        &Some(owner_session),
    )
    .await;
    let after_join = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-join")
        .expect("activity post-join");
    assert_eq!(
        after_join.presence_show,
        Some(crate::notification_activity::NotificationPresenceShow::Chat)
    );

    // No sleep needed: the monotonic UPSERTs in
    // `NotificationActivityStore` now use `>=` so tie-millis writes
    // (rapid join/leave landing in the same millisecond) still apply
    // the latest writer's columns — the leave's `last_active_at_ms`
    // and cleared `<show/>` are persisted even when `now_ms` matches
    // the join's. The previous 2ms sleep was a workaround for the
    // strict-`>` race (Codex/Copilot review on PR #731). The assertion
    // below uses `>=` to allow same-millisecond ties without flaking.
    let _ = handle_muc_leave(state.as_ref(), &room_jid, &alice, "alice", None).await;
    let after_leave = state
        .deps
        .protocol
        .notification_activity
        .read_activity(&alice.to_bare(), &room_jid)
        .await
        .expect("read post-leave")
        .expect("activity post-leave");
    assert!(
        after_leave.last_active_at_ms >= after_join.last_active_at_ms,
        "leave MUST NOT regress last_active_at_ms below join (got join={} leave={})",
        after_join.last_active_at_ms,
        after_leave.last_active_at_ms,
    );
    assert!(
        after_leave.presence_show.is_none(),
        "leave MUST clear persisted `<show/>`; got {:?}",
        after_leave.presence_show,
    );
}

// --- Issue #935: involuntary MUC removal must evict SFU call participants ---

fn build_admin_set_iq_xml(room_jid: &BareJid, id: &str, item: Element) -> String {
    element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(item)
                    .build(),
            )
            .build(),
    )
}

/// Flip an existing room to XEP-0045 moderated. Required for any
/// devoice test: the visitor/voice distinction only withholds voice in
/// a moderated room, so in the default (unmoderated) fixture a visitor
/// still has voice and there is nothing to converge.
async fn make_room_moderated(state: &WebSocketState, room_jid: &BareJid) {
    let actor = crate::server::routes::websocket::get_room_actor_result(state, room_jid)
        .await
        .expect("room lookup")
        .expect("room actor exists");
    let config = actor
        .ask(waddle_xmpp::muc::room_actor::GetConfig)
        .await
        .expect("read room config");
    actor
        .ask(waddle_xmpp::muc::room_actor::UpdateConfig {
            config: waddle_xmpp::muc::RoomConfig {
                moderated: true,
                ..config
            },
        })
        .await
        .expect("set room moderated");
}

async fn join_alice_owner_and_bob(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> (Session, FullJid, FullJid) {
    let alice_session = create_test_server_owner_session(state, "alice").await;
    let bob_session = create_test_session(state, "bob").await;
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    handle_muc_join(
        state,
        "example.com",
        room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state,
        "example.com",
        room_jid,
        &bob_jid,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    (alice_session, alice_jid, bob_jid)
}

#[tokio::test]
async fn muc_admin_kick_evicts_target_sessions_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "kick-evicts@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let kick_iq = build_admin_set_iq_xml(
        &room_jid,
        "kick-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
            .build(),
    );
    let responses = handle_iq(
        &kick_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    // XEP-0045 §8.2/§9.1 ordering (#1265 item 6): the moderator sees the
    // IQ result first, then their own copy of the broadcast presence.
    assert_eq!(responses.len(), 2, "kick response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");
    assert!(
        responses[1].contains("<presence"),
        "moderator's broadcast copy follows the IQ result: {responses:?}"
    );

    let evicted = recorder.snapshot();
    assert_eq!(
        evicted.len(),
        1,
        "exactly the kicked occupant's session is evicted: {evicted:?}"
    );
    assert_eq!(evicted[0].0.as_str(), "kick-evicts@muc.example.com");
    assert_eq!(evicted[0].1.as_livekit_identity(), "bob@example.com/web");
    assert!(
        recorder.note_snapshot().is_empty(),
        "moderation must use the full admin-evict path, not webhook bookkeeping"
    );
}

#[tokio::test]
async fn muc_admin_ban_evicts_target_sessions_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "ban-evicts@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let ban_iq = build_admin_set_iq_xml(
        &room_jid,
        "ban-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "bob@example.com",
            )
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                "outcast",
            )
            .build(),
    );
    let responses = handle_iq(
        &ban_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    // XEP-0045 §8.2/§9.1 ordering (#1265 item 6): the moderator sees the
    // IQ result first, then their own copy of the broadcast presence.
    assert_eq!(responses.len(), 2, "ban response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");
    assert!(
        responses[1].contains("<presence"),
        "moderator's broadcast copy follows the IQ result: {responses:?}"
    );

    let evicted = recorder.snapshot();
    assert_eq!(
        evicted.len(),
        1,
        "exactly the banned occupant's session is evicted: {evicted:?}"
    );
    assert_eq!(evicted[0].0.as_str(), "ban-evicts@muc.example.com");
    assert_eq!(evicted[0].1.as_livekit_identity(), "bob@example.com/web");
}

#[tokio::test]
async fn muc_admin_role_demotion_does_not_evict_from_room_call() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "demote-keeps@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let demote_iq = build_admin_set_iq_xml(
        &room_jid,
        "demote-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "visitor")
            .build(),
    );
    let responses = handle_iq(
        &demote_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    // XEP-0045 §8.2/§9.1 ordering (#1265 item 6): the moderator sees the
    // IQ result first, then their own copy of the broadcast presence.
    assert_eq!(responses.len(), 2, "demote response: {responses:?}");
    assert!(responses[0].contains("type='result'"), "{responses:?}");
    assert!(
        responses[1].contains("<presence"),
        "moderator's broadcast copy follows the IQ result: {responses:?}"
    );

    assert!(
        recorder.snapshot().is_empty(),
        "a role change that keeps the occupant must not end their call session"
    );
}

/// Role-derived media grants: revoking voice (role → visitor) must
/// converge the target's live SFU permission to listen-only, without
/// ending their call session.
#[tokio::test]
async fn muc_admin_voice_revocation_downgrades_live_sfu_grants() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "voice-revoke-grants@muc.example.com"
        .parse()
        .expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    make_room_moderated(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    let demote_iq = build_admin_set_iq_xml(
        &room_jid,
        "revoke-bob-voice",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "visitor")
            .build(),
    );
    let responses = handle_iq(
        &demote_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready,
    )
    .await;
    assert!(responses[0].contains("type='result'"), "{responses:?}");

    let updates = recorder.update_snapshot();
    assert_eq!(
        updates.len(),
        1,
        "exactly the demoted occupant's session gets a grant update: {updates:?}"
    );
    assert_eq!(updates[0].0.as_str(), "voice-revoke-grants@muc.example.com");
    assert_eq!(updates[0].1.as_livekit_identity(), "bob@example.com/web");
    assert_eq!(
        updates[0].2,
        waddle_sfu::MediaCapabilities::listen_only(),
        "a devoiced occupant's live grants are listen-only"
    );
    assert!(updates[0].2.is_listen_only());
    assert!(
        recorder.snapshot().is_empty(),
        "a grant downgrade must not end the call session"
    );
}

/// Role-derived media grants: granting voice (visitor → participant)
/// must converge the target's live SFU permission back to publishable.
#[tokio::test]
async fn muc_admin_voice_grant_upgrades_live_sfu_grants() {
    let recorder = Arc::new(crate::server::routes::websocket::tests::RecordingSfu::default());
    let state = create_test_websocket_state_with_sfu(recorder.clone()).await;
    let room_jid: BareJid = "voice-grant-grants@muc.example.com"
        .parse()
        .expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;
    make_room_moderated(state.as_ref(), &room_jid).await;
    let ready = ready_phase(&alice_jid);

    for (id, role) in [("revoke-bob", "visitor"), ("grant-bob", "participant")] {
        let iq = build_admin_set_iq_xml(
            &room_jid,
            id,
            Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "bob")
                .attr(minidom::rxml::xml_ncname!("role").to_owned(), role)
                .build(),
        );
        let responses = handle_iq(
            &iq,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(alice_session.clone()),
            &ready,
        )
        .await;
        assert!(responses[0].contains("type='result'"), "{responses:?}");
    }

    let updates = recorder.update_snapshot();
    assert_eq!(updates.len(), 2, "one update per role change: {updates:?}");
    assert!(updates[0].2.is_listen_only(), "revocation first");
    assert_eq!(
        updates[1].2,
        waddle_sfu::MediaCapabilities::from_muc_voice(waddle_xmpp_core::types::Voice::Voiced),
        "restored voice restores publish grants"
    );
    assert!(updates[1].2.can_publish);
}

/// Round-3 concurrency review: MUC occupancy is keyed by FULL JID, so
/// a detached session's invalidation cleanup must NOT evict the JID
/// from its rooms while a live same-JID replacement session exists —
/// the replacement shares the occupancy and would be kicked out of
/// every room it just (re)joined.
#[tokio::test]
async fn detached_invalidation_skips_muc_cleanup_when_live_replacement_exists() {
    use super::cleanup::cleanup_invalidated_detached_session;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "replacement-race-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // A live replacement session holds the registry slot for the SAME
    // full JID (fresh bind after the old session detached).
    let (repl_tx, _repl_rx) = mpsc::channel::<OutboundStanza>(4);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), repl_tx);

    // The stale detached session (a stream id the registry no longer
    // holds) gets invalidated.
    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stale-stream".to_string(),
        user_id: "alice@example.com".to_string(),
        jid: alice.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    cleanup_invalidated_detached_session(state.as_ref(), detached.clone(), None).await;

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        room.find_nick_by_real_jid(&alice),
        Some("alice"),
        "the live replacement's room occupancy must survive the stale \
         detached session's invalidation cleanup"
    );

    // Companion: with no live entry for the JID, the invalidation DOES
    // evict the occupancy.
    state.deps.protocol.connection_registry.unregister(&alice);
    cleanup_invalidated_detached_session(state.as_ref(), detached, None).await;
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "with no live replacement the invalidated session's occupancy \
         must be cleaned up"
    );
}

/// codex P1 on PR #1207: when the invalidation is triggered BY the
/// same client's fresh bind (the replacement owner IS the invalidating
/// caller), the new stream cannot have joined any rooms yet — the dead
/// session's occupancies are certainly stale and MUST be cleaned, or
/// the fresh connection inherits room fan-out without ever joining.
/// Only a FOREIGN live entry (someone else's slot, unknown join state)
/// warrants skipping room cleanup.
#[tokio::test]
async fn fresh_bind_invalidation_cleans_the_dead_sessions_room_occupancy() {
    use super::cleanup::cleanup_invalidated_detached_session;

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "fresh-bind-cleanup-channel@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;

    // The same client fresh-binds: its NEW connection owns the slot and
    // triggers invalidation of the old detached session.
    let (fresh_tx, _fresh_rx) = mpsc::channel::<OutboundStanza>(4);
    let fresh_owner = state
        .deps
        .protocol
        .connection_registry
        .register(alice.clone(), fresh_tx);

    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: "stale-stream-fresh-bind".to_string(),
        user_id: "alice@example.com".to_string(),
        jid: alice.clone(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    cleanup_invalidated_detached_session(state.as_ref(), detached, Some(&fresh_owner)).await;

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert!(
        room.find_nick_by_real_jid(&alice).is_none(),
        "a fresh bind's invalidation must clean the dead session's room \
         occupancy — the new stream has not joined anything yet"
    );
}

// ---------------------------------------------------------------------------
// XEP-0045 §7.6 nick change (#1252): Waddle locks nicknames to identity,
// so an in-room presence addressed to a different nick MUST be denied
// with <not-acceptable/> — and the denial must happen BEFORE any
// destructive side effect (previously it cleared the sender's Muji/SFU
// call state and then silently dropped the stanza).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn xep0045_nick_change_denied_with_not_acceptable_without_call_teardown() {
    let recorder = std::sync::Arc::new(RecordingSfu::default());
    let state = state_with_recording_sfu(std::sync::Arc::clone(&recorder)).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "nick-change-denied@muc.example.com"
        .parse()
        .expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;

    // Alice advertises an active call (Muji presence)…
    let alice_phase = waddle_xmpp::protocol::ConnectionPhase::ready(alice.clone(), false);
    let mut active = muc_presence_to(&room_jid, "alice");
    active.payloads.push(active_muji().to_element());
    let _ = handlers::presence::handle_presence(
        active,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &Some(owner_session),
        None,
    )
    .await;

    // …then attempts a rename by sending in-room presence to a new nick.
    let rename = muc_presence_to(&room_jid, "alice-renamed");
    let responses = handlers::presence::handle_presence(
        rename,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &alice_phase,
        &None,
        None,
    )
    .await;

    // §7.6: the service MUST return a presence error with
    // <not-acceptable/> from the requested occupant JID.
    assert_eq!(responses.len(), 1, "rename must be answered: {responses:?}");
    let error_presence = Element::from_str(&responses[0]).expect("error presence XML");
    assert_eq!(error_presence.name(), "presence");
    assert_eq!(error_presence.attr("type"), Some("error"));
    assert_eq!(
        error_presence.attr("from"),
        Some(format!("{room_jid}/alice-renamed").as_str())
    );
    let error = error_presence
        .get_child("error", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .or_else(|| error_presence.get_child("error", "jabber:client"))
        .expect("error child present");
    assert!(
        error
            .get_child("not-acceptable", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "denied nick change must be <not-acceptable/>: {responses:?}"
    );

    // The denial must be side-effect free: no SFU unregistration and
    // the sender's Muji advertisement stays intact under the old nick.
    assert!(
        recorder.snapshot().is_empty(),
        "denied nick change must not unregister the SFU participant"
    );
    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        room.find_nick_by_real_jid(&alice),
        Some("alice"),
        "occupant keeps the original nick"
    );
    assert!(
        room.muji_for_session("alice", &alice).is_some(),
        "denied nick change must not clear the sender's Muji call state"
    );
}

// ---------------------------------------------------------------------------
// XEP-0045 §10.9 room destroy (#1261): every occupant SESSION gets the
// unavailable+<destroy/> presence (not one arbitrary session per nick),
// and the durable state the join path would resurrect the room from —
// channel catalog row, invite ledger — is wiped.
// ---------------------------------------------------------------------------

fn owner_destroy_iq_frame(room_jid: &BareJid) -> String {
    format!(
        "<iq xmlns='jabber:client' id='destroy-1' type='set' to='{room_jid}'>\
           <query xmlns='http://jabber.org/protocol/muc#owner'>\
             <destroy><reason>macbeth is dead</reason></destroy>\
           </query>\
         </iq>"
    )
}

fn presence_has_muc_user_destroy(element: &Element) -> bool {
    element.name() == "presence"
        && element.attr("type") == Some("unavailable")
        && element
            .get_child("x", "http://jabber.org/protocol/muc#user")
            .is_some_and(|x| {
                x.get_child("destroy", "http://jabber.org/protocol/muc#user")
                    .is_some()
            })
}

#[tokio::test]
async fn xep0045_destroy_notifies_every_occupant_session_and_wipes_durable_state() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "destroy-wipe@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_web: FullJid = "bob@example.com/web".parse().expect("bob web jid");
    let bob_phone: FullJid = "bob@example.com/phone".parse().expect("bob phone jid");

    // The channel catalog row the join path resurrects a managed room
    // from — #1261's exact resurrection vector.
    crate::server::xmpp_channels::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_channels::XmppChannelUpsert {
            id: "destroy-wipe".to_string(),
            name: "Doomed".to_string(),
            description: None,
            channel_type: "text".to_string(),
            position: 0,
            is_default: false,
            pin_permission: Default::default(),
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("seed channel row");

    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let (bob_web_tx, mut bob_web_rx) = mpsc::channel(8);
    let (bob_phone_tx, mut bob_phone_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob_web, bob_web_tx).await;
    register_test_connection(state.as_ref(), &bob_phone, bob_phone_tx).await;

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    for bob in [&bob_web, &bob_phone] {
        let join_frames = handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            bob,
            "bob",
            None,
            &Some(bob_session.clone()),
        )
        .await;
        assert!(
            !join_frames
                .iter()
                .any(|frame| frame.contains("type='error'") || frame.contains("type=\"error\"")),
            "bob join must succeed: {join_frames:?}"
        );
    }
    while bob_web_rx.try_recv().is_ok() {}
    while bob_phone_rx.try_recv().is_ok() {}

    // Make alice the room owner so the destroy authorizes.
    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room exists");
    room_actor
        .ask(ChangeAffiliation {
            jid: alice.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");

    // Seed an outstanding invite: destroy must wipe the ledger.
    crate::server::routes::websocket::muc_invites::record_invite(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::routes::websocket::muc_invites::OutstandingInvite {
            room: room_jid.clone(),
            invitee: "hecate@example.com".parse().expect("invitee"),
            inviter: alice.to_bare(),
        },
    )
    .await
    .expect("seed invite ledger row");

    let responses = handle_iq(
        &owner_destroy_iq_frame(&room_jid),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(owner_session),
        &ready_phase(&alice),
    )
    .await;

    // The destroying owner gets their own destroy presence inline plus
    // the IQ result.
    assert!(
        responses.iter().any(
            |frame| Element::from_str(frame).is_ok_and(|el| presence_has_muc_user_destroy(&el))
        ),
        "owner receives their own destroy presence: {responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("type=\"result\"") || frame.contains("type='result'")),
        "owner receives the IQ result: {responses:?}"
    );

    // §10.9: EVERY occupant session is notified — both of bob's devices.
    for (label, rx) in [("web", &mut bob_web_rx), ("phone", &mut bob_phone_rx)] {
        let outbound = rx.try_recv().unwrap_or_else(|_| {
            panic!("bob/{label} must receive the destroy presence: {responses:?}")
        });
        let element =
            Element::from_str(&stanza_to_xml(&outbound.stanza)).expect("destroy presence XML");
        assert!(
            presence_has_muc_user_destroy(&element),
            "bob/{label} destroy presence must carry <destroy/>: {element:?}"
        );
    }

    // Durable wipe: channel catalog row and invite ledger are gone.
    let channel = crate::server::xmpp_state::get_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        "destroy-wipe",
    )
    .await
    .expect("channel lookup");
    assert!(
        channel.is_none(),
        "destroy must delete the channel catalog row (resurrection vector)"
    );
    let invite = crate::server::routes::websocket::muc_invites::list_invites(
        state.deps.app_state.db_pool.global_actor().clone(),
        &room_jid,
        &"hecate@example.com".parse().expect("invitee"),
    )
    .await
    .expect("ledger lookup");
    assert!(invite.is_empty(), "destroy must wipe the invite ledger");

    // And the room actor itself is gone from the registry.
    let room_after = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask");
    assert!(
        room_after.is_none(),
        "destroyed room must leave the registry"
    );
}

/// §10.9 destroy-then-rejoin: the recreated room must be a FRESH room —
/// no config, subject, or ban list carried over from the destroyed one.
#[tokio::test]
async fn xep0045_destroy_then_rejoin_yields_fresh_room_without_old_bans() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "destroy-rejoin@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let banned: BareJid = "hecate@example.com".parse().expect("banned jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room exists");
    room_actor
        .ask(ChangeAffiliation {
            jid: alice.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");
    room_actor
        .ask(ChangeAffiliation {
            jid: banned.clone(),
            affiliation: Affiliation::Outcast,
        })
        .await
        .expect("ban hecate");

    let responses = handle_iq(
        &owner_destroy_iq_frame(&room_jid),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(owner_session.clone()),
        &ready_phase(&alice),
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("result")),
        "destroy must succeed: {responses:?}"
    );

    // Rejoin re-creates the room from scratch.
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let fresh = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(
        fresh.get_affiliation(&banned),
        Affiliation::None,
        "a destroyed room's ban list must not resurrect on rejoin"
    );
}

/// §10.9 destroy of a group DM must also revoke each member's durable
/// permission tuple — those are otherwise only cleaned by the admin
/// group-DM deletion flow, leaving members durably authorized for a
/// room that no longer exists.
#[tokio::test]
async fn xep0045_destroy_group_dm_revokes_member_permission_tuples() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "destroy-gdm@muc.example.com".parse().expect("room jid");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: BareJid = "bob@example.com".parse().expect("bob jid");

    crate::server::xmpp_channels::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_channels::XmppChannelUpsert {
            id: "destroy-gdm".to_string(),
            name: "Doomed DM".to_string(),
            description: None,
            channel_type: waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string(),
            position: 0,
            is_default: false,
            pin_permission: Default::default(),
            members_only: false,
            public_room: false,
        },
    )
    .await
    .expect("seed group-DM channel row");
    crate::admin::channels::persist_group_dm_member_tuple(
        &state.deps.app_state,
        "destroy-gdm",
        &owner_session
            .user_jid
            .parse()
            .expect("owner-session permission principal"),
    )
    .await
    .expect("seed alice member tuple");
    crate::admin::channels::persist_group_dm_member_tuple(
        &state.deps.app_state,
        "destroy-gdm",
        &bob,
    )
    .await
    .expect("seed bob member tuple");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room exists");
    room_actor
        .ask(ChangeAffiliation {
            jid: alice.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");
    room_actor
        .ask(ChangeAffiliation {
            jid: bob.clone(),
            affiliation: Affiliation::Member,
        })
        .await
        .expect("grant bob member");

    let member_before = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(bob.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, "destroy-gdm"),
        })
        .await
        .expect("permission check before");
    assert!(
        member_before.allowed,
        "precondition: bob is a durable member"
    );

    let responses = handle_iq(
        &owner_destroy_iq_frame(&room_jid),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(owner_session),
        &ready_phase(&alice),
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("result")),
        "group-DM destroy must succeed: {responses:?}"
    );

    let member_after = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(bob.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, "destroy-gdm"),
        })
        .await
        .expect("permission check after");
    assert!(
        !member_after.allowed,
        "destroying a group DM must revoke members' durable permission tuples"
    );
    let channel = crate::server::xmpp_state::get_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        "destroy-gdm",
    )
    .await
    .expect("channel lookup");
    assert!(channel.is_none(), "group-DM channel row wiped");
}

// ─── Lane J6 (#1259 #1260 #1265): disco truthfulness + XEP-0045 minor
// conformance. Dedicated coverage for the disco#info well-formedness
// contract, reserved-nick discovery, occupant-JID disco, member-list
// access, admin error mapping, config knobs, and the RSM room list.

fn owner_config_submit_iq(room_jid: &BareJid, id: &str, fields: &[(&str, &str)]) -> String {
    let mut form = Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    for (var, value) in fields {
        form = form.append(
            Element::builder("field", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), *var)
                .append(
                    Element::builder("value", waddle_xmpp::muc::DATA_FORMS_NS)
                        .append(*value)
                        .build(),
                )
                .build(),
        );
    }
    element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_OWNER)
                    .append(form.build())
                    .build(),
            )
            .build(),
    )
}

fn disco_query_from_response(frame: &str) -> Element {
    let iq = Element::from_str(frame).expect("disco response XML");
    assert_eq!(iq.attr("type"), Some("result"), "disco result: {frame}");
    iq.get_child("query", waddle_xmpp::disco::DISCO_INFO_NS)
        .expect("disco#info query payload")
        .clone()
}

/// #1259 / XEP-0115 §5.4: a live room's disco#info response must be
/// well-formed — no duplicate features and exactly one `muc#roominfo`
/// FORM_TYPE extension form (carrying the XEP-0500 slow-mode field).
#[tokio::test]
async fn xep0115_room_disco_info_is_well_formed_with_single_roominfo_form() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "wellformed@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let frame = disco_info_iq_frame("room-wf-1", &room_jid.to_string(), None);
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let query = disco_query_from_response(responses.first().expect("room disco response"));
    let parsed =
        waddle_xmpp::disco::info::parse_disco_info_response(&query).expect("parseable disco#info");
    assert!(
        !parsed.ill_formed,
        "room disco#info must satisfy XEP-0115 §5.4 well-formedness"
    );

    // #1265 items 13+14 on the wire.
    let vars: Vec<&str> = parsed.features.iter().map(|f| f.0.as_str()).collect();
    assert!(vars.contains(&"muc_unsecured"), "{vars:?}");
    assert!(
        vars.contains(&"http://jabber.org/protocol/muc#stable_id"),
        "{vars:?}"
    );
    assert!(!vars.contains(&"muc_passwordprotected"), "{vars:?}");

    // Exactly one muc#roominfo FORM_TYPE, carrying the slow-mode field.
    let roominfo_forms: Vec<&Element> = parsed
        .extensions
        .iter()
        .filter(|form| {
            form.children().any(|field| {
                field.attr("var") == Some("FORM_TYPE")
                    && field
                        .children()
                        .any(|v| v.text() == "http://jabber.org/protocol/muc#roominfo")
            })
        })
        .collect();
    assert_eq!(
        roominfo_forms.len(),
        1,
        "exactly one muc#roominfo form (#1259): {:?}",
        parsed.extensions
    );
    assert!(
        roominfo_forms[0]
            .children()
            .any(|field| field.attr("var") == Some("muc#roominfo_slow_mode_duration")),
        "slow-mode duration rides inside the single muc#roominfo form"
    );
}

/// #1265 item 9 / XEP-0045 §7.12: reserved-nick discovery. An occupant
/// gets their locked nick as a conference identity name; a user with no
/// occupancy gets an empty query.
#[tokio::test]
async fn xep0045_reserved_nick_discovery_returns_locked_nick() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "reserved-nick@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;

    let frame = disco_info_iq_frame("nick-1", &room_jid.to_string(), Some("x-roomuser-item"));
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let query = disco_query_from_response(responses.first().expect("reserved nick response"));
    assert_eq!(query.attr("node"), Some("x-roomuser-item"));
    let identity = query
        .get_child("identity", waddle_xmpp::disco::DISCO_INFO_NS)
        .expect("occupant has a reserved-nick identity");
    assert_eq!(identity.attr("category"), Some("conference"));
    assert_eq!(identity.attr("name"), Some("alice"));

    // A different resource of the SAME account sees the same reserved
    // nick — the lock is on the identity, not the joining session.
    let alice_other: FullJid = "alice@example.com/other".parse().expect("alice other jid");
    let frame = disco_info_iq_frame("nick-1b", &room_jid.to_string(), Some("x-roomuser-item"));
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_other),
    )
    .await;
    let query = disco_query_from_response(responses.first().expect("sibling resource response"));
    let identity = query
        .get_child("identity", waddle_xmpp::disco::DISCO_INFO_NS)
        .expect("sibling resource sees the reserved nick");
    assert_eq!(identity.attr("name"), Some("alice"));

    let frame = disco_info_iq_frame("nick-2", &room_jid.to_string(), Some("x-roomuser-item"));
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;
    let query = disco_query_from_response(responses.first().expect("no reserved nick response"));
    assert!(
        query
            .get_child("identity", waddle_xmpp::disco::DISCO_INFO_NS)
            .is_none(),
        "no occupancy → empty query per XEP-0030"
    );
}

/// #1265 item 10 / XEP-0045 §6.6: disco to `room@service/nick` is
/// <bad-request/> for non-occupants (MUST); occupants get
/// <feature-not-implemented/> because pass-through is unsupported.
#[tokio::test]
async fn xep0045_occupant_jid_disco_rejected_per_section_6_6() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "occupant-disco@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session),
    )
    .await;
    let occupant_target = format!("{room_jid}/alice");

    // Non-occupant → bad-request, for both disco flavors.
    for (id, frame) in [
        (
            "66-info",
            disco_info_iq_frame("66-info", &occupant_target, None),
        ),
        (
            "66-items",
            disco_items_iq_frame("66-items", &occupant_target, None),
        ),
    ] {
        let responses = handle_iq(
            &frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&bob_jid),
        )
        .await;
        let response = responses.first().unwrap_or_else(|| panic!("{id} response"));
        assert!(
            response.contains("bad-request"),
            "§6.6 MUST bad-request for non-occupant {id}: {response}"
        );
    }

    // Occupant → feature-not-implemented (no pass-through support).
    let frame = disco_info_iq_frame("66-occ", &occupant_target, None);
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("occupant disco response");
    assert!(
        response.contains("feature-not-implemented"),
        "occupant pass-through unsupported: {response}"
    );
}

/// #1265 item 16 / XEP-0045 §8.2: kicking a nick that is not in the
/// room returns <item-not-found/>, not <forbidden/>.
#[tokio::test]
async fn xep0045_kick_absent_nick_returns_item_not_found() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "kick-ghost@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, _bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;

    let kick_iq = build_admin_set_iq_xml(
        &room_jid,
        "kick-ghost",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(minidom::rxml::xml_ncname!("nick").to_owned(), "ghost")
            .attr(minidom::rxml::xml_ncname!("role").to_owned(), "none")
            .build(),
    );
    let responses = handle_iq(
        &kick_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("kick error response");
    assert!(
        response.contains("item-not-found"),
        "absent nick is item-not-found: {response}"
    );
    assert!(!response.contains("<forbidden"), "{response}");
}

/// #1265 item 12 / XEP-0045 §9.5: a member can retrieve the member
/// list; other affiliation lists stay admin-only.
#[tokio::test]
async fn xep0045_member_can_retrieve_member_list() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "member-list@muc.example.com".parse().expect("room jid");
    let (alice_session, alice_jid, bob_jid) =
        join_alice_owner_and_bob(state.as_ref(), &room_jid).await;

    // Alice grants bob explicit membership.
    let grant_iq = build_admin_set_iq_xml(
        &room_jid,
        "grant-bob",
        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                bob_jid.to_bare().to_string(),
            )
            .attr(
                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                "member",
            )
            .build(),
    );
    let grant_responses = handle_iq(
        &grant_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready_phase(&alice_jid),
    )
    .await;
    assert!(
        grant_responses
            .first()
            .is_some_and(|r| r.contains("type='result'")),
        "membership grant: {grant_responses:?}"
    );

    let member_list_get = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "get-members")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "member",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let responses = handle_iq(
        &member_list_get,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;
    let response = responses.first().expect("member list response");
    assert!(
        response.contains("type='result'"),
        "member may GET the member list (§9.5): {response}"
    );
    assert!(
        response.contains(&bob_jid.to_bare().to_string()),
        "member list includes bob: {response}"
    );

    // The owner list remains privileged.
    let owner_list_get = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "get-owners")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                room_jid.to_string(),
            )
            .append(
                Element::builder("query", waddle_xmpp::muc::NS_MUC_ADMIN)
                    .append(
                        Element::builder("item", waddle_xmpp::muc::NS_MUC_ADMIN)
                            .attr(
                                minidom::rxml::xml_ncname!("affiliation").to_owned(),
                                "owner",
                            )
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let responses = handle_iq(
        &owner_list_get,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;
    let response = responses.first().expect("owner list response");
    assert!(
        response.contains("forbidden"),
        "owner list stays admin+: {response}"
    );
}

/// #1265 item 7 / XEP-0045 §10.2: `muc#roomconfig_maxusers` and (for
/// ad-hoc rooms) `muc#roomconfig_persistentroom` apply on submit.
#[tokio::test]
async fn xep0045_owner_config_maxusers_and_persistentroom_apply() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "config-knobs@muc.example.com".parse().expect("room jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice_jid,
        "alice",
        None,
        &Some(alice_session.clone()),
    )
    .await;

    let owner_iq = owner_config_submit_iq(
        &room_jid,
        "knobs-1",
        &[
            ("muc#roomconfig_maxusers", "17"),
            ("muc#roomconfig_persistentroom", "0"),
            ("muc#roomconfig_changesubject", "1"),
        ],
    );
    let responses = handle_iq(
        &owner_iq,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice_session),
        &ready_phase(&alice_jid),
    )
    .await;
    assert!(
        responses
            .first()
            .is_some_and(|r| r.contains("type='result'")),
        "owner config submit: {responses:?}"
    );

    let room = snapshot_room(state.as_ref(), &room_jid).await.room;
    assert_eq!(room.config.max_occupants, 17, "maxusers honored");
    // #1265 item 7: persistentroom is NOT offered in the form —
    // configuring a room persists it into the channel catalog, so the
    // submitted (unoffered) field is ignored and the room stays
    // persistent, which is what the disco features truthfully say.
    assert!(
        room.config.persistent,
        "unoffered persistentroom field is ignored; configured rooms are persistent"
    );
    assert!(
        room.config.occupants_may_change_subject,
        "changesubject knob honored"
    );
}

/// #1265 item 11 / XEP-0030 §3.2: disco#items on the MUC domain with an
/// unknown node is <item-not-found/>, never the room list.
#[tokio::test]
async fn xep0030_muc_disco_items_unknown_node_returns_item_not_found() {
    let state = create_test_websocket_state().await;
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let frame = disco_items_iq_frame("items-node-1", "muc.example.com", Some("bogus-node"));
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let response = responses.first().expect("node disco response");
    assert!(response.contains("item-not-found"), "{response}");
}

/// #1265 item 11 / XEP-0045 §6.3 + XEP-0059: the MUC room list pages
/// via RSM and includes public live instant rooms.
#[tokio::test]
async fn xep0059_muc_disco_items_rsm_pages_room_list() {
    let state = create_test_websocket_state().await;
    let alice_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    for room in ["rsm-a", "rsm-b", "rsm-c"] {
        let room_jid: BareJid = format!("{room}@muc.example.com").parse().expect("room jid");
        handle_muc_join(
            state.as_ref(),
            "example.com",
            &room_jid,
            &alice_jid,
            "alice",
            None,
            &Some(alice_session.clone()),
        )
        .await;
    }

    // Full (un-paged) list surfaces the public live instant rooms.
    let frame = disco_items_iq_frame("rsm-full", "muc.example.com", None);
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let full = responses.first().expect("full room list");
    for room in ["rsm-a", "rsm-b", "rsm-c"] {
        assert!(
            full.contains(&format!("{room}@muc.example.com")),
            "live public instant room {room} listed: {full}"
        );
    }

    // Paged request: max=2 → 2 items + <set/> metadata.
    let rsm_query = Element::builder("query", waddle_xmpp::disco::DISCO_ITEMS_NS)
        .append(
            Element::builder("set", "http://jabber.org/protocol/rsm")
                .append(
                    Element::builder("max", "http://jabber.org/protocol/rsm")
                        .append("2")
                        .build(),
                )
                .build(),
        )
        .build();
    let frame = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "rsm-page")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                "muc.example.com",
            )
            .append(rsm_query)
            .build(),
    );
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let page =
        Element::from_str(responses.first().expect("paged room list")).expect("paged response XML");
    let query = page
        .get_child("query", waddle_xmpp::disco::DISCO_ITEMS_NS)
        .expect("paged query");
    let items: Vec<&Element> = query
        .children()
        .filter(|child| child.name() == "item")
        .collect();
    assert_eq!(items.len(), 2, "max=2 returns two items");
    let set = query
        .get_child("set", "http://jabber.org/protocol/rsm")
        .expect("XEP-0059 <set/> in paged response");
    let count: usize = set
        .get_child("count", "http://jabber.org/protocol/rsm")
        .expect("count")
        .text()
        .parse()
        .expect("count number");
    assert!(count >= 3, "count covers all rooms: {count}");
    assert!(
        set.get_child("first", "http://jabber.org/protocol/rsm")
            .is_some()
            && set
                .get_child("last", "http://jabber.org/protocol/rsm")
                .is_some(),
        "first/last present"
    );
}

/// XEP-0410 clustered gap (race review P1 on PR #1277): a self-ping to
/// a room with NO local actor must still answer the optimized "joined"
/// result when THIS node admitted the session into that room on a
/// remote node (recorded in `remote_muc_memberships`) — otherwise every
/// cross-node occupant enters a perpetual leave/rejoin loop. A
/// different nick (or absent membership) keeps the not-joined answer.
#[tokio::test]
async fn muc_self_ping_answers_joined_for_recorded_remote_membership() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let ready = ready_phase(&alice_jid);
    let room_jid: BareJid = "remote-room@muc.example.com".parse().expect("room jid");

    state
        .deps
        .protocol
        .remote_muc_memberships
        .record_join(&alice_jid, &room_jid, "alice");

    let ping = |nick: &str, id: &str| {
        let iq = xmpp_parsers::iq::Iq::Get {
            from: Some(jid::Jid::from(alice_jid.clone())),
            to: Some(format!("{room_jid}/{nick}").parse().expect("occupant jid")),
            id: id.to_string(),
            payload: Element::builder("ping", "urn:xmpp:ping").build(),
        };
        let element = Element::from(iq);
        let mut bytes = Vec::new();
        element.write_to(&mut bytes).expect("serialize ping");
        String::from_utf8(bytes).expect("utf8 ping")
    };

    let responses = handle_iq(
        &ping("alice", "sp-remote-1"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("type='result'"),
        "recorded remote membership must answer the optimized joined result: {}",
        responses[0]
    );

    // Wrong nick → authoritative not-joined answer.
    let responses = handle_iq(
        &ping("other-nick", "sp-remote-2"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("not-acceptable"),
        "a nick not held by this session answers not-joined: {}",
        responses[0]
    );

    // After the membership is tombstoned (leave/cleanup in flight),
    // the not-joined answer returns.
    let _ = state
        .deps
        .protocol
        .remote_muc_memberships
        .take_for_occupant(&alice_jid);
    let responses = handle_iq(
        &ping("alice", "sp-remote-3"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;
    assert_eq!(responses.len(), 1);
    assert!(
        responses[0].contains("not-acceptable"),
        "a tombstoned membership answers not-joined: {}",
        responses[0]
    );
}

/// Greptile P1 (#1276): the registry destroy is the point of no
/// return. When its fenced durable wipe fails (`DurableWipeFailed`),
/// the owner gets a retryable error and — critically — NO occupant
/// receives a destroy presence and no SFU participant is torn down:
/// nobody may be told the room died while it lives on.
#[tokio::test]
async fn xep0045_destroy_wipe_failure_sends_no_destroy_presence() {
    use waddle_xmpp::muc::durable::{MucDurableFuture, MucDurableStore, RoomClaimFenceContext};
    use waddle_xmpp::muc::room_registry_actor::WireClusteringClaims;
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    /// Every write succeeds except the destroy-time wipe.
    struct FailingDeleteStore {
        expected_owner: NodeIdentity,
    }
    impl MucDurableStore for FailingDeleteStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<waddle_xmpp::muc::durable::DurableRoomState>> {
            let expected =
                expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            let validation = (fence == &expected)
                .then_some(())
                .ok_or_else(|| waddle_xmpp::XmppError::internal("unexpected exact room fence"));
            Box::pin(async move {
                validation?;
                Ok(None)
            })
        }
        fn save_config_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a waddle_xmpp::muc::RoomConfig,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let expected =
                expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            let validation = (fence == &expected)
                .then_some(())
                .ok_or_else(|| waddle_xmpp::XmppError::internal("unexpected exact room fence"));
            Box::pin(async move { validation })
        }
        fn save_subject_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let expected =
                expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            let validation = (fence == &expected)
                .then_some(())
                .ok_or_else(|| waddle_xmpp::XmppError::internal("unexpected exact room fence"));
            Box::pin(async move { validation })
        }
        fn save_affiliation_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let expected =
                expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            let validation = (fence == &expected)
                .then_some(())
                .ok_or_else(|| waddle_xmpp::XmppError::internal("unexpected exact room fence"));
            Box::pin(async move { validation })
        }
        fn delete_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, ()> {
            let expected =
                expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            if fence != &expected {
                let error = waddle_xmpp::XmppError::internal("unexpected exact room fence");
                return Box::pin(async move { Err(error) });
            }
            Box::pin(async {
                Err(waddle_xmpp::XmppError::internal(
                    "destroy-time wipe refused by test store",
                ))
            })
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let matches =
                fence == &expected_room_fence(room_jid, self.expected_owner.clone(), ClaimEpoch(0));
            Box::pin(async move { Ok(matches) })
        }
    }

    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "destroy-wipe-fails@muc.example.com"
        .parse()
        .expect("room jid");
    // Seed the managed-channel catalog row: the failed destroy must
    // leave it untouched (the preparatory clustering wipe aborts the
    // sequence before any waddle-side delete runs).
    crate::server::xmpp_channels::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_channels::XmppChannelUpsert {
            id: "destroy-wipe-fails".to_string(),
            name: "Unkillable".to_string(),
            description: None,
            channel_type: "text".to_string(),
            position: 0,
            is_default: false,
            pin_permission: Default::default(),
            members_only: false,
            public_room: true,
        },
    )
    .await
    .expect("seed channel row");
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_session = create_test_session(state.as_ref(), "bob").await;
    let bob: FullJid = "bob@example.com/web".parse().expect("bob jid");

    let durable_owner = NodeIdentity::new("test-node", "epoch-1");
    state
        .deps
        .protocol
        .room_registry
        .ask(WireClusteringClaims {
            claim_store: std::sync::Arc::new(InProcessClaimStore::new())
                as std::sync::Arc<dyn ClaimStore>,
            node_identity: SharedNodeIdentity::new(durable_owner.clone()),
            durable_store: Some(std::sync::Arc::new(FailingDeleteStore {
                expected_owner: durable_owner,
            })),
            rollout_backoff: None,
        })
        .await
        .expect("wire failing durable store");

    let (bob_tx, mut bob_rx) = mpsc::channel(8);
    register_test_connection(state.as_ref(), &bob, bob_tx).await;

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &alice,
        "alice",
        None,
        &Some(owner_session.clone()),
    )
    .await;
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &bob,
        "bob",
        None,
        &Some(bob_session),
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let room_actor = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask")
        .expect("room exists");
    room_actor
        .ask(ChangeAffiliation {
            jid: alice.to_bare(),
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("grant owner");

    let responses = handle_iq(
        &owner_destroy_iq_frame(&room_jid),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(owner_session),
        &ready_phase(&alice),
    )
    .await;

    assert_eq!(responses.len(), 1, "wipe-failed destroy: {responses:?}");
    assert!(
        responses[0].contains("internal-server-error"),
        "a destroy whose fenced wipe failed must error, not succeed: {responses:?}"
    );
    assert!(
        !responses.iter().any(
            |frame| Element::from_str(frame).is_ok_and(|el| presence_has_muc_user_destroy(&el))
        ),
        "the owner must not receive a destroy presence: {responses:?}"
    );
    assert!(
        bob_rx.try_recv().is_err(),
        "no occupant may be told the room died while it lives on"
    );
    let still_there = state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::GetRoom {
            room_jid: room_jid.clone(),
        })
        .await
        .expect("registry ask");
    assert!(still_there.is_some(), "the room stays registered for retry");
    let channel = crate::server::xmpp_state::get_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        "destroy-wipe-fails",
    )
    .await
    .expect("channel lookup");
    assert!(
        channel.is_some(),
        "a destroy aborted by the preparatory clustering wipe must leave \
         the channel catalog row (and all other waddle-side rows) intact"
    );
}
