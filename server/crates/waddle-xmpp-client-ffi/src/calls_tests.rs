//! Per-XEP FFI test suites for the A/V call verbs.
//!
//! Each module pins one XEP's wire shape as produced by the exact FFI
//! construction path (the pure factories in `calls.rs` + the shared
//! `waddle-xmpp-client` builders) plus the exported methods' typed
//! failure behavior (not-connected, invalid JID) through a recording
//! listener — same fixture style as `messaging_verbs_tests.rs`.

use std::sync::{Arc, Mutex as StdMutex};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use waddle_xmpp_client::messaging::{
    build_finish_with_reason, build_propose, build_ringing, CallMedia, JingleReason, SessionId,
};
use waddle_xmpp_client::xep::xep0215::{parse_external_services, NS_EXT_DISCO};
use waddle_xmpp_client::{ClientError, StanzaError, StanzaErrorType};

use crate::calls::{
    classify_terminate_result, extdisco_reply_from_own_server, external_service_to_ffi,
    is_orphaned_dm_call_terminate, session_terminate_iq,
};
use crate::stanza::message_with_jmi;
use crate::{
    WaddleCallSessionTerminateOutcome, WaddleClient, WaddleClientEvent, WaddleConfig,
    WaddleEventListener, WaddleExternalServiceTransport, WaddleExternalServiceType,
    WaddleJingleReason,
};

const NS_JINGLE: &str = "urn:xmpp:jingle:1";
const NS_JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";
// XEP-0334 hint namespace; the client crate keeps its constant
// pub(crate), so the expected wire value is pinned here literally.
const NS_HINTS: &str = "urn:xmpp:hints";

#[derive(Clone, Default)]
struct RecordingListener {
    errors: Arc<StdMutex<Vec<String>>>,
}

impl RecordingListener {
    fn errors(&self) -> Vec<String> {
        self.errors.lock().expect("test mutex poisoned").clone()
    }
}

impl WaddleEventListener for RecordingListener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let WaddleClientEvent::Error { description } = event {
            self.errors
                .lock()
                .expect("test mutex poisoned")
                .push(description);
        }
    }
}

fn test_client(listener: RecordingListener) -> Arc<WaddleClient> {
    Arc::new(WaddleClient {
        config: WaddleConfig {
            server_url: "wss://xmpp.waddle.test".to_string(),
            jid: "alice@waddle.test".to_string(),
            access_token: "token".to_string(),
            resource: "test".to_string(),
            resume_state: None,
        },
        listener: Arc::new(Box::new(listener) as Box<dyn WaddleEventListener>),
        handle: tokio::sync::Mutex::new(None),
    })
}

