//! Typed IQ transport plumbing shared by WebSocket protocol suites.
//! Payloads and expected replies belong to the calling suite.

use xmpp_parsers::iq::Iq;
use xmpp_parsers::jid::Jid;
use xmpp_parsers::minidom::Element;

use super::{WsXmppClient, RECV_TIMEOUT};

/// Serialize a typed IQ onto an already connected socket.
pub async fn send_iq(client: &mut WsXmppClient, iq: Iq) {
    let element: Element = iq.into();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize IQ");
    client
        .send(&String::from_utf8(bytes).expect("IQ UTF-8"))
        .await
        .expect("send IQ");
}

/// Await a correlated IQ under the harness's existing overall receive budget.
pub async fn receive_iq(client: &mut WsXmppClient, id: &str) -> Iq {
    let frame = client
        .recv_matching_within(RECV_TIMEOUT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "iq" && element.attr("id") == Some(id))
        })
        .await
        .expect("correlated IQ");
    Iq::try_from(frame.parse::<Element>().expect("IQ XML")).expect("typed IQ")
}

/// Send a request and receive the reply with the same stanza ID.
pub async fn round_trip(client: &mut WsXmppClient, iq: Iq) -> Iq {
    let id = iq.id().to_owned();
    send_iq(client, iq).await;
    receive_iq(client, &id).await
}

/// Parse the resource address returned by the real binding exchange.
pub fn bound_jid(client: &WsXmppClient) -> Jid {
    client
        .full_jid
        .as_deref()
        .expect("bound client")
        .parse()
        .expect("bound JID")
}
