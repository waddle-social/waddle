use super::super::{
    cleanup::{cleanup_connection_shutdown, cleanup_muc_presence_for_jid},
    frame::{handle_xmpp_frame, settle_inbound_dispatch},
    frame_backstop::InboundDisposition,
    handlers::{self, presence::handle_muc_join},
    interpret_loop::build_interpret_deps,
    replay::drive_interpret_loop,
    state::WsConnState,
    stream_management::is_countable_stanza,
    transport_xml::{build_stream_features_xml, element_to_xml, sasl_success_xml, stanza_to_xml},
};
use super::{
    create_test_server_owner_session, create_test_session, create_test_websocket_state,
    create_test_websocket_state_with_sm_registry, message_frame_xml_with_id,
    register_test_native_user, scram_client_final_from_challenge, snapshot_room,
};
use crate::auth::Session;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use jid::{BareJid, FullJid};
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use waddle_xmpp::{
    protocol::{Blocklist, ConnectionPhase, InboundEvent},
    registry::{DeliveryKind, OutboundStanza},
    stream_management::{SmSessionRegistry, SM_NS},
    Stanza,
};
use xmpp_parsers::minidom::Element;

struct HangingEnsureClaimStore {
    inner: waddle_xmpp::ownership::InProcessClaimStore,
}

fn register_sm_publish_owner(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) {
    let (tx, _rx) = mpsc::channel(1);
    conn.registry_owner = Some(
        state
            .deps
            .protocol
            .connection_registry
            .register(jid.clone(), tx),
    );
}

#[async_trait::async_trait]
impl waddle_xmpp::ownership::ClaimStore for HangingEnsureClaimStore {
    async fn ensure_schema(&self) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.ensure_schema().await
    }

    async fn acquire(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner.acquire(entity, me).await
    }

    async fn ensure_claimed(
        &self,
        _entity: &waddle_xmpp::ownership::Entity,
        _me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        std::future::pending().await
    }

    async fn steal_stale(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        staleness: waddle_xmpp::ownership::StalePredicate,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_stale(entity, observed, staleness, me)
            .await
    }

    async fn steal_for_resume(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        observed: waddle_xmpp::ownership::ClaimEpoch,
        witness: waddle_xmpp::ownership::ResumeIdentityProof,
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<waddle_xmpp::ownership::ClaimEpoch, waddle_xmpp::ownership::ClaimError> {
        self.inner
            .steal_for_resume(entity, observed, witness, me)
            .await
    }

    async fn current_claim(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
    ) -> Result<Option<waddle_xmpp::ownership::ClaimSnapshot>, waddle_xmpp::ownership::ClaimError>
    {
        self.inner.current_claim(entity).await
    }

    async fn fence(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<bool, waddle_xmpp::ownership::ClaimError> {
        self.inner.fence(entity, me, mine).await
    }

    async fn release(
        &self,
        entity: &waddle_xmpp::ownership::Entity,
        me: &waddle_xmpp::ownership::NodeIdentity,
        mine: waddle_xmpp::ownership::ClaimEpoch,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release(entity, me, mine).await
    }

    async fn release_many(
        &self,
        entities: &[waddle_xmpp::ownership::Entity],
        me: &waddle_xmpp::ownership::NodeIdentity,
    ) -> Result<(), waddle_xmpp::ownership::ClaimError> {
        self.inner.release_many(entities, me).await
    }
}

fn resume_frame_xml(stream_id: &str, handled_count: u32) -> String {
    element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(minidom::rxml::xml_ncname!("previd").to_owned(), stream_id)
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                handled_count.to_string(),
            )
            .build(),
    )
}

// ---- XEP-0198 stream management --------------------------------

#[test]
fn timed_out_inbound_stanza_preserves_sender_responsibility() {
    let mut state = waddle_xmpp::stream_management::StreamManagementState::new();
    state.enable("timeout-regression".to_string(), true, Some(300));
    let mut completion = crate::server::routes::interpret::SmInboundCompletionTracker::default();
    let sequence = completion.reserve(&state);

    settle_inbound_dispatch(
        InboundDisposition::Unhandled,
        true,
        Some(sequence),
        &mut completion,
        &mut state,
    );

    assert_eq!(state.get_inbound_count(), 0);
    assert!(!completion.has_pending());
    assert!(completion.has_unhandled_hole());

    // A late ordered-relay completion cannot turn the cancelled dispatch into
    // an acknowledgement: the sender must retain and replay this stanza.
    completion.complete(sequence, &mut state);
    assert_eq!(state.get_inbound_count(), 0);
}

#[tokio::test]
async fn timed_out_inbound_stanza_detaches_and_resumes_before_the_hole() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("timeout-detach".to_string(), true, Some(300));

    let handled = conn.sm_inbound_completion.reserve(&conn.sm_state);
    settle_inbound_dispatch(
        InboundDisposition::Handled,
        false,
        Some(handled),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );
    let timed_out = conn.sm_inbound_completion.reserve(&conn.sm_state);
    settle_inbound_dispatch(
        InboundDisposition::Unhandled,
        false,
        Some(timed_out),
        &mut conn.sm_inbound_completion,
        &mut conn.sm_state,
    );

    assert_eq!(conn.sm_state.get_inbound_count(), 1);
    assert!(conn.sm_inbound_completion.has_unhandled_hole());
    assert!(
        !conn.phase.is_closing(),
        "timeout must use resumable transport termination, not a clean stream close"
    );

    let outcome = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;
    assert_eq!(
        outcome,
        super::super::cleanup::ConnectionShutdownOutcome::Detached
    );
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .peek_session("timeout-detach")
        .await
        .expect("registry lookup")
        .expect("resumable snapshot");
    assert_eq!(detached.inbound_count, 1);

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("timeout-detach", 0),
        "example.com",
        state.as_ref(),
        &mut resumed,
    )
    .await;
    let resumed_frame = responses
        .iter()
        .map(|xml| Element::from_str(xml).expect("response xml"))
        .find(|element| element.name() == "resumed")
        .expect("resume succeeds");
    assert_eq!(
        resumed_frame.attr("h"),
        Some("1"),
        "the timed-out second stanza must remain outside the server acknowledgement"
    );
}

#[tokio::test]
async fn sm_features_advertise_sm_namespace() {
    // Stream features after successful auth must include <sm/>.
    let features = build_stream_features_xml(true);
    let el = Element::from_str(&features).expect("features xml");
    assert!(
        el.children()
            .any(|child| child.name() == "sm" && child.ns() == SM_NS),
        "post-auth features must advertise urn:xmpp:sm:3"
    );
}

#[test]
fn is_countable_stanza_matches_element_name_not_prefix() {
    // Real stanzas that must count toward SM handled/sent counters.
    assert!(is_countable_stanza(
        "<iq xmlns='jabber:client' type='get' id='1'/>"
    ));
    assert!(is_countable_stanza("<message xmlns='jabber:client'/>"));
    assert!(is_countable_stanza("<presence xmlns='jabber:client'/>"));
    assert!(is_countable_stanza(
        "<jc:message xmlns:jc='jabber:client'/>"
    ));
    assert!(is_countable_stanza(
        "<jc:presence xmlns:jc='jabber:client'/>"
    ));
    assert!(is_countable_stanza(
        "<jc:iq xmlns:jc='jabber:client' id='1'/>"
    ));
    // Leading whitespace is tolerated (matches the pre-existing
    // trim behaviour — frames are always serialized with a
    // namespace by minidom, so callers never produce bare `<iq/>`).
    assert!(is_countable_stanza("  <iq xmlns='jabber:client' id='1'/>"));

    // SM control nonzas and stream-level frames must NOT count.
    assert!(!is_countable_stanza("<r xmlns='urn:xmpp:sm:3'/>"));
    assert!(!is_countable_stanza("<a xmlns='urn:xmpp:sm:3' h='1'/>"));
    assert!(!is_countable_stanza(
        "<enable xmlns='urn:xmpp:sm:3' resume='1'/>"
    ));
    assert!(!is_countable_stanza(
        "<resumed xmlns='urn:xmpp:sm:3' previd='x' h='0'/>"
    ));

    // Substring prefix collisions that the old `starts_with`
    // implementation would have accepted. These are all non-standard
    // today but the element-name match is how we stay safe if any
    // future XEP introduces similarly-named nonzas.
    assert!(!is_countable_stanza("<messages xmlns='urn:example'/>"));
    assert!(!is_countable_stanza("<presences xmlns='urn:example'/>"));
    assert!(!is_countable_stanza("<iqsomething/>"));
    assert!(!is_countable_stanza(
        "<jc:messages xmlns:jc='urn:example'/>"
    ));

    // Malformed XML just doesn't count — no panic, no false positive.
    assert!(!is_countable_stanza("not-xml-at-all"));
    assert!(!is_countable_stanza(""));
}

#[tokio::test]
async fn handle_xmpp_frame_drops_oversized_sm_nonza_before_parse() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = element_to_xml(
        Element::builder("r", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("note").to_owned(),
                "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE),
            )
            .build(),
    );

    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;

    assert!(responses.is_empty());
}

#[tokio::test]
async fn sm_enable_requires_resource_binding() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    // Without resource_bound, enable must fail.
    let frame = "<enable xmlns='urn:xmpp:sm:3' resume='true'/>";
    let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(!conn.sm_state.enabled);
}

#[tokio::test]
async fn sm_enable_claim_timeout_returns_failure_without_enabling_state() {
    let registry = Arc::new(
        waddle_xmpp::stream_management::InMemorySmSessionRegistry::new().with_claim_store(
            Arc::new(HangingEnsureClaimStore {
                inner: waddle_xmpp::ownership::InProcessClaimStore::new(),
            }),
            waddle_xmpp::ownership::SharedNodeIdentity::new(
                waddle_xmpp::ownership::NodeIdentity::new("sm-node", "incarnation"),
            ),
        ),
    );
    let state = create_test_websocket_state_with_sm_registry(registry).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready("alice@example.com/web".parse().expect("bound jid"), false);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1);
    let failed = Element::from_str(&responses[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(!conn.sm_state.enabled);
    assert!(conn.sm_state.stream_id.is_none());
}

#[tokio::test]
async fn sm_resume_requires_authentication() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
}

#[tokio::test]
async fn sm_resume_is_rejected_during_scram_and_scram_can_still_complete() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let password = "correct horse battery staple";
    let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
    register_test_native_user(state.as_ref(), "alice", password).await;

    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(BASE64_STANDARD.encode(format!("n,,n=alice,r={client_nonce}")))
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
    let challenge_b64 = challenge.text();
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let resume_responses =
        handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(resume_responses.len(), 1);
    let failed = Element::from_str(&resume_responses[0]).expect("failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let response_frame = element_to_xml(
        Element::builder("response", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode(scram_client_final_from_challenge(
                "alice",
                password,
                client_nonce,
                &challenge_b64,
            )))
            .build(),
    );
    let response_responses =
        handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(response_responses.len(), 1);
    let success = Element::from_str(&response_responses[0]).expect("success xml");
    assert_eq!(success.name(), "success");
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(conn.phase.is_authenticated());
}

#[tokio::test]
async fn sm_resume_is_allowed_after_auth_before_bind() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.phase.is_resumed());
}