fn bare(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

fn full(value: &str) -> FullJid {
    value.parse().expect("test full JID parses")
}

fn sid(value: &str) -> SessionId {
    SessionId(value.to_string())
}

fn stanza_error(error_type: StanzaErrorType, condition: &str, text: Option<&str>) -> StanzaError {
    StanzaError {
        error_type,
        condition: condition.to_string(),
        text: text.map(str::to_string),
        application_condition: None,
    }
}

// ── XEP-0353: Jingle Message Initiation — envelope + ringing ────────────────

mod xep0353 {
    use super::*;

    /// XEP-0353 §3: every JMI envelope MUST be `type='chat'` and MUST
    /// carry a XEP-0334 `<store/>` hint so MAM archives keep the call
    /// timeline reconstructible. Pins the FFI envelope helper the
    /// eleven message-level verbs route through — the regression this
    /// guards is the FFI shipping a bare `<message/>` while the wasm
    /// client shipped the conformant envelope.
    #[test]
    fn jmi_envelope_stamps_chat_type_and_store_hint() {
        let stanza = message_with_jmi(
            &Jid::from(bare("bob@waddle.test")),
            build_propose(&sid("c1"), CallMedia::audio_only()),
        );
        assert_eq!(stanza.name(), "message");
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
        assert!(
            stanza.get_child("store", NS_HINTS).is_some(),
            "XEP-0334 <store/> hint required by XEP-0353 §3"
        );
        let propose = stanza
            .get_child("propose", NS_JINGLE_MESSAGE)
            .expect("JMI propose child preserved");
        assert_eq!(propose.attr("id"), Some("c1"));
    }

    /// XEP-0353 §3.2 "Intermediate State: Device Rings": the
    /// responder's `<ringing/>` is addressed to the caller's BARE JID
    /// so the initiator's server fans the ring state out to every
    /// caller resource.
    #[test]
    fn ringing_targets_bare_jid_with_jmi_id() {
        let stanza = message_with_jmi(
            &Jid::from(bare("caller@waddle.test")),
            build_ringing(&sid("c2")),
        );
        assert_eq!(stanza.attr("to"), Some("caller@waddle.test"));
        assert_eq!(stanza.attr("type"), Some("chat"));
        let ringing = stanza
            .get_child("ringing", NS_JINGLE_MESSAGE)
            .expect("JMI ringing child present");
        assert_eq!(ringing.attr("id"), Some("c2"));
        assert_eq!(ringing.children().count(), 0);
    }

    /// A responder that already sent `<proceed/>` abandons the session
    /// with `<finish/>` + an explicit `<reason/>` (the tie-break-2
    /// shape) — never a contradictory late `<reject/>`. Pins the wire
    /// shape behind `send_call_finish_with_reason`.
    #[test]
    fn finish_with_reason_carries_the_reason_child() {
        let stanza = message_with_jmi(
            &Jid::from(full("caller@waddle.test/phone")),
            build_finish_with_reason(&sid("c3"), Some(JingleReason::Timeout)),
        );
        assert_eq!(stanza.attr("to"), Some("caller@waddle.test/phone"));
        assert_eq!(stanza.attr("type"), Some("chat"));
        assert!(stanza.get_child("store", NS_HINTS).is_some());
        let finish = stanza
            .get_child("finish", NS_JINGLE_MESSAGE)
            .expect("JMI finish child present");
        assert_eq!(finish.attr("id"), Some("c3"));
        let reason = finish
            .get_child("reason", NS_JINGLE)
            .expect("finish carries an explicit reason");
        assert!(reason.get_child("timeout", NS_JINGLE).is_some());
    }

    #[tokio::test]
    async fn send_call_finish_with_reason_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let sent = client
            .send_call_finish_with_reason(
                "caller@waddle.test/phone".into(),
                "c3".into(),
                WaddleJingleReason::Timeout,
            )
            .await;
        assert!(!sent);
        assert!(listener
            .errors()
            .iter()
            .any(|e| e.contains("Not connected")));
    }

    #[tokio::test]
    async fn send_call_ringing_returns_false_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        assert!(
            !client
                .send_call_ringing("caller@waddle.test".into(), "c1".into())
                .await
        );
        assert!(listener
            .errors()
            .iter()
            .any(|e| e.contains("Not connected")));
    }

    /// A resource-bearing JID must be refused so a `<ringing/>` can
    /// never target a single caller resource.
    #[tokio::test]
    async fn send_call_ringing_rejects_full_jid() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        assert!(
            !client
                .send_call_ringing("caller@waddle.test/phone".into(), "c1".into())
                .await
        );
        assert!(listener
            .errors()
            .iter()
            .any(|e| e.contains("invalid bare JID")));
    }

    #[tokio::test]
    async fn send_call_ringing_rejects_blank_sid() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        assert!(
            !client
                .send_call_ringing("caller@waddle.test".into(), "   ".into())
                .await
        );
        assert!(listener.errors().iter().any(|e| e.contains("session id")));
    }
}

// ── XEP-0166: Jingle — session-terminate with typed outcome ─────────────────

mod xep0166 {
    use super::*;

