//! Waddle group-call IQ handler — `urn:waddle:muc-call:0`.
//!
//! Wire shape:
//!
//! ```xml
//! <!-- Join the room's group call: -->
//! <iq type='set' to='room@muc.host' id='j1'>
//!   <request-join xmlns='urn:waddle:muc-call:0' room='room@muc.host'/>
//! </iq>
//!
//! <!-- Response: -->
//! <iq type='result' id='j1'>
//!   <joined xmlns='urn:waddle:muc-call:0'>
//!     <transport xmlns='urn:waddle:transports:livekit:0'>
//!       <url>…</url><room>…</room><identity>…</identity><token>…</token>
//!     </transport>
//!   </joined>
//! </iq>
//!
//! <!-- Leave the group call (unregister + revoke previously-issued
//!      tokens): -->
//! <iq type='set' to='room@muc.host' id='l1'>
//!   <request-leave xmlns='urn:waddle:muc-call:0' room='room@muc.host'/>
//! </iq>
//! ```
//!
//! The room JID is the SFU `CallId`. Every occupant who joins the
//! call shares the same room, distinguished by their full JID as
//! the LiveKit `identity`. This handler is the *token surface*; the
//! `<call xmlns='urn:waddle:muc-call:0'/>` presence extension lives
//! in [`crate::xep::xep_waddle_muc_call`] and is broadcast through
//! the MUC presence pipeline so non-participants see "in-call"
//! indicators.

use std::sync::Arc;

use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use waddle_sfu::{CallId, Identity, MediaCapabilities, SfuService};

use crate::protocol::event::{OutboundEvent, StanzaContext};
use crate::protocol::traits::IqHandler;
use crate::xep::xep_waddle_livekit_transport::{IssuedTransport, WaddleLiveKitTransport};
use crate::xep::xep_waddle_muc_call::NS_WADDLE_MUC_CALL;
use crate::Stanza;

const REQUEST_JOIN: &str = "request-join";
const REQUEST_LEAVE: &str = "request-leave";
const JOINED: &str = "joined";
const ATTR_ROOM: &str = "room";

#[derive(Clone)]
pub struct MucCallHandler {
    sfu: Arc<dyn SfuService>,
}

impl std::fmt::Debug for MucCallHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MucCallHandler").finish_non_exhaustive()
    }
}

impl MucCallHandler {
    pub fn new(sfu: Arc<dyn SfuService>) -> Self {
        Self { sfu }
    }
}

impl IqHandler for MucCallHandler {
    fn namespace(&self) -> &'static str {
        NS_WADDLE_MUC_CALL
    }

    fn handle(&self, iq: &Iq, ctx: &StanzaContext<'_>) -> Vec<OutboundEvent> {
        let IqType::Set(payload) = &iq.payload else {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "MUC call IQ must be type='set'",
            );
        };
        if payload.ns() != NS_WADDLE_MUC_CALL {
            return error_reply(
                iq,
                DefinedCondition::BadRequest,
                "expected child in urn:waddle:muc-call:0",
            );
        }
        match payload.name() {
            REQUEST_JOIN => self.handle_join(iq, payload, ctx),
            REQUEST_LEAVE => self.handle_leave(iq, payload, ctx),
            other => error_reply(
                iq,
                DefinedCondition::BadRequest,
                &format!("unsupported muc-call element: {other}"),
            ),
        }
    }
}

impl MucCallHandler {
    fn parse_room(&self, payload: &Element) -> Result<CallId, &'static str> {
        let room = payload.attr(ATTR_ROOM).ok_or("missing 'room' attribute")?;
        CallId::new(room.to_string()).map_err(|_| "invalid room JID")
    }

    fn handle_join(
        &self,
        iq: &Iq,
        payload: &Element,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let call_id = match self.parse_room(payload) {
            Ok(c) => c,
            Err(text) => return error_reply(iq, DefinedCondition::BadRequest, text),
        };
        // MUC membership enforcement happens upstream in the
        // waddle-server IQ dispatch path, before the dispatcher
        // reaches this handler — see `handle_sans_io_iq` and the
        // pre-dispatch room-actor occupancy check. By the time
        // execution reaches this handler the caller is known to be
        // a current occupant of `call_id`'s room (or the request
        // would never have been dispatched). What we DO enforce
        // here is that the authenticated session matches the
        // LiveKit identity: a peer can't mint a token claiming to
        // be someone else.
        let identity = Identity::from_jid(ctx.full_jid.clone());

        let token = match self.sfu.issue_join_token(
            &call_id,
            &identity,
            MediaCapabilities::full_participant(),
        ) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "muc-call join: SFU token mint failed");
                return error_reply(iq, DefinedCondition::InternalServerError, "internal error");
            }
        };
        self.sfu.register_call_participant(&call_id, &identity);

        let issued = WaddleLiveKitTransport::Issued(IssuedTransport {
            url: token.url.clone(),
            room: token.room.clone(),
            identity: token.identity.clone(),
            token: token.jwt.clone(),
        });
        let joined = Element::builder(JOINED, NS_WADDLE_MUC_CALL)
            .append(issued.to_element())
            .build();
        let reply = Iq {
            from: iq.to.clone(),
            to: iq.from.clone(),
            id: iq.id.clone(),
            payload: IqType::Result(Some(joined)),
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(reply)))]
    }

    fn handle_leave(
        &self,
        iq: &Iq,
        payload: &Element,
        ctx: &StanzaContext<'_>,
    ) -> Vec<OutboundEvent> {
        let call_id = match self.parse_room(payload) {
            Ok(c) => c,
            Err(text) => return error_reply(iq, DefinedCondition::BadRequest, text),
        };
        let identity = Identity::from_jid(ctx.full_jid.clone());
        let _ = self.sfu.unregister_call_participant(&call_id, &identity);
        let reply = Iq {
            from: iq.to.clone(),
            to: iq.from.clone(),
            id: iq.id.clone(),
            payload: IqType::Result(None),
        };
        vec![OutboundEvent::SendStanza(Box::new(Stanza::Iq(reply)))]
    }
}