#[tokio::test]
async fn sm_resume_rejects_when_replay_window_has_gap() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let mut detached = DetachedSession {
        stream_id: "stream-gap".to_string(),
        user_id: format!("alice@{domain}"),
        jid: format!("alice@{domain}/web").parse().expect("jid"),
        inbound_count: 5,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
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
    for sequence in 1..=(waddle_xmpp::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE as u32 + 1)
    {
        detached.record_detached_outbound_at(
            sequence,
            message_frame_xml_with_id(format!("m{sequence}")),
            chrono::Utc::now(),
        );
    }
    assert_eq!(detached.replay_gap_through, Some(1));
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(detached.clone())
        .await
        .expect("store");

    let resume_frame = resume_frame_xml("stream-gap", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert_eq!(el.attr("h"), Some("5"));
    assert!(el
        .get_child("resource-constraint", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert!(!conn.phase.is_resumed());

    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-gap")
        .await
        .expect("take")
        .expect("detached session should remain for expiry/fallback handling");
    assert_eq!(stored.jid, detached.jid);
}

#[tokio::test]
async fn sm_resume_rejects_authenticated_identity_mismatch_and_preserves_session() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

    let detached = DetachedSession {
        stream_id: "stream-auth-mismatch".to_string(),
        user_id: format!("alice@{domain}"),
        jid: format!("alice@{domain}/web").parse().expect("jid"),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
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
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(detached.clone())
        .await
        .expect("store");

    let resume_frame = resume_frame_xml("stream-auth-mismatch", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("not-authorized", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
    assert_eq!(
        conn.phase.authenticated_bare_jid().map(ToString::to_string),
        Some(format!("bob@{domain}"))
    );

    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-auth-mismatch")
        .await
        .expect("take")
        .expect("detached session should remain");
    assert_eq!(stored.jid, detached.jid);
}

#[tokio::test]
async fn sm_resume_matching_authenticated_identity_preserves_current_session_without_sidecar() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));

    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-auth-match".to_string(),
            user_id: format!("bob@{domain}"),
            jid: detached_jid.clone(),
            inbound_count: 2,
            outbound_count: 3,
            last_acked: 3,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store");

    let resume_frame = resume_frame_xml("stream-auth-match", 3);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");
    assert_eq!(conn.phase.bound_jid(), Some(&detached_jid));
    assert!(conn.phase.is_ready());
    assert!(conn.phase.is_resumed());
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &detached_jid
    ));
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.user_jid.as_str()),
        Some(session.user_jid.as_str())
    );
}

#[tokio::test]
async fn sm_resume_matching_authenticated_identity_prefers_detached_sidecar_session() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let fresh_session = create_test_session(state.as_ref(), "bob").await;
    let payload =
        BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", fresh_session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let stream_id = "stream-auth-match-with-sidecar";
    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    let resumed_session = Session::new(&fresh_session.user_jid, "bob", "bob");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.to_string(),
            user_id: format!("bob@{domain}"),
            jid: detached_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
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
        })
        .await
        .expect("store");
    state
        .deps
        .protocol
        .resumable_sessions
        .insert(stream_id.to_string(), resumed_session.clone());

    let resume_frame = resume_frame_xml(stream_id, 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &detached_jid
    ));
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.id.as_str()),
        Some(resumed_session.id.as_str())
    );
    assert_ne!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.id.as_str()),
        Some(fresh_session.id.as_str())
    );
}

#[tokio::test]
async fn sm_resume_rejects_ready_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let resume_frame = resume_frame_xml("stream-xyz", 0);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    assert!(el
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
}

#[tokio::test]
async fn sm_enable_after_bind_returns_enabled_and_tracks_counters() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "enabled");
    assert_eq!(el.attr("resume"), Some("true"));
    assert!(el.attr("id").filter(|s| !s.is_empty()).is_some());
    assert!(
        !conn.sm_state.enabled,
        "SM must remain unpublished until the <enabled/> write succeeds"
    );
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(conn.sm_state.is_resumable());

    // An ack request bumps no counters but produces <a h=inbound_count/>.
    let ack_responses = handle_xmpp_frame(
        "<r xmlns='urn:xmpp:sm:3'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(ack_responses.len(), 1);
    let ack_el = Element::from_str(&ack_responses[0]).expect("xml");
    assert_eq!(ack_el.name(), "a");
    assert_eq!(ack_el.attr("h"), Some("0"));

    // A countable inbound stanza bumps the inbound counter.
    let _ = handle_xmpp_frame(
        "<presence xmlns='jabber:client'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(conn.sm_state.get_inbound_count(), 1);

    // Subsequent <r/> should now report h=1.
    let ack2 = handle_xmpp_frame(
        "<r xmlns='urn:xmpp:sm:3'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let ack2_el = Element::from_str(&ack2[0]).expect("xml");
    assert_eq!(ack2_el.attr("h"), Some("1"));
}

#[tokio::test]
async fn pipelined_sm_enable_cannot_replace_the_unpublished_commit() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/pipelined-enable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let first = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let first_enabled = Element::from_str(&first[0]).expect("first enabled xml");
    let first_stream_id = first_enabled
        .attr("id")
        .expect("first stream id")
        .to_string();
    assert!(conn.pending_sm_enable_commit.is_some());
    assert!(!conn.sm_state.enabled);

    let second = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let failed = Element::from_str(&second[0]).expect("second failed xml");
    assert_eq!(failed.name(), "failed");
    assert!(failed
        .get_child("unexpected-request", "urn:ietf:params:xml:ns:xmpp-stanzas")
        .is_some());

    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert_eq!(
        conn.sm_state.stream_id.as_deref(),
        Some(first_stream_id.as_str())
    );
}

#[tokio::test]
async fn resumable_enable_cancelled_before_write_never_publishes_and_releases_exact_claim() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/cancelled-enable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = enabled.attr("id").expect("stream id").to_string();
    assert!(!conn.sm_state.enabled);
    assert!(conn.pending_sm_enable_commit.is_some());

    drop(conn);

    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
    assert!(
        !registry
            .locally_owned_claim_ids()
            .expect("local ownership inventory")
            .contains(&stream_id),
        "an unpublished previd must not retain a resumable claim"
    );
}

#[tokio::test]
async fn replaced_connection_commits_written_enable_without_publishing_stale_alias() {
    let state = create_test_websocket_state().await;
    let mut old_conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/replaced-enable".parse().expect("jid");
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut old_conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let stream_id = waddle_xmpp::pending_delivery::SmSessionId::new(
        enabled.attr("id").expect("stream id").to_string(),
    );

    let (replacement_tx, _replacement_rx) = mpsc::channel(1);
    let _replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);

    old_conn.publish_pending_sm_enable(state.as_ref());

    assert!(
        old_conn.sm_state.enabled,
        "a successfully written <enabled/> commits local XEP-0198 state"
    );
    assert!(old_conn.pending_sm_enable_commit.is_none());
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .expect("replacement entry")
            .sm_stream_id()
            .is_none(),
        "stale publication must not stamp the replacement entry"
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .sm_stream_owner(&stream_id)
            .is_none(),
        "stale publication must not create a reverse-index alias"
    );
    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 0);

    let (_outbound_tx, mut outbound_rx) = mpsc::channel(1);
    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
}

#[tokio::test]
async fn replacement_after_enable_publication_terminalizes_only_the_old_stream_claim() {
    let state = create_test_websocket_state().await;
    let mut old_conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/replaced-after-enable"
        .parse()
        .expect("jid");
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut old_conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state.as_ref(),
        &mut old_conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    let old_stream_id = enabled.attr("id").expect("stream id").to_string();
    old_conn.publish_pending_sm_enable(state.as_ref());
    assert!(old_conn.sm_state.enabled);

    let (replacement_tx, _replacement_rx) = mpsc::channel(1);
    let replacement_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), replacement_tx);
    let replacement_stream =
        waddle_xmpp::pending_delivery::SmSessionId::new("replacement-owned-stream".to_string());
    assert!(state
        .deps
        .protocol
        .connection_registry
        .set_sm_stream_id_if_owner(&jid, &replacement_owner, Some(replacement_stream.clone()),));

    let (_outbound_tx, mut outbound_rx) = mpsc::channel(1);
    assert_eq!(
        cleanup_connection_shutdown(state.as_ref(), &mut outbound_rx, &mut old_conn, false).await,
        super::super::cleanup::ConnectionShutdownOutcome::NotPersisted
    );

    assert_eq!(
        state
            .deps
            .protocol
            .connection_registry
            .get_entry(&jid)
            .expect("replacement entry")
            .sm_stream_id(),
        Some(replacement_stream),
        "old-stream cleanup must not alter the replacement entry"
    );
    let registry = &state.deps.protocol.sm_session_registry;
    assert_eq!(registry.pending_claim_release_count(), 1);
    assert!(registry
        .locally_owned_claim_ids()
        .expect("ownership inventory")
        .contains(&old_stream_id));
    assert_eq!(registry.retry_pending_claim_releases(1).await, 1);
}

#[tokio::test]
async fn non_resumable_sm_enable_does_not_create_cluster_claim() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/non-resumable".parse().expect("jid");
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state.as_ref(), &mut conn, &jid);

    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='false'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(responses.len(), 1);
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    assert_eq!(enabled.attr("resume"), None);
    let stream_id = enabled.attr("id").expect("SM id");
    conn.publish_pending_sm_enable(state.as_ref());
    assert!(conn.sm_state.enabled);
    assert!(!conn.sm_state.is_resumable());
    assert!(
        !state
            .deps
            .protocol
            .sm_session_registry
            .locally_owned_claim_ids()
            .expect("local claim snapshot")
            .contains(&stream_id.to_string()),
        "non-resumable SM must not retain a clustered ownership claim"
    );
}

/// Enable SM on a fresh ready connection and return the negotiated
/// stream id. Shared setup for the live `<a h='N'/>` validation tests
/// (issue #1099).
async fn enable_sm_for_live_ack_tests(
    state: &super::super::state::WebSocketState,
    conn: &mut WsConnState,
    jid: &FullJid,
) -> String {
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    register_sm_publish_owner(state, conn, jid);
    let responses = handle_xmpp_frame(
        "<enable xmlns='urn:xmpp:sm:3' resume='true'/>",
        "example.com",
        state,
        conn,
    )
    .await;
    let enabled = Element::from_str(&responses[0]).expect("enabled xml");
    assert_eq!(enabled.name(), "enabled");
    conn.publish_pending_sm_enable(state);
    enabled.attr("id").expect("stream id").to_string()
}

/// Seed a pending_delivery row claimed by `stream_id` whose flush
/// stanza was recorded at `outbound_sequence`, mirroring the Q7b
/// SM-ack lifecycle rows that `<a h='N'/>` range-deletes.
async fn seed_claimed_pending_row(
    state: &super::super::state::WebSocketState,
    recipient: &BareJid,
    stream_id: &str,
    outbound_sequence: u32,
) {
    state
        .deps
        .protocol
        .pending_delivery_storage
        .insert(waddle_xmpp::pending_delivery::PendingRow {
            id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: waddle_xmpp::pending_delivery::PendingPayload::Transient(Box::new({
                let mut m =
                    xmpp_parsers::message::Message::new(Some(jid::Jid::from(recipient.clone())));
                m.id = Some(xmpp_parsers::message::Id("pd-1".to_string()));
                m
            })),
            flushed_in_session: Some(waddle_xmpp::pending_delivery::SmSessionId::new(
                stream_id.to_string(),
            )),
            outbound_sequence: Some(outbound_sequence),
        })
        .await
        .expect("seed claimed pending_delivery row");
}

