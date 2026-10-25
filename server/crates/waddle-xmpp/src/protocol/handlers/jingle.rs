//! XEP-0166 Jingle session signaling.
//!
//! Validates that every `<content/>` carries a Waddle LiveKit
//! transport (`urn:waddle:transports:livekit:0`); rewrites the empty
//! `<transport/>` placeholder with a server-issued LiveKit join token
//! before forwarding the stanza to the peer; ACKs the requester.
//!
//! Sync handler — JWT signing is CPU-only and well under a
//! millisecond per stanza, so we hold an [`Arc<dyn SfuService>`]
//! directly rather than going through the two-phase AskSfu callback
//! machinery.

use crate::telemetry::call::PendingCallSetupRoute;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::jingle::{Action, Content, Jingle, SessionId, Transport};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{
    CallCorrelationId, CallId, Identity, MediaCapabilities, SessionBinding, SessionScopedTeardown,
    SfuError, SfuService,
};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::handlers::session_initiate_rate_limit::{
    MujiActionRateLimit, SessionInitiateRateLimit, TerminateRateLimit,
};
use crate::protocol::traits::IqHandler;
use crate::telemetry::attributes::{
    CallControlRateLimitedSurface, CallSetupFailureReason, MetricAttribute, SfuDenialReason,
};
use crate::xep::xep0166::NS_JINGLE;
use crate::xep::xep0272::{find_muji, Muji};
use crate::xep::xep_waddle_livekit_transport::{
    IssuedTransport, TransportParseError, WaddleLiveKitTransport, NS_WADDLE_LIVEKIT_TRANSPORT,
};
use crate::Stanza;

mod undeliverable;

