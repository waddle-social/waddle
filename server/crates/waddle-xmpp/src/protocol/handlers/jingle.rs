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

use std::sync::Arc;

use jid::Jid;
use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::jingle::{Action, Content, Jingle, Transport};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{Identity, MediaCapabilities, SfuService};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep0166::NS_JINGLE;
use crate::xep::xep_waddle_livekit_transport::{
    IssuedTransport, WaddleLiveKitTransport, NS_WADDLE_LIVEKIT_TRANSPORT,
};
use crate::Stanza;

#[derive(Clone)]
pub struct JingleHandler {
    sfu: Arc<dyn SfuService>,
}

impl std::fmt::Debug for JingleHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JingleHandler").finish_non_exhaustive()
    }
}

impl JingleHandler {
    pub fn new(sfu: Arc<dyn SfuService>) -> Self {
        Self { sfu }
    }
}

impl IqHandler for JingleHandler {
    fn namespace(&self) -> &'static str {
        NS_JINGLE
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let Some(jingle_elem) = iq_set_jingle(iq) else {
            return error_reply(
                iq,
                ctx,
                DefinedCondition::BadRequest,
                "expected IQ set with a <jingle/> payload",
            );
        };

        let jingle = match Jingle::try_from(jingle_elem.clone()) {
            Ok(j) => j,
            Err(_) => {
                return error_reply(
                    iq,
                    ctx,
                    DefinedCondition::BadRequest,
                    "malformed <jingle/> stanza",
                );
            }
        };

        let Some(peer) = iq.to.clone() else {
            return error_reply(
                iq,
                ctx,
                DefinedCondition::BadRequest,
                "Jingle stanza missing 'to' attribute",
            );
        };

        match jingle.action {
            Action::SessionInitiate | Action::SessionAccept => {
                self.handle_session_negotiation(iq, jingle, peer, ctx)
            }
            Action::SessionTerminate => self.handle_session_terminate(iq, jingle, peer, ctx),
            Action::SessionInfo
            | Action::TransportInfo
            | Action::ContentAdd
            | Action::ContentRemove
            | Action::ContentModify
            | Action::ContentAccept
            | Action::ContentReject
            | Action::TransportAccept
            | Action::TransportReject
            | Action::TransportReplace
            | Action::DescriptionInfo
            | Action::SecurityInfo => self.route_unchanged(iq, peer, ctx),
        }
    }
}

impl JingleHandler {
    /// Rewrite every Waddle LiveKit transport in the jingle stanza
    /// with a freshly-issued LiveKit join token for `peer` (the
    /// recipient of the forwarded stanza), then forward to that peer
    /// and ACK the requester. Used for session-initiate and
    /// session-accept.
    fn handle_session_negotiation(
        &self,
        iq: &Iq,
        mut jingle: Jingle,
        peer: Jid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let peer_identity = match peer.clone().try_into_full() {
            Ok(full) => Identity::from_jid(full),
            Err(_) => {
                return error_reply(
                    iq,
                    ctx,
                    DefinedCondition::BadRequest,
                    "Jingle 'to' must be a full JID (resource required)",
                );
            }
        };

        // Each <content/> must carry a Waddle LiveKit transport. We
        // rewrite empty (request) transports with issued tokens scoped
        // to (sid, peer); leave already-issued transports alone (the
        // responder may have populated their own copy).
        for content in &mut jingle.contents {
            match rewrite_content_transport(content, &jingle.sid.0, &peer_identity, &*self.sfu) {
                Ok(()) => {}
                Err(reason) => return reason.into_error_reply(iq, ctx),
            }
        }

        // Register the peer with the in-memory call registry so the
        // MUC focus path knows about active calls. Call-id is the
        // Jingle sid.
        if let Ok(call_id) = waddle_sfu::CallId::new(jingle.sid.0.clone()) {
            self.sfu.register_call_participant(&call_id, &peer_identity);
        }

        let forwarded_elem: Element = jingle.into();
        let forwarded_iq = Iq {
            from: iq.from.clone(),
            to: Some(peer),
            id: iq.id.clone(),
            payload: IqType::Set(forwarded_elem),
        };

        let ack = empty_iq_result_for(iq, ctx.domain);
        vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(ack))),
            OutboundEvent::RouteToConnection {
                jid: forwarded_iq
                    .to
                    .clone()
                    .unwrap_or_else(|| ctx.full_jid.clone().into()),
                stanza: Box::new(Stanza::Iq(forwarded_iq)),
            },
        ]
    }

    fn handle_session_terminate(
        &self,
        iq: &Iq,
        jingle: Jingle,
        peer: Jid,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        if let Ok(call_id) = waddle_sfu::CallId::new(jingle.sid.0.clone()) {
            let _ = self
                .sfu
                .unregister_call_participant(&call_id, &Identity::from_jid(ctx.full_jid.clone()));
        }
        self.route_unchanged(iq, peer, ctx)
    }

    /// Forward the stanza verbatim and ACK. Used for transport-info,
    /// session-info, content-* and the like that don't need server
    /// mediation.
    fn route_unchanged(&self, iq: &Iq, peer: Jid, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let ack = empty_iq_result_for(iq, ctx.domain);
        vec![
            OutboundEvent::SendStanza(Box::new(Stanza::Iq(ack))),
            OutboundEvent::RouteToConnection {
                jid: peer,
                stanza: Box::new(Stanza::Iq(iq.clone())),
            },
        ]
    }
}