#[tokio::test]
async fn sm_live_ack_with_impossible_handled_count_closes_stream_without_purging() {
    // Issue #1099 / XEP-0198 §4: "If the value of 'h' is greater than
    // the number of stanzas sent by the server... it is RECOMMENDED
    // to close the stream with an undefined-condition stream error"
    // carrying <handled-count-too-high/>. The live `<a h='N'/>` path
    // previously acknowledged unconditionally, silently destroying
    // the replay queue and the claimed pending_delivery rows.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    // Two outbound stanzas recorded → send-count is 2.
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    // Client claims it handled 5 stanzas; we only sent 2.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='5'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(
        responses.len(),
        2,
        "bogus live ack must yield stream error + close: {responses:?}"
    );
    let stream_error = Element::from_str(&responses[0]).expect("stream error xml");
    assert_eq!(stream_error.name(), "error");
    assert_eq!(stream_error.ns(), waddle_xmpp::ns::STREAM);
    assert!(
        responses[0].contains("undefined-condition")
            && responses[0].contains("handled-count-too-high")
            && (responses[0].contains("h='5'") || responses[0].contains("h=\"5\""))
            && (responses[0].contains("send-count='2'")
                || responses[0].contains("send-count=\"2\"")),
        "expected handled-count-too-high stream error: {responses:?}"
    );
    let close = Element::from_str(&responses[1]).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    assert!(
        conn.phase.is_closing(),
        "connection must be Closing after handled-count-too-high"
    );

    // Nothing was purged: both stanzas remain replayable and the
    // claimed pending_delivery row survives.
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        2,
        "bogus ack must not purge the replay queue"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert_eq!(rows.len(), 1, "bogus ack must not delete pending rows");
}

#[tokio::test]
async fn sm_live_ack_with_valid_handled_count_purges_queue_and_rows() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='1'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "valid ack yields no response frames");
    assert!(!conn.phase.is_closing(), "valid ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        1,
        "acked prefix must be purged, unacked tail retained"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert!(
        rows.is_empty(),
        "acked pending_delivery rows must be range-deleted"
    );
}

#[tokio::test]
async fn sm_live_ack_at_exact_outbound_count_is_accepted() {
    // Boundary: h == send-count is a full ack, not a violation
    // (XEP-0198 §4 only forbids h GREATER than the sent count).
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());

    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='2'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "h == send-count is a valid full ack");
    assert!(!conn.phase.is_closing());
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        0,
        "full ack empties the replay queue"
    );
}

#[tokio::test]
async fn sm_live_ack_is_wrap_aware_past_u32_max() {
    // XEP-0198 §4: counters wrap at 2^32 ("in the unlikely case that
    // the number of stanzas handled ... exceeds 2^32"). "Greater than"
    // must therefore be judged mod 2^32: with outbound_count wrapped
    // to 2, a client ack of h = 4294967295 (u32::MAX, i.e. 3 stanzas
    // behind the wrap) is VALID, not "too high".
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    // Restore counters as a wrapped session would carry them: the
    // server has sent 2^32 + 2 stanzas, the client acked u32::MAX - 1.
    let detached = waddle_xmpp::stream_management::DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 0,
        outbound_count: 2,
        last_acked: u32::MAX - 1,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: u32::MAX,
                stanza_xml: "<message xmlns='jabber:client' id='pre-wrap'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message xmlns='jabber:client' id='post-wrap-1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 2,
                stanza_xml: "<message xmlns='jabber:client' id='post-wrap-2'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
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
    conn.sm_state.restore_from_session(&detached);

    // h = u32::MAX acks the pre-wrap stanza. A naive `h > outbound`
    // comparison would misread this as handled-count-too-high.
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(u32::MAX).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "wrapped ack behind the counter is valid: {responses:?}"
    );
    assert!(!conn.phase.is_closing(), "wrapped valid ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(u32::MAX).len(),
        2,
        "pre-wrap stanza purged; post-wrap stanzas retained"
    );
}

#[tokio::test]
async fn sm_live_ack_at_half_window_distance_is_ignored_not_acknowledged() {
    // XEP-0198 §4 exact-window corner: h == outbound_count +
    // 0x8000_0000 sits at exactly mod-2^32 distance 2^31 from the
    // confirmed window — the one point where "ahead" and "behind" are
    // indistinguishable. Whatever it is classified as, it MUST NOT be
    // acknowledged (that would poison last_acked and purge the replay
    // queue). The regress guard runs first and its half-space
    // comparison is true at exactly 2^31, so the live path ignores it
    // inert, exactly like any other stale mod-behind h.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());
    // Full valid ack first: last_acked == outbound_count == 2.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='2'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(responses.is_empty(), "full ack is valid");

    let bogus_h = 2u32.wrapping_add(0x8000_0000);
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(bogus_h).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "half-window h must be ignored inert: {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "ignored half-window h must not close the stream"
    );
    // MUST NOT have acknowledged: a later in-window ack still works
    // against uncorrupted state.
    assert_eq!(
        conn.sm_state.unacked_count(),
        0,
        "last_acked must not be poisoned by the ignored h"
    );
}

#[tokio::test]
async fn sm_live_ack_in_regressed_half_space_is_ignored_without_purge() {
    // h == outbound_count + 0x8000_0001 lands mod-2^32 BEHIND
    // last_acked: the regress guard must ignore it wholesale (before
    // the exact-window too-high check reclassifies it), leaving the
    // replay queue and last_acked untouched.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let _stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());
    // Partial ack: last_acked = 1, one stanza still unacked.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='1'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(responses.is_empty(), "partial ack is valid");

    let stale_h = 2u32.wrapping_add(0x8000_0001);
    let responses = handle_xmpp_frame(
        &waddle_xmpp::stream_management::SmAck::new(stale_h).to_xml(),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "regressed-half-space h must be ignored inert: {responses:?}"
    );
    assert!(!conn.phase.is_closing(), "ignored ack must not close");
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(1).len(),
        1,
        "ignored ack must not purge the replay queue"
    );
}

#[tokio::test]
async fn sm_resume_at_half_window_distance_is_rejected_as_handled_count_too_high() {
    // Resume-path twin of the live half-window corner: a detached
    // session with outbound_count == last_acked == 2 must reject
    // h == 2 + 0x8000_0000 as handled-count-too-high instead of
    // resuming and poisoning the restored counters.
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-half-window".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 2,
            last_acked: 2,
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let bogus_h = 2u32.wrapping_add(0x8000_0000);
    let resume_frame = resume_frame_xml("stream-half-window", bogus_h);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(
        !responses.iter().any(|frame| frame.contains("<resumed")),
        "half-window h must not resume: {responses:?}"
    );
    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("handled-count-too-high")),
        "expected handled-count-too-high stream error: {responses:?}"
    );
    assert!(
        conn.phase.is_closing(),
        "connection must be Closing after handled-count-too-high"
    );
}

#[tokio::test]
async fn sm_resume_with_regressed_h_fails_resume_instead_of_stream_error() {
    // A resume h mod-2^32 BEHIND the detached last_acked is a failed
    // resume (<failed/> resource-constraint via can_resume_from), NOT
    // a handled-count-too-high stream error — matching the live path
    // where the regress guard runs before the exact-window check.
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-regressed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 2,
            last_acked: 2,
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let resume_frame = resume_frame_xml("stream-regressed", 1);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("<failed") && frame.contains("resource-constraint")),
        "regressed h must produce a failed resume: {responses:?}"
    );
    assert!(
        !responses
            .iter()
            .any(|frame| frame.contains("handled-count-too-high")),
        "regressed h must not be reclassified as too-high: {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "failed resume keeps the stream open for a fresh session"
    );
}

#[tokio::test]
async fn sm_resume_restores_session_and_replays_unacked() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    // Seed a detached session directly in the registry — this is the
    // shape left behind by a prior WebSocket task after detach-on-close.
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-xyz".to_string();
    let detached = DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 7,
        outbound_count: 10,
        last_acked: 8,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 9,
                stanza_xml: "<message id='m9'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 10,
                stanza_xml: "<message id='m10'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: true,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    };
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(detached)
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // Client reports it has acked through 9, so only m10 needs replay.
    let frame = resume_frame_xml(&stream_id, 9);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    // Expect <resumed/> first, then exactly the one unacked stanza.
    assert!(!responses.is_empty());
    let resumed = Element::from_str(&responses[0]).expect("resumed xml");
    assert_eq!(resumed.name(), "resumed");
    assert_eq!(resumed.attr("previd"), Some(stream_id.as_str()));

    let replay_count = responses.len() - 1;
    assert_eq!(
        replay_count, 1,
        "only m10 should be replayed: {responses:?}"
    );
    assert!(responses[1].contains("m10"));

    // Session identity restored without SASL or bind frames.
    assert!(conn.phase.is_authenticated());
    assert!(conn.phase.is_ready());
    assert_eq!(conn.phase.bound_jid(), Some(&jid));
    assert!(conn.phase.is_resumed());
    assert!(conn.carbons_enabled);
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: true,
            ..
        } if full_jid == &jid
    ));
}

#[tokio::test]
async fn sm_resume_rejects_impossible_client_handled_count() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-too-far".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 4,
            outbound_count: 2,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message id='m1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            }],
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let resume_frame = resume_frame_xml("stream-too-far", 3);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 2);
    let stream_error = Element::from_str(&responses[0]).expect("stream error xml");
    assert_eq!(stream_error.name(), "error");
    assert_eq!(stream_error.ns(), waddle_xmpp::ns::STREAM);
    assert!(
        !responses[0].contains("</stream:stream>"),
        "RFC 7395 WebSocket stream close must be a separate close frame"
    );
    assert!(
        responses[0].contains("stream:error")
            && responses[0].contains("undefined-condition")
            && responses[0].contains("handled-count-too-high")
            && (responses[0].contains("h='3'") || responses[0].contains("h=\"3\""))
            && (responses[0].contains("send-count='2'")
                || responses[0].contains("send-count=\"2\"")),
        "invalid resume count should be a handled-count-too-high stream error: {responses:?}"
    );
    let close = Element::from_str(&responses[1]).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
    assert!(
        !conn.sm_state.enabled,
        "rejected resume must not pollute the fresh stream SM state"
    );
    assert!(
        !conn.sm_state.is_resumable(),
        "rejected resume must not make the fresh stream resumable"
    );
    assert!(
        state
            .deps
            .protocol
            .sm_session_registry
            .take_session("stream-too-far")
            .await
            .expect("lookup")
            .is_some(),
        "rejected resume must release the detached session for a valid retry"
    );
}