    /// XEP-0166 §7.4: the terminate IQ both terminate verbs share —
    /// `<iq type='set'>` to the peer's full JID carrying
    /// `<jingle action='session-terminate' sid='…'><reason>…</reason></jingle>`.
    #[test]
    fn terminate_iq_carries_action_sid_and_reason() {
        let iq = session_terminate_iq(
            &full("bob@waddle.test/phone"),
            &sid("c3"),
            Some(waddle_xmpp_client::messaging::JingleReason::Success),
        );
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("bob@waddle.test/phone"));
        assert!(iq.attr("id").is_some(), "correlation id minted");
        let jingle = iq.get_child("jingle", NS_JINGLE).expect("jingle payload");
        assert_eq!(jingle.attr("action"), Some("session-terminate"));
        assert_eq!(jingle.attr("sid"), Some("c3"));
        let reason = jingle.get_child("reason", NS_JINGLE).expect("reason");
        assert!(reason.get_child("success", NS_JINGLE).is_some());
    }

    #[test]
    fn terminate_iq_omits_reason_when_none() {
        let iq = session_terminate_iq(&full("bob@waddle.test/phone"), &sid("c4"), None);
        let jingle = iq.get_child("jingle", NS_JINGLE).expect("jingle payload");
        assert!(jingle.get_child("reason", NS_JINGLE).is_none());
    }

    /// The orphaned classification ported from the wasm client: ONLY
    /// `forbidden` with the Waddle Jingle handler's exact text counts.
    #[test]
    fn classifies_only_orphaned_dm_terminate_stanza_error() {
        let orphaned = stanza_error(
            StanzaErrorType::Auth,
            "forbidden",
            Some("Jingle terminator is not a participant in this call"),
        );
        assert!(is_orphaned_dm_call_terminate(&orphaned));

        let other_forbidden = stanza_error(
            StanzaErrorType::Auth,
            "forbidden",
            Some("Jingle responder was not invited to this call party"),
        );
        assert!(!is_orphaned_dm_call_terminate(&other_forbidden));

        let textless_forbidden = stanza_error(StanzaErrorType::Auth, "forbidden", None);
        assert!(!is_orphaned_dm_call_terminate(&textless_forbidden));

        let transportish = stanza_error(StanzaErrorType::Cancel, "remote-server-timeout", None);
        assert!(!is_orphaned_dm_call_terminate(&transportish));
    }

    #[test]
    fn terminate_result_classification_maps_all_three_outcomes() {
        let ok: Result<Element, ClientError> = Ok(Element::builder("iq", "jabber:client").build());
        assert_eq!(
            classify_terminate_result(&ok),
            WaddleCallSessionTerminateOutcome::Ok
        );

        let orphaned: Result<Element, ClientError> = Err(ClientError::StanzaError(stanza_error(
            StanzaErrorType::Auth,
            "forbidden",
            Some("Jingle terminator is not a participant in this call"),
        )));
        assert_eq!(
            classify_terminate_result(&orphaned),
            WaddleCallSessionTerminateOutcome::Orphaned
        );

        let refused: Result<Element, ClientError> = Err(ClientError::StanzaError(stanza_error(
            StanzaErrorType::Cancel,
            "service-unavailable",
            None,
        )));
        assert_eq!(
            classify_terminate_result(&refused),
            WaddleCallSessionTerminateOutcome::Error
        );

        let transport: Result<Element, ClientError> = Err(ClientError::Disconnected);
        assert_eq!(
            classify_terminate_result(&transport),
            WaddleCallSessionTerminateOutcome::Error
        );
    }

    #[tokio::test]
    async fn terminate_with_outcome_reports_error_when_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let outcome = client
            .send_call_session_terminate_with_outcome(
                "bob@waddle.test/phone".into(),
                "c1".into(),
                Some(WaddleJingleReason::Success),
            )
            .await;
        assert_eq!(outcome, WaddleCallSessionTerminateOutcome::Error);
        assert!(listener
            .errors()
            .iter()
            .any(|e| e.contains("Not connected")));
    }

    /// A bare peer JID must be refused before anything ships: the
    /// terminate targets the exact resource that owns the Jingle
    /// session (XEP-0166 §7.1).
    #[tokio::test]
    async fn terminate_with_outcome_rejects_bare_jid() {
        let listener = RecordingListener::default();
        let client = test_client(listener.clone());
        let outcome = client
            .send_call_session_terminate_with_outcome("bob@waddle.test".into(), "c1".into(), None)
            .await;
        assert_eq!(outcome, WaddleCallSessionTerminateOutcome::Error);
        assert!(listener
            .errors()
            .iter()
            .any(|e| e.contains("invalid full JID")));
    }
}

// ── XEP-0215: External Service Discovery ─────────────────────────────────────

mod xep0215 {
    use super::*;

