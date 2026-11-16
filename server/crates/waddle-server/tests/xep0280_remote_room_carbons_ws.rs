//! XEP-0280 §6.1 and §7 for a local account's traffic with a room on a
//! remote domain, over the production `/ws` router on a loopback socket.
//!
//! The deployment's remote route is a stand-in that records every stanza
//! it is offered and accepts it, as the S2S router accepts a resource's
//! traffic for a remote room it has joined, unless the case names it for
//! refusal. Stanzas from the room reach the
//! occupant through the WebSocket local delivery the S2S router delivers
//! through, as stanzas of remote origin.
//!
//! Each negative case ends with a sentinel message that is copied: the
//! first carbon the observing resource receives must be the sentinel's.
#![cfg(feature = "test-support")]

// The shared loopback fixture serves several suites; this one does not
// write catalog rows.
#[allow(dead_code)]
mod ws_loopback_support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jid::{FullJid, Jid};
use minidom::Element;
use waddle_server::destination::{
    RemoteOutbound, RemoteRoute, RemoteRouteOutcome, UnroutableReason,
};
use waddle_server::s2s::route::{LocalDelivery, LocalDeliveryOutcome, LocalOrigin};
use waddle_server::test_support::{websocket_state, WebSocketLocalDelivery};
use waddle_ws_test_support::iq::{bound_jid, round_trip};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::carbons::CARBONS_NS;
use waddle_xmpp::muc::presence::NS_MUC_USER;
use waddle_xmpp::Stanza;
use ws_loopback_support::Loopback;
use xmpp_parsers::carbons::{Enable, Received, Sent};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::muc::user::MucUser;

const ROOM: &str = "room@muc.remote.test";
const OCCUPANT: &str = "room@muc.remote.test/host";
const REMOTE_CONTACT: &str = "bob@remote.test/desk";
const WAIT: Duration = Duration::from_secs(10);

/// A remote route that records every stanza it is offered and accepts it,
/// except a message whose id is in `refused`.
#[derive(Default)]
struct AcceptingRoute {
    refused: Vec<&'static str>,
    offered: Mutex<Vec<Stanza>>,
}

impl AcceptingRoute {
    fn refusing(refused: &[&'static str]) -> Self {
        Self {
            refused: refused.to_vec(),
            offered: Mutex::default(),
        }
    }

    fn offered_ids(&self) -> Vec<String> {
        let offered = match self.offered.lock() {
            Ok(offered) => offered,
            Err(poisoned) => poisoned.into_inner(),
        };
        offered
            .iter()
            .filter_map(|stanza| match stanza {
                Stanza::Message(message) => message.id.as_ref().map(|id| id.0.clone()),
                Stanza::Presence(_) | Stanza::Iq(_) => None,
            })
            .collect()
    }
}

#[async_trait]
impl RemoteRoute for AcceptingRoute {
    async fn route(&self, outbound: RemoteOutbound) -> RemoteRouteOutcome {
        match self.offered.lock() {
            Ok(mut offered) => offered.push(outbound.stanza().clone()),
            Err(poisoned) => poisoned.into_inner().push(outbound.stanza().clone()),
        }
        let refused = match outbound.stanza() {
            Stanza::Message(message) => message
                .id
                .as_ref()
                .is_some_and(|id| self.refused.iter().any(|refused| *refused == id.0)),
            Stanza::Presence(_) | Stanza::Iq(_) => false,
        };
        if refused {
            return RemoteRouteOutcome::Unroutable {
                reason: UnroutableReason::FederationDisabled,
                outbound: Box::new(outbound),
            };
        }
        RemoteRouteOutcome::Accepted
    }
}

/// The test state, with `route` as its remote route, served over loopback,
/// and `alice@example.com` bound as `phone` and as `laptop`, with Carbons
/// enabled on `laptop`.
async fn alice_with_two_resources(
    route: Arc<AcceptingRoute>,
) -> (Loopback, WsXmppClient, WsXmppClient) {
    let mut state = websocket_state().await;
    let route: Arc<dyn RemoteRoute> = route;
    Arc::get_mut(&mut state)
        .expect("an unshared test state")
        .deps
        .remote_route = route;
    let loopback = Loopback::serve(state).await;
    loopback.account("alice").await;
    let phone = loopback.client("alice", "phone").await;
    let mut laptop = loopback.client("alice", "laptop").await;
    let enabled = round_trip(&mut laptop, Iq::from_set("laptop-carbons", Enable)).await;
    assert!(
        matches!(enabled, Iq::Result { payload: None, .. }),
        "{enabled:?}"
    );
    (loopback, phone, laptop)
}

fn jid(raw: &str) -> Jid {
    raw.parse().expect("a test JID")
}

fn full_jid(client: &WsXmppClient) -> FullJid {
    bound_jid(client).try_into_full().expect("a bound full JID")
}

fn chat(to: &str, id: &str, body: &str) -> Message {
    let mut message = Message::chat(Some(jid(to)));
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), body.to_owned());
    message
}

