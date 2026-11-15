//! IQ correlation across S2S streams (ADR-0020 transport; gate G8): the
//! shared correlation table completes a pending request at most once, by
//! the response that matches its sender, recipient, id and payload
//! namespace before it expires, whichever replica that response reaches;
//! a replayed, forged or late response completes nothing; a request
//! without a response is answered once at expiry; the replica that sent a
//! request another replica completed releases it at its next sweep; a
//! response carrying an address outside the identity profile answers its
//! request with an error at once; the expired requests of a replica that
//! stopped recording that it is alive are pruned, and a live replica prunes
//! its own requests long past expiry; and a key too long for the table is
//! refused.
//!
//! Two correlation stores over one database stand for two replicas of one
//! deployment. The routed cases use two in-process deployments on loopback
//! with the connection-manager fixture's test CA; one of them runs two
//! routers that share a database, as two replicas would. No PostgreSQL,
//! SpiceDB or second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use std::time::Duration;

use minidom::Element;
use support::{
    correlation_database, full, jid, listener, routed_and_capturing, send, start_routed, wait_for,
    Pki, Spec, ARRIVES, HOME_A, HOME_B, QUIET,
};
use waddle_server::destination::IqOffer;
use waddle_server::s2s::route::{
    CorrelationDirection, CorrelationError, CorrelationKey, CorrelationOwner, IqCorrelations,
    LocalOrigin, PendingRequest, RequestKind, RouterLimits, SqlIqCorrelations,
    MAX_CORRELATED_ADDRESS_BYTES,
};
use waddle_xmpp::disco::{DISCO_INFO_NS, DISCO_ITEMS_NS};
use waddle_xmpp::xep::xep0199::NS_PING;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::DefinedCondition;

const ALICE: &str = "alice@home-a.test/phone";
const NOW: i64 = 1_900_000_000_000;

fn key(id: &str) -> CorrelationKey {
    CorrelationKey {
        direction: CorrelationDirection::Outbound,
        local: jid(ALICE),
        remote: jid(HOME_B),
        id: id.to_owned(),
    }
}

fn pending(id: &str, expires_at_ms: i64) -> PendingRequest {
    owned(id, expires_at_ms, "replica-1")
}

fn owned(id: &str, expires_at_ms: i64, owner: &str) -> PendingRequest {
    PendingRequest {
        key: key(id),
        kind: RequestKind::Get,
        payload_namespace: DISCO_INFO_NS.to_owned(),
        owner: CorrelationOwner::named(owner),
        expires_at_ms,
    }
}

/// Two replicas' views of one deployment's correlation table.
async fn replicas(name: &str) -> (SqlIqCorrelations, SqlIqCorrelations) {
    let database = correlation_database(name).await;
    (
        SqlIqCorrelations::new(database.clone()),
        SqlIqCorrelations::new(database),
    )
}

#[tokio::test]
async fn a_request_completes_at_most_once_on_any_replica() {
    let (first, second) = replicas("correlation-once").await;
    first
        .register(&pending("c1", NOW + 60_000), NOW)
        .await
        .expect("registered");
    assert_eq!(
        second
            .complete(&key("c1"), Some(DISCO_INFO_NS), NOW + 1)
            .await,
        Ok(true)
    );
    // A replay, on either replica, completes nothing.
    assert_eq!(second.complete(&key("c1"), None, NOW + 2).await, Ok(false));
    assert_eq!(first.complete(&key("c1"), None, NOW + 2).await, Ok(false));
    // Nor does a failure or expiry afterwards answer it again.
    assert_eq!(first.withdraw(&key("c1")).await, Ok(false));
    assert_eq!(first.expire(&key("c1"), NOW + 120_000).await, Ok(false));
}

#[tokio::test]
async fn a_response_must_match_recipient_sender_id_and_payload_namespace() {
    let (first, second) = replicas("correlation-match").await;
    first
        .register(&pending("c2", NOW + 60_000), NOW)
        .await
        .expect("registered");
    let mismatches = [
        CorrelationKey {
            remote: jid("mallory@home-b.test"),
            ..key("c2")
        },
        CorrelationKey {
            remote: jid("evil.test"),
            ..key("c2")
        },
        CorrelationKey {
            local: jid("alice@home-a.test/laptop"),
            ..key("c2")
        },
        CorrelationKey {
            local: jid("alice@home-a.test"),
            ..key("c2")
        },
        CorrelationKey {
            direction: CorrelationDirection::Inbound,
            ..key("c2")
        },
        key("c2-other"),
    ];
    for mismatch in &mismatches {
        assert_eq!(
            second.complete(mismatch, None, NOW + 1).await,
            Ok(false),
            "{mismatch:?}"
        );
    }
    // A result whose payload is in another namespace answers another
    // request.
    assert_eq!(
        second.complete(&key("c2"), Some(NS_PING), NOW + 1).await,
        Ok(false)
    );
    // An error, or a result without a payload, may answer any request.
    assert_eq!(second.complete(&key("c2"), None, NOW + 1).await, Ok(true));
}

