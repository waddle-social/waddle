//! XEP-0198 §5: post-registration replay settles retained ingress obligations.
//!
//! Receipt storage fails during the original write. Restarting after detach
//! discards the old authority's background retry proofs, so only the durable
//! replay carrier can settle the canonical row on the new connection.

use minidom::Element;
use std::time::Duration;
use waddle_server::{db::Database, sm_persistence::DatabaseSmPersistence};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::{persistence::SmPersistenceStorage, SM_NS};
use xmpp_parsers::message::{Id, Message, MessageType};

const ACCOUNTS: &[(&str, &str)] = &[("alice", "resume-receipts-password")];

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

async fn counts(db: &Database) -> (i64, i64, i64) {
    let guard = db.guard().await.expect("database");
    let mut rows = guard
        .query(
            "SELECT (SELECT COUNT(*) FROM ingress_messages), \
         (SELECT COUNT(*) FROM ingress_effect_receipts), \
         (SELECT COUNT(*) FROM ingress_messages WHERE terminal_at IS NOT NULL)",
            (),
        )
        .await
        .expect("receipt state");
    let row = rows.next().await.expect("row").expect("counts");
    (
        row.get(0).expect("canonical rows"),
        row.get(1).expect("receipts"),
        row.get(2).expect("terminal rows"),
    )
}

#[tokio::test]
async fn post_registration_resume_settles_replayed_receipt() {
    let directory = tempfile::tempdir().expect("database directory");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("resume.db").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("inspection storage");
    let db = storage.database();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        ACCOUNTS[0].0,
        ACCOUNTS[0].1,
        "original",
    )
    .await
    .expect("original connection");
    send(
        &mut client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled: Element = client
        .recv()
        .await
        .expect("enabled frame")
        .parse()
        .expect("enabled XML");
    assert!(enabled.is("enabled", SM_NS));
    assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
    let stream_id =
        waddle_xmpp::pending_delivery::SmSessionId::new(enabled.attr("id").expect("resumption ID"));

    db.guard()
        .await
        .expect("database")
        .execute(
            "CREATE TRIGGER fail_frame_receipt BEFORE INSERT ON ingress_effect_receipts \
         BEGIN SELECT RAISE(ABORT, 'injected receipt outage'); END",
            (),
        )
        .await
        .expect("inject receipt storage failure");
    // This semantic rejection produces exactly one error frame/obligation,
    // with no recipient delivery or other effects that could terminalize it.
    let mut offered = Message::new(Some("bob@localhost".parse().expect("target JID")));
    offered.id = Some(Id("resume-receipt".to_owned()));
    offered.type_ = MessageType::Chat;
    offered
        .payloads
        .push(Element::builder("result", waddle_xmpp::xep::NS_INBOX).build());
    send(&mut client, offered.into()).await;
    let original: Element = client
        .recv_matching(|frame| frame.contains("resume-receipt"))
        .await
        .expect("original error on wire")
        .parse()
        .expect("error XML");
    assert_eq!(original.attr("type"), Some("error"));
    assert_eq!(counts(&db).await, (1, 0, 0));
    // Deliberately send no <a/> and no XMPP close: lose only the transport.
    drop(client);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let sessions = storage
                .list_all_sessions_with_unacked()
                .await
                .expect("detached sessions");
            if let Some((_, frames)) = sessions
                .iter()
                .find(|(session, _)| session.stream_id == stream_id)
            {
                assert_eq!(frames.len(), 1, "one unacknowledged error frame");
                assert_eq!(
                    frames[0].ingress_receipts.len(),
                    1,
                    "durable replay must retain the outstanding obligation"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transport loss persists detached session");
    drop(server);
    assert_eq!(counts(&db).await, (1, 0, 0));
    db.guard()
        .await
        .expect("database")
        .execute("DROP TRIGGER fail_frame_receipt", ())
        .await
        .expect("restore receipt storage");

    // Restart against the same database with the same account set: the
    // detached session, its replay queue and its unsettled obligation are all
    // durable, so the resume below runs against a server that has no in-memory
    // knowledge of the original connection.
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("new transport");
    resumed
        .authenticate("localhost", ACCOUNTS[0].0, ACCOUNTS[0].1)
        .await
        .expect("authenticate before resume");
    assert_eq!(
        counts(&db).await,
        (1, 0, 0),
        "restart must not settle the replay obligation"
    );
    send(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream_id.as_str(),
            )
            .attr(minidom::rxml::xml_ncname!("h").to_owned(), "0")
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
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("previd"), Some(stream_id.as_str()));
    let replay: Element = resumed
        .recv()
        .await
        .expect("retained stanza replayed on wire")
        .parse()
        .expect("replay XML");
    assert!(replay.is("message", xmpp_parsers::ns::JABBER_CLIENT));
    assert_eq!(replay.attr("id"), original.attr("id"));
    assert_eq!(replay.attr("type"), Some("error"));
    assert_eq!(
        replay.get_child("error", xmpp_parsers::ns::JABBER_CLIENT),
        original.get_child("error", xmpp_parsers::ns::JABBER_CLIENT)
    );
    let settled = tokio::time::timeout(Duration::from_secs(10), async {
        while counts(&db).await != (1, 1, 1) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        settled.is_ok(),
        "replay must settle its receipt and terminalize the canonical row; observed {:?}",
        counts(&db).await
    );
}