#[tokio::test]
async fn sm_resume_replays_roster_push_recorded_while_detached() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-roster-replay".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store");

    let recorded = state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(
                &jid,
                &Stanza::Iq(Box::new(
                    xmpp_parsers::iq::Iq::try_from(
                        Element::from_str(
                            "<iq xmlns='jabber:client' type='set' id='detached-roster-push'><query xmlns='jabber:iq:roster'/></iq>",
                        )
                        .expect("iq element"),
                    )
                    .expect("iq stanza"),
                )),
                chrono::Utc::now(),
            )
            .await
            .expect("record detached roster push");
    assert!(recorded);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml(&stream_id, 0);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(
        responses.len(),
        2,
        "expected resumed plus replay: {responses:?}"
    );
    assert!(responses[0].contains("<resumed"));
    assert!(
        responses[1].contains("detached-roster-push"),
        "detached roster push should replay after resume: {responses:?}"
    );
    assert!(conn.roster_interested);
}

#[tokio::test]
async fn direct_full_jid_message_records_for_detached_resource_replay() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let stream_id = "stream-detached-direct-message".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
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
        })
        .await
        .expect("store detached alice");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&stream_id)
        .await
        .expect("take detached")
        .expect("detached session remains");
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("detached-dm-1")),
        "full-JID direct message should be recorded for detached replay: {detached:?}"
    );
}

#[tokio::test]
async fn bare_jid_message_records_for_detached_resource_replay() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let stream_id = "stream-detached-bare-message".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
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
        })
        .await
        .expect("store detached alice");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com" id="detached-bare-dm-1"><body>queued while detached</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&stream_id)
        .await
        .expect("take detached")
        .expect("detached session remains");
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("detached-bare-dm-1")),
        "bare-JID direct message should be recorded for detached replay: {detached:?}"
    );
    // RFC 6121 §8.5.2.1.1: bare-JID delivery routes the original
    // stanza to each available resource without rewriting `to`.
    // The dispatcher path preserves this — legacy `handle_message`
    // rewrote `to` to the per-resource full JID, which was a
    // server-side deviation from the RFC. Assert only the
    // reachability semantic here; integration tests verify the
    // wire shape end-to-end.
}

#[tokio::test]
async fn message_carbons_record_for_detached_enabled_resources() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;
    // #1246: an unregistered recipient would bounce with
    // <service-unavailable/>; this test is about carbons for the
    // sender's detached sibling, so give bob a real account.
    crate::server::routes::websocket::tests::seed_local_account(state.as_ref(), "bob").await;

    let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
    let alice_laptop: FullJid = "alice@example.com/laptop".parse().expect("alice laptop");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let sent_stream_id = "stream-detached-sent-carbon".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: sent_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice laptop");

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    alice.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        alice_phone.clone(),
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="bob@example.com/web" id="detached-sent-carbon-source"><body>copy me</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
    assert!(responses.is_empty());

    let sent_detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&sent_stream_id)
        .await
        .expect("take sent detached")
        .expect("sent detached session remains");
    assert!(
        sent_detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<sent")
                && entry.stanza_xml.contains("urn:xmpp:carbons:2")
                && entry.stanza_xml.contains("detached-sent-carbon-source")),
        "sent carbon should be recorded for detached opted-in resource: {sent_detached:?}"
    );

    let received_stream_id = "stream-detached-received-carbon".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: received_stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_laptop,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: true,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice laptop again");

    // Build alice/phone's per-connection state machine so we can
    // drive the recipient-pass carbon fan-out the dispatcher path
    // owns. In production this happens automatically via
    // alice/phone's main loop dispatching the queued
    // `DeliveryKind::PeerStanza`; the unit test reproduces the
    // same step explicitly.
    let mut alice_phone_conn = WsConnState::new();
    alice_phone_conn.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    alice_phone_conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        alice_phone.clone(),
        false,
        Blocklist::empty(),
    );
    let (alice_phone_tx, mut alice_phone_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Slice 2: delivery reads the actor tree, so register into both.
    super::register_test_connection(state.as_ref(), &alice_phone, alice_phone_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        bob_jid,
        false,
        Blocklist::empty(),
    );
    let responses = handle_xmpp_frame(
            r#"<message xmlns="jabber:client" type="chat" to="alice@example.com/phone" id="detached-received-carbon-source"><body>copy me too</body></message>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    assert!(responses.is_empty());

    // Pump the queued PeerStanza through alice/phone's SM so the
    // recipient pass runs and the dispatcher emits the
    // received-carbon fan-out. This is the same dispatch the
    // production main loop performs on `DeliveryKind::PeerStanza`.
    while let Ok(outbound) = alice_phone_rx.try_recv() {
        if !matches!(outbound.kind, DeliveryKind::PeerStanza) {
            continue;
        }
        let sm = alice_phone_conn.state_machine.as_mut().expect("alice SM");
        let events = sm.handle(InboundEvent::StanzaFromPeer(Box::new(outbound.stanza)));
        let deps = build_interpret_deps(state.as_ref(), None);
        let _ = drive_interpret_loop(events, sm, &deps).await;
    }

    let received_detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session(&received_stream_id)
        .await
        .expect("take received detached")
        .expect("received detached session remains");
    assert!(
        received_detached.unacked_stanzas.iter().any(|entry| entry
            .stanza_xml
            .contains("<received")
            && entry.stanza_xml.contains("urn:xmpp:carbons:2")
            && entry.stanza_xml.contains("detached-received-carbon-source")),
        "received carbon should be recorded for detached opted-in resource: {received_detached:?}"
    );
}

#[tokio::test]
async fn duplicate_subscribe_ack_reaches_non_roster_interested_resource() {
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    let (alice_tx, mut alice_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the subscription-ack path enumerates the
    // requester's (bob's) resources through the actor-authoritative registry,
    // so bob must be dual-registered exactly as production bind does. Alice is
    // reached via the available/roster-interested paths (unchanged), so a bare
    // DashMap register still suffices for her.
    let bob_owner = super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;
    let alice_owner = state
        .deps
        .protocol
        .connection_registry
        .register(alice_jid.clone(), alice_tx);

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    // #1208: presence registry writes are owner-gated; the fixture
    // registers out-of-band, so carry the owner token like real
    // registration does.
    alice.registry_owner = Some(alice_owner);
    let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="alice-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while alice_rx.try_recv().is_ok() {}

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    bob.registry_owner = Some(bob_owner);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let _ = tokio::time::timeout(std::time::Duration::from_millis(250), alice_rx.recv())
        .await
        .expect("alice receives initial subscribe")
        .expect("subscribe stanza");

    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let ack = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("duplicate subscribe ack")
        .expect("ack stanza");
    let frame = stanza_to_xml(&ack.stanza);
    assert!(
        frame.contains("from='alice@example.com'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("type='subscribed'"),
        "duplicate subscribe ack should reach a live resource even before roster get: {frame}"
    );
}

#[tokio::test]
async fn roster_set_records_push_for_detached_interested_resource() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let detached_jid: FullJid = "alice@example.com/web".parse().expect("detached jid");
    let source_jid: FullJid = "alice@example.com/phone".parse().expect("source jid");
    let stream_id = "stream-roster-fanout".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: detached_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached session");

    let mut source = WsConnState::new();
    source.phase = ConnectionPhase::ready(source_jid, false);
    let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="set" id="roster-detached-fanout"><query xmlns="jabber:iq:roster"><item jid="bob@example.com" name="Bob"/></query></iq>"#,
            "example.com",
            state.as_ref(),
            &mut source,
        )
        .await;
    assert!(
        responses.iter().any(
            |frame| frame.contains("roster-detached-fanout") && frame.contains("type='result'")
        ),
        "roster set should succeed: {responses:?}"
    );

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&detached_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay
            .iter()
            .any(|frame| frame.contains("jabber:iq:roster") && frame.contains("bob@example.com")),
        "detached interested resource should replay roster fanout push: {replay:?}"
    );
    assert!(resumed.roster_interested);
}

#[tokio::test]
async fn blocking_set_records_push_for_detached_blocklist_interested_resource() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let detached_jid: FullJid = "alice@example.com/web".parse().expect("detached jid");
    let source_jid: FullJid = "alice@example.com/phone".parse().expect("source jid");
    let stream_id = "stream-blocking-fanout".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: detached_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: true,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached session");

    let mut source = WsConnState::new();
    source.phase = ConnectionPhase::ready(source_jid, false);
    let responses = handle_xmpp_frame(
        r#"<iq xmlns="jabber:client" type="set" id="blocking-detached-fanout"><block xmlns="urn:xmpp:blocking"><item jid="bob@example.com"/></block></iq>"#,
        "example.com",
        state.as_ref(),
        &mut source,
    )
    .await;
    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("blocking-detached-fanout")
                && frame.contains("type='result'")),
        "block set should succeed: {responses:?}"
    );

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&detached_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay
            .iter()
            .any(|frame| frame.contains("urn:xmpp:blocking") && frame.contains("bob@example.com")),
        "detached blocklist-interested resource should replay blocking push: {replay:?}"
    );
    assert!(resumed.blocklist_interested);
}

#[tokio::test]
async fn subscription_approval_replays_current_presence_from_detached_available_resource() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_web_jid: FullJid = "alice@example.com/web".parse().expect("alice web jid");
    let alice_phone_jid: FullJid = "alice@example.com/phone".parse().expect("alice phone jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob_jid.clone(), bob_tx);
    state
        .deps
        .protocol
        .connection_registry
        .mark_roster_interested(&bob_jid);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&bob_jid, true, 0);

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    let stream_id = "stream-detached-current-presence".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id,
            user_id: "alice@example.com".to_string(),
            jid: alice_web_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Chat),
            presence_status: Some("ready from detach".to_string()),
            presence_priority: 7,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice web");

    let mut alice_phone = WsConnState::new();
    alice_phone.phase = ConnectionPhase::ready(alice_phone_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice_phone,
    )
    .await;

    let mut delivered = Vec::new();
    for _ in 0..4 {
        if let Ok(Some(outbound)) =
            tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv()).await
        {
            delivered.push(stanza_to_xml(&outbound.stanza));
        }
    }
    assert!(
        delivered.iter().any(|frame| {
            frame.contains("from='alice@example.com/web'")
                && frame.contains("<show>chat</show>")
                && frame.contains("<status>ready from detach</status>")
                && frame.contains("<priority>7</priority>")
        }),
        "approval should deliver current rich presence from detached available resource: {delivered:?}"
    );
}

#[tokio::test]
async fn presence_probe_returns_detached_available_resource_presence() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the probe path enumerates the requester's
    // (bob's) resources through the actor-authoritative registry, so bob must
    // be dual-registered exactly as production bind does.
    super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-detached-probe".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: Some("stepped away".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    assert!(responses.is_empty());

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("<show>away</show>")
            && frame.contains("<status>stepped away</status>")
            && frame.contains("<priority>5</priority>"),
        "probe should return rich presence from detached available resource: {frame}"
    );
}

