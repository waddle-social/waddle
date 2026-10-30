use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use jid::{BareJid, FullJid};
use tokio::sync::Notify;
use waddle_sfu::{
    ApiKey, ApiSecret, CallId, Identity, JoinToken, Jti, ListedRoom, LiveKitAdmin, LiveKitSfu,
    MediaCapabilities, ObservedCallSids, ParticipantPreparationFuture,
    ParticipantRegistrationExpectation, PendingMediaRetirement, RoomOccupancy, SessionBinding,
    SessionScopedTeardown, SfuConfig, SfuError, SfuService, SidObservationDirection,
    SidObservationDisposition, TeardownDisposition, TurnCredential, TurnHost, TurnSharedSecret,
    WebsocketUrl,
};
use waddle_xmpp::muc::room_actor::{
    ConfigEffectPlan, GetConfig, GetOccupantVoice, GetSnapshot, Join, UpdateConfig,
};
use waddle_xmpp::muc::room_registry_actor::CreateInstantRoom;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::xep::xep0166::NS_JINGLE;
use waddle_xmpp::xep::xep0167::opus_audio_description;
use waddle_xmpp::xep::xep0272::Muji;
use waddle_xmpp::xep::xep_waddle_livekit_transport::WaddleLiveKitTransport;
use waddle_xmpp::{Affiliation, Role};
use waddle_xmpp_core::types::Voice;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{
    Action, Content, ContentId, Creator, Description, Jingle, SessionId, Transport,
};
use xmpp_parsers::minidom::Element;

use super::{IqConnState, IqHandlerContext};
use crate::call_teardown_outbox::{CallTeardownOutboxStore, TeardownTarget};
use crate::db::{Database, DatabaseConfig};
use crate::server::routes::websocket::tests::create_test_websocket_state_with_sfu;
use crate::server::routes::websocket::WebSocketState;

const SIGNING_SECRET: &str = "preparation-test-secret-at-least-32-bytes";

struct EmptyProviderAdmin;

impl LiveKitAdmin for EmptyProviderAdmin {
    fn list_rooms(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ListedRoom>, SfuError>> + Send + '_>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn remove_participant<'a>(
        &'a self,
        _: &'a CallId,
        _: &'a Identity,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    fn delete_room<'a>(
        &'a self,
        _: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    fn update_participant<'a>(
        &'a self,
        _: &'a CallId,
        _: &'a Identity,
        _: MediaCapabilities,
    ) -> Pin<Box<dyn Future<Output = Result<(), SfuError>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
    fn room_occupancy<'a>(
        &'a self,
        _: &'a CallId,
    ) -> Pin<Box<dyn Future<Output = Result<RoomOccupancy, SfuError>> + Send + 'a>> {
        Box::pin(async { Ok(RoomOccupancy::default()) })
    }
}

/// Only the asynchronous provider preparation boundary is controlled. Token
/// construction, publication CAS and registration are the real SFU behavior.
struct PausedPreparationSfu {
    inner: LiveKitSfu,
    entered: Notify,
    released: Notify,
    preparation_calls: AtomicUsize,
    pending_observed: Notify,
    published: Mutex<Vec<JoinToken>>,
}

impl PausedPreparationSfu {
    fn new() -> Self {
        Self {
            inner: LiveKitSfu::with_admin(
                SfuConfig {
                    api_key: ApiKey::new("APIpreparation"),
                    api_secret: ApiSecret::from_text(SIGNING_SECRET).expect("signing secret"),
                    webhook_secret: ApiSecret::from_text(SIGNING_SECRET).expect("webhook secret"),
                    ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().expect("URL"))
                        .expect("WebSocket URL"),
                    turn_host: TurnHost::new("turn.test"),
                    turn_tls_port: 443,
                    turn_udp_port: 3478,
                    turn_shared_secret: TurnSharedSecret::from_text("preparation-turn-secret"),
                    token_ttl: chrono::Duration::seconds(3600),
                    turn_ttl: chrono::Duration::seconds(3600),
                },
                Arc::new(EmptyProviderAdmin),
            ),
            entered: Notify::new(),
            released: Notify::new(),
            preparation_calls: AtomicUsize::new(0),
            pending_observed: Notify::new(),
            published: Mutex::new(Vec::new()),
        }
    }
}