#[tokio::test]
async fn an_expired_request_completes_nothing_and_expires_once() {
    let (first, second) = replicas("correlation-expiry").await;
    first
        .register(&pending("c3", NOW + 1_000), NOW)
        .await
        .expect("registered");
    assert_eq!(second.expire(&key("c3"), NOW + 999).await, Ok(false));
    assert_eq!(
        second.complete(&key("c3"), None, NOW + 1_000).await,
        Ok(false)
    );
    assert_eq!(first.expire(&key("c3"), NOW + 1_000).await, Ok(true));
    assert_eq!(second.expire(&key("c3"), NOW + 1_000).await, Ok(false));
}

#[tokio::test]
async fn a_pending_key_is_refused_until_it_expires() {
    let (first, second) = replicas("correlation-duplicate").await;
    first
        .register(&pending("c4", NOW + 1_000), NOW)
        .await
        .expect("registered");
    assert_eq!(
        second.register(&pending("c4", NOW + 2_000), NOW + 1).await,
        Err(CorrelationError::Duplicate)
    );
    // Once expired, the key is free again, and the new request is the one
    // a response completes.
    second
        .register(&pending("c4", NOW + 5_000), NOW + 1_000)
        .await
        .expect("an expired request is replaced");
    assert_eq!(
        first.complete(&key("c4"), None, NOW + 4_000).await,
        Ok(true)
    );
}

#[tokio::test]
async fn withdrawal_and_completion_have_one_winner() {
    let (first, second) = replicas("correlation-withdraw").await;
    first
        .register(&pending("c5", NOW + 60_000), NOW)
        .await
        .expect("registered");
    assert_eq!(first.withdraw(&key("c5")).await, Ok(true));
    assert_eq!(second.complete(&key("c5"), None, NOW + 1).await, Ok(false));

    first
        .register(&pending("c6", NOW + 60_000), NOW)
        .await
        .expect("registered");
    let raced = key("c6");
    let (completed, withdrawn) = tokio::join!(
        second.complete(&raced, None, NOW + 1),
        first.withdraw(&raced),
    );
    assert_eq!(
        [completed, withdrawn]
            .into_iter()
            .filter(|outcome| *outcome == Ok(true))
            .count(),
        1
    );
}

#[tokio::test]
async fn requests_left_by_a_stopped_replica_are_pruned() {
    let (first, second) = replicas("correlation-prune").await;
    let (stopped, alive) = ("stopped", "alive");
    first
        .register(&owned("old", NOW + 1_000, stopped), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("unexpired", NOW + 600_000, stopped), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("kept", NOW + 1_000, alive), NOW)
        .await
        .expect("registered");
    first
        .heartbeat(&CorrelationOwner::named(stopped), NOW)
        .await
        .expect("recorded");
    first
        .heartbeat(&CorrelationOwner::named(alive), NOW + 2_500)
        .await
        .expect("recorded");
    // Only the expired request of the replica that has not recorded since
    // the cut-off is pruned; a live replica answers its own.
    assert_eq!(second.prune(NOW + 2_000).await, Ok(1));
    assert_eq!(
        second.complete(&key("unexpired"), None, NOW + 3_000).await,
        Ok(true)
    );
    assert_eq!(first.expire(&key("kept"), NOW + 3_000).await, Ok(true));
}

#[tokio::test]
async fn a_live_replica_prunes_its_own_requests_long_past_expiry() {
    let (first, second) = replicas("correlation-prune-own").await;
    let (alive, other) = ("alive", "other");
    first
        .register(&owned("orphaned", NOW + 1_000, alive), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("recent", NOW + 5_000, alive), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("theirs", NOW + 1_000, other), NOW)
        .await
        .expect("registered");
    // Only this replica's request that expired at or before the cut-off is
    // removed; another replica's rows are left to that replica.
    assert_eq!(
        first
            .prune_own(&CorrelationOwner::named(alive), NOW + 2_000)
            .await,
        Ok(1)
    );
    assert_eq!(
        second.expire(&key("orphaned"), NOW + 6_000).await,
        Ok(false)
    );
    assert_eq!(second.expire(&key("recent"), NOW + 6_000).await, Ok(true));
    assert_eq!(second.expire(&key("theirs"), NOW + 6_000).await, Ok(true));
}

