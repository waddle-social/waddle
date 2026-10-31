//! Shared plumbing for destination-policy frame suites. Protocol payloads and
//! expected error conditions remain in each suite; this fixture makes no policy decision.

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::sync::mpsc;
use waddle_xmpp::protocol::ConnectionPhase;
use waddle_xmpp::registry::OutboundStanza;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};

use super::super::frame::handle_xmpp_frame;
use super::super::state::WsConnState;
use super::super::WebSocketState;
use super::{register_test_connection, stanza_to_xml};

pub(super) const DOMAIN: &str = "example.com";
pub(super) const ORIGIN: &str = "alice@example.com/web";

pub(super) fn origin() -> FullJid {
    ORIGIN.parse().expect("bound origin")
}

pub(super) async fn bound_connection(
    state: &WebSocketState,
) -> (WsConnState, mpsc::Receiver<OutboundStanza>) {
    let (tx, rx) = mpsc::channel(8);
    let owner = register_test_connection(state, &origin(), tx).await;
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(origin(), false);
    conn.registry_owner = Some(owner);
    (conn, rx)
}

pub(super) fn request(
    payload: IqPayload,
    target: Option<&str>,
    from: Option<&str>,
    id: &str,
) -> Iq {
    IqHeader {
        from: from.map(|value| value.parse().expect("request sender")),
        to: target.map(|value| value.parse().expect("request destination")),
        id: id.into(),
    }
    .assemble(payload)
}

pub(super) async fn dispatch(state: &WebSocketState, conn: &mut WsConnState, iq: Iq) -> Vec<Iq> {
    handle_xmpp_frame(
        &stanza_to_xml(&Stanza::Iq(Box::new(iq))),
        DOMAIN,
        state,
        conn,
    )
    .await
    .into_iter()
    .map(|frame| Iq::try_from(frame.parse::<Element>().expect("response XML")).expect("IQ"))
    .collect()
}

pub(super) fn assert_reply_addressing(reply: &Iq, request: &Iq) {
    assert_eq!(reply.id(), request.id());
    assert_eq!(reply.from(), request.to());
    assert_eq!(
        reply.to(),
        Some(&Jid::from(origin())),
        "bound origin, not client from"
    );
}