impl SfuService for PausedPreparationSfu {
    fn pending_media_retirements(
        &self,
        call: &CallId,
        identity: &Identity,
    ) -> Vec<PendingMediaRetirement> {
        let pending = self.inner.pending_media_retirements(call, identity);
        if !pending.is_empty() {
            self.pending_observed.notify_one();
        }
        pending
    }
    fn acknowledge_media_retirement(&self, receipt: &PendingMediaRetirement) {
        self.inner.acknowledge_media_retirement(receipt);
    }
    fn issue_join_token(
        &self,
        call: &CallId,
        identity: &Identity,
        caps: MediaCapabilities,
    ) -> Result<JoinToken, SfuError> {
        self.inner.issue_join_token(call, identity, caps)
    }
    fn participant_registration_expectation(
        &self,
        call: &CallId,
        identity: &Identity,
    ) -> Option<ParticipantRegistrationExpectation> {
        self.inner
            .participant_registration_expectation(call, identity)
    }
    fn prepare_participant_registration<'a>(
        &'a self,
        call: &'a CallId,
        identity: &'a Identity,
    ) -> ParticipantPreparationFuture<'a> {
        self.preparation_calls.fetch_add(1, Ordering::SeqCst);
        let expected = self
            .inner
            .participant_registration_expectation(call, identity);
        Box::pin(async move {
            self.entered.notify_one();
            self.released.notified().await;
            expected
        })
    }
    fn issue_join_token_with_session(
        &self,
        call: &CallId,
        identity: &Identity,
        caps: MediaCapabilities,
        session: &SessionBinding,
        occupant: OccupancySessionGeneration,
        expected: ParticipantRegistrationExpectation,
    ) -> Result<Option<JoinToken>, SfuError> {
        let token = self
            .inner
            .issue_join_token_with_session(call, identity, caps, session, occupant, expected)?;
        if let Some(token) = &token {
            self.published
                .lock()
                .expect("published tokens")
                .push(token.clone());
        }
        Ok(token)
    }
    fn rollback_participant_registration(
        &self,
        call: &CallId,
        identity: &Identity,
        expected: ParticipantRegistrationExpectation,
    ) -> SessionScopedTeardown {
        self.inner
            .rollback_participant_registration(call, identity, expected)
    }
    fn issue_turn_credentials(&self, identity: &Identity) -> Result<TurnCredential, SfuError> {
        self.inner.issue_turn_credentials(identity)
    }
    fn register_call_participant(&self, call: &CallId, identity: &Identity) {
        self.inner.register_call_participant(call, identity);
    }
    fn register_call_participant_observed(
        &self,
        call: &CallId,
        identity: &Identity,
        observed: &ObservedCallSids,
    ) -> SidObservationDisposition {
        self.inner
            .register_call_participant_observed(call, identity, observed)
    }
    fn has_call_participant(&self, call: &CallId, identity: &Identity) -> bool {
        self.inner.has_call_participant(call, identity)
    }
    fn participant_session_binding(
        &self,
        call: &CallId,
        identity: &Identity,
    ) -> Option<SessionBinding> {
        self.inner.participant_session_binding(call, identity)
    }
    fn participant_occupant_session(
        &self,
        call: &CallId,
        identity: &Identity,
    ) -> Option<OccupancySessionGeneration> {
        self.inner.participant_occupant_session(call, identity)
    }
    fn unregister_call_participant(
        &self,
        call: &CallId,
        identity: &Identity,
        observed: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        self.inner
            .unregister_call_participant(call, identity, observed)
    }
    fn revoke_issued_token(&self, call: &CallId, identity: &Identity, jti: &Jti) {
        self.inner.revoke_issued_token(call, identity, jti);
    }
    fn note_participant_left(
        &self,
        call: &CallId,
        identity: &Identity,
        observed: Option<&ObservedCallSids>,
    ) -> TeardownDisposition {
        self.inner.note_participant_left(call, identity, observed)
    }
    fn observe_call_participant_sids(
        &self,
        call: &CallId,
        identity: &Identity,
        observed: Option<&ObservedCallSids>,
        direction: SidObservationDirection,
    ) -> SidObservationDisposition {
        self.inner
            .observe_call_participant_sids(call, identity, observed, direction)
    }
    fn update_participant_capabilities(
        &self,
        call: &CallId,
        identity: &Identity,
        caps: MediaCapabilities,
    ) {
        self.inner
            .update_participant_capabilities(call, identity, caps);
    }
    fn is_revoked(&self, jti: &Jti) -> bool {
        self.inner.is_revoked(jti)
    }
    fn ws_url(&self) -> &WebsocketUrl {
        self.inner.ws_url()
    }
    fn turn_host(&self) -> &TurnHost {
        self.inner.turn_host()
    }
    fn webhook_secret(&self) -> &ApiSecret {
        self.inner.webhook_secret()
    }
    fn participants_for_call(&self, call: &CallId) -> Vec<Identity> {
        self.inner.participants_for_call(call)
    }
}