#[tokio::test]
async fn a_replica_reads_which_of_its_requests_are_still_stored() {
    let (first, second) = replicas("correlation-outstanding").await;
    first
        .register(&owned("o1", NOW + 60_000, "replica-1"), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("o2", NOW + 60_000, "replica-1"), NOW)
        .await
        .expect("registered");
    first
        .register(&owned("o3", NOW + 60_000, "replica-2"), NOW)
        .await
        .expect("registered");
    assert_eq!(second.complete(&key("o1"), None, NOW + 1).await, Ok(true));
    let outstanding = first
        .outstanding(&CorrelationOwner::named("replica-1"))
        .await
        .expect("read");
    assert_eq!(outstanding, vec![key("o2")]);
}

#[tokio::test]
async fn a_key_too_long_for_the_table_is_refused() {
    let (first, _) = replicas("correlation-too-long").await;
    let long_local = [
        "a".repeat(MAX_CORRELATED_ADDRESS_BYTES).as_str(),
        "@home-a.test/phone",
    ]
    .concat();
    let mut request = pending("t1", NOW + 60_000);
    request.key.local = jid(&long_local);
    assert!(!request.key.fits());
    assert_eq!(
        first.register(&request, NOW).await,
        Err(CorrelationError::KeyTooLong)
    );
    let mut request = pending(&"x".repeat(257), NOW + 60_000);
    assert!(!request.key.fits());
    assert_eq!(
        first.register(&request, NOW).await,
        Err(CorrelationError::KeyTooLong)
    );
    request.key.id = String::new();
    assert_eq!(
        first.register(&request, NOW).await,
        Err(CorrelationError::KeyTooLong)
    );
}

fn ping(id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(ALICE)),
        to: None,
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

fn result(from: &str, id: &str) -> Iq {
    Iq::Result {
        from: Some(jid(from)),
        to: Some(jid(ALICE)),
        id: id.to_owned(),
        payload: None,
    }
}

#[tokio::test]
async fn a_response_reaching_another_replica_completes_the_request_there_once() {
    let pki = Pki::new();
    let database = correlation_database("correlation-replicas").await;
    let (listener_a1, _) = listener().await;
    let (listener_a2, address_a2) = listener().await;
    let (listener_b, address_b) = listener().await;
    // A lifetime far longer than the test, so only the sweep's reading of
    // the shared table can release the sending replica's reservation.
    let limits = RouterLimits {
        request_ttl: Duration::from_secs(120),
        sweep_interval: Duration::from_millis(200),
        ..RouterLimits::default()
    };
    // Two replicas of home-a.test share one correlation table; home-b.test
    // dials only the second.
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    let a1 = start_routed(&pki, &spec_a, listener_a1, database.clone(), limits).await;
    let a2 = start_routed(&pki, &spec_a, listener_a2, database, limits).await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.endpoints = vec![(HOME_A, address_a2)];
    let _b = start_routed(
        &pki,
        &spec_b,
        listener_b,
        correlation_database("correlation-replicas-peer").await,
        limits,
    )
    .await;

    assert!(matches!(
        a1.offer_iq(HOME_B, ping("x1")).await,
        IqOffer::Accepted
    ));
    let delivered = wait_for(ARRIVES, || a2.delivery.iqs("x1").into_iter().next())
        .await
        .expect("the replica that received the response delivers it");
    assert_eq!(delivered.0, full(ALICE));
    assert!(matches!(delivered.1, Iq::Result { .. }));
    assert_eq!(a2.delivery.origins("x1"), vec![LocalOrigin::Remote]);
    // The sending replica learns at its next sweep that the request was
    // completed elsewhere and releases it, long before it would expire,
    // without answering it.
    assert!(
        wait_for(Duration::from_secs(3), || (a1.router.pending_requests()
            == 0)
            .then_some(()))
        .await
        .is_some(),
        "the sending replica releases the request promptly"
    );
    tokio::time::sleep(QUIET).await;
    assert!(a1.delivery.iqs("x1").is_empty());
    assert_eq!(a2.delivery.iqs("x1").len(), 1);
}

