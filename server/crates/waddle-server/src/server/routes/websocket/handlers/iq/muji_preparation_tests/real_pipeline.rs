use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use kameo::actor::ActorRef;
use waddle_sfu::{
    ApiKey, ApiSecret, CallId, Identity, Jti, ListedRoom, ListedRoomName, LiveKitAdmin, LiveKitSfu,
    LiveParticipant, MediaCapabilities, ObservedCallSids, ParticipantAttributes, ParticipantSid,
    RoomOccupancy, RoomSid, SessionBinding, SfuConfig, SfuError, SfuService,
    SidObservationDisposition, TurnHost, TurnSharedSecret, WebsocketUrl,
};
use waddle_xmpp::muc::room_actor::{
    GetSnapshot, JoinAffiliationGrant, JoinWithAffiliation, RoomActor, UpsertMujiPresence,
};
use waddle_xmpp::muc::room_registry_actor::CreateInstantRoom;
use waddle_xmpp::xep::xep0167::MediaKind;
use waddle_xmpp::xep::xep0272::{Creator, Muji, MujiContent};
use waddle_xmpp::Affiliation;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::minidom::Element;

use super::{execute_adapter, initiate_iq, Adapter, SIGNING_SECRET};
use crate::call_teardown_outbox::{
    drain_due, CallTeardownIntent, CallTeardownStatus, TeardownTarget,
};
use crate::server::routes::websocket::tests::create_test_websocket_state_with_sfu;

/// Only the external provider boundary is simulated. Its inventory is the
/// same evidence consumed by real preparation and the real outbox executor.
struct PipelineProvider {
    call: CallId,
    room_sid: RoomSid,
    live: Mutex<Vec<LiveParticipant>>,
    removed: Mutex<Vec<LiveParticipant>>,
    deleted_rooms: AtomicUsize,
}

impl LiveKitAdmin for PipelineProvider {
    fn list_rooms(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>> {
        Box::pin(async move {
            Ok(vec![ListedRoom {
                name: ListedRoomName::classify(self.call.as_str().to_owned()),
                sid: Some(self.room_sid.clone()),
                num_participants: Some(
                    u64::try_from(self.live.lock().expect("provider inventory").len())
                        .expect("small inventory"),
                ),
            }])
        })
    }

    fn room_occupancy<'a>(
        &'a self,
        call: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(call, &self.call);
            Ok(RoomOccupancy {
                waddle: self.live.lock().expect("provider inventory").clone(),
                foreign: 0,
            })
        })
    }

    fn remove_participant<'a>(
        &'a self,
        call: &'a CallId,
        identity: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(call, &self.call);
            let mut live = self.live.lock().expect("provider inventory");
            if let Some(index) = live
                .iter()
                .position(|participant| &participant.identity == identity)
            {
                let removed = live.remove(index);
                self.removed
                    .lock()
                    .expect("provider removals")
                    .push(removed);
            }
            Ok(())
        })
    }

    fn delete_room<'a>(
        &'a self,
        _: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async move {
            self.deleted_rooms.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }

    fn update_participant<'a>(
        &'a self,
        _: &'a CallId,
        _: &'a Identity,
        _: MediaCapabilities,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

async fn occupy(
    actor: &ActorRef<RoomActor>,
    sender: &FullJid,
    generation: OccupancySessionGeneration,
) {
    let admission_revision = actor
        .ask(GetSnapshot)
        .await
        .expect("admission snapshot")
        .admission_revision;
    actor
        .ask(JoinWithAffiliation {
            sender_jid: sender.clone(),
            nick: "alice".into(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".into(),
            admission_revision,
            session: generation,
        })
        .await
        .expect("join exact generation");
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("joined snapshot")
            .room
            .session_generation(sender),
        Some(generation)
    );
}

#[derive(serde::Deserialize)]
struct IssuedClaims {
    jti: String,
    attributes: ParticipantAttributes,
}

