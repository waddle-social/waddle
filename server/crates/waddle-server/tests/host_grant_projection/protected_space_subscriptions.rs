use std::time::Duration;

use jid::{BareJid, FullJid};
use minidom::rxml::xml_ncname;
use minidom::Element;
use waddle_ws_test_support::iq::{receive_iq, round_trip, send_stanza};
use waddle_ws_test_support::stanza::{is_error, is_result, pubsub_subscribe, result_payload, set};
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::pubsub::NS_PUBSUB;
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

use super::host_grant_revocation_support::{authenticate_unbound, Replica};
use super::{CAROL, OWNER, PEER};

const STATE_DETACHED: i64 = 2;
const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

pub(super) async fn receive_sm(client: &mut WsXmppClient, names: &[&str]) -> Element {
    client
        .recv_matching_within(Duration::from_secs(10), |frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| names.iter().any(|name| element.is(*name, SM_NS)))
        })
        .await
        .expect("stream-management response")
        .parse()
        .expect("stream-management XML")
}

pub(super) async fn wait_for_detach(
    pool: &sqlx::PgPool,
    resource: &jid::FullJid,
    newer_than: i64,
) -> i64 {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detached = sqlx::query_as::<_, (i64, i64, i64)>(
                "SELECT generation, inbound_handled, outbound_appended FROM sm_durable_sessions
                 WHERE full_jid = $1 AND state = $2 AND generation > $3",
            )
            .bind(resource.as_str())
            .bind(STATE_DETACHED)
            .bind(newer_than)
            .fetch_optional(pool)
            .await
            .expect("read detached durable session");
            if let Some((generation, inbound, outbound)) = detached {
                assert_eq!(inbound, 1, "original protected request was handled once");
                assert_eq!(outbound, 1, "exactly one protected reply is in custody");
                return generation;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("session becomes durable and detached")
}

fn subscription_id(reply: &Iq, service: &BareJid, node: &str, full_jid: &str) -> String {
    let Iq::Result {
        from: Some(from),
        to: Some(to),
        ..
    } = reply
    else {
        panic!("expected addressed subscription result: {reply:?}");
    };
    assert_eq!(from.as_str(), service.as_str());
    assert_eq!(to.as_str(), full_jid);
    let subscription = result_payload(reply)
        .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
        .expect("subscription result");
    assert_eq!(subscription.attr("node"), Some(node));
    assert_eq!(subscription.attr("jid"), Some(PEER));
    assert_eq!(subscription.attr("subscription"), Some("subscribed"));
    subscription
        .attr("subid")
        .filter(|id| !id.is_empty())
        .expect("subid")
        .to_owned()
}

fn assert_pubsub_error(reply: &Iq, condition: DefinedCondition, extension: &str) {
    let Iq::Error { error, .. } = reply else {
        panic!("expected subscription error {extension}: {reply:?}");
    };
    assert_eq!(error.defined_condition, condition, "{reply:?}");
    assert!(
        error
            .other
            .as_ref()
            .is_some_and(|element| element.is(extension, NS_PUBSUB_ERRORS)),
        "{reply:?}"
    );
}

pub(super) async fn assert_local_subscription_lifecycle(
    service: &BareJid,
    node: &str,
    first: &Replica,
    member: &mut WsXmppClient,
    database_url: &str,
) {
    let subscribe = |id, target| set(id, service.as_str(), pubsub_subscribe(node, target));
    let unsubscribe = |id: &str, target: &str, subid: Option<&str>| {
        let mut operation = Element::builder("unsubscribe", NS_PUBSUB)
            .attr(xml_ncname!("node").to_owned(), node)
            .attr(xml_ncname!("jid").to_owned(), target);
        if let Some(subid) = subid {
            operation = operation.attr(xml_ncname!("subid").to_owned(), subid);
        }
        set(
            id,
            service.as_str(),
            Element::builder("pubsub", NS_PUBSUB)
                .append(operation.build())
                .build(),
        )
    };

    first.account("carol").await;
    let mut outsider = first.client("carol", "desk").await;
    let denied = round_trip(&mut outsider, subscribe("space-subscribe-outsider", CAROL)).await;
    assert!(
        is_error(&denied, DefinedCondition::ItemNotFound),
        "{denied:?}"
    );
    let denied = round_trip(member, subscribe("space-subscribe-wrong-subject", OWNER)).await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");

    let first_reply = round_trip(member, subscribe("space-subscribe-member", PEER)).await;
    let subid = subscription_id(
        &first_reply,
        service,
        node,
        member.full_jid.as_deref().expect("bound member"),
    );

    let mut alternate = first.client("alice", "subscription-desk").await;
    send_stanza(
        &mut alternate,
        Element::builder("enable", SM_NS)
            .attr(xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = receive_sm(&mut alternate, &["enabled", "failed"]).await;
    assert!(enabled.is("enabled", SM_NS), "{enabled:?}");
    let token = enabled.attr("id").expect("durable resume token");
    let resource: FullJid = alternate
        .full_jid
        .as_deref()
        .expect("bound")
        .parse()
        .expect("JID");

    let retained = round_trip(&mut alternate, subscribe("space-subscribe-retained", PEER)).await;
    assert_eq!(
        subscription_id(&retained, service, node, resource.as_str()),
        subid,
        "the other resource and replica renew the same account preference"
    );
    drop(alternate);

    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url)
        .await
        .expect("test readback pool");
    let first_detach = wait_for_detach(&pool, &resource, 0).await;
    let resume = || {
        Element::builder("resume", SM_NS)
            .attr(xml_ncname!("previd").to_owned(), token)
            .attr(xml_ncname!("h").to_owned(), "0")
            .build()
    };
    let mut resumed = authenticate_unbound(&first.url, "alice").await;
    send_stanza(&mut resumed, resume()).await;
    let confirmation = receive_sm(&mut resumed, &["resumed", "failed"]).await;
    assert!(confirmation.is("resumed", SM_NS), "{confirmation:?}");
    assert_eq!(confirmation.attr("previd"), Some(token));
    assert_eq!(confirmation.attr("h"), Some("1"));
    assert_eq!(
        receive_iq(&mut resumed, "space-subscribe-retained").await,
        retained,
        "a current subscription confirmation replays exactly"
    );
    drop(resumed);
    // A second detach has a newer durable session generation.
    wait_for_detach(&pool, &resource, first_detach).await;

    let denied = round_trip(
        &mut outsider,
        unsubscribe("space-unsubscribe-wrong-subject", PEER, Some(&subid)),
    )
    .await;
    assert!(is_error(&denied, DefinedCondition::Forbidden), "{denied:?}");
    let missing_id = round_trip(
        member,
        unsubscribe("space-unsubscribe-missing-subid", PEER, None),
    )
    .await;
    assert_pubsub_error(&missing_id, DefinedCondition::BadRequest, "subid-required");
    let denied = round_trip(
        member,
        unsubscribe("space-unsubscribe-wrong-subid", PEER, Some("unknown")),
    )
    .await;
    assert_pubsub_error(&denied, DefinedCondition::NotAcceptable, "invalid-subid");
    let removed = round_trip(
        member,
        unsubscribe("space-unsubscribe-own", PEER, Some(&subid)),
    )
    .await;
    assert!(is_result(&removed), "{removed:?}");
    let absent = round_trip(
        member,
        unsubscribe("space-unsubscribe-absent", PEER, Some(&subid)),
    )
    .await;
    assert_pubsub_error(
        &absent,
        DefinedCondition::UnexpectedRequest,
        "not-subscribed",
    );

    let renewed = round_trip(member, subscribe("space-resubscribe-member", PEER)).await;
    let renewed_subid = subscription_id(
        &renewed,
        service,
        node,
        member.full_jid.as_deref().expect("bound member"),
    );
    assert_ne!(
        renewed_subid, subid,
        "a new preference has a different subid"
    );

    let mut resumed = authenticate_unbound(&first.url, "alice").await;
    let registry = &first.state.deps.protocol.connection_registry;
    let registration_lock = registry.lock_bind(&resource).await;
    let mut pending_bind = registry.observe_next_pending_bind(&resource);
    send_stanza(&mut resumed, resume()).await;
    let failure = tokio::select! {
        biased;
        observed = &mut pending_bind => panic!("stale subscription replay reached registration: {observed:?}"),
        failure = receive_sm(&mut resumed, &["resumed", "failed"]) => failure,
    };
    assert!(failure.is("failed", SM_NS), "{failure:?}");
    assert!(
        failure
            .get_child("item-not-found", xmpp_parsers::ns::XMPP_STANZAS)
            .is_some(),
        "{failure:?}"
    );
    assert_eq!(
        failure.attr("h"),
        Some("1"),
        "the retained subscribe IQ was handled"
    );
    assert!(!registry.list_connections().contains(&resource));
    drop(registration_lock);
    let removed = round_trip(
        member,
        unsubscribe("space-unsubscribe-renewed", PEER, Some(&renewed_subid)),
    )
    .await;
    assert!(is_result(&removed), "{removed:?}");
    pool.close().await;
}