#[tokio::test]
async fn full_jid_presence_probe_returns_only_that_resources_availability() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_phone: FullJid = "alice@example.com/phone".parse().expect("alice phone");
    let alice_tablet: FullJid = "alice@example.com/tablet".parse().expect("alice tablet");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the probe path enumerates the requester's
    // (bob's) resources through the actor-authoritative registry, so bob must
    // be dual-registered exactly as production bind does.
    super::register_test_connection(state.as_ref(), &bob_jid, bob_tx).await;

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_phone.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}

    for (stream_id, jid, show, status) in [
        (
            "stream-probe-phone",
            alice_phone.clone(),
            xmpp_parsers::presence::Show::Away,
            "phone detail",
        ),
        (
            "stream-probe-tablet",
            alice_tablet,
            xmpp_parsers::presence::Show::Chat,
            "tablet detail",
        ),
    ] {
        state
            .deps
            .protocol
            .sm_session_registry
            .store_session(DetachedSession {
                stream_id: stream_id.to_string(),
                user_id: "alice@example.com".to_string(),
                jid,
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: std::time::Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: true,
                presence_show: Some(show),
                presence_status: Some(status.to_string()),
                presence_priority: 5,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            })
            .await
            .expect("store detached alice resource");
    }

    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com/phone"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    assert!(responses.is_empty());

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("full-jid probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("<show>away</show>")
            && frame.contains("<status>phone detail</status>")
            && frame.contains("<priority>5</priority>")
            && !frame.contains("alice@example.com/tablet"),
        "full-JID probe should return rich presence only for the requested resource: {frame}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), bob_rx.recv())
            .await
            .is_err(),
        "full-JID probe must not return sibling resources"
    );
}

#[tokio::test]
async fn presence_probe_without_subscription_does_not_reveal_detached_presence() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let mallory_jid: FullJid = "mallory@example.com/web".parse().expect("mallory jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let (mallory_tx, mut mallory_rx) = mpsc::channel::<OutboundStanza>(16);
    // ADR-0017 Phase 3 Slice 9: the (unsubscribed) probe path enumerates the
    // requester's (mallory's) resources through the actor-authoritative
    // registry to deliver the `unsubscribed` signal, so mallory must be
    // dual-registered exactly as production bind does. The privacy guarantee
    // (no detached presence leaked to an unauthorized prober) is unchanged.
    super::register_test_connection(state.as_ref(), &mallory_jid, mallory_tx).await;
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-detached-probe-denied".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid,
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: Some("private".to_string()),
            presence_priority: 5,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    let mut mallory = WsConnState::new();
    mallory.phase = ConnectionPhase::ready(mallory_jid, false);
    let responses = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="probe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut mallory,
    )
    .await;
    assert!(responses.is_empty());
    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), mallory_rx.recv())
        .await
        .expect("unsubscribed probe response")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com'")
            && frame.contains("to='mallory@example.com'")
            && frame.contains("type='unsubscribed'")
            && !frame.contains("alice@example.com/phone")
            && !frame.contains("private"),
        "unauthorized probe must return only an unsubscribed signal: {frame}"
    );
}

#[tokio::test]
async fn expired_detached_available_session_broadcasts_unavailable_to_subscribers() {
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/phone".parse().expect("alice jid");
    let alice_sibling_jid: FullJid = "alice@example.com/laptop".parse().expect("alice sibling");
    let (bob_tx, mut bob_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(bob_jid.clone(), bob_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&bob_jid, true, 0);
    let (alice_sibling_tx, mut alice_sibling_rx) = mpsc::channel::<OutboundStanza>(16);
    state
        .deps
        .protocol
        .connection_registry
        .register(alice_sibling_jid.clone(), alice_sibling_tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&alice_sibling_jid, true, 0);

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;
    while bob_rx.try_recv().is_ok() {}
    while alice_sibling_rx.try_recv().is_ok() {}

    handlers::presence::broadcast_unavailable_for_terminated_session(state.as_ref(), &alice_jid)
        .await;

    let outbound = tokio::time::timeout(std::time::Duration::from_millis(250), bob_rx.recv())
        .await
        .expect("unavailable broadcast")
        .expect("outbound stanza");
    let frame = stanza_to_xml(&outbound.stanza);
    assert!(
        frame.contains("from='alice@example.com/phone'")
            && frame.contains("to='bob@example.com'")
            && frame.contains("type='unavailable'"),
        "expired detached session should broadcast unavailable presence: {frame}"
    );
    let sibling_outbound = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        alice_sibling_rx.recv(),
    )
    .await
    .expect("sibling unavailable broadcast")
    .expect("outbound stanza");
    let sibling_frame = stanza_to_xml(&sibling_outbound.stanza);
    assert!(
        sibling_frame.contains("from='alice@example.com/phone'")
            && sibling_frame.contains("to='alice@example.com'")
            && sibling_frame.contains("type='unavailable'"),
        "expired detached session should notify sibling resources: {sibling_frame}"
    );
}

#[tokio::test]
async fn subscription_approval_records_roster_push_for_detached_interested_resource() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" type="get" id="bob-roster"><query xmlns="jabber:iq:roster"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut bob,
        )
        .await;
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;

    let stream_id = "stream-detached-subscription-roster-push".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "bob@example.com".to_string(),
            jid: bob_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: true,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached bob");

    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&bob_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("jabber:iq:roster")
                && frame.contains("alice@example.com")
                && frame.contains("subscription='to'")
        }),
        "detached interested resource should replay subscription roster push: {replay:?}"
    );
}

#[tokio::test]
async fn subscribe_to_detached_available_resource_replays_on_resume() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let stream_id = "stream-detached-subscribe-recipient".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: alice_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached alice");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid, false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&alice_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("type='subscribe'") && frame.contains("from='bob@example.com'")
        }),
        "detached available recipient should replay inbound subscribe: {replay:?}"
    );
}

#[tokio::test]
async fn presence_broadcast_to_detached_available_subscriber_replays_on_resume() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let mut bob = WsConnState::new();
    bob.phase = ConnectionPhase::ready(bob_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribe" to="alice@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut bob,
    )
    .await;
    let mut alice = WsConnState::new();
    alice.phase = ConnectionPhase::ready(alice_jid.clone(), false);
    let _ = handle_xmpp_frame(
        r#"<presence xmlns="jabber:client" type="subscribed" to="bob@example.com"/>"#,
        "example.com",
        state.as_ref(),
        &mut alice,
    )
    .await;

    let stream_id = "stream-detached-presence-broadcast".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "bob@example.com".to_string(),
            jid: bob_jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(300),
            detached_at: std::time::Instant::now(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store detached bob");

    let _ = handle_xmpp_frame(
            r#"<presence xmlns="jabber:client"><show>away</show><status>broadcast while detached</status><priority>5</priority></presence>"#,
            "example.com",
            state.as_ref(),
            &mut alice,
        )
        .await;

    let mut resumed = WsConnState::new();
    resumed.phase = ConnectionPhase::authenticated(&bob_jid);
    let resume_frame = resume_frame_xml(&stream_id, 0);
    let replay =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut resumed).await;
    assert!(
        replay.iter().any(|frame| {
            frame.contains("from='alice@example.com/web'")
                && frame.contains("<show>away</show>")
                && frame.contains("<status>broadcast while detached</status>")
                && frame.contains("<priority>5</priority>")
        }),
        "detached available subscriber should replay presence broadcast: {replay:?}"
    );
}

#[tokio::test]
async fn sm_resume_with_unknown_stream_id_fails() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml("does-not-exist", 0);
    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(responses.len(), 1);
    let el = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(el.name(), "failed");
    // Must NOT mark the session as bound/resumed.
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert!(!conn.phase.is_resumed());
}

#[tokio::test]
async fn sm_resume_signals_suppress_record_so_main_loop_skips_replay() {
    // Regression guard for the double-record bug reported in PR review:
    // `handle_sm_resume` must request suppression of outbound recording
    // for its own response batch. Replayed stanzas are already in the
    // unacked queue — re-recording them would bump `outbound_count` and
    // create duplicates.
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = "stream-dup-check".to_string();
    let detached = DetachedSession {
        stream_id: stream_id.clone(),
        user_id: "alice@example.com".to_string(),
        jid: jid.clone(),
        inbound_count: 0,
        outbound_count: 2,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: vec![
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 1,
                stanza_xml: "<message id='m1'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
            waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 2,
                stanza_xml: "<message id='m2'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            },
        ],
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
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(detached)
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let frame = resume_frame_xml(&stream_id, 0);
    let _ = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    // The resume handler must have raised the suppress flag so the main
    // loop skips re-recording its own response batch.
    assert!(
        conn.suppress_sm_record_next_batch,
        "handle_sm_resume must ask the main loop to skip SM recording for this batch"
    );
    // And the restored counters must still reflect what the client had
    // acknowledged, not the inflated post-re-record values (2, not 4).
    assert_eq!(conn.sm_state.outbound_count, 2);
    assert_eq!(conn.sm_state.queue_len(), 2);
}

#[tokio::test]
async fn cleanup_shutdown_detaches_resumable_session_on_transport_drop() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "detached-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);
    state
        .deps
        .protocol
        .connection_registry
        .update_presence_state(
            &jid,
            Some("away".to_string()),
            Some("stepped out".to_string()),
            3,
            Vec::new(),
        );

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.roster_interested = true;
    conn.sm_state
        .enable("stream-detach".to_string(), true, Some(300));
    state
        .deps
        .protocol
        .connection_registry
        .send_to(
            &jid,
            Stanza::Presence(xmpp_parsers::presence::Presence::new(
                xmpp_parsers::presence::Type::None,
            )),
        )
        .await;

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-detach")
        .await
        .expect("registry lookup");
    let detached = detached.expect("detached session");
    assert!(
        detached.roster_interested,
        "detached session must preserve roster-interest state"
    );
    assert!(
        detached.presence_available,
        "detached session must preserve available-presence state"
    );
    assert_eq!(
        detached.presence_show,
        Some(xmpp_parsers::presence::Show::Away)
    );
    assert_eq!(detached.presence_status.as_deref(), Some("stepped out"));
    assert_eq!(detached.presence_priority, 3);
    assert!(
        detached
            .unacked_stanzas
            .iter()
            .any(|entry| entry.stanza_xml.contains("<presence")),
        "cleanup must record queued-but-unwritten outbound stanzas before detaching"
    );
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_some());
}

/// ADR-0017 Phase 1 (Greptile review on PR #1177): an SM detach prunes the
/// resource's actor-tree entry as well as its DashMap entry. Without this, a
/// session that detaches and then expires without ever resuming would leak its
/// `UserActor` entry forever — the SM-expiry janitor cannot converge it because
/// the DashMap entry is already gone at detach, so its removal-gated mirror
/// never fires. Detached delivery is unaffected (it is sourced from the SM
/// session registry, not the actor), and a resume re-registers a fresh entry.
#[tokio::test]
async fn cleanup_shutdown_detach_prunes_actor_tree_entry() {
    use waddle_xmpp::registry::GetUser;
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    // Mirror into the actor tree exactly as the production bind path does,
    // sharing the same Arc-backed entry.
    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("entry just registered");
    assert!(
        crate::server::dual_registration::mirror_register(
            &state.deps.protocol.user_registry,
            jid.clone(),
            entry,
        )
        .await,
        "actor mirror register should confirm the resource"
    );
    state
        .deps
        .protocol
        .connection_registry
        .update_presence(&jid, true, 0);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-detach-actor".to_string(), true, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    // DashMap entry removed AND the resumable session stored (detached).
    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let stored = state
        .deps
        .protocol
        .sm_session_registry
        .peek_session("stream-detach-actor")
        .await
        .expect("registry lookup");
    assert!(stored.is_some(), "detach stores the resumable session");

    // The actor-tree entry for this bare JID is pruned (its only resource was
    // removed, so the UserActor reports empty and is pruned). GetUser is
    // FIFO-ordered after the unregister mirror on the same registry mailbox, so
    // this observes the pruned state deterministically — no leak.
    let user = state
        .deps
        .protocol
        .user_registry
        .ask(GetUser {
            bare_jid: jid.to_bare(),
        })
        .await
        .expect("get user");
    assert!(
        user.is_none(),
        "SM detach must prune the actor-tree entry, not leak it until expiry"
    );
}

