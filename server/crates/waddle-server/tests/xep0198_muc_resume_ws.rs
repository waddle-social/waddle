//! XEP-0198 §5 continuity also preserves XEP-0045 room occupancy.
//!
//! A real authenticated WebSocket resume must retain the original occupancy
//! generation and deliver room messages without a MUC rejoin or departure.

use jid::{BareJid, FullJid};
use minidom::Element;
use std::time::Duration;
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::{
    persistence::{PersistedSession, SmPersistenceStorage},
    SM_NS,
};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

async fn join(client: &mut WsXmppClient, occupant: &FullJid) -> u32 {
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(occupant.clone().into());
    presence
        .payloads
        .push(Element::builder("x", waddle_xmpp::muc::presence::NS_MUC).build());
    send(client, presence.into()).await;
    let frames = client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("room join completes");
    // SM counts stanzas, not stream-control elements such as <r/>.
    let handled = frames
        .iter()
        .filter(|frame| {
            let element: Element = frame.parse().expect("join response XML");
            element.ns() == xmpp_parsers::ns::JABBER_CLIENT
                && matches!(element.name(), "iq" | "message" | "presence")
        })
        .count();
    u32::try_from(handled).expect("bounded join stanza count")
}

async fn wait_for_detached(
    storage: &DatabaseSmPersistence,
    stream: &SmSessionId,
    minimum_inbound: u32,
) -> PersistedSession {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(session) = storage
                .get_session(stream)
                .await
                .expect("persisted session")
            {
                if session.inbound_count >= minimum_inbound {
                    return session;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transport loss persists the detached session")
}

async fn send_room_message(client: &mut WsXmppClient, room: &BareJid, id: &str) {
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = MessageType::Groupchat;
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), id.to_owned());
    send(client, message.into()).await;
}

/// The room's reflected message is a positive barrier after resume/room
/// processing, not a timer-based assertion that happens to see no departure.
async fn observe_message_without_departure(
    observer: &mut WsXmppClient,
    occupant: &FullJid,
    message_id: &str,
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame: Element = observer
                .recv()
                .await
                .expect("observer traffic")
                .parse()
                .expect("observer XML");
            if frame.is("presence", xmpp_parsers::ns::JABBER_CLIENT) {
                let presence = Presence::try_from(frame).expect("typed presence");
                assert_ne!(
                    presence.from,
                    Some(occupant.clone().into()),
                    "successful stream resumption must not announce a MUC departure or rejoin"
                );
            } else if frame.is("message", xmpp_parsers::ns::JABBER_CLIENT)
                && frame.attr("id") == Some(message_id)
            {
                assert_eq!(frame.attr("type"), Some("groupchat"));
                return;
            }
        }
    })
    .await
    .expect("room traffic reaches the observer");
}

#[tokio::test]
async fn successful_resume_preserves_room_generation_without_leave_or_rejoin() {
    let directory = tempfile::tempdir().expect("database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("resume.db").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("alice", "muc-resume-password")],
    );
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("inspection storage");
    let mut observer = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "admin",
        server.fixed_account_password(),
        "observer",
    )
    .await
    .expect("observer connects");
    let mut original = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "alice",
        "muc-resume-password",
        "phone",
    )
    .await
    .expect("original connects");
    let room: BareJid = "resume-contract@muc.localhost".parse().expect("room JID");
    let alice_occupant = room.with_resource_str("alice").expect("alice occupant JID");
    join(
        &mut observer,
        &room
            .with_resource_str("observer")
            .expect("observer occupant JID"),
    )
    .await;
    send(
        &mut original,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled: Element = original
        .recv()
        .await
        .expect("enabled response")
        .parse()
        .expect("enabled XML");
    assert!(enabled.is("enabled", SM_NS));
    assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
    let stream = SmSessionId::new(enabled.attr("id").expect("resumable stream ID"));
    let handled = join(&mut original, &alice_occupant).await;
    // Drain the original join from the observer before testing continuity.
    let initial: Element = observer
        .recv()
        .await
        .expect("initial join broadcast")
        .parse()
        .expect("join XML");
    let initial = Presence::try_from(initial).expect("join presence");
    assert_eq!(initial.from, Some(alice_occupant.clone().into()));
    assert_eq!(initial.type_, PresenceType::None);
    drop(original);
    let detached = wait_for_detached(&storage, &stream, 1).await;

    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("replacement transport");
    resumed
        .authenticate("localhost", "alice", "muc-resume-password")
        .await
        .expect("authenticate before resume");
    send(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                handled.to_string(),
            )
            .build(),
    )
    .await;
    let response: Element = resumed
        .recv()
        .await
        .expect("resume response")
        .parse()
        .expect("resume XML");
    assert!(
        response.is("resumed", SM_NS),
        "expected successful resume: {response:?}"
    );
    assert_eq!(response.attr("previd"), Some(stream.as_str()));
    assert_eq!(
        response
            .attr("h")
            .expect("server handled count")
            .parse::<u32>()
            .expect("handled sequence"),
        detached.inbound_count
    );

    // No bind and no MUC join: the resumed stream still owns the old seat.
    send_room_message(&mut resumed, &room, "after-successful-resume").await;
    observe_message_without_departure(&mut observer, &alice_occupant, "after-successful-resume")
        .await;
    let reflection: Element = resumed
        .recv_matching(|frame| frame.contains("after-successful-resume"))
        .await
        .expect("resumed session receives room fan-out")
        .parse()
        .expect("reflection XML");
    assert_eq!(reflection.attr("type"), Some("groupchat"));
    assert_eq!(
        reflection.attr("from"),
        Some(alice_occupant.to_string().as_str())
    );

    // Persist again after actual resumed traffic to inspect the generation
    // used by the new connection, rather than rereading its predecessor's row.
    drop(resumed);
    let detached_again = wait_for_detached(&storage, &stream, detached.inbound_count + 1).await;
    assert_eq!(detached_again.occupancy_session, detached.occupancy_session);
    send_room_message(&mut observer, &room, "after-resumed-detach").await;
    observe_message_without_departure(&mut observer, &alice_occupant, "after-resumed-detach").await;
    observer.close().await.expect("observer closes");
}