async fn send(client: &mut WsXmppClient, message: Message) {
    let element: Element = message.into();
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize message");
    client
        .send(&String::from_utf8(bytes).expect("message UTF-8"))
        .await
        .expect("send message");
}

/// Wait for the message `id` to reach `client` directly, not as a carbon.
async fn receive_message(client: &mut WsXmppClient, id: &str) -> Message {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "message" && element.attr("id") == Some(id))
        })
        .await
        .expect("the message is delivered");
    Message::try_from(frame.parse::<Element>().expect("message XML")).expect("typed message")
}

/// The first carbon wrapped in `<name xmlns='urn:xmpp:carbons:2'/>` to
/// reach `client`, checked for its addressing, and the message it forwards.
async fn first_carbon(client: &mut WsXmppClient, name: &str) -> Message {
    let frame = client
        .recv_matching_within(WAIT, |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.name() == "message" && element.get_child(name, CARBONS_NS).is_some()
            })
        })
        .await
        .expect("a carbon reaches the Carbons-enabled resource");
    let carbon =
        Message::try_from(frame.parse::<Element>().expect("carbon XML")).expect("typed carbon");
    let observer = full_jid(client);
    assert_eq!(carbon.from, Some(Jid::from(observer.to_bare())));
    assert_eq!(carbon.to, Some(Jid::from(observer)));
    let forwarded = carbon.payloads.into_iter().find_map(|payload| {
        if name == "sent" {
            Sent::try_from(payload).ok().map(|sent| sent.forwarded)
        } else {
            Received::try_from(payload)
                .ok()
                .map(|received| received.forwarded)
        }
    });
    forwarded.expect("a typed carbon").message
}

fn id_of(message: &Message) -> Option<&str> {
    message.id.as_ref().map(|id| id.0.as_str())
}

#[tokio::test]
async fn a_private_message_to_a_remote_room_occupant_is_copied_as_a_sent_carbon() {
    let route = Arc::new(AcceptingRoute::default());
    let (_loopback, mut phone, mut laptop) = alice_with_two_resources(route.clone()).await;

    send(&mut phone, chat(OCCUPANT, "pm-to-host", "a private word")).await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("pm-to-host"));
    assert_eq!(copy.type_, MessageType::Chat);
    assert_eq!(copy.from, Some(Jid::from(full_jid(&phone))));
    assert_eq!(copy.to, Some(jid(OCCUPANT)));
    assert_eq!(
        copy.bodies.get(&Lang::new()).map(String::as_str),
        Some("a private word")
    );
    assert_eq!(route.offered_ids(), vec!["pm-to-host".to_owned()]);
}

#[tokio::test]
async fn a_private_message_the_remote_route_refuses_is_not_copied() {
    let route = Arc::new(AcceptingRoute::refusing(&["refused-pm"]));
    let (_loopback, mut phone, mut laptop) = alice_with_two_resources(route.clone()).await;

    send(&mut phone, chat(OCCUPANT, "refused-pm", "never sent")).await;
    let reply = receive_message(&mut phone, "refused-pm").await;
    assert_eq!(reply.type_, MessageType::Error);
    send(&mut phone, chat(OCCUPANT, "sentinel", "after the refusal")).await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("sentinel"));
    assert_eq!(
        route.offered_ids(),
        vec!["refused-pm".to_owned(), "sentinel".to_owned()]
    );
}

#[tokio::test]
async fn groupchat_to_a_remote_room_is_not_copied() {
    let route = Arc::new(AcceptingRoute::default());
    let (_loopback, mut phone, mut laptop) = alice_with_two_resources(route.clone()).await;

    let mut groupchat = Message::groupchat(Some(jid(ROOM)));
    groupchat.id = Some(Id("to-the-room".to_owned()));
    groupchat
        .bodies
        .insert(Lang::new(), "to everyone".to_owned());
    send(&mut phone, groupchat).await;
    send(
        &mut phone,
        chat(OCCUPANT, "sentinel", "after the groupchat"),
    )
    .await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("sentinel"));
    assert_eq!(
        route.offered_ids(),
        vec!["to-the-room".to_owned(), "sentinel".to_owned()]
    );
}