#[tokio::test]
async fn cleanup_shutdown_does_not_detach_explicit_close() {
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "closing-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-close".to_string(), false, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(!state.deps.protocol.connection_registry.is_connected(&jid));
    let detached = state
        .deps
        .protocol
        .sm_session_registry
        .take_session("stream-close")
        .await
        .expect("registry lookup");
    assert!(
        detached.is_none(),
        "explicit <close/> must not leave a resumable detached session behind"
    );
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_none());
}

#[tokio::test]
async fn cleanup_shutdown_does_not_unregister_replacement_session() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let (old_tx, mut old_rx) = mpsc::channel::<OutboundStanza>(4);
    let (new_tx, _new_rx) = mpsc::channel::<OutboundStanza>(4);

    let old_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), old_tx);
    let new_owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), new_tx);

    let mut old_conn = WsConnState::new();
    old_conn.phase = ConnectionPhase::ready(jid.clone(), false);
    old_conn.registry_owner = Some(old_owner);

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut old_rx, &mut old_conn, false).await;

    assert!(
        state.deps.protocol.connection_registry.is_connected(&jid),
        "cleanup for a replaced connection must leave the replacement registered"
    );
    assert!(
        state
            .deps
            .protocol
            .connection_registry
            .unregister_if_owner(&jid, &new_owner)
            .is_some(),
        "the remaining registry owner should be the replacement session"
    );
}

#[tokio::test]
async fn sm_janitor_helper_drains_expired_and_cleans_muc() {
    // Exercise the pieces the janitor composes: drain_expired() returns
    // the removed sessions, and cleanup_muc_presence_for_jid removes the
    // occupant that was held while the session was detached.
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "expired-channel@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    // Put alice in the room, as if she'd detached with SM.
    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    assert!(snapshot_room(state.as_ref(), &room_jid)
        .await
        .room
        .find_nick_by_real_jid(&jid)
        .is_some());

    // Seed an immediately-expired detached session for that JID.
    let stream_id = "already-expired".to_string();
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: stream_id.clone(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 0,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: Vec::new(),
            max_resume_time: Some(0), // already expired
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
        })
        .await
        .expect("store");
    state
        .deps
        .protocol
        .resumable_sessions
        .insert(stream_id.clone(), Session::new("uid", "alice", "alice"));

    // Wait a hair so the 0-second TTL is definitely in the past.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    let drained = state
        .deps
        .protocol
        .sm_session_registry
        .drain_expired()
        .await
        .expect("drain");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].stream_id, stream_id);

    // The janitor body: remove sidecar + MUC occupant + any routing slot.
    state.deps.protocol.resumable_sessions.remove(&stream_id);
    state
        .deps
        .protocol
        .connection_registry
        .unregister(&drained[0].jid);
    cleanup_muc_presence_for_jid(state.as_ref(), &drained[0].jid).await;

    assert!(!state
        .deps
        .protocol
        .resumable_sessions
        .contains_key(&stream_id));
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_none(),
        "MUC occupant must be gone after janitor sweep"
    );
}

#[tokio::test]
async fn sm_resume_replay_stamps_xep0203_delay_with_original_receipt_time() {
    // Issue #1178: stanzas replayed after <resumed/> must carry a
    // XEP-0203 <delay/> whose stamp is the ORIGINAL server receipt
    // time — otherwise clients timestamp them at drain time and sort
    // them to the bottom of the timeline.
    use chrono::{TimeZone, Utc};
    use waddle_xmpp::stream_management::{DetachedSession, DetachedUnackedStanza};

    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    let session = create_test_session(state.as_ref(), "bob").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();
    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let original_receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 15, 30).unwrap();
    let detached_jid: FullJid = format!("bob@{domain}/web").parse().expect("jid");
    let queued_message_xml = {
        let mut message =
            xmpp_parsers::message::Message::new(Some(jid::Jid::from(detached_jid.clone())));
        message.from = Some(
            format!("alice@{domain}/a")
                .parse::<jid::Jid>()
                .expect("jid"),
        );
        message.type_ = xmpp_parsers::message::MessageType::Chat;
        message.id = Some(xmpp_parsers::message::Id("replayed-1".to_string()));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "while you were away".to_string(),
        );
        stanza_to_xml(&Stanza::Message(message))
    };
    let queued_iq_xml = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                domain.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                detached_jid.to_string(),
            )
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "replayed-iq")
            .build(),
    );
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-replay-delay".to_string(),
            user_id: format!("bob@{domain}"),
            jid: detached_jid.clone(),
            inbound_count: 1,
            outbound_count: 2,
            last_acked: 0,
            replay_gap_through: None,
            unacked_stanzas: vec![
                DetachedUnackedStanza {
                    sequence: 1,
                    stanza_xml: queued_message_xml,
                    original_receipt_at: original_receipt,
                },
                DetachedUnackedStanza {
                    sequence: 2,
                    stanza_xml: queued_iq_xml,
                    original_receipt_at: original_receipt,
                },
            ],
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
        })
        .await
        .expect("store");

    let resume_frame = resume_frame_xml("stream-replay-delay", 0);
    let responses = handle_xmpp_frame(&resume_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 3, "<resumed/> + 2 replayed stanzas");
    let resumed = Element::from_str(&responses[0]).expect("xml");
    assert_eq!(resumed.name(), "resumed");

    // The replayed <message/> carries the server's delay stamp with the
    // original receipt time, not the resume time.
    let replayed_message = Element::from_str(&responses[1]).expect("replayed message xml");
    assert_eq!(replayed_message.name(), "message");
    let delay = replayed_message
        .children()
        .find(|child| child.name() == "delay" && child.ns() == "urn:xmpp:delay")
        .expect("replayed message must carry a XEP-0203 delay");
    assert_eq!(delay.attr("from"), Some(domain.as_str()));
    assert_eq!(delay.attr("stamp"), Some("2026-07-01T09:15:30Z"));

    // The replayed <iq/> stays unstamped — XEP-0203 covers message and
    // presence only.
    let replayed_iq = Element::from_str(&responses[2]).expect("replayed iq xml");
    assert_eq!(replayed_iq.name(), "iq");
    assert!(
        !replayed_iq.children().any(|child| child.name() == "delay"),
        "iq replay must not gain a delay element"
    );
}

#[tokio::test]
async fn sm_detach_on_transport_drop_does_not_evict_sfu_call_session() {
    // #935 decided behavior: presence loss must never end a healthy
    // LiveKit session — an SM-resumable transport drop keeps the MUC
    // occupant slot, and the SFU participant must survive with it.
    // Only involuntary moderation (kick 307 / ban 301) or terminal
    // session death may tear the call down.
    let recorder = std::sync::Arc::new(super::RecordingSfu::default());
    let state = super::create_test_websocket_state_with_sfu(recorder.clone()).await;
    let owner_session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let room_jid: BareJid = "detach-keeps-call@muc.example.com".parse().expect("room");
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");

    let _ = handle_muc_join(
        state.as_ref(),
        "example.com",
        &room_jid,
        &jid,
        "alice",
        None,
        &Some(owner_session),
    )
    .await;
    let (tx, mut rx) = mpsc::channel::<OutboundStanza>(4);
    let owner = state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.registry_owner = Some(owner);
    conn.sm_state
        .enable("stream-detach-call".to_string(), true, Some(300));

    let _ = cleanup_connection_shutdown(state.as_ref(), &mut rx, &mut conn, false).await;

    assert!(
        recorder.snapshot().is_empty(),
        "resumable SM detach must not evict the SFU participant"
    );
    assert!(
        recorder.note_snapshot().is_empty(),
        "resumable SM detach must not touch SFU bookkeeping either"
    );
    assert!(
        snapshot_room(state.as_ref(), &room_jid)
            .await
            .room
            .find_nick_by_real_jid(&jid)
            .is_some(),
        "occupant slot survives the detach"
    );
}

/// Council-adjudicated FIX 3: race `attempt_cross_node_resume` against this
/// node's graceful-shutdown token, Postgres-gated (needs a real
/// `PostgresClaimStore` foreign claim so `attempt_cross_node_resume`'s
/// retry loop actually runs, rather than short-circuiting to `NotFound`).
/// Skipped (not failed) when `WADDLE_TEST_POSTGRES_URL` is unset, mirroring
/// every other Postgres-gated test in this crate.
#[cfg(feature = "clustering")]
mod fix3_shutdown_race {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::clustering::ClusteringHandles;
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, NodeIdentity, SharedNodeIdentity,
    };
    use waddle_xmpp::stream_management::{
        InMemorySmSessionRegistry, RemoteResumeAskOutcome, RemoteResumeAsker,
    };

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    /// Never resolves — the asker equivalent of a permanently wedged owner.
    /// Without FIX 3, a resume attempt against this asker would hold this
    /// connection's own graceful shutdown hostage for the entire
    /// (here, deliberately generous) handshake budget.
    struct HangingAsker;

    #[async_trait::async_trait]
    impl RemoteResumeAsker for HangingAsker {
        async fn ask_remote_detach(
            &self,
            _node_id: &str,
            _stream_id: &str,
            _requester_bare_jid: &BareJid,
        ) -> RemoteResumeAskOutcome {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn shutdown_mid_resume_abandons_the_attempt_without_waiting_out_the_budget() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "fix3-shutdown-race-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let claim_store = PostgresClaimStore::new(db.clone());
        claim_store
            .ensure_schema()
            .await
            .expect("ensure claims schema");
        {
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
        }

        // A foreign, live claim: owned by a different node than the one
        // about to attempt the resume, so `attempt_cross_node_resume`
        // actually dispatches into branch 2/3 (live handshake) instead of
        // short-circuiting.
        let owner = node_identity();
        let entity = Entity::new(EntityType::SmSession, "stream-shutdown-race".to_string());
        claim_store
            .acquire(&entity, &owner)
            .await
            .expect("owner claims the entity");

        let resuming_identity = node_identity();
        let resuming_identity_handle = SharedNodeIdentity::new(resuming_identity.clone());
        let resuming_claim_store: Arc<dyn ClaimStore> =
            Arc::new(PostgresClaimStore::new(db.clone()));
        let sm_session_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_claim_store(
                    Arc::clone(&resuming_claim_store),
                    resuming_identity_handle.clone(),
                )
                .with_remote_resume_asker(Arc::new(HangingAsker)),
        );

        // Deliberately generous: if FIX 3's shutdown race did not work,
        // this test would need to wait out the whole budget before
        // observing the (wrong) outcome — this value is what proves the
        // difference.
        let handshake_budget = Duration::from_secs(30);
        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&resuming_claim_store)),
            node_identity: Some(resuming_identity_handle),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: None,
            node_lease: None,
            lease_ttl: None,
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            fatal_fence: None,
            resume_handshake_timeout: Some(handshake_budget),
        };

        let mut state =
            create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
        let graceful = waddle_ecdysis::GracefulShutdown::new(Duration::from_secs(5));
        Arc::get_mut(&mut state)
            .expect("sole owner immediately after construction")
            .deps
            .shutdown = graceful.handle();

        let jid: FullJid = "alice@example.com/phone".parse().expect("valid jid");
        let frame = resume_frame_xml("stream-shutdown-race", 0);
        let state_for_task = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            let started = std::time::Instant::now();
            let responses =
                handle_xmpp_frame(&frame, "example.com", state_for_task.as_ref(), &mut conn).await;
            (started.elapsed(), responses)
        });

        // Give the spawned resume attempt a moment to actually enter its
        // held-response retry loop (past the `current_claim`/persistence
        // reads, into the `HangingAsker` ask) before shutdown fires.
        tokio::time::sleep(Duration::from_millis(200)).await;
        graceful.trigger_stop();

        let (elapsed, responses) = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the resume attempt must return promptly once shutdown fires")
            .expect("task must not panic");

        assert!(
            elapsed < handshake_budget,
            "resume attempt took {elapsed:?}, which did not plausibly finish faster than \
             the {handshake_budget:?} budget — shutdown did not preempt the held resume"
        );
        assert!(
            responses.is_empty(),
            "an abandoned-on-shutdown resume must send no response of its own — the \
             connection's own system-shutdown close is the actual signal to the client; \
             got {responses:?}"
        );
    }
}