    /// RFC 6120 §8.1.2.1 defense-in-depth ported from the wasm client:
    /// a stamped `from` MUST match the queried server domain
    /// (case-insensitively); an absent `from` is our own server.
    #[test]
    fn reply_from_guard_accepts_own_server_and_absent_from() {
        let own: Element = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='e1'/>"
            .parse()
            .expect("valid xml");
        assert!(extdisco_reply_from_own_server(&own, "waddle.test"));

        let case_insensitive: Element =
            "<iq xmlns='jabber:client' type='result' from='WADDLE.TEST' id='e1'/>"
                .parse()
                .expect("valid xml");
        assert!(extdisco_reply_from_own_server(
            &case_insensitive,
            "waddle.test"
        ));

        let absent: Element = "<iq xmlns='jabber:client' type='result' id='e1'/>"
            .parse()
            .expect("valid xml");
        assert!(extdisco_reply_from_own_server(&absent, "waddle.test"));
    }

    #[test]
    fn reply_from_guard_rejects_foreign_sender() {
        let foreign: Element =
            "<iq xmlns='jabber:client' type='result' from='evil.example' id='e1'/>"
                .parse()
                .expect("valid xml");
        assert!(!extdisco_reply_from_own_server(&foreign, "waddle.test"));
    }

    /// XEP-0215 §3.2 request wire shape as the FFI emits it: IQ-get to
    /// the user's own server domain with an EMPTY `<services/>` (which
    /// requests every advertised service type per §3.6.1).
    #[test]
    fn extdisco_request_targets_own_server_with_empty_services() {
        let domain = crate::boundary_convert::jid_domain("alice@waddle.test");
        let iq = waddle_xmpp_client::xep::xep0215::build_extdisco_services_iq(domain, "req-1");
        assert_eq!(iq.attr("type"), Some("get"));
        assert_eq!(iq.attr("to"), Some("waddle.test"));
        let services = iq
            .get_child("services", NS_EXT_DISCO)
            .expect("services payload");
        assert!(services.attr("type").is_none());
        assert_eq!(services.children().count(), 0);
    }

    /// End-to-end conversion: the server's advertised TURNS + STUN
    /// entries (the exact shape `waddle-xmpp` emits) map into typed
    /// FFI records, `expires` serialized as RFC 3339 for the boundary.
    #[test]
    fn external_services_convert_to_typed_ffi_records() {
        let reply: Element = "<iq xmlns='jabber:client' type='result' from='waddle.test' id='e1'>\
               <services xmlns='urn:xmpp:extdisco:2'>\
                 <service action='add' type='turns' transport='tcp' \
                          host='turn.waddle.social' port='443' \
                          username='1700000000:alice@waddle.social/desktop' \
                          password='base64hmac==' \
                          expires='2026-06-17T12:00:00Z' restricted='1'/>\
                 <service action='add' type='stun' transport='udp' \
                          host='turn.waddle.social' port='3478'/>\
               </services>\
             </iq>"
            .parse()
            .expect("valid xml");
        let services: Vec<_> = parse_external_services(&reply)
            .into_iter()
            .map(external_service_to_ffi)
            .collect();

        assert_eq!(services.len(), 2);
        let turns = &services[0];
        assert_eq!(turns.service_type, WaddleExternalServiceType::Turns);
        assert_eq!(turns.host, "turn.waddle.social");
        assert_eq!(turns.port, Some(443));
        assert_eq!(turns.transport, Some(WaddleExternalServiceTransport::Tcp));
        assert_eq!(
            turns.username.as_deref(),
            Some("1700000000:alice@waddle.social/desktop")
        );
        assert_eq!(turns.password.as_deref(), Some("base64hmac=="));
        assert_eq!(turns.expires.as_deref(), Some("2026-06-17T12:00:00+00:00"));
        assert!(turns.restricted);

        let stun = &services[1];
        assert_eq!(stun.service_type, WaddleExternalServiceType::Stun);
        assert_eq!(stun.port, Some(3478));
        assert_eq!(stun.transport, Some(WaddleExternalServiceTransport::Udp));
        assert!(stun.username.is_none());
        assert!(stun.password.is_none());
        assert!(stun.expires.is_none());
        assert!(!stun.restricted);
    }

    #[tokio::test]
    async fn fetch_external_services_throws_not_connected() {
        let listener = RecordingListener::default();
        let client = test_client(listener);
        let result = client.fetch_external_services().await;
        assert!(matches!(result, Err(crate::WaddleError::NotConnected)));
    }
}