fn error_reply(original: &Iq, cond: DefinedCondition, text: &str) -> Vec<OutboundEvent> {
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
    use chrono::Duration;
    use jid::FullJid;
    use waddle_sfu::{
        ApiKey, ApiSecret, LiveKitSfu, SfuConfig, TurnHost, TurnSharedSecret, WebsocketUrl,
    };

    fn fixture_sfu() -> Arc<LiveKitSfu> {
        let cfg = SfuConfig {
            api_key: ApiKey::new("APIxxxxxxxx"),
            api_secret: ApiSecret::from_text("super-secret-secret-32-bytes-min")
                .expect("test secret meets min length"),
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

    fn test_jid() -> FullJid {
        "alice@waddle.test/desktop".parse().unwrap()
    }

    fn ctx<'a>(jid: &'a FullJid) -> StanzaContext<'a> {
        StanzaContext {
            domain: "waddle.test",
            full_jid: jid,
        }
    }

    fn request_join_iq(room: &str) -> Iq {
        let payload = Element::builder(REQUEST_JOIN, NS_WADDLE_MUC_CALL)
            .attr(ATTR_ROOM, room)
            .build();
        Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some(room.parse().unwrap()),
            id: "j1".into(),
            payload: IqType::Set(payload),
        }
    }

    #[test]
    fn handler_namespace_is_muc_call() {
        let sfu: Arc<dyn SfuService> = fixture_sfu();
        assert_eq!(MucCallHandler::new(sfu).namespace(), NS_WADDLE_MUC_CALL);
    }

    #[test]
    fn request_join_mints_token_and_registers_participant() {
        let sfu = fixture_sfu();
        let handler = MucCallHandler::new(sfu.clone());
        let jid = test_jid();
        let events = handler.handle(&request_join_iq("room@muc.test"), &ctx(&jid));
        assert_eq!(events.len(), 1);
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!("expected SendStanza");
        };
        let Stanza::Iq(reply) = *stanza else {
            panic!("expected Iq")
        };
        let IqType::Result(Some(elem)) = reply.payload else {
            panic!("expected result with body, got {:?}", reply.payload);
        };
        assert_eq!(elem.name(), JOINED);
        assert_eq!(elem.ns(), NS_WADDLE_MUC_CALL);
        let transport = elem
            .children()
            .find(|c| c.name() == "transport")
            .expect("response embeds the issued transport");
        let parsed = WaddleLiveKitTransport::try_from(transport).expect("transport parses");
        let WaddleLiveKitTransport::Issued(issued) = parsed else {
            panic!("expected an issued transport, not request placeholder");
        };
        assert_eq!(issued.room.as_str(), "room@muc.test");
        assert_eq!(
            issued.identity.as_livekit_identity(),
            "alice@waddle.test/desktop"
        );
        assert_eq!(
            sfu.participant_count(&CallId::new("room@muc.test").unwrap()),
            1
        );
    }

    #[test]
    fn request_leave_unregisters_participant() {
        let sfu = fixture_sfu();
        let handler = MucCallHandler::new(sfu.clone());
        let jid = test_jid();
        let _ = handler.handle(&request_join_iq("room@muc.test"), &ctx(&jid));
        assert_eq!(
            sfu.participant_count(&CallId::new("room@muc.test").unwrap()),
            1
        );

        let leave_payload = Element::builder(REQUEST_LEAVE, NS_WADDLE_MUC_CALL)
            .attr(ATTR_ROOM, "room@muc.test")
            .build();
        let leave_iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("room@muc.test".parse().unwrap()),
            id: "l1".into(),
            payload: IqType::Set(leave_payload),
        };
        let events = handler.handle(&leave_iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!();
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        assert!(matches!(reply.payload, IqType::Result(None)));
        assert_eq!(
            sfu.participant_count(&CallId::new("room@muc.test").unwrap()),
            0
        );
    }

    #[test]
    fn missing_room_attribute_is_bad_request() {
        let sfu = fixture_sfu();
        let handler = MucCallHandler::new(sfu);
        let payload = Element::builder(REQUEST_JOIN, NS_WADDLE_MUC_CALL).build();
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("muc.test".parse().unwrap()),
            id: "j2".into(),
            payload: IqType::Set(payload),
        };
        let jid = test_jid();
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!();
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!()
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }

    #[test]
    fn iq_get_rejected_as_bad_request() {
        let sfu = fixture_sfu();
        let handler = MucCallHandler::new(sfu);
        let payload = Element::builder(REQUEST_JOIN, NS_WADDLE_MUC_CALL)
            .attr(ATTR_ROOM, "room@muc.test")
            .build();
        let iq = Iq {
            from: Some("alice@waddle.test/desktop".parse().unwrap()),
            to: Some("room@muc.test".parse().unwrap()),
            id: "j3".into(),
            payload: IqType::Get(payload),
        };
        let jid = test_jid();
        let events = handler.handle(&iq, &ctx(&jid));
        let OutboundEvent::SendStanza(stanza) = events.into_iter().next().unwrap() else {
            panic!();
        };
        let Stanza::Iq(reply) = *stanza else { panic!() };
        let IqType::Error(err) = reply.payload else {
            panic!()
        };
        assert_eq!(err.defined_condition, DefinedCondition::BadRequest);
    }
}