/// ADR-0017 Phase 3 deviation 55 (FIX A): a second adversarial convergence
/// pass found deviation 47's shutdown race (`fix3_shutdown_race`, above)
/// was unsound past the CAS itself — `tokio::select!` could drop the whole
/// `attempt_cross_node_resume` future between `steal_for_resume` committing
/// in Postgres and `hydrate_reclaimed`/`claim_session` completing, stranding
/// a self-owned, un-hydrated claim. The fix splits the call at its write
/// boundary (`prepare_cross_node_resume` + `finish_cross_node_steal`) and
/// only races the read-only `prepare` half; `finish_cross_node_steal` is
/// never raced once reached. This module proves that end-to-end through
/// `handle_xmpp_frame`, Postgres-gated (needs a real `PostgresClaimStore` so
/// `steal_for_resume` genuinely commits, and a real `PostgresFencedSmPersistence`
/// so `hydrate_reclaimed` has somewhere to read from).
#[cfg(feature = "clustering")]
mod fix_a_post_cas_shutdown {
    use super::*;
    use crate::clustering::claims::{clustering_control_plane_table_lock, PostgresClaimStore};
    use crate::clustering::ClusteringHandles;
    use crate::db::{Database, DatabaseConfig, DatabaseDriver, DEFAULT_CONTROL_PLANE_POOL_SIZE};
    use crate::server::routes::websocket::tests::create_test_websocket_state_with_clustering;
    use crate::sm_persistence_fenced::PostgresFencedSmPersistence;
    use std::sync::Arc;
    use std::time::Duration;
    use waddle_xmpp::ownership::{
        ClaimEpoch, ClaimError, ClaimSnapshot, ClaimStore, Entity, NodeIdentity,
        ResumeIdentityProof, SharedNodeIdentity, StalePredicate,
    };
    use waddle_xmpp::stream_management::{
        DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
    };

    fn node_identity() -> NodeIdentity {
        NodeIdentity::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        )
    }

    /// `ClaimStore` test double: delegates every method to a real
    /// `PostgresClaimStore`, except `ensure_claimed`, which notifies
    /// `arrived` and then waits on `release_gate` before delegating. In
    /// this test's flow, `hydrate_reclaimed`'s own internal self-reacquire
    /// `ensure_claimed` call — issued only AFTER `steal_for_resume` has
    /// already won — is the sole call this double ever sees, giving the
    /// test a precise, deterministic window "the CAS has committed, the
    /// finish sequence is mid-flight" to fire shutdown into.
    struct GatedEnsureClaimedStore {
        inner: Arc<dyn ClaimStore>,
        arrived: Arc<tokio::sync::Notify>,
        release_gate: Arc<tokio::sync::Notify>,
        /// Only the FIRST `ensure_claimed` call (`hydrate_reclaimed`'s own
        /// post-CAS-win self-reacquire) gates — `claim_session`'s own
        /// subsequent self-reacquire call must pass straight through, or
        /// this double would need a second `release_gate` notification the
        /// test never sends.
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl ClaimStore for GatedEnsureClaimedStore {
        async fn ensure_schema(&self) -> Result<(), ClaimError> {
            self.inner.ensure_schema().await
        }

        async fn acquire(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner.acquire(entity, me).await
        }

        async fn ensure_claimed(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                self.arrived.notify_one();
                self.release_gate.notified().await;
            }
            self.inner.ensure_claimed(entity, me).await
        }

        async fn steal_stale(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            staleness: StalePredicate,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_stale(entity, observed, staleness, me)
                .await
        }

        async fn steal_for_resume(
            &self,
            entity: &Entity,
            observed: ClaimEpoch,
            witness: ResumeIdentityProof,
            me: &NodeIdentity,
        ) -> Result<ClaimEpoch, ClaimError> {
            self.inner
                .steal_for_resume(entity, observed, witness, me)
                .await
        }

        async fn current_claim(
            &self,
            entity: &Entity,
        ) -> Result<Option<ClaimSnapshot>, ClaimError> {
            self.inner.current_claim(entity).await
        }

        async fn fence(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<bool, ClaimError> {
            self.inner.fence(entity, me, mine).await
        }

        async fn release(
            &self,
            entity: &Entity,
            me: &NodeIdentity,
            mine: ClaimEpoch,
        ) -> Result<(), ClaimError> {
            self.inner.release(entity, me, mine).await
        }

        async fn release_many(
            &self,
            entities: &[Entity],
            me: &NodeIdentity,
        ) -> Result<(), ClaimError> {
            self.inner.release_many(entities, me).await
        }
    }

    #[tokio::test]
    async fn shutdown_firing_mid_finish_does_not_abandon_an_already_won_steal() {
        let _guard = clustering_control_plane_table_lock().lock().await;
        let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set");
            return;
        };
        let db = Database::from_config(
            "fix-a-post-cas-shutdown-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        {
            let claim_store = PostgresClaimStore::new(db.clone());
            claim_store
                .ensure_schema()
                .await
                .expect("ensure claims schema");
            // Provision the SM schema BEFORE cleaning it: under CI's fresh
            // per-run Postgres this test can be the first to touch
            // sm_sessions/sm_unacked, and a bare DELETE against a
            // not-yet-created table fails 42P01 (caught by nixTest on the
            // Slice 7 push — locally another suite always ran first).
            let schema_identity = SharedNodeIdentity::new(node_identity());
            let schema_claims: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
            let _schema_only = PostgresFencedSmPersistence::open(
                db.clone(),
                Arc::clone(&schema_claims),
                schema_identity,
            )
            .await
            .expect("provision sm schema");
            let conn = db.guard().await.expect("guard");
            conn.execute("DELETE FROM clustering_claims", ())
                .await
                .expect("clean claims");
            conn.execute("DELETE FROM clustering_nodes", ())
                .await
                .expect("clean nodes");
            conn.execute("DELETE FROM sm_unacked", ())
                .await
                .expect("clean sm_unacked");
            conn.execute("DELETE FROM sm_sessions", ())
                .await
                .expect("clean sm_sessions");
        }

        // Owner node: stores a real detached session via a real
        // `PostgresFencedSmPersistence`, so the resuming node below reads
        // a genuine persisted row (branch 1's fast path) rather than
        // needing a live-handshake asker at all.
        let owner_identity = SharedNodeIdentity::new(node_identity());
        let owner_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        let owner_persistence = PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::clone(&owner_claim_store),
            owner_identity.clone(),
        )
        .await
        .expect("open owner fenced persistence");
        let owner_registry = InMemorySmSessionRegistry::new()
            .with_persistence(Arc::new(owner_persistence))
            .with_claim_store(owner_claim_store, owner_identity);

        let jid: FullJid = "alice@example.com/phone".parse().expect("valid full jid");
        owner_registry
            .store_session(DetachedSession {
                stream_id: "stream-post-cas-shutdown".to_string(),
                user_id: "alice@example.com".to_string(),
                jid: jid.clone(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
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
            })
            .await
            .expect("owner stores the detached session");

        // Resuming node: the one wired into the websocket state under
        // test. Its `ClaimStore` is gated on `ensure_claimed` — the call
        // `hydrate_reclaimed` issues right after `steal_for_resume` wins.
        let resuming_identity = node_identity();
        let resuming_identity_handle = SharedNodeIdentity::new(resuming_identity.clone());
        let real_claim_store: Arc<dyn ClaimStore> = Arc::new(PostgresClaimStore::new(db.clone()));
        let arrived = Arc::new(tokio::sync::Notify::new());
        let release_gate = Arc::new(tokio::sync::Notify::new());
        let gated_claim_store: Arc<dyn ClaimStore> = Arc::new(GatedEnsureClaimedStore {
            inner: Arc::clone(&real_claim_store),
            arrived: Arc::clone(&arrived),
            release_gate: Arc::clone(&release_gate),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let resuming_persistence = PostgresFencedSmPersistence::open(
            db.clone(),
            Arc::clone(&gated_claim_store),
            resuming_identity_handle.clone(),
        )
        .await
        .expect("open resuming fenced persistence");
        let sm_session_registry = Arc::new(
            InMemorySmSessionRegistry::new()
                .with_persistence(Arc::new(resuming_persistence))
                .with_claim_store(
                    Arc::clone(&gated_claim_store),
                    resuming_identity_handle.clone(),
                ),
        );

        // Generous and, crucially, irrelevant to the outcome: branch 1's
        // persisted-snapshot fast path fires immediately (no remote ask
        // needed), and once `finish_cross_node_steal` starts it no longer
        // consults this budget at all (FIX A).
        let handshake_budget = Duration::from_secs(30);
        let clustering = ClusteringHandles {
            claim_store: Some(Arc::clone(&gated_claim_store)),
            node_identity: Some(resuming_identity_handle),
            local_claims: None,
            room_local_claims: None,
            user_local_claims: None,
            muc_durable_store: None,
            node_lease: None,
            lease_ttl: None,
            pod_template_hash: None,
            resume_bridge: None,
            ordered_relay_delivery_bridge: None,
            stop_token: None,
            fatal_fence: None,
            resume_handshake_timeout: Some(handshake_budget),
        };

        let mut state =
            create_test_websocket_state_with_clustering(clustering, sm_session_registry).await;
        let graceful = waddle_ecdysis::GracefulShutdown::new(Duration::from_secs(5));
        Arc::get_mut(&mut state)
            .expect("sole owner immediately after construction")
            .deps
            .shutdown = graceful.handle();

        let frame = resume_frame_xml("stream-post-cas-shutdown", 0);
        let state_for_task = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            let mut conn = WsConnState::new();
            conn.phase = ConnectionPhase::authenticated(&jid);
            handle_xmpp_frame(&frame, "example.com", state_for_task.as_ref(), &mut conn).await
        });

        // Wait until the finish sequence is genuinely mid-flight — PAST
        // `steal_for_resume`'s real Postgres commit, INSIDE
        // `hydrate_reclaimed`'s self-reacquire — before firing shutdown.
        tokio::time::timeout(Duration::from_secs(5), arrived.notified())
            .await
            .expect("hydrate_reclaimed's ensure_claimed must be reached promptly");
        graceful.trigger_stop();
        // Give the connection's own select loop a moment to observe the
        // (now-irrelevant, since `finish_cross_node_steal` is never raced)
        // cancelled token, proving it has no effect on the in-flight finish.
        tokio::time::sleep(Duration::from_millis(200)).await;
        release_gate.notify_one();

        let responses = tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the resume must complete promptly once the gate is released")
            .expect("task must not panic");

        assert_eq!(
            responses.len(),
            1,
            "shutdown firing mid-finish must not truncate/abandon the response; got {responses:?}"
        );
        let resumed = Element::from_str(&responses[0]).expect("xml");
        assert_eq!(
            resumed.name(),
            "resumed",
            "the already-won steal must complete to a real <resumed/>, not be dropped by the \
             shutdown token that fired mid-sequence; got {responses:?}"
        );
    }
}