#[tokio::test]
async fn a_private_message_to_a_remote_room_occupant_marked_private_is_not_copied() {
    let route = Arc::new(AcceptingRoute::default());
    let (_loopback, mut phone, mut laptop) = alice_with_two_resources(route.clone()).await;

    let mut private = chat(OCCUPANT, "kept-private", "not for my other devices");
    private
        .payloads
        .push(Element::builder("private", CARBONS_NS).build());
    send(&mut phone, private).await;
    send(
        &mut phone,
        chat(OCCUPANT, "sentinel", "after the private one"),
    )
    .await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("sentinel"));
    assert_eq!(
        route.offered_ids(),
        vec!["kept-private".to_owned(), "sentinel".to_owned()]
    );
}

#[tokio::test]
async fn a_mediated_invitation_through_a_remote_room_is_copied_as_a_sent_carbon() {
    let route = Arc::new(AcceptingRoute::default());
    let (_loopback, mut phone, mut laptop) = alice_with_two_resources(route.clone()).await;

    let mut invitation = Message::new(Some(jid(ROOM)));
    invitation.id = Some(Id("invite-carol".to_owned()));
    invitation.payloads.push(
        Element::builder("x", NS_MUC_USER)
            .append(
                Element::builder("invite", NS_MUC_USER)
                    .attr(
                        minidom::rxml::xml_ncname!("to").to_owned(),
                        "carol@remote.test",
                    )
                    .build(),
            )
            .build(),
    );
    send(&mut phone, invitation).await;

    let copy = first_carbon(&mut laptop, "sent").await;
    assert_eq!(id_of(&copy), Some("invite-carol"));
    assert_eq!(copy.to, Some(jid(ROOM)));
    assert!(copy.payloads.iter().any(|payload| {
        payload.is("x", NS_MUC_USER) && payload.has_child("invite", NS_MUC_USER)
    }));
}

#[tokio::test]
async fn a_private_message_from_a_remote_room_occupant_is_not_copied_as_a_received_carbon() {
    let route = Arc::new(AcceptingRoute::default());
    let (loopback, mut phone, mut laptop) = alice_with_two_resources(route).await;
    let delivery = WebSocketLocalDelivery::new(&loopback.state);
    let phone_jid = full_jid(&phone);

    let mut private = chat(phone_jid.as_str(), "pm-from-host", "a private word back");
    private.from = Some(jid(OCCUPANT));
    private.payloads.push(MucUser::new().into());
    let outcome = delivery
        .deliver(&phone_jid, Stanza::Message(private), LocalOrigin::Remote)
        .await;
    assert!(matches!(outcome, LocalDeliveryOutcome::Delivered));
    let delivered = receive_message(&mut phone, "pm-from-host").await;
    assert_eq!(delivered.from, Some(jid(OCCUPANT)));

    // A chat message from a remote contact's full JID is copied, so the
    // laptop's first `received` carbon shows whether the room's private
    // message was.
    let mut sentinel = chat(phone_jid.as_str(), "sentinel", "from a contact");
    sentinel.from = Some(jid(REMOTE_CONTACT));
    let outcome = delivery
        .deliver(&phone_jid, Stanza::Message(sentinel), LocalOrigin::Remote)
        .await;
    assert!(matches!(outcome, LocalDeliveryOutcome::Delivered));
    receive_message(&mut phone, "sentinel").await;

    let copy = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&copy), Some("sentinel"));
    assert_eq!(copy.from, Some(jid(REMOTE_CONTACT)));
}

#[tokio::test]
async fn a_private_message_from_a_remote_room_occupant_without_the_muc_user_element_is_copied() {
    // XEP-0045 §7.5 only recommends the `muc#user` element on a private
    // message; without it the server cannot tell the message from a chat
    // message from a remote full JID, and copies it as one.
    let route = Arc::new(AcceptingRoute::default());
    let (loopback, mut phone, mut laptop) = alice_with_two_resources(route).await;
    let delivery = WebSocketLocalDelivery::new(&loopback.state);
    let phone_jid = full_jid(&phone);

    let mut private = chat(
        phone_jid.as_str(),
        "pm-unmarked",
        "a private word, unmarked",
    );
    private.from = Some(jid(OCCUPANT));
    let outcome = delivery
        .deliver(&phone_jid, Stanza::Message(private), LocalOrigin::Remote)
        .await;
    assert!(matches!(outcome, LocalDeliveryOutcome::Delivered));
    receive_message(&mut phone, "pm-unmarked").await;

    let copy = first_carbon(&mut laptop, "received").await;
    assert_eq!(id_of(&copy), Some("pm-unmarked"));
    assert_eq!(copy.from, Some(jid(OCCUPANT)));
}