enum RewriteError {
    UnsupportedTransport,
    InvalidWaddleTransport(String),
    SfuFailed(String),
}

impl RewriteError {
    fn into_error_reply(self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        match self {
            // XEP-0166 §10.2: <unsupported-transports/> is signalled
            // as a Jingle-specific application condition. We surface
            // it through the generic feature-not-implemented stanza
            // error because xmpp_parsers' StanzaError model doesn't
            // carry the Jingle-side application condition directly.
            Self::UnsupportedTransport => error_reply(
                iq,
                ctx,
                DefinedCondition::FeatureNotImplemented,
                "only urn:waddle:transports:livekit:0 is supported",
            ),
            Self::InvalidWaddleTransport(msg) => {
                error_reply(iq, ctx, DefinedCondition::BadRequest, &msg)
            }
            Self::SfuFailed(msg) => {
                error_reply(iq, ctx, DefinedCondition::InternalServerError, &msg)
            }
        }
    }
}

fn rewrite_content_transport(
    content: &mut Content,
    call_sid: &str,
    peer_identity: &Identity,
    sfu: &dyn SfuService,
) -> Result<(), RewriteError> {
    let Some(Transport::Unknown(transport_elem)) = &content.transport else {
        return Err(RewriteError::UnsupportedTransport);
    };
    if transport_elem.ns() != NS_WADDLE_LIVEKIT_TRANSPORT {
        return Err(RewriteError::UnsupportedTransport);
    }
    let parsed = WaddleLiveKitTransport::try_from(transport_elem)
        .map_err(|e| RewriteError::InvalidWaddleTransport(e.to_string()))?;
    match parsed {
        WaddleLiveKitTransport::Issued(_) => Ok(()),
        WaddleLiveKitTransport::Request => {
            let call_id = waddle_sfu::CallId::new(call_sid.to_string())
                .map_err(|e| RewriteError::InvalidWaddleTransport(e.to_string()))?;
            let token = sfu
                .issue_join_token(
                    &call_id,
                    peer_identity,
                    MediaCapabilities::full_participant(),
                )
                .map_err(|e| RewriteError::SfuFailed(e.to_string()))?;
            let issued = WaddleLiveKitTransport::Issued(IssuedTransport {
                url: token.url,
                room: token.room,
                identity: token.identity,
                token: token.jwt,
            });
            content.transport = Some(Transport::Unknown(issued.to_element()));
            Ok(())
        }
    }
}

fn iq_set_jingle(iq: &Iq) -> Option<&Element> {
    match &iq.payload {
        IqType::Set(elem) if elem.name() == "jingle" && elem.ns() == NS_JINGLE => Some(elem),
        _ => None,
    }
}

fn empty_iq_result_for(original: &Iq, _domain: &str) -> Iq {
    Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Result(None),
    }
}