pub use undeliverable::{
    credential_free_jingle_echo, undeliverable_negotiation_rollback,
    UndeliverableNegotiationRollback,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0166;

    /// Pins the 1:1 room-name format through the correlation digest.
    /// The chat twin (`dmCallRoomName` pin in
    /// `chat/tests/call-correlation.test.ts`) asserts the same hex for
    /// the same (initiator, sid), so renaming the format breaks CI on
    /// both sides instead of silently un-joining declined/failed call
    /// lifecycle events from server telemetry (#1452).
    #[test]
    fn dm_room_name_format_digest_is_pinned() {
        let initiator: BareJid = "alice@waddle.test".parse().expect("valid bare jid");
        let call_id = scoped_call_id(&initiator, "c1").expect("valid call id");
        assert_eq!(call_id.as_str(), "alice@waddle.test::c1");
        assert_eq!(
            CallCorrelationId::for_call(&call_id).as_str(),
            "585e23a731089821",
        );
    }

    use crate::xep::xep0167::opus_audio_description;
    use chrono::Duration;
    use jid::FullJid;
    use std::io;
    use waddle_sfu::{
        ApiKey, ApiSecret, LiveKitSfu, SfuConfig, TurnHost, TurnSharedSecret, WebsocketUrl,
    };
    use xmpp_parsers::jingle::{Content, ContentId, Creator, SessionId};

    fn fixture_livekit_sfu() -> Arc<LiveKitSfu> {
        let cfg = SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            webhook_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
            ws_url: WebsocketUrl::try_new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::try_new(cfg).expect("LiveKitSfu init in test"))
    }

    fn fixture_sfu() -> Arc<dyn SfuService> {
        fixture_livekit_sfu()
    }

    fn test_ctx_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
        // Mirrors what the websocket layer's Muji gate supplies for a
        // voiced occupant; tests for the fail-closed path override
        // `media_capabilities` explicitly.
        StanzaContext {
            participant_registration: Some(waddle_sfu::ParticipantRegistrationExpectation::absent()),
            domain: "waddle.test",
            full_jid: jid,
            occupant_session: Some(waddle_xmpp_core::OccupancySessionGeneration::mint()),
            media_capabilities: Some(MediaCapabilities::from_muc_voice(
                waddle_xmpp_core::types::Voice::Voiced,
            )),
        }
    }

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

    fn session_initiate_iq(initiator: &str, responder: &str, sid: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId(sid.into()));
        jingle.initiator = Some(initiator.parse().unwrap());
        jingle.contents.push(content);
        Iq::Set {
            from: Some(initiator.parse().unwrap()),
            to: Some(responder.parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        }
    }

    fn session_accept_iq(responder: &str, to: &str, sid: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionAccept, SessionId(sid.into()));
        jingle.responder = Some(responder.parse().unwrap());
        jingle.contents.push(content);
        Iq::Set {
            from: Some(responder.parse().unwrap()),
            to: Some(to.parse().unwrap()),
            id: "a1".into(),
            payload: jingle.into(),
        }
    }

    fn muji_session_initiate_iq(room: &str) -> Iq {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("muji-observe".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let mut payload: Element = jingle.into();
        payload.append_child(Muji::for_room(room.parse().expect("valid room JID")).to_element());
        Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("calls.waddle.test".parse().unwrap()),
            id: "muji-observe".into(),
            payload,
        }
    }

    fn muji_transport_info_iq(room: &str, sid: &str) -> Iq {
        let mut jingle = Jingle::new(Action::TransportInfo, SessionId(sid.into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        let mut payload: Element = jingle.into();
        payload.append_child(Muji::for_room(room.parse().expect("valid room JID")).to_element());
        Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("calls.waddle.test".parse().unwrap()),
            id: format!("muji-transport-{sid}"),
            payload,
        }
    }

    fn session_terminate_iq(initiator: &str, responder: &str, sid: &str) -> Iq {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId(sid.into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        Iq::Set {
            from: Some(initiator.parse().unwrap()),
            to: Some(responder.parse().unwrap()),
            id: format!("terminate-{sid}"),
            payload: jingle.into(),
        }
    }

    fn register_dm_call(
        sfu: &Arc<LiveKitSfu>,
        initiator: &str,
        responder: &str,
        sid: &str,
    ) -> waddle_sfu::CallId {
        let initiator_bare: BareJid = initiator.parse::<FullJid>().unwrap().to_bare();
        let call = scoped_call_id(&initiator_bare, sid).unwrap();
        sfu.register_call_participant(&call, &Identity::from_jid(initiator.parse().unwrap()));
        sfu.register_call_participant(&call, &Identity::from_jid(responder.parse().unwrap()));
        call
    }

    fn assert_error_condition(events: &[OutboundEvent], condition: DefinedCondition) {
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, condition);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn handler_namespace_is_jingle() {
        assert_eq!(JingleHandler::new(fixture_sfu()).namespace(), NS_JINGLE);
    }

    #[test]
    fn session_initiate_rewrites_empty_transport_with_issued_token() {
        let iq = session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c1");
        let jid = test_ctx_jid();
        let ctx = ctx(&jid);
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx);

        // Server does NOT pre-ACK; the responder's real client emits
        // the IQ-result. We just forward the rewritten stanza.
        assert_eq!(events.len(), 1, "expected forward only, got {events:?}");
        let OutboundEvent::RouteToConnection {
            jid: peer, stanza, ..
        } = events.into_iter().next().unwrap()
        else {
            panic!("expected RouteToConnection");
        };
        assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
        let Stanza::Iq(fwd) = *stanza else {
            panic!("expected Iq")
        };
        // Forwarded stanza's `from` must be the authenticated session.
        assert_eq!(
            fwd.from().map(|j| j.to_string()),
            Some("alice@waddle.test/desktop".to_string())
        );
        let Iq::Set { payload: elem, .. } = *fwd else {
            panic!("expected Iq set, got error or result")
        };
        let forwarded = Jingle::try_from(elem).expect("forwarded jingle reparses");
        let content = &forwarded.contents[0];
        let Some(Transport::Unknown(t)) = &content.transport else {
            panic!("expected Waddle transport, got other variant")
        };
        let parsed = WaddleLiveKitTransport::try_from(t).expect("transport parses");
        match parsed {
            WaddleLiveKitTransport::Issued(t) => {
                // Room is namespaced by initiator bare JID so an
                // attacker can't pick a sid that collides with a
                // victim's live call.
                assert_eq!(t.room.as_str(), "alice@waddle.test::c1");
                assert_eq!(t.identity.as_livekit_identity(), "bob@waddle.test/desktop");
                assert!(!t.token.as_str().is_empty());
            }
            WaddleLiveKitTransport::Request => panic!("server must populate the transport"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_token_mint_emits_info_with_bare_jid_and_counter() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(
            &muji_session_initiate_iq("general@muc.waddle.test"),
            &ctx(&jid),
        );

        assert!(
            !events.is_empty(),
            "successful Muji token mint must produce the focus response"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.sfu_token.minted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.sfu_token.minted"),
            Some("1".to_string())
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("general@muc.waddle.test"), "{logs}");
        assert!(logs.contains("alice@waddle.test"), "{logs}");
        assert!(logs.contains("\"level\":\"INFO\""), "{logs}");
        assert!(logs.contains("LiveKit SFU token minted"), "{logs}");
        assert!(!logs.contains("alice@waddle.test/desktop"), "{logs}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_spoofed_initiator_emits_not_authorized_denial() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::WARN)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let mut iq = muji_session_initiate_iq("general@muc.waddle.test");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected IQ set");
        };
        payload.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("initiator").to_owned(),
            "mallory@waddle.test/laptop",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.sfu_token.denied",
                &[("reason", "not_authorized")]
            ),
            Some(1)
        );
        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        assert!(logs.contains("\"level\":\"WARN\""), "{logs}");
        assert!(logs.contains("general@muc.waddle.test"), "{logs}");
        assert!(logs.contains("alice@waddle.test"), "{logs}");
        assert!(!logs.contains("alice@waddle.test/desktop"), "{logs}");
        assert!(!logs.contains("mallory@waddle.test/laptop"), "{logs}");
    }

    #[test]
    fn session_initiate_with_spoofed_initiator_is_rejected() {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("charlie@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "spoof".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_initiate_with_same_bare_different_resource_is_rejected() {
        let iq = session_initiate_iq(
            "alice@waddle.test/mobile",
            "bob@waddle.test/desktop",
            "same-bare-spoof",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_initiate_with_bare_initiator_is_rejected_on_p2p_path() {
        let iq = session_initiate_iq(
            "alice@waddle.test",
            "bob@waddle.test/desktop",
            "bare-initiator",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::Forbidden);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn session_accept_with_spoofed_responder_is_rejected() {
        let iq = session_accept_iq(
            "charlie@waddle.test/mobile",
            "alice@waddle.test/desktop",
            "c1",
        );
        let jid: FullJid = "bob@waddle.test/phone".parse().unwrap();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn session_accept_without_responder_uses_authenticated_sender() {
        let initiate =
            session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/phone", "c1");
        let mut iq = session_accept_iq("bob@waddle.test/phone", "alice@waddle.test/desktop", "c1");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected set");
        };
        let mut jingle = Jingle::try_from(payload.clone()).expect("fixture reparses");
        jingle.responder = None;
        *payload = jingle.into();
        let alice: FullJid = "alice@waddle.test/desktop".parse().unwrap();
        let jid: FullJid = "bob@waddle.test/phone".parse().unwrap();
        let sfu = fixture_livekit_sfu();
        let handler = JingleHandler::new(sfu);
        let initiate_events = handler.handle(&initiate, &ctx(&alice));
        assert!(
            initiate_events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "session-initiate should invite the responder"
        );

        let events = handler.handle(&iq, &ctx(&jid));

        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "missing responder should resolve to authenticated sender and route"
        );
    }

    #[test]
    fn third_party_session_accept_without_invitation_is_rejected() {
        let mut iq = session_accept_iq("eve@waddle.test/laptop", "alice@waddle.test/desktop", "c1");
        let Iq::Set { payload, .. } = &mut iq else {
            panic!("expected set");
        };
        let mut jingle = Jingle::try_from(payload.clone()).expect("fixture reparses");
        jingle.responder = None;
        *payload = jingle.into();
        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::try_new("alice@waddle.test::c1").unwrap();
        let handler = JingleHandler::new(sfu.clone());

        let events = handler.handle(&iq, &ctx(&eve));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert!(
            !sfu.has_call_participant(&call, &Identity::from_jid(eve)),
            "third-party accept must not register an uninvited participant"
        );
    }

    #[test]
    fn stale_participant_cannot_accept_reused_sid_without_current_invitation() {
        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::try_new("alice@waddle.test::c1").unwrap();
        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let eve_identity = Identity::from_jid(eve.clone());
        sfu.register_call_participant(&call, &eve_identity);
        assert!(
            sfu.has_call_participant(&call, &eve_identity),
            "fixture starts with Eve as a stale participant for the reused call id"
        );

        let handler = JingleHandler::new(sfu.clone());
        let alice: FullJid = "alice@waddle.test/desktop".parse().unwrap();
        let fresh_invite =
            session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/phone", "c1");
        let initiate_events = handler.handle(&fresh_invite, &ctx(&alice));
        assert!(
            initiate_events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "fresh invite should route to Bob"
        );

        let eve_accept =
            session_accept_iq("eve@waddle.test/laptop", "alice@waddle.test/desktop", "c1");
        let events = handler.handle(&eve_accept, &ctx(&eve));

        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn muji_session_initiate_rejects_bare_initiator() {
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("muji-bare".into()));
        jingle.initiator = Some("alice@waddle.test".parse().unwrap());
        let jid = test_ctx_jid();
        let err = resolve_muji_initiator(&jingle, &ctx(&jid))
            .expect_err("Muji path requires full initiator when present");
        assert!(matches!(err, InitiatorError::InitiatorMismatch));
    }

    #[test]
    fn unsupported_transport_emits_xep_0166_section_10_2_termination() {
        // XEP-0166 §10.2: a session-initiate with an unacceptable
        // transport MUST be rejected with an IQ-result ack followed
        // by a SEPARATE server-initiated session-terminate IQ
        // carrying `<reason><unsupported-transports/></reason>`.
        // Stanza errors like `<feature-not-implemented/>` are
        // NOT the right shape for Jingle negotiation failures
        // with a defined `<reason/>` condition.
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        // ICE-UDP transport — not supported by Waddle.
        content.transport = Some(Transport::Unknown(
            Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_eq!(
            events.len(),
            2,
            "expected ack + session-terminate per XEP-0166 §10.2"
        );

        // First stanza: empty IQ-result ack.
        let OutboundEvent::SendStanza(ack_stanza) = &events[0] else {
            panic!("expected SendStanza for ack");
        };
        let Stanza::Iq(ack_iq) = ack_stanza.as_ref() else {
            panic!("expected Iq for ack");
        };
        assert!(
            matches!(&**ack_iq, Iq::Result { payload: None, .. }),
            "ack must be an empty IQ result"
        );

        // Second stanza: server-initiated session-terminate.
        let OutboundEvent::SendStanza(term_stanza) = &events[1] else {
            panic!("expected SendStanza for terminate");
        };
        let Stanza::Iq(term_iq) = term_stanza.as_ref() else {
            panic!("expected Iq for terminate");
        };
        let Iq::Set {
            from, to, payload, ..
        } = &**term_iq
        else {
            panic!("expected IQ-set for the session-terminate");
        };
        assert_eq!(
            from.as_ref().map(ToString::to_string),
            Some("bob@waddle.test/desktop".to_string()),
            "unsupported-transport terminate must come from the addressed peer"
        );
        assert_eq!(
            to.as_ref().map(ToString::to_string),
            Some("alice@waddle.test/desktop".to_string()),
            "unsupported-transport terminate must be addressed back to requester"
        );
        let term_jingle =
            Jingle::try_from(payload.clone()).expect("session-terminate Jingle reparses");
        assert_eq!(
            term_jingle.action,
            xmpp_parsers::jingle::Action::SessionTerminate
        );
        let reason = term_jingle
            .reason
            .as_ref()
            .expect("session-terminate must carry a <reason/>");
        assert!(
            matches!(
                reason.reason,
                xmpp_parsers::jingle::Reason::UnsupportedTransports
            ),
            "reason must be <unsupported-transports/> per XEP-0166 §7.4"
        );
    }

    #[test]
    fn session_terminate_routes_to_peer_and_unregisters() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t1".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        // Pre-register under the scoped id so the unregister path
        // has something to remove.
        let call = waddle_sfu::CallId::try_new("alice@waddle.test::c1").unwrap();
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        assert_eq!(sfu.participant_count(&call), 2);

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&jid));
        assert_eq!(sfu.participant_count(&call), 0);

        // No server-forged ACK — just the forwarded terminate.
        assert_eq!(events.len(), 1);
        let mut routed = false;
        for ev in events {
            if let OutboundEvent::RouteToConnection { jid: peer, .. } = ev {
                assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
                routed = true;
            }
        }
        assert!(routed);
    }

    #[test]
    fn session_terminate_does_not_unregister_peer_when_sender_is_not_participant() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("eve@waddle.test/laptop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t-third-party".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        let call = waddle_sfu::CallId::try_new("bob@waddle.test::c1").unwrap();
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&call, &alice);
        sfu.register_call_participant(&call, &bob);
        assert_eq!(sfu.participant_count(&call), 2);

        let eve: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&eve));

        assert_eq!(sfu.participant_count(&call), 2);
        assert_error_condition(&events, DefinedCondition::Forbidden);
    }

    #[test]
    fn session_terminate_requires_sender_and_peer_in_same_call() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("eve@waddle.test/laptop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t-cross-call".into(),
            payload: jingle.into(),
        };

        let sfu = fixture_livekit_sfu();
        let eve_call = waddle_sfu::CallId::try_new("eve@waddle.test::c1").unwrap();
        let bob_call = waddle_sfu::CallId::try_new("alice@waddle.test::c1").unwrap();
        let eve = Identity::from_jid("eve@waddle.test/laptop".parse().unwrap());
        let mallory = Identity::from_jid("mallory@waddle.test/phone".parse().unwrap());
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        let bob = Identity::from_jid("bob@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&eve_call, &eve);
        sfu.register_call_participant(&eve_call, &mallory);
        sfu.register_call_participant(&bob_call, &alice);
        sfu.register_call_participant(&bob_call, &bob);

        let eve_jid: FullJid = "eve@waddle.test/laptop".parse().unwrap();
        let handler = JingleHandler::new(sfu.clone());
        let events = handler.handle(&iq, &ctx(&eve_jid));

        assert_error_condition(&events, DefinedCondition::Forbidden);
        assert_eq!(sfu.participant_count(&eve_call), 2);
        assert_eq!(sfu.participant_count(&bob_call), 2);
    }

    #[test]
    fn session_terminate_to_bare_peer_is_rejected() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test".parse().unwrap()),
            id: "t-bare".into(),
            payload: jingle.into(),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::BadRequest);
    }

    #[test]
    fn malformed_jingle_payload_returns_bad_request() {
        // Right namespace, wrong element name — Jingle::try_from rejects.
        let bogus = Element::builder("garbage", NS_JINGLE).build();
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "m1".into(),
            payload: bogus,
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn bare_to_jid_returns_bad_request() {
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            WaddleLiveKitTransport::Request.to_element(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().expect("valid full jid")),
            // Bare JID — no resource.
            to: Some("bob@waddle.test".parse().expect("valid jid")),
            id: "b1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!()
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let Iq::Error { error: err, .. } = *reply else {
            panic!("expected error")
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn transport_info_forwards_unchanged_with_sanitised_from() {
        let mut jingle = Jingle::new(Action::TransportInfo, SessionId("c1".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        let iq = Iq::Set {
            // Spoofed: client claims `iq.from` is charlie's.
            from: Some(
                "charlie@waddle.test/desktop"
                    .parse()
                    .expect("valid full jid"),
            ),
            to: Some("bob@waddle.test/desktop".parse().expect("valid full jid")),
            id: "ti1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        assert_eq!(events.len(), 1, "no server-forged ACK on transport-info");
        let OutboundEvent::RouteToConnection {
            jid: peer, stanza, ..
        } = events.into_iter().next().unwrap()
        else {
            panic!("expected RouteToConnection");
        };
        assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
        let Stanza::Iq(fwd) = *stanza else { panic!() };
        // route_unchanged must overwrite the spoofed `from`.
        assert_eq!(
            fwd.from().map(|j| j.to_string()),
            Some("alice@waddle.test/desktop".to_string())
        );
    }

    #[test]
    fn cross_domain_jingle_returns_feature_not_implemented() {
        let iq = session_initiate_iq("alice@waddle.test/desktop", "bob@other.test/desktop", "c1");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = reply.as_ref() else {
                        panic!("expected error")
                    };
                    assert_eq!(
                        err.defined_condition,
                        DefinedCondition::FeatureNotImplemented,
                        "federation is intentionally not supported yet"
                    );
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    #[test]
    fn same_domain_p2p_jingle_still_passes_federation_guard() {
        // Regression for the relaxed federation guard: a P2P
        // session-initiate between two same-apex-domain accounts
        // must continue to forward to the peer connection without
        // tripping `feature-not-implemented`. Muji equivalents live
        // in the XEP-0272 dedicated test suite at
        // `crates/waddle-xmpp/tests/xep0272_muji.rs` per the
        // CLAUDE.md "XEP custom test-suite hard rule."
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "p2p-1",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        let no_feature_not_implemented = !events.iter().any(|ev| match ev {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => matches!(
                    reply.as_ref(),
                    Iq::Error { error, .. }
                        if error.defined_condition == DefinedCondition::FeatureNotImplemented
                ),
                _ => false,
            },
            _ => false,
        });
        assert!(
            no_feature_not_implemented,
            "same-apex P2P must not trip the federation guard: {events:?}",
        );
        assert!(
            events
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "expected forward to peer, got: {events:?}",
        );
    }

    #[test]
    fn rate_limited_session_initiate_returns_policy_violation() {
        use std::time::Duration;
        // Tight budget: 1 initiate per 30s.
        let rl = Arc::new(SessionInitiateRateLimit::new(1, Duration::from_secs(30)));
        let handler = JingleHandler::with_rate_limit(fixture_sfu(), rl);
        let jid = test_ctx_jid();
        let ctx = ctx(&jid);

        let first = handler.handle(
            &session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c1"),
            &ctx,
        );
        assert!(
            first
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "first initiate forwards normally"
        );

        // Second initiate within the window must be rejected with
        // policy-violation; no forward, no SFU registration.
        let second = handler.handle(
            &session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "c2"),
            &ctx,
        );
        assert_eq!(second.len(), 1);
        match &second[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error reply, got {reply:?}");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::PolicyViolation);
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn rate_limited_session_terminate_returns_policy_violation() {
        let sfu = fixture_livekit_sfu();
        register_dm_call(
            &sfu,
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "t1",
        );
        register_dm_call(
            &sfu,
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "t2",
        );
        let handler = JingleHandler::with_rate_limits(
            sfu,
            Arc::new(SessionInitiateRateLimit::with_defaults()),
            Arc::new(TerminateRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
            Arc::new(MujiActionRateLimit::with_defaults()),
        );
        let jid = test_ctx_jid();

        let first = handler.handle(
            &session_terminate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "t1"),
            &ctx(&jid),
        );
        assert!(
            first
                .iter()
                .any(|ev| matches!(ev, OutboundEvent::RouteToConnection { .. })),
            "first terminate should still route"
        );

        let second = handler.handle(
            &session_terminate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", "t2"),
            &ctx(&jid),
        );
        assert_error_condition(&second, DefinedCondition::PolicyViolation);
    }

    #[test]
    fn rate_limited_muji_non_initiate_action_returns_policy_violation() {
        let handler = JingleHandler::with_rate_limits(
            fixture_sfu(),
            Arc::new(SessionInitiateRateLimit::with_defaults()),
            Arc::new(TerminateRateLimit::with_defaults()),
            Arc::new(MujiActionRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
        );
        let jid = test_ctx_jid();

        let first = handler.handle(
            &muji_transport_info_iq("general@muc.waddle.test", "muji-ti-1"),
            &ctx(&jid),
        );
        assert_error_condition(&first, DefinedCondition::BadRequest);

        let second = handler.handle(
            &muji_transport_info_iq("general@muc.waddle.test", "muji-ti-2"),
            &ctx(&jid),
        );
        assert_error_condition(&second, DefinedCondition::PolicyViolation);
    }

    #[test]
    fn missing_to_returns_bad_request() {
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle
            .contents
            .push(Content::new(Creator::Initiator, ContentId("audio".into())));
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: None,
            id: "i1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let Iq::Error { error: err, .. } = &**reply else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }

    // ------------------------------------------------------------------
    // #1452 — call setup success-rate counters (`waddle.call.setup.*`).
    //
    // Asserted through the in-memory reader seam, never instrument
    // internals. Every case pins that `attempted` is the true
    // denominator: exactly one terminal `ok`/`failed` per attempt, and
    // non-initiate actions never open one.
    // ------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn session_initiate_records_attempted_and_defers_ok_to_the_router() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ok",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        // #1488: the handler no longer counts `ok` at emit time — the
        // route disposition is unknown here. It counts `attempted` and
        // hands the open attempt to the routing interpreter as a
        // ticket on the routing effect.
        assert!(
            matches!(
                events[0],
                OutboundEvent::RouteToConnection {
                    call_setup: Some(_),
                    ..
                }
            ),
            "{events:?}"
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.failed", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.metric_unit("waddle.call.setup.attempted"),
            Some("1".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_initiate_ticket_closes_the_attempt_as_ok_when_delivered() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ticket-ok",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let event = handler
            .handle(&iq, &ctx(&jid))
            .into_iter()
            .next()
            .expect("one event");
        let OutboundEvent::RouteToConnection {
            call_setup: Some(ticket),
            ..
        } = event
        else {
            panic!("expected a routed invite carrying a call-setup ticket: {event:?}");
        };

        ticket.delivered();

        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
        assert_eq!(
            metrics.metric_unit("waddle.call.setup.ok"),
            Some("1".to_string())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routed_initiate_ticket_closes_the_attempt_as_peer_unavailable() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "setup-ticket-unroutable",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let event = handler
            .handle(&iq, &ctx(&jid))
            .into_iter()
            .next()
            .expect("one event");
        let OutboundEvent::RouteToConnection {
            call_setup: Some(ticket),
            ..
        } = event
        else {
            panic!("expected a routed invite carrying a call-setup ticket: {event:?}");
        };

        ticket.undeliverable();

        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "peer_unavailable")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_accept_is_not_counted_as_a_setup_attempt() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let handler = JingleHandler::new(fixture_sfu());
        let jid = test_ctx_jid();
        // Seed the pending invite the accept path requires.
        let initiate = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@waddle.test/desktop",
            "accept-not-setup",
        );
        handler.handle(&initiate, &ctx(&jid));

        let bob: FullJid = "bob@waddle.test/desktop".parse().unwrap();
        let accept = session_accept_iq(
            "bob@waddle.test/desktop",
            "alice@waddle.test/desktop",
            "accept-not-setup",
        );
        handler.handle(&accept, &ctx(&bob));

        // Only the initiate counted; the accept added nothing. `ok`
        // stays 0 at the handler seam — the routed initiate defers it
        // to the interpreter (#1488).
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_domain_session_initiate_records_a_federation_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = session_initiate_iq(
            "alice@waddle.test/desktop",
            "bob@other.example/desktop",
            "setup-federated",
        );
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::FeatureNotImplemented);
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "federation_unsupported")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn foreign_transport_session_initiate_records_an_unsupported_transport_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let mut content = Content::new(Creator::Initiator, ContentId("audio".into()));
        content.description = Some(xmpp_parsers::jingle::Description::Rtp(
            opus_audio_description(),
        ));
        content.transport = Some(Transport::Unknown(
            Element::builder("transport", "urn:xmpp:jingle:transports:ice-udp:1").build(),
        ));
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("setup-transport".into()));
        jingle.initiator = Some("alice@waddle.test/desktop".parse().unwrap());
        jingle.contents.push(content);
        let iq = Iq::Set {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "i1".into(),
            payload: jingle.into(),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        handler.handle(&iq, &ctx(&jid));

        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum(
                "waddle.call.setup.failed",
                &[("reason", "unsupported_transport")]
            ),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rate_limited_session_initiate_records_a_rate_limited_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let handler = JingleHandler::with_rate_limit(
            fixture_sfu(),
            Arc::new(SessionInitiateRateLimit::new(
                1,
                std::time::Duration::from_secs(30),
            )),
        );
        let jid = test_ctx_jid();
        for sid in ["rl-1", "rl-2"] {
            let iq =
                session_initiate_iq("alice@waddle.test/desktop", "bob@waddle.test/desktop", sid);
            handler.handle(&iq, &ctx(&jid));
        }

        // Two attempts: the first was routed (its `ok`/`failed` close
        // is deferred to the interpreter, #1488), the second was
        // rejected by the limiter — which counts the attempted/failed
        // pair itself because the per-attempt tracker never opens.
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(2)
        );
        assert_eq!(
            metrics
                .counter_sum("waddle.call.setup.ok", &[])
                .unwrap_or(0),
            0
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "rate_limited")]),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_session_initiate_records_an_attempted_and_ok_setup() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = muji_session_initiate_iq("general@muc.waddle.test");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_eq!(events.len(), 2, "ack + session-accept: {events:?}");
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(metrics.counter_sum("waddle.call.setup.ok", &[]), Some(1));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn muji_session_initiate_to_a_foreign_room_records_a_bad_request_failure() {
        let metrics = crate::telemetry::test_support::acquire().await;
        let iq = muji_session_initiate_iq("general@muc.other.example");
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());

        let events = handler.handle(&iq, &ctx(&jid));

        assert_error_condition(&events, DefinedCondition::BadRequest);
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.attempted", &[]),
            Some(1)
        );
        assert_eq!(
            metrics.counter_sum("waddle.call.setup.failed", &[("reason", "bad_request")]),
            Some(1)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn minted_token_log_carries_the_bounded_call_correlation_id() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _subscriber = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .json()
                .with_max_level(tracing::Level::INFO)
                .with_writer(CaptureWriter(buffer.clone()))
                .finish(),
        );
        let iq = muji_session_initiate_iq("general@muc.waddle.test");
        let jid = test_ctx_jid();
        JingleHandler::new(fixture_sfu()).handle(&iq, &ctx(&jid));

        let logs = String::from_utf8(buffer.lock().expect("capture buffer lock").clone())
            .expect("captured logs are UTF-8");
        let expected =
            waddle_sfu::CallCorrelationId::for_room_name("general@muc.waddle.test").to_string();
        assert!(logs.contains("LiveKit SFU token minted"), "{logs}");
        assert!(logs.contains(&expected), "{logs}");
        assert_eq!(
            expected.len(),
            waddle_sfu::CORRELATION_ID_HEX_LEN,
            "{expected}"
        );
    }
}

mod implementation;
pub use implementation::calls_mixer_jid;
pub use implementation::room_is_on_local_muc_service;
use implementation::scoped_call_id;
pub use implementation::JingleHandler;
pub use implementation::MIXER_LOCALPART;
pub use implementation::NS_COIN;

#[cfg(test)]
use implementation::{resolve_muji_initiator, InitiatorError};