fn initiate_iq(room: &BareJid, sender: &FullJid) -> Iq {
    let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
    content.description = Some(Description::Rtp(opus_audio_description()));
    content.transport = Some(Transport::Unknown(
        WaddleLiveKitTransport::Request.to_element(),
    ));
    let mut jingle = Jingle::new(
        Action::SessionInitiate,
        SessionId("paused-preparation".into()),
    );
    jingle.initiator = Some(sender.clone().into());
    jingle.contents.push(content);
    let mut payload: Element = jingle.into();
    payload.append_child(Muji::for_room(room.clone()).to_element());
    Iq::Set {
        from: Some(sender.clone().into()),
        to: Some("calls.example.com".parse().expect("mixer JID")),
        id: "paused-preparation".into(),
        payload,
    }
}

#[derive(Clone, Copy)]
enum Adapter {
    Local,
    #[cfg(feature = "clustering")]
    Relayed,
}

async fn execute_adapter(
    adapter: Adapter,
    state: Arc<WebSocketState>,
    iq: Iq,
    sender: FullJid,
    generation: OccupancySessionGeneration,
) -> Option<Vec<String>> {
    match adapter {
        #[cfg(feature = "clustering")]
        Adapter::Relayed => {
            super::jingle_muji_relay::handle_relayed_muji_initiate(&state, &iq, generation).await
        }
        Adapter::Local => {
            let mut carbons = false;
            let mut roster = false;
            let mut blocklist = false;
            let mut conn = IqConnState {
                carbons_enabled: &mut carbons,
                roster_interested: &mut roster,
                blocklist_interested: &mut blocklist,
                occupancy_session: &generation,
                registry_owner: None,
                state_machine: None,
                ordered_relay_origin: None,
            };
            super::sans_io::handle_sans_io_iq(
                IqHandlerContext {
                    iq: &iq,
                    id: iq.id(),
                    payload_ns: NS_JINGLE,
                    target_to: Some("calls.example.com"),
                    has_destroy: false,
                    domain: "example.com",
                    muc_domain: "muc.example.com",
                    upload_domain: "upload.example.com",
                    spaces_domain: "spaces.example.com",
                    community_domain: "community.example.com",
                    extensions_domain: "extensions.example.com",
                    push_domain: "push.example.com",
                    response_from: Some("calls.example.com"),
                    response_to: Some(sender.as_str()),
                },
                &state,
                None,
                &ConnectionPhase::ready(sender.clone(), false),
                &mut conn,
            )
            .await
        }
    }
}