#[tokio::test]
async fn forged_replayed_and_uncorrelated_responses_are_dropped() {
    let pki = Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    // A response to nothing.
    send(&b, HOME_B, HOME_A, result(HOME_B, "nothing")).await;
    // A request home-b.test receives and does not answer itself.
    assert!(matches!(
        a.offer_iq(HOME_B, ping("f1")).await,
        IqOffer::Accepted
    ));
    assert!(
        wait_for(ARRIVES, || captured.with_id("f1").into_iter().next())
            .await
            .is_some()
    );
    // From another entity of the same domain: not the request's recipient.
    send(&b, HOME_B, HOME_A, result("mallory@home-b.test", "f1")).await;
    // The genuine response, then its replay.
    send(&b, HOME_B, HOME_A, result(HOME_B, "f1")).await;
    send(&b, HOME_B, HOME_A, result(HOME_B, "f1")).await;
    assert!(
        wait_for(ARRIVES, || a.delivery.iqs("f1").into_iter().next())
            .await
            .is_some()
    );
    tokio::time::sleep(QUIET).await;
    let delivered = a.delivery.iqs("f1");
    assert_eq!(delivered.len(), 1, "completed once");
    assert_eq!(delivered[0].1.from(), Some(&jid(HOME_B)));
    assert!(a.delivery.iqs("nothing").is_empty());
}

#[tokio::test]
async fn a_request_without_a_response_is_answered_once_at_expiry() {
    let pki = Pki::new();
    let limits = RouterLimits {
        request_ttl: Duration::from_secs(1),
        sweep_interval: Duration::from_millis(100),
        ..RouterLimits::default()
    };
    let (a, _b, captured) = routed_and_capturing(&pki, limits, |_| {}).await;
    assert!(matches!(
        a.offer_iq(HOME_B, ping("e1")).await,
        IqOffer::Accepted
    ));
    assert!(
        wait_for(ARRIVES, || captured.with_id("e1").into_iter().next())
            .await
            .is_some()
    );
    let answered = wait_for(ARRIVES, || a.delivery.iqs("e1").into_iter().next())
        .await
        .expect("the requester is answered at expiry");
    let Iq::Error { error, from, .. } = &answered.1 else {
        panic!("an error, got {:?}", answered.1);
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::RemoteServerTimeout
    );
    assert_eq!(from.as_ref(), Some(&jid(HOME_B)));
    // The error is this server's, so the requester's blocking list does not
    // drop it although it names the remote entity.
    assert_eq!(a.delivery.origins("e1"), vec![LocalOrigin::Server]);
    tokio::time::sleep(QUIET).await;
    assert_eq!(a.delivery.iqs("e1").len(), 1);
    assert_eq!(a.router.pending_requests(), 0);
}

#[tokio::test]
async fn a_response_with_an_address_outside_the_profile_answers_its_request_at_once() {
    let pki = Pki::new();
    // The default lifetime: only the refused response can answer in time.
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;
    let items = Iq::Get {
        from: Some(jid(ALICE)),
        to: None,
        id: "u1".to_owned(),
        payload: Element::builder("query", DISCO_ITEMS_NS).build(),
    };
    assert!(matches!(a.offer_iq(HOME_B, items).await, IqOffer::Accepted));
    assert!(
        wait_for(ARRIVES, || captured.with_id("u1").into_iter().next())
            .await
            .is_some()
    );
    // home-b.test answers with an item whose localpart is in a spelling the
    // identity profile refuses.
    let item = Element::builder("item", DISCO_ITEMS_NS)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "Bob@home-b.test",
        )
        .build();
    let result = Iq::Result {
        from: Some(jid(HOME_B)),
        to: Some(jid(ALICE)),
        id: "u1".to_owned(),
        payload: Some(
            Element::builder("query", DISCO_ITEMS_NS)
                .append(item)
                .build(),
        ),
    };
    send(&b, HOME_B, HOME_A, result).await;
    let (to, answered) = wait_for(ARRIVES, || a.delivery.iqs("u1").into_iter().next())
        .await
        .expect("the requester is answered");
    assert_eq!(to, full(ALICE));
    let Iq::Error { error, from, .. } = &answered else {
        panic!("an error, got {answered:?}");
    };
    assert_eq!(from.as_ref(), Some(&jid(HOME_B)));
    assert_eq!(
        error.defined_condition,
        DefinedCondition::ServiceUnavailable
    );
    assert_eq!(error.by.as_ref(), Some(&jid(HOME_A)));
    assert_eq!(a.delivery.origins("u1"), vec![LocalOrigin::Server]);
    assert_eq!(a.router.pending_requests(), 0);
    // An IQ result is never answered with an error (RFC 6120 §8.3.1).
    tokio::time::sleep(QUIET).await;
    assert!(captured
        .with_id("u1")
        .iter()
        .all(|stanza| stanza.attr("type") != Some("error")));
}