/// XEP-0198 §4 counters are mod 2^32: a resume whose `h` sits just
/// behind a freshly wrapped `outbound_count` is VALID, not
/// handled-count-too-high (gpt-5.5 review follow-up to #1099 — the
/// live ack path was made wrap-aware, resume must agree).
#[tokio::test]
async fn sm_resume_accepts_handled_count_behind_wrapped_outbound() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-wrapped".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            // The server's send counter wrapped past 2^32: it now
            // reads 2, while the client last handled u32::MAX.
            outbound_count: 2,
            last_acked: u32::MAX - 1,
            replay_gap_through: None,
            unacked_stanzas: vec![
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: u32::MAX,
                    stanza_xml: "<message xmlns='jabber:client' id='pre-wrap'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: 1,
                    stanza_xml: "<message xmlns='jabber:client' id='post-wrap-1'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
                waddle_xmpp::stream_management::DetachedUnackedStanza {
                    sequence: 2,
                    stanza_xml: "<message xmlns='jabber:client' id='post-wrap-2'/>".to_string(),
                    original_receipt_at: chrono::Utc::now(),
                },
            ],
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // h = u32::MAX acks the pre-wrap stanza; a naive `h > outbound`
    // comparison misreads this as handled-count-too-high.
    let resume_frame = resume_frame_xml("stream-wrapped", u32::MAX);
    let responses =
        handle_xmpp_frame(&resume_frame, "example.com", state.as_ref(), &mut conn).await;

    let resumed = responses
        .iter()
        .any(|frame| frame.contains("<resumed") && frame.contains("stream-wrapped"));
    assert!(
        resumed,
        "wrapped-counter resume must succeed, got frames: {responses:?}"
    );
    assert!(
        responses.iter().any(|frame| frame.contains("post-wrap-1"))
            && responses.iter().any(|frame| frame.contains("post-wrap-2")),
        "post-wrap unacked stanzas must be replayed: {responses:?}"
    );
    assert!(
        !responses.iter().any(|frame| frame.contains("pre-wrap")),
        "the acked pre-wrap stanza must not be replayed: {responses:?}"
    );
}

/// Conformance review follow-up to #1103: resuming must carry the
/// detached session's presence extension payloads back onto the live
/// registry entry. RFC 6121 §4.3.2 requires probe responses to
/// reproduce the full last presence stanza; before this fix a resume
/// silently stripped the XEP-0319 idle stamp (and caps) even though
/// the client sent no new presence.
#[tokio::test]
async fn sm_resume_restores_presence_payloads_to_the_live_registry() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let idle = waddle_xmpp::xep::xep0319::build_idle_element(
        chrono::DateTime::parse_from_rfc3339("2026-07-07T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc),
    );
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-idle".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
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
            presence_available: true,
            presence_show: Some(xmpp_parsers::presence::Show::Away),
            presence_status: None,
            presence_priority: 0,
            presence_payloads: vec![idle.clone()],
            pending_subscribes_flushed: false,
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-idle", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let presence_state = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(&jid)
        .expect("live presence state after resume");
    assert!(
        presence_state.payloads.contains(&idle),
        "the XEP-0319 idle payload must survive resume onto the live \
         registry entry, got {:?}",
        presence_state.payloads
    );
}

/// Conformance review follow-up to #1104: an XEP-0198 resume is the
/// SAME session (§5) — the once-per-session pending-subscribe claim
/// must survive it. Before this fix the fresh ConnectionEntry's CAS
/// re-armed, so the first auto-away flip after a resume re-prompted
/// the user with the still-unanswered subscribe.
#[tokio::test]
async fn sm_resume_preserves_the_pending_subscribe_once_per_session_claim() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-claimed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
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
            // Still available at detach; the claim consumed by the
            // initial available presence is recorded explicitly.
            presence_available: true,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: true,
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-claimed", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        !entry.claim_pending_subscribes_flush(),
        "the once-per-session claim must already be consumed on the \
         resumed entry — a presence flip after resume must not \
         re-deliver pending subscribes"
    );
}

/// The consumed claim must survive detach even when the session went
/// UNAVAILABLE after its initial available presence: presence state at
/// detach says nothing about whether the flush already happened, so
/// the claim is carried explicitly on the detached session. Before
/// this fix the pre-claim was gated on `presence_available`, so an
/// available → unavailable → detach → resume sequence re-armed the CAS
/// and the next available re-prompted the user.
#[tokio::test]
async fn sm_resume_preserves_consumed_claim_when_detached_unavailable() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-claimed-unavail".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
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
            // Went available (claim consumed), then unavailable before
            // the transport dropped.
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: true,
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-claimed-unavail", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        !entry.claim_pending_subscribes_flush(),
        "the claim consumed before the unavailable flip must stay \
         consumed across resume — the next available presence must \
         not re-deliver pending subscribes"
    );
}

/// Companion: a session that NEVER went available before detaching has
/// an unconsumed claim, and resume must keep it armed — the resumed
/// session's true initial available presence still owes the RFC 6121
/// §3.1.3 pending-subscribe delivery.
#[tokio::test]
async fn sm_resume_keeps_unconsumed_claim_armed() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-unclaimed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-unclaimed", 0),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(
        responses.iter().any(|frame| frame.contains("<resumed")),
        "resume must succeed: {responses:?}"
    );
    let (tx, _rx) = mpsc::channel::<OutboundStanza>(8);
    let mut pending_tx = Some(tx);
    super::super::registration::register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;

    let entry = state
        .deps
        .protocol
        .connection_registry
        .get_entry(&jid)
        .expect("registered entry after resume");
    assert!(
        entry.claim_pending_subscribes_flush(),
        "a never-available session's claim must still be armed after \
         resume so the initial available presence delivers the queued \
         subscribes"
    );
}

/// Round-2 concurrency review on #1099: an `h` in the wrap-BEHIND
/// half-space (mod-2^32 "less than" everything the session ever acked)
/// passed the too-high guard as "behind", then the numeric range-delete
/// wiped every claimed pending_delivery row and corrupted last_acked.
/// A live ack outside the valid window [last_acked, outbound_count] on
/// the low side is stale garbage: it must be ignored wholesale — no
/// purge, no counter movement, no stream error.
#[tokio::test]
async fn sm_live_ack_behind_last_acked_is_ignored_without_purging() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    let stream_id = enable_sm_for_live_ack_tests(state.as_ref(), &mut conn, &jid).await;

    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o1'/>".to_string());
    let _ = conn
        .sm_state
        .record_outbound("<message xmlns='jabber:client' id='o2'/>".to_string());
    let recipient: BareJid = "alice@example.com".parse().expect("bare jid");
    seed_claimed_pending_row(state.as_ref(), &recipient, &stream_id, 1).await;

    // 0xC0000000 is mod-2^32 "behind" outbound_count=2, so the
    // too-high guard alone does not catch it — but numerically it
    // exceeds every real sequence, so an unguarded range-delete would
    // destroy all rows.
    let responses = handle_xmpp_frame(
        "<a xmlns='urn:xmpp:sm:3' h='3221225472'/>",
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses.is_empty(),
        "stale wrap-behind ack must be ignored, got {responses:?}"
    );
    assert!(
        !conn.phase.is_closing(),
        "stale ack must not terminate the stream"
    );
    assert_eq!(
        conn.sm_state.last_acked, 0,
        "stale ack must not move last_acked"
    );
    assert_eq!(
        conn.sm_state.get_stanzas_to_resend(0).len(),
        2,
        "stale ack must not purge the replay queue"
    );
    let rows = state
        .deps
        .protocol
        .pending_delivery_storage
        .list(&recipient)
        .await
        .expect("list pending rows");
    assert_eq!(
        rows.len(),
        1,
        "stale ack must not range-delete pending rows"
    );
}

/// Companion to the wrap-behind live-ack guard: a resume whose `h`
/// regressed mod-2^32 behind the session's last_acked cannot be
/// replayed (that prefix was purged when the ack landed) and must be
/// refused as a failed resume — session preserved, nothing purged.
#[tokio::test]
async fn sm_resume_rejects_handled_count_behind_last_acked() {
    use waddle_xmpp::stream_management::{DetachedSession, SmSessionRegistry};
    let state = create_test_websocket_state().await;

    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    state
        .deps
        .protocol
        .sm_session_registry
        .store_session(DetachedSession {
            stream_id: "stream-regressed".to_string(),
            user_id: "alice@example.com".to_string(),
            jid: jid.clone(),
            inbound_count: 0,
            outbound_count: 4,
            last_acked: 3,
            replay_gap_through: None,
            unacked_stanzas: vec![waddle_xmpp::stream_management::DetachedUnackedStanza {
                sequence: 4,
                stanza_xml: "<message xmlns='jabber:client' id='m4'/>".to_string(),
                original_receipt_at: chrono::Utc::now(),
            }],
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
        })
        .await
        .expect("store");

    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::authenticated(&jid);
    // h = 0xC0000000 is mod-2^32 behind last_acked=3 (stale garbage);
    // the wrap-aware too-high guard alone would classify it "behind
    // outbound" and let acknowledge() + the numeric row range-delete
    // run.
    let responses = handle_xmpp_frame(
        &resume_frame_xml("stream-regressed", 0xC000_0000),
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(
        responses
            .iter()
            .any(|frame| frame.contains("<failed") && frame.contains("resource-constraint")),
        "regressed-h resume must fail as unresumable, got {responses:?}"
    );
    // The detached session survives for a corrected retry.
    let restored = state
        .deps
        .protocol
        .sm_session_registry
        .claim_session("stream-regressed")
        .await
        .expect("registry")
        .expect("session preserved after failed resume");
    assert_eq!(restored.last_acked, 3);
}
