//! XEP-0313 across S2S, inbound: an asker at another server reads the
//! archive of a room of this deployment (ADR-0020 D10).
//!
//! The room's archive is served on its bare JID (§3.2 of the XEP's MUC
//! rules) to an asker that may enter the room at the time of the query,
//! decided for the asker on the evidence of the stream that carried the
//! query: results come from the room's bare JID, in order, before the
//! `<fin/>` that answers the query. An asker of a domain that may not enter
//! is refused, as is a query for a room that does not exist.
//!
//! The deployments are those of `s2s_inbound_muc_support`, with rooms no
//! host-grant scope manages. The case named for a managed room runs the
//! same wiring over the revocation fixture's deployment
//! (`s2s_inbound_muc_support/managed.rs`): a remote member reads the
//! archive on its canonical grant and a stranger is refused. It needs
//! PostgreSQL only and returns early without it; a lane that sets
//! `WADDLE_TEST_REQUIRE_POSTGRES=1` fails it instead. No SpiceDB or second
//! process is involved.
#![cfg(feature = "test-support")]

// The shared fixtures serve several suites; this one uses a subset.
#[allow(dead_code)]
mod host_grant_revocation_support;
#[allow(dead_code)]
mod host_grant_support;
#[allow(dead_code)]
mod ingress_support;
#[allow(dead_code)]
#[path = "s2s_inbound_muc_support/managed.rs"]
mod managed;
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use managed::{Managed, GUEST_PHONE, STRANGER_PHONE};
use minidom::Element;
use s2s_inbound_muc_support::{
    error_condition, error_of, jid, local_groupchat, local_receives, not_federating,
    room_with_remote_occupant, Hosted, ARRIVES, BOB, ROOM,
};
use waddle_server::s2s::route::RouterLimits;
use xmpp_parsers::iq::Iq;

const NS_MAM: &str = "urn:xmpp:mam:2";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

/// A XEP-0313 query of `archive` from `from`.
fn query(from: &str, archive: &str, id: &str, query_id: &str) -> Iq {
    Iq::Set {
        from: Some(jid(from)),
        to: Some(jid(archive)),
        id: id.to_owned(),
        payload: Element::builder("query", NS_MAM)
            .attr(minidom::rxml::xml_ncname!("queryid").to_owned(), query_id)
            .build(),
    }
}

/// Whether `stanza` is a result of the query `query_id`.
fn is_result_of(stanza: &Element, query_id: &str) -> bool {
    stanza.name() == "message"
        && stanza
            .children()
            .any(|child| child.is("result", NS_MAM) && child.attr("queryid") == Some(query_id))
}

#[tokio::test]
async fn a_remote_occupant_reads_the_rooms_archive_in_order_before_the_fin() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    for (id, text) in [("m1", "first"), ("m2", "second")] {
        local_groupchat(&mut alice, ROOM, id, text).await;
        // The room has accepted the message once its sender sees it.
        local_receives(&mut alice, ARRIVES, |stanza| {
            stanza.name() == "message" && stanza.attr("id") == Some(id)
        })
        .await
        .expect("the sender's reflection");
    }
    hosted.peer_sends(query(BOB, ROOM, "q1", "page-1")).await;
    let fin = hosted.answered("q1").await;
    assert_eq!(fin.attr("type"), Some("result"), "{fin:?}");
    assert_eq!(fin.attr("from"), Some(ROOM));
    assert!(fin.children().any(|child| child.is("fin", NS_MAM)));
    let stanzas = hosted.captured.stanzas();
    let results: Vec<&Element> = stanzas
        .iter()
        .filter(|stanza| is_result_of(stanza, "page-1"))
        .collect();
    assert_eq!(results.len(), 2, "{results:?}");
    // §"Sender Impersonation": results come from the archive's own JID, to
    // the asker.
    for result in &results {
        assert_eq!(result.attr("from"), Some(ROOM));
        assert_eq!(result.attr("to"), Some(BOB));
    }
    // The results precede the `<fin/>` that ends the page.
    let position = |found: &dyn Fn(&Element) -> bool| stanzas.iter().position(found);
    let last_result = stanzas
        .iter()
        .rposition(|stanza| is_result_of(stanza, "page-1"));
    let fin_at = position(&|stanza| stanza.attr("id") == Some("q1"));
    assert!(last_result < fin_at, "{last_result:?} {fin_at:?}");
}

