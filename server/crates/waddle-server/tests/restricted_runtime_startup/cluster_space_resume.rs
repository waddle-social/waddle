use std::time::Duration;

use jid::BareJid;
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_server::db::Database;
use waddle_server::permissions::host_grant::{
    FenceAcknowledgement, HostGrantStore, ScopeLifecycleId,
};
use waddle_ws_test_support::iq::{receive_iq, round_trip, send_stanza};
use waddle_ws_test_support::stanza::{get, is_error, is_result, pubsub_owner_affiliations, set};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::ns;
use xmpp_parsers::stanza_error::DefinedCondition;

use super::super::{owner_database, DOMAIN};

const RESOURCE: &str = "space-subscriber";
const RETAINED_ID: &str = "cluster-space-protected-retained";
const DETACHED: i64 = 2;
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn assert_protected_resume(
    owner_url: &str,
    [destination, origin]: [&TestServer; 2],
    owner: &mut WsXmppClient,
    mut member: WsXmppClient,
    service: &BareJid,
    node: &str,
    lifecycle: ScopeLifecycleId,
) {
    let db = owner_database(owner_url).await;
    let store = HostGrantStore::open(db.clone()).expect("inspect canonical fence");
    let full_jid = member.full_jid.clone().expect("bound resource");
    let discover = |id| {
        get(
            id,
            service.as_str(),
            Element::builder("query", ns::DISCO_INFO)
                .attr(xml_ncname!("node").to_owned(), node)
                .build(),
        )
    };
    send_stanza(
        &mut member,
        Element::builder("enable", SM_NS)
            .attr(xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = receive_sm(&mut member, &["enabled"]).await;
    let token = enabled.attr("id").expect("resume token");
    let resume = || {
        Element::builder("resume", SM_NS)
            .attr(xml_ncname!("previd").to_owned(), token)
            .attr(xml_ncname!("h").to_owned(), "0")
            .build()
    };
    let retained = round_trip(&mut member, discover(RETAINED_ID)).await;
    assert!(is_result(&retained), "{retained:?}");
    drop(member);
    let first = wait_for_detach(&db, &full_jid, 0).await;

    let mut resumed = authenticate_unbound(destination).await;
    send_stanza(&mut resumed, resume()).await;
    let confirmation = receive_sm(&mut resumed, &["resumed", "failed"]).await;
    assert!(confirmation.is("resumed", SM_NS), "{confirmation:?}");
    assert_eq!(confirmation.attr("previd"), Some(token));
    assert_eq!(confirmation.attr("h"), Some("1"));
    assert_eq!(receive_iq(&mut resumed, RETAINED_ID).await, retained);
    drop(resumed);
    wait_for_detach(&db, &full_jid, first).await;

    let revoked = round_trip(
        owner,
        set(
            "cluster-space-revoke",
            service.as_str(),
            pubsub_owner_affiliations(node, &[("alice@localhost", "none")]),
        ),
    )
    .await;
    assert!(is_result(&revoked), "{revoked:?}");
    let scope = store
        .scope(lifecycle)
        .await
        .expect("read scope")
        .expect("enrolled scope");
    assert_eq!(
        store
            .revocation_fence(lifecycle, scope.generation)
            .await
            .expect("membership revocation fence"),
        FenceAcknowledgement::Acknowledged { replicas: 2 }
    );

    let mut rejected = authenticate_unbound(origin).await;
    send_stanza(&mut rejected, resume()).await;
    let failure = receive_sm(&mut rejected, &["resumed", "failed"]).await;
    assert!(failure.is("failed", SM_NS), "{failure:?}");
    assert_eq!(failure.attr("h"), Some("1"));
    assert!(
        failure
            .get_child("item-not-found", ns::XMPP_STANZAS)
            .is_some(),
        "{failure:?}"
    );
    let leaked = rejected
        .recv_matching_within(Duration::from_millis(300), |frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("iq", waddle_xmpp::ns::JABBER_CLIENT)
                    && element.attr("id") == Some(RETAINED_ID)
            })
        })
        .await;
    assert!(leaked.is_err(), "revoked session replayed {leaked:?}");
    assert_eq!(
        rejected.bind(RESOURCE).await.expect("fresh resource bind"),
        full_jid
    );
    let no_discovery = round_trip(&mut rejected, discover("cluster-space-after-revoke")).await;
    assert!(
        is_error(&no_discovery, DefinedCondition::ItemNotFound),
        "{no_discovery:?}"
    );
    rejected.close().await.expect("close member");
}

async fn authenticate_unbound(server: &TestServer) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("connect");
    client
        .authenticate(DOMAIN, "alice", "alice-password")
        .await
        .expect("authenticate Alice");
    client
}

async fn receive_sm(client: &mut WsXmppClient, names: &[&str]) -> Element {
    client
        .recv_matching_within(RECEIVE_TIMEOUT, |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| names.iter().any(|name| element.is(*name, SM_NS)))
        })
        .await
        .expect("stream management frame")
        .parse()
        .expect("stream management XML")
}

async fn wait_for_detach(db: &Database, full_jid: &str, newer_than: i64) -> i64 {
    tokio::time::timeout(RECEIVE_TIMEOUT, async {
        loop {
            let conn = db.guard().await.expect("attach owner read");
            let mut rows = conn
                .query(
                    "SELECT generation, inbound_handled, outbound_appended FROM sm_durable_sessions
                     WHERE full_jid = ? AND state = ? AND generation > ?",
                    waddle_server::db_params![full_jid.to_owned(), DETACHED, newer_than],
                )
                .await
                .expect("query detached session");
            if let Some(row) = rows.next().await.expect("read detached session") {
                assert_eq!(row.get::<i64>(1).expect("handled count"), 1);
                assert_eq!(row.get::<i64>(2).expect("retained count"), 1);
                return row.get::<i64>(0).expect("detached generation");
            }
            drop(rows);
            drop(conn);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("protected IQ is captured in a detached session")
}