async fn assert_voice_is_read_after_preparation(adapter: Adapter) {
    let sfu = Arc::new(PausedPreparationSfu::new());
    let state = create_test_websocket_state_with_sfu(sfu.clone() as Arc<_>).await;
    let room: BareJid = "preparation-grants@muc.example.com".parse().expect("room");
    let sender: FullJid = "alice@example.com/web".parse().expect("JID");
    let generation = OccupancySessionGeneration::mint();
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
    actor
        .ask(Join {
            session: generation,
            nick: "alice".into(),
            real_jid: sender.clone(),
            role: Role::Visitor,
            affiliation: Affiliation::Member,
        })
        .await
        .expect("join");
    assert_eq!(
        actor
            .ask(GetOccupantVoice {
                jid: sender.clone()
            })
            .await
            .expect("initial voice"),
        Some(Voice::Voiced)
    );
    let iq = initiate_iq(&room, &sender);
    let execution = tokio::spawn(execute_adapter(
        adapter,
        state.clone(),
        iq,
        sender.clone(),
        generation,
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), sfu.entered.notified())
        .await
        .expect("provider preparation must actually pause");
    let config = actor.ask(GetConfig).await.expect("config");
    actor
        .ask(UpdateConfig {
            config: waddle_xmpp::muc::RoomConfig {
                moderated: true,
                ..config
            },
            effect_plan: ConfigEffectPlan::DirectAudience,
        })
        .await
        .expect("revoke visitor voice");
    assert_eq!(
        actor
            .ask(GetOccupantVoice {
                jid: sender.clone()
            })
            .await
            .expect("new voice"),
        Some(Voice::Muted)
    );
    assert_eq!(
        actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot")
            .room
            .session_generation(&sender),
        Some(generation)
    );
    sfu.released.notify_one();
    let frames = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
        .await
        .expect("adapter completes")
        .expect("adapter task")
        .expect("IQ handled");
    let token = frames
        .iter()
        .find_map(|frame| {
            let frame: Element = frame.parse().ok()?;
            let jingle = frame.children().find(|child| child.name() == "jingle")?;
            if jingle.attr("action") != Some("session-accept") {
                return None;
            }
            jingle
                .children()
                .find(|child| child.name() == "content")?
                .children()
                .find(|child| child.name() == "transport")?
                .children()
                .find(|child| child.name() == "token")
                .map(Element::text)
        })
        .unwrap_or_else(|| panic!("expected issued session-accept, got {frames:?}"));
    #[derive(serde::Deserialize)]
    struct Claims {
        video: waddle_sfu::VideoGrant,
    }
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.set_required_spec_claims::<&str>(&[]);
    let claims = jsonwebtoken::decode::<Claims>(
        &token,
        &jsonwebtoken::DecodingKey::from_secret(SIGNING_SECRET.as_bytes()),
        &validation,
    )
    .expect("verify actual issued JWT")
    .claims;
    assert!(
        !claims.video.can_publish,
        "a pre-preparation voice grant must not survive its revocation"
    );
    assert!(claims.video.can_subscribe);
}

#[tokio::test]
async fn local_muji_preparation_cannot_freeze_voice_grants_before_async_work() {
    assert_voice_is_read_after_preparation(Adapter::Local).await;
}

