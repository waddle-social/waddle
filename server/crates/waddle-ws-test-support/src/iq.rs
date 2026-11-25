//! Typed stanza transport plumbing, chiefly IQs, shared by WebSocket
//! protocol suites.
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

/// Serialize any typed stanza (a presence, a message or an IQ) onto an
/// already connected socket.
pub async fn send_stanza(client: &mut WsXmppClient, stanza: impl Into<Element>) {
    let element: Element = stanza.into();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize stanza");
    client
        .send(&String::from_utf8(bytes).expect("stanza UTF-8"))
        .await
        .expect("send stanza");
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

const NS_MUC_OWNER: &str = "http://jabber.org/protocol/muc#owner";
const NS_DATA: &str = "jabber:x:data";

/// Unlock a room `client` just created by joining it (XEP-0045 §10.1.2).
///
/// A room its first occupant creates through MUC presence is locked, so no
/// one else may enter it, until the owner submits the instant-room request:
/// a `muc#owner` query carrying one empty `jabber:x:data` form of type
/// `submit`. Call it after the creating join's self-presence (status 201)
/// has been received, before any other user joins. Rooms the service
/// provisions (Waddle channels) are already configured and need no call.
pub async fn unlock_created_room(client: &mut WsXmppClient, room: &str) {
    let id = send_unlock_created_room(client, room).await;
    let reply = receive_iq(client, &id).await;
    assert!(
        matches!(reply, Iq::Result { .. }),
        "the instant-room request is answered with a result: {reply:?}"
    );
}

/// Send the instant-room request of [`unlock_created_room`] without reading
/// the reply, for a driver that must leave the frames the join produced
/// (the room subject, for one) unread. Returns the request's stanza ID.
pub async fn send_unlock_created_room(client: &mut WsXmppClient, room: &str) -> String {
    let id = format!("instant-room-{}", uuid::Uuid::new_v4());
    let form = Element::builder("x", NS_DATA)
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "submit",
        )
        .build();
    let query = Element::builder("query", NS_MUC_OWNER).append(form).build();
    let iq = Element::builder("iq", "jabber:client")
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("type").to_owned(),
            "set",
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("id").to_owned(),
            id.as_str(),
        )
        .attr(
            xmpp_parsers::minidom::rxml::xml_ncname!("to").to_owned(),
            room,
        )
        .append(query)
        .build();
    send_stanza(client, iq).await;
    id
}