fn issued_claims(frames: &[String]) -> IssuedClaims {
    let token = frames
        .iter()
        .find_map(|frame| {
            let frame: Element = frame.parse().ok()?;
            let jingle = frame.children().find(|child| {
                child.name() == "jingle" && child.attr("action") == Some("session-accept")
            })?;
            jingle
                .children()
                .find(|child| child.name() == "content")?
                .children()
                .find(|child| child.name() == "transport")?
                .children()
                .find(|child| child.name() == "token")
                .map(Element::text)
        })
        .unwrap_or_else(|| panic!("real pipeline must issue credentials: {frames:?}"));
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_required_spec_claims::<&str>(&[]);
    jsonwebtoken::decode::<IssuedClaims>(
        &token,
        &jsonwebtoken::DecodingKey::from_secret(SIGNING_SECRET.as_bytes()),
        &validation,
    )
    .expect("verify actual issued token")
    .claims
}

#[tokio::test]
async fn real_muji_pipeline_persists_and_retires_old_media_then_rejects_unfenced_cleanup() {
    let room: BareJid = "real-retirement-pipeline@muc.example.com"
        .parse()
        .expect("room");
    let call = CallId::try_new(room.to_string()).expect("call");
    let sender: FullJid = "alice@example.com/web".parse().expect("JID");
    let identity = Identity::from_jid(sender.clone());
    let provider = Arc::new(PipelineProvider {
        call: call.clone(),
        room_sid: RoomSid::try_new("RM_pipeline").expect("room SID"),
        live: Mutex::new(Vec::new()),
        removed: Mutex::new(Vec::new()),
        deleted_rooms: AtomicUsize::new(0),
    });
    let sfu = Arc::new(LiveKitSfu::with_admin(
        SfuConfig {
            api_key: ApiKey::new("APIpipeline"),
            api_secret: ApiSecret::from_text(SIGNING_SECRET).expect("signing secret"),
            webhook_secret: ApiSecret::from_text(SIGNING_SECRET).expect("webhook secret"),
            ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().expect("URL"))
                .expect("WebSocket URL"),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("real-pipeline-turn-secret"),
            token_ttl: chrono::Duration::seconds(3600),
            turn_ttl: chrono::Duration::seconds(3600),
        },
        provider.clone() as Arc<_>,
    ));
    let mut state = create_test_websocket_state_with_sfu(sfu.clone() as Arc<_>).await;
    Arc::get_mut(&mut state)
        .expect("unshared state")
        .deps
        .protocol
        .call_teardown_executor = Some(sfu.teardown_executor());
    let actor = state
        .deps
        .protocol
        .room_registry
        .ask(CreateInstantRoom {
            room_jid: room.clone(),
        })
        .await
        .expect("create room")
        .actor_ref;
    let first = OccupancySessionGeneration::mint();
    occupy(&actor, &sender, first).await;
    let first_frames = execute_adapter(
        Adapter::Local,
        state.clone(),
        initiate_iq(&room, &sender),
        sender.clone(),
        first,
    )
    .await
    .expect("initial IQ handled");
    let first_claims = issued_claims(&first_frames);
    assert_eq!(first_claims.attributes.media_generation, Some(first));
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .queue_stats()
            .await
            .expect("initial queue")
            .queued_count,
        0,
        "the initial publication has no displaced predecessor"
    );
    let old_sid = ParticipantSid::try_new("PA_pipeline_old").expect("old SID");
    *provider.live.lock().expect("provider inventory") = vec![LiveParticipant {
        identity: identity.clone(),
        participant_sid: Some(old_sid.clone()),
        media_generation: first_claims.attributes.media_generation,
    }];
    let mut old_media =
        ObservedCallSids::new(Some(provider.room_sid.clone()), Some(old_sid.clone()));
    old_media.media_generation = first_claims.attributes.media_generation;
    assert_eq!(
        sfu.register_call_participant_observed(&call, &identity, &old_media),
        SidObservationDisposition::Applied
    );

    let second = OccupancySessionGeneration::mint();
    occupy(&actor, &sender, second).await;
    actor
        .ask(UpsertMujiPresence {
            sender_jid: sender.clone(),
            occupant: Some(second),
            muji: Muji::with_contents(vec![MujiContent::new(
                "audio",
                Creator::Initiator,
                MediaKind::Audio,
            )]),
        })
        .await
        .expect("new Muji presence")
        .expect("current occupant");
    let second_frames = execute_adapter(
        Adapter::Local,
        state.clone(),
        initiate_iq(&room, &sender),
        sender.clone(),
        second,
    )
    .await
    .expect("replacement IQ handled");
    let second_claims = issued_claims(&second_frames);
    let second_jti = Jti::from_claim(second_claims.jti);
    assert_eq!(second_claims.attributes.media_generation, Some(second));
    assert!(sfu.is_revoked(&Jti::from_claim(first_claims.jti)));
    assert!(!sfu.is_revoked(&second_jti));
    assert!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .queue_stats()
            .await
            .expect("durable queue")
            .queued_count
            > 0,
        "replacement session-accept must follow committed retirement responsibility"
    );
    assert!(
        provider
            .removed
            .lock()
            .expect("provider removals")
            .is_empty(),
        "the provider must remain untouched before the real drain"
    );
    drain_due(&state, 32).await.expect("real retirement drain");
    let removed = provider.removed.lock().expect("provider removals").clone();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].participant_sid, Some(old_sid.clone()));
    assert_eq!(removed[0].media_generation, Some(first));
    assert_eq!(
        sfu.participant_occupant_session(&call, &identity),
        Some(second)
    );
    assert!(!sfu.is_revoked(&second_jti));

    // The same Jingle SID was used for both real publications. An old
    // generationless row must not turn that reused string into authority.
    let legacy_id = state
        .deps
        .protocol
        .call_teardown_outbox
        .enqueue(CallTeardownIntent {
            call_id: call.clone(),
            target: TeardownTarget::MujiPresenceClear {
                room_jid: room.clone(),
                departed: sender.clone(),
                participant_sid: Some(old_sid),
            },
            generation: None,
            occupant: None,
            unbound_occupant: waddle_sfu::UnboundOccupantPolicy::TearDown,
            room_sid: Some(provider.room_sid.clone()),
            session: Some(SessionBinding::try_new("paused-preparation").expect("reused session")),
        })
        .await
        .expect("post-publication legacy cleanup");
    drain_due(&state, 32)
        .await
        .expect("real legacy cleanup drain");
    assert_eq!(
        state
            .deps
            .protocol
            .call_teardown_outbox
            .find(&legacy_id)
            .await
            .expect("legacy row")
            .expect("stored legacy row")
            .status,
        CallTeardownStatus::Done
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &identity),
        Some(second)
    );
    assert!(!sfu.is_revoked(&second_jti));
    assert!(actor
        .ask(GetSnapshot)
        .await
        .expect("preserved Muji snapshot")
        .room
        .muji_for_session("alice", &sender)
        .is_some());

    let new_sid = ParticipantSid::try_new("PA_pipeline_successor").expect("new SID");
    *provider.live.lock().expect("provider inventory") = vec![LiveParticipant {
        identity: identity.clone(),
        participant_sid: Some(new_sid.clone()),
        media_generation: second_claims.attributes.media_generation,
    }];
    let mut new_media = ObservedCallSids::new(Some(provider.room_sid.clone()), Some(new_sid));
    new_media.media_generation = second_claims.attributes.media_generation;
    assert_eq!(
        sfu.register_call_participant_observed(&call, &identity, &new_media),
        SidObservationDisposition::Applied
    );
    assert_eq!(
        sfu.participant_occupant_session(&call, &identity),
        Some(second)
    );
    assert!(!sfu.is_revoked(&second_jti));
    assert_eq!(provider.removed.lock().expect("provider removals").len(), 1);
    assert_eq!(provider.deleted_rooms.load(Ordering::SeqCst), 0);
}