async fn assert_admission_denies_before_preparation(adapter: Adapter, exhaust_read_budget: bool) {
    let sfu = Arc::new(PausedPreparationSfu::new());
    let state = create_test_websocket_state_with_sfu(sfu.clone() as Arc<_>).await;
    let room: BareJid = "preparation-admission@muc.example.com"
        .parse()
        .expect("room");
    let sender: FullJid = "alice@example.com/web".parse().expect("JID");
    let generation = OccupancySessionGeneration::mint();
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
    if exhaust_read_budget {
        actor
            .ask(Join {
                session: generation,
                nick: "alice".into(),
                real_jid: sender.clone(),
                role: Role::Participant,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");
        for _ in 0
            ..waddle_xmpp::protocol::handlers::session_initiate_rate_limit::DEFAULT_MAX_MUJI_ACTIONS
        {
            state
                .deps
                .protocol
                .muji_pre_dispatch_action_rate_limit
                .check_and_record(&sender.to_bare())
                .expect("fill control-plane read budget");
        }
    }
    let iq = initiate_iq(&room, &sender);
    let frames = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        execute_adapter(adapter, state, iq, sender, generation),
    )
    .await
    .expect("rejection must not enter paused provider preparation")
    .expect("IQ handled");
    assert_eq!(
        sfu.preparation_calls.load(Ordering::SeqCst),
        0,
        "membership/read admission must reject before any provider preparation"
    );
    let condition = if exhaust_read_budget {
        "policy-violation"
    } else {
        "forbidden"
    };
    assert!(
        frames.iter().any(|frame| frame.contains(condition)),
        "{frames:?}"
    );
    assert!(!frames.iter().any(|frame| frame.contains("<token")));
}

#[tokio::test]
async fn local_muji_nonmember_cannot_start_provider_preparation() {
    assert_admission_denies_before_preparation(Adapter::Local, false).await;
}

#[tokio::test]
async fn local_muji_exhausted_read_budget_cannot_start_provider_preparation() {
    assert_admission_denies_before_preparation(Adapter::Local, true).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_nonmember_cannot_start_provider_preparation() {
    assert_admission_denies_before_preparation(Adapter::Relayed, false).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_exhausted_read_budget_cannot_start_provider_preparation() {
    assert_admission_denies_before_preparation(Adapter::Relayed, true).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_preparation_cannot_freeze_voice_grants_before_async_work() {
    assert_voice_is_read_after_preparation(Adapter::Relayed).await;
}

struct PublicationFixture {
    sfu: Arc<PausedPreparationSfu>,
    state: Arc<WebSocketState>,
    database: Database,
    outbox: Arc<CallTeardownOutboxStore>,
    room: BareJid,
    sender: FullJid,
    generation: OccupancySessionGeneration,
    displaced: OccupancySessionGeneration,
}

impl PublicationFixture {
    async fn new() -> Self {
        let sfu = Arc::new(PausedPreparationSfu::new());
        let mut state = create_test_websocket_state_with_sfu(sfu.clone() as Arc<_>).await;
        // A separate one-slot pool gives the test an actual blocked SQL
        // boundary without blocking room actor reads or authorization writes.
        let database = Database::from_config(
            "muji-retirement-reply",
            &DatabaseConfig {
                pool_size: 1,
                ..DatabaseConfig::default()
            },
        )
        .await
        .expect("dedicated outbox database");
        let outbox = Arc::new(
            CallTeardownOutboxStore::try_new(database.clone())
                .await
                .expect("outbox"),
        );
        Arc::get_mut(&mut state)
            .expect("state not yet shared")
            .deps
            .protocol
            .call_teardown_outbox = outbox.clone();
        let room: BareJid = "publication-retirement@muc.example.com"
            .parse()
            .expect("room");
        let sender: FullJid = "alice@example.com/web".parse().expect("JID");
        let generation = OccupancySessionGeneration::mint();
        let displaced = OccupancySessionGeneration::mint();
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
        actor
            .ask(Join {
                session: generation,
                nick: "alice".into(),
                real_jid: sender.clone(),
                role: Role::Visitor,
                affiliation: Affiliation::Member,
            })
            .await
            .expect("join");
        let call = CallId::try_new(room.to_string()).expect("call");
        let identity = Identity::from_jid(sender.clone());
        sfu.inner
            .issue_join_token_with_session(
                &call,
                &identity,
                MediaCapabilities::direct_call_peer(),
                &SessionBinding::try_new("displaced-session").expect("session"),
                displaced,
                sfu.inner
                    .participant_registration_expectation(&call, &identity)
                    .expect("old expectation"),
            )
            .expect("old mint")
            .expect("old publication");
        Self {
            sfu,
            state,
            database,
            outbox,
            room,
            sender,
            generation,
            displaced,
        }
    }
}

#[derive(Clone, Copy)]
enum PublicationOutcome {
    Commit,
    ClosedDatabase,
    VoiceRevoked,
}

async fn assert_retirement_is_durable_before_reply(adapter: Adapter, outcome: PublicationOutcome) {
    let fixture = PublicationFixture::new().await;
    let transaction = if matches!(outcome, PublicationOutcome::ClosedDatabase) {
        fixture
            .database
            .sqlite_pool()
            .expect("SQLite pool")
            .close()
            .await;
        None
    } else {
        Some(
            fixture
                .database
                .begin_immediate()
                .await
                .expect("hold outbox SQL slot"),
        )
    };
    let iq = initiate_iq(&fixture.room, &fixture.sender);
    let mut execution = tokio::spawn(execute_adapter(
        adapter,
        fixture.state.clone(),
        iq,
        fixture.sender.clone(),
        fixture.generation,
    ));
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.sfu.entered.notified(),
    )
    .await
    .expect("preparation reached");
    fixture.sfu.released.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        fixture.sfu.pending_observed.notified(),
    )
    .await
    .expect("publication produced pending displaced-media responsibility");
    let minted = {
        let published = fixture.sfu.published.lock().expect("published tokens");
        assert_eq!(published.len(), 1);
        published[0].clone()
    };
    if transaction.is_some() {
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(75), &mut execution)
                .await
                .is_err(),
            "no session-accept may escape while retirement SQL cannot commit"
        );
        assert!(!fixture.sfu.is_revoked(&minted.jti));
    }
    if matches!(outcome, PublicationOutcome::VoiceRevoked) {
        let actor = crate::server::routes::websocket::get_room_actor(&fixture.state, &fixture.room)
            .await
            .expect("room actor");
        let config = actor.ask(GetConfig).await.expect("config");
        actor
            .ask(UpdateConfig {
                config: waddle_xmpp::muc::RoomConfig {
                    moderated: true,
                    ..config
                },
                effect_plan: ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("revoke voice while SQL blocked");
        assert_eq!(
            actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot")
                .room
                .session_generation(&fixture.sender),
            Some(fixture.generation)
        );
        assert_eq!(
            actor
                .ask(GetOccupantVoice {
                    jid: fixture.sender.clone()
                })
                .await
                .expect("new voice"),
            Some(Voice::Muted)
        );
    }
    if let Some(transaction) = transaction {
        transaction.commit().await.expect("release SQL slot");
    }
    let frames = tokio::time::timeout(std::time::Duration::from_secs(5), execution)
        .await
        .expect("adapter finishes")
        .expect("adapter task")
        .expect("IQ handled");
    let call = CallId::try_new(fixture.room.to_string()).expect("call");
    let identity = Identity::from_jid(fixture.sender.clone());
    match outcome {
        PublicationOutcome::Commit => {
            assert!(
                frames
                    .iter()
                    .any(|frame| frame.contains("session-accept") && frame.contains("<token")),
                "{frames:?}"
            );
            assert!(!fixture.sfu.is_revoked(&minted.jti));
            assert_eq!(
                fixture.sfu.participant_occupant_session(&call, &identity),
                Some(fixture.generation)
            );
        }
        PublicationOutcome::ClosedDatabase | PublicationOutcome::VoiceRevoked => {
            assert!(
                !frames
                    .iter()
                    .any(|frame| frame.contains("session-accept") || frame.contains("<token")),
                "{frames:?}"
            );
            let error = if matches!(outcome, PublicationOutcome::ClosedDatabase) {
                "internal-server-error"
            } else {
                "forbidden"
            };
            assert!(
                frames.iter().any(|frame| frame.contains(error)),
                "{frames:?}"
            );
            assert!(
                fixture.sfu.is_revoked(&minted.jti),
                "the exact held publication must be rolled back"
            );
            assert_eq!(
                fixture.sfu.participant_occupant_session(&call, &identity),
                None
            );
        }
    }
    if matches!(outcome, PublicationOutcome::ClosedDatabase) {
        assert!(
            !fixture
                .sfu
                .inner
                .pending_media_retirements(&call, &identity)
                .is_empty(),
            "a failed SQL enqueue cannot acknowledge the old retirement obligation"
        );
    } else {
        let rows = fixture
            .outbox
            .claim_due(8)
            .await
            .expect("read committed retirement");
        assert!(rows.iter().any(|job| {
            job.intent.occupant == Some(fixture.displaced)
                && matches!(&job.intent.target, TeardownTarget::DisplacedParticipant { identity, .. } if identity == &fixture.sender)
        }), "retirement must already be committed when the reply is released: {rows:?}");
        assert!(fixture
            .sfu
            .inner
            .pending_media_retirements(&call, &identity)
            .is_empty());
    }
}

#[tokio::test]
async fn local_muji_reply_waits_for_retirement_sql_commit() {
    assert_retirement_is_durable_before_reply(Adapter::Local, PublicationOutcome::Commit).await;
}

#[tokio::test]
async fn local_muji_retirement_sql_failure_rolls_back_and_suppresses_credentials() {
    assert_retirement_is_durable_before_reply(Adapter::Local, PublicationOutcome::ClosedDatabase)
        .await;
}

#[tokio::test]
async fn local_muji_voice_revoked_during_retirement_sql_wait_invalidates_held_credentials() {
    assert_retirement_is_durable_before_reply(Adapter::Local, PublicationOutcome::VoiceRevoked)
        .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_reply_waits_for_retirement_sql_commit() {
    assert_retirement_is_durable_before_reply(Adapter::Relayed, PublicationOutcome::Commit).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_retirement_sql_failure_rolls_back_and_suppresses_credentials() {
    assert_retirement_is_durable_before_reply(Adapter::Relayed, PublicationOutcome::ClosedDatabase)
        .await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_voice_revoked_during_retirement_sql_wait_invalidates_held_credentials() {
    assert_retirement_is_durable_before_reply(Adapter::Relayed, PublicationOutcome::VoiceRevoked)
        .await;
}