#[tokio::test]
async fn an_asker_that_may_enter_an_open_room_reads_its_archive_without_joining() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    s2s_inbound_muc_support::local_join(&mut alice, ROOM, "alice").await;
    local_groupchat(&mut alice, ROOM, "m1", "for the record").await;
    local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "message" && stanza.attr("id") == Some("m1")
    })
    .await
    .expect("the sender's reflection");
    // XEP-0313: an open room's archive is read by whoever may enter it.
    hosted.peer_sends(query(BOB, ROOM, "q2", "page-2")).await;
    let fin = hosted.answered("q2").await;
    assert_eq!(fin.attr("type"), Some("result"), "{fin:?}");
    assert!(hosted
        .captured
        .stanzas()
        .iter()
        .any(|stanza| is_result_of(stanza, "page-2")));
    // Reading the archive seats nobody.
    assert!(hosted.seat(ROOM, BOB).is_none());
}

#[tokio::test]
async fn a_query_for_a_room_that_does_not_exist_or_an_occupant_jid_is_refused() {
    let hosted = Hosted::start(RouterLimits::default(), no_change).await;
    hosted
        .peer_sends(query(BOB, "nowhere@muc.example.com", "q3", "page-3"))
        .await;
    let refused = hosted.answered("q3").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("item-not-found"));
    // Only a room's bare JID serves an archive (XEP-0313 §3.2).
    hosted
        .peer_sends(query(BOB, "lounge@muc.example.com/alice", "q4", "page-4"))
        .await;
    let refused = hosted.answered("q4").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(
        error_condition(&refused).as_deref(),
        Some("service-unavailable")
    );
    assert!(!hosted
        .captured
        .stanzas()
        .iter()
        .any(|stanza| is_result_of(stanza, "page-3") || is_result_of(stanza, "page-4")));
}

#[tokio::test]
async fn the_archive_of_a_room_that_is_not_federated_is_not_found() {
    let hosted = Hosted::start(RouterLimits::default(), not_federating).await;
    let mut alice = hosted.room_creator("alice", "desk").await;
    s2s_inbound_muc_support::local_join(&mut alice, ROOM, "alice").await;
    local_groupchat(&mut alice, ROOM, "m1", "not for other servers").await;
    local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "message" && stanza.attr("id") == Some("m1")
    })
    .await
    .expect("the sender's reflection");
    // The room is open to any local account; its archive is refused to an
    // asker at another server as that of a room that does not exist.
    hosted.peer_sends(query(BOB, ROOM, "q5", "page-5")).await;
    let closed = hosted.answered("q5").await;
    hosted
        .peer_sends(query(BOB, "nowhere@muc.example.com", "q6", "page-6"))
        .await;
    let absent = hosted.answered("q6").await;
    assert_eq!(closed.attr("type"), Some("error"), "{closed:?}");
    assert_eq!(error_condition(&closed).as_deref(), Some("item-not-found"));
    assert_eq!(error_of(&closed), error_of(&absent));
    assert!(!hosted
        .captured
        .stanzas()
        .iter()
        .any(|stanza| is_result_of(stanza, "page-5")));
}

#[tokio::test]
async fn a_remote_member_reads_a_managed_rooms_archive_and_a_stranger_is_refused() {
    let Some(managed) = Managed::start("s2s_in_archive", no_change).await else {
        return;
    };
    let room = managed.room();
    let mut owner = managed.deployment.replicas[0].client("owner", "desk").await;
    let joined = s2s_inbound_muc_support::local_join(&mut owner, room.as_str(), "owner").await;
    assert_eq!(error_condition(&joined), None, "{joined:?}");
    local_groupchat(&mut owner, room.as_str(), "m1", "for members").await;
    local_receives(&mut owner, ARRIVES, |stanza| {
        stanza.name() == "message" && stanza.attr("id") == Some("m1")
    })
    .await
    .expect("the sender's reflection");

    // The member's read is decided on its grant and the evidence of the
    // stream that carried the query; it need not be joined.
    managed
        .hosted
        .peer_sends(query(GUEST_PHONE, room.as_str(), "q5", "page-5"))
        .await;
    let fin = managed.hosted.answered("q5").await;
    assert_eq!(fin.attr("type"), Some("result"), "{fin:?}");
    assert!(managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .any(|stanza| is_result_of(stanza, "page-5")));

    // An account of the same admitted home without a grant reads nothing.
    managed
        .hosted
        .peer_sends(query(STRANGER_PHONE, room.as_str(), "q6", "page-6"))
        .await;
    let refused = managed.hosted.answered("q6").await;
    assert_eq!(refused.attr("type"), Some("error"));
    assert_eq!(error_condition(&refused).as_deref(), Some("forbidden"));
    assert!(!managed
        .hosted
        .captured
        .stanzas()
        .iter()
        .any(|stanza| is_result_of(stanza, "page-6")));
    managed.deployment.close().await;
}
