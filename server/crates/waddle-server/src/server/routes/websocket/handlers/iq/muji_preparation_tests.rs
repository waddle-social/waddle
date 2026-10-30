use std::sync::Arc;

use jid::{BareJid, FullJid};
use tokio::sync::Notify;
use waddle_sfu::{
    ApiKey, ApiSecret, CallId, Identity, JoinToken, Jti, LiveKitSfu, MediaCapabilities,
    ObservedCallSids, ParticipantPreparationFuture, ParticipantRegistrationExpectation,
    SessionBinding, SessionScopedTeardown, SfuConfig, SfuError, SfuService,
    SidObservationDirection, SidObservationDisposition, TeardownDisposition, TurnCredential,
    TurnHost, TurnSharedSecret, WebsocketUrl,
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
use crate::server::routes::websocket::tests::create_test_websocket_state_with_sfu;

const SIGNING_SECRET: &str = "preparation-test-secret-at-least-32-bytes";

/// Only the asynchronous provider preparation boundary is controlled. Token
/// construction, publication CAS and registration are the real SFU behavior.
struct PausedPreparationSfu {
    inner: LiveKitSfu,
    entered: Notify,
    released: Notify,
}

impl PausedPreparationSfu {
    fn new() -> Self {
        Self {
            inner: LiveKitSfu::try_new(SfuConfig {
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
            })
            .expect("SFU"),
            entered: Notify::new(),
            released: Notify::new(),
        }
    }
}

impl SfuService for PausedPreparationSfu {
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
        self.inner
            .issue_join_token_with_session(call, identity, caps, session, occupant, expected)
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

enum Adapter {
    Local,
    #[cfg(feature = "clustering")]
    Relayed,
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
    let task_state = state.clone();
    let task_sender = sender.clone();
    let execution = tokio::spawn(async move {
        match adapter {
            #[cfg(feature = "clustering")]
            Adapter::Relayed => {
                super::jingle_muji_relay::handle_relayed_muji_initiate(&task_state, &iq, generation)
                    .await
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
                        response_to: Some(task_sender.as_str()),
                    },
                    &task_state,
                    None,
                    &ConnectionPhase::ready(task_sender.clone(), false),
                    &mut conn,
                )
                .await
            }
        }
    });
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

#[cfg(feature = "clustering")]
#[tokio::test]
async fn relayed_muji_preparation_cannot_freeze_voice_grants_before_async_work() {
    assert_voice_is_read_after_preparation(Adapter::Relayed).await;
}