fn error_reply(
    original: &Iq,
    _ctx: &StanzaContext<'_>,
    cond: DefinedCondition,
    text: &str,
) -> Vec<OutboundEvent> {
    let err = Iq {
        from: original.to.clone(),
        to: original.from.clone(),
        id: original.id.clone(),
        payload: IqType::Error(StanzaError::new(ErrorType::Cancel, cond, "en", text)),
    };
    vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(err)))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0166;
    use crate::xep::xep0167::opus_audio_description;
    use chrono::Duration;
    use jid::FullJid;
    use waddle_sfu::{
        ApiKey, ApiSecret, LiveKitSfu, SfuConfig, TurnHost, TurnSharedSecret, WebsocketUrl,
    };
    use xmpp_parsers::jingle::{Content, ContentId, Creator, SessionId};

    fn fixture_sfu() -> Arc<dyn SfuService> {
        let cfg = SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min"),
            ws_url: WebsocketUrl::new("wss://livekit.test/".parse().unwrap()).unwrap(),
            turn_host: TurnHost::new("turn.test"),
            turn_tls_port: 443,
            turn_udp_port: 3478,
            turn_shared_secret: TurnSharedSecret::from_text("turn-secret"),
            token_ttl: Duration::seconds(3600),
            turn_ttl: Duration::seconds(3600),
        };
        Arc::new(LiveKitSfu::new(cfg))
    }

    fn test_ctx_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
        StanzaContext {
            domain: "waddle.test",
            full_jid: jid,
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
        Iq {
            from: Some(initiator.parse().unwrap()),
            to: Some(responder.parse().unwrap()),
            id: "i1".into(),
            payload: IqType::Set(jingle.into()),
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

        // Expect: ACK to initiator + forwarded stanza to responder.
        assert_eq!(events.len(), 2, "expected ack + forward, got {events:?}");

        let mut ack_seen = false;
        let mut forward_seen = false;
        for ev in events {
            match ev {
                OutboundEvent::SendStanza(stanza) => {
                    if let Stanza::Iq(reply) = *stanza {
                        assert!(matches!(reply.payload, IqType::Result(_)));
                        ack_seen = true;
                    }
                }
                OutboundEvent::RouteToConnection { jid: peer, stanza } => {
                    assert_eq!(peer.to_string(), "bob@waddle.test/desktop");
                    let Stanza::Iq(fwd) = *stanza else {
                        panic!("expected Iq")
                    };
                    let IqType::Set(elem) = fwd.payload else {
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
                            assert_eq!(t.room.as_str(), "c1");
                            assert_eq!(t.identity.as_livekit_identity(), "bob@waddle.test/desktop");
                            assert!(!t.token.as_str().is_empty());
                        }
                        WaddleLiveKitTransport::Request => {
                            panic!("server must populate the transport")
                        }
                    }
                    forward_seen = true;
                }
                _ => panic!("unexpected event: {ev:?}"),
            }
        }
        assert!(ack_seen);
        assert!(forward_seen);
    }

    #[test]
    fn unsupported_transport_returns_feature_not_implemented() {
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
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "i1".into(),
            payload: IqType::Set(jingle.into()),
        };

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));

        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let IqType::Error(err) = &reply.payload else {
                        panic!("expected error reply")
                    };
                    assert_eq!(
                        err.defined_condition,
                        DefinedCondition::FeatureNotImplemented
                    );
                }
                other => panic!("expected Iq, got {other:?}"),
            },
            other => panic!("expected SendStanza, got {other:?}"),
        }
    }

    #[test]
    fn session_terminate_routes_to_peer_and_unregisters() {
        let jingle = Jingle::new(Action::SessionTerminate, SessionId("c1".into())).set_reason(
            xep0166::reason_element(xmpp_parsers::jingle::Reason::Success),
        );
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("bob@waddle.test/desktop".parse().unwrap()),
            id: "t1".into(),
            payload: IqType::Set(jingle.into()),
        };

        let sfu = fixture_sfu();
        // pre-register so the unregister path has something to remove
        let call = waddle_sfu::CallId::new("c1").unwrap();
        let alice = Identity::from_jid("alice@waddle.test/desktop".parse().unwrap());
        sfu.register_call_participant(&call, &alice);

        let jid = test_ctx_jid();
        let handler = JingleHandler::new(sfu);
        let events = handler.handle(&iq, &ctx(&jid));

        // ack + forward
        assert_eq!(events.len(), 2);
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
    fn missing_to_returns_bad_request() {
        let mut jingle = Jingle::new(Action::SessionInitiate, SessionId("c1".into()));
        jingle
            .contents
            .push(Content::new(Creator::Initiator, ContentId("audio".into())));
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: None,
            id: "i1".into(),
            payload: IqType::Set(jingle.into()),
        };
        let jid = test_ctx_jid();
        let handler = JingleHandler::new(fixture_sfu());
        let events = handler.handle(&iq, &ctx(&jid));
        match &events[0] {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Iq(reply) => {
                    let IqType::Error(err) = &reply.payload else {
                        panic!("expected error");
                    };
                    assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
                }
                _ => panic!("expected Iq"),
            },
            _ => panic!("expected SendStanza"),
        }
    }
}
