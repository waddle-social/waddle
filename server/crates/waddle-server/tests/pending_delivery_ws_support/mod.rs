//! Real WebSocket delivery with typed inspection of the durable pending queue.

use jid::BareJid;
use minidom::Element;
use std::time::Duration;
use waddle_server::pending_delivery::DatabasePendingDeliveryStorage;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::{
    pending_delivery::{storage::PendingDeliveryStorage, PendingRow, QuotaPolicy, SmSessionId},
    stream_management::{SmAck, SmRequest, SM_NS},
};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

const ACCOUNTS: &[(&str, &str)] = &[
    ("alice", "pending-delivery-password"),
    ("bob", "pending-delivery-password"),
];

pub struct PendingFixture {
    pub server: TestServer,
    pub storage: DatabasePendingDeliveryStorage,
    pub recipient: BareJid,
    // Keep the database directory alive until the server and storage drop.
    pub directory: tempfile::TempDir,
}

impl PendingFixture {
    pub async fn start() -> Self {
        let directory = tempfile::tempdir().expect("pending database directory");
        let database_url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("pending.db").display()
        );
        let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
        let storage =
            DatabasePendingDeliveryStorage::open(Some(&database_url), QuotaPolicy::Unlimited)
                .await
                .expect("pending inspection storage");
        Self {
            server,
            storage,
            recipient: "bob@localhost".parse().expect("recipient JID"),
            directory,
        }
    }

    pub async fn connect(&self, username: &str, resource: &str) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server.ws_url(),
            "localhost",
            username,
            ACCOUNTS[0].1,
            resource,
        )
        .await
        .expect("authenticated WebSocket")
    }

    pub async fn rows(&self) -> Vec<PendingRow> {
        self.storage
            .list(&self.recipient)
            .await
            .expect("pending rows")
    }

    pub async fn wait_rows(&self, ready: impl Fn(&[PendingRow]) -> bool) -> Vec<PendingRow> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let rows = self.rows().await;
                if ready(&rows) {
                    return rows;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("durable pending state converges")
    }

    pub async fn offer(&self, sender: &mut WsXmppClient, id: &str) {
        let mut message = Message::new(Some(self.recipient.clone().into()));
        message.type_ = MessageType::Chat;
        message.id = Some(Id(id.to_owned()));
        message.bodies.insert(Lang::new(), id.to_owned());
        send(sender, message.into()).await;
    }
}

pub async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

pub async fn recv(client: &mut WsXmppClient) -> Element {
    client
        .recv()
        .await
        .expect("wire frame")
        .parse()
        .expect("wire XML")
}

pub async fn enable(client: &mut WsXmppClient) -> SmSessionId {
    send(
        client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = recv(client).await;
    assert!(enabled.is("enabled", SM_NS), "SM enabled: {enabled:?}");
    SmSessionId::new(enabled.attr("id").expect("SM session ID"))
}

pub async fn presence(client: &mut WsXmppClient, priority: i8) {
    send(
        client,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT)
            .append(
                Element::builder("priority", xmpp_parsers::ns::JABBER_CLIENT)
                    .append(priority.to_string())
                    .build(),
            )
            .build(),
    )
    .await;
}

/// Count every received stanza; controls never increment the XEP-0198 counter.
pub async fn message(client: &mut WsXmppClient, handled: &mut u32, id: &str) -> Message {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = recv(client).await;
            if frame.ns() == xmpp_parsers::ns::JABBER_CLIENT
                && matches!(frame.name(), "iq" | "presence" | "message")
            {
                *handled += 1;
            }
            if frame.is("message", xmpp_parsers::ns::JABBER_CLIENT) {
                let message = Message::try_from(frame).expect("typed delivered message");
                assert_eq!(message.id.as_ref().map(|value| value.0.as_str()), Some(id));
                assert_eq!(
                    message.bodies.get(&Lang::new()).map(String::as_str),
                    Some(id)
                );
                assert!(message
                    .payloads
                    .iter()
                    .any(|payload| payload.is("delay", xmpp_parsers::ns::DELAY)));
                return message;
            }
        }
    })
    .await
    .expect("expected pending message arrives")
}

pub async fn ack(client: &mut WsXmppClient, handled: u32) {
    client
        .send(&SmAck::new(handled).to_xml())
        .await
        .expect("client acknowledgement");
}

/// A positive wire barrier, rejecting any message before the server handles
/// all preceding input. In particular this does not acknowledge outbound A.
pub async fn barrier_without_message(client: &mut WsXmppClient, handled: &mut u32, inbound: u32) {
    client
        .send(&SmRequest::to_xml())
        .await
        .expect("SM barrier request");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = recv(client).await;
            assert!(
                !frame.is("message", xmpp_parsers::ns::JABBER_CLIENT),
                "message overtook its unacknowledged predecessor: {frame:?}"
            );
            if frame.is("a", SM_NS) {
                assert_eq!(
                    frame
                        .attr("h")
                        .expect("server handled count")
                        .parse::<u32>()
                        .expect("unsigned handled count"),
                    inbound
                );
                return;
            }
            if frame.ns() == xmpp_parsers::ns::JABBER_CLIENT
                && matches!(frame.name(), "iq" | "presence")
            {
                *handled += 1;
            }
        }
    })
    .await
    .expect("server handles preceding presence");
}
