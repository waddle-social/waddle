//! The S2S router's half of account traffic (ADR-0020 D7; gate G18;
//! `server/docs/planning/s2s-account-traffic-2047.md`), with a recording
//! account side in place of the WebSocket one: what reaches the account
//! side and in which order, the answers the router sends back and their
//! bound, the deadline after which the router abandons the account side's
//! work, IQ requests to local resources and their correlation, the bound
//! on remote requests per domain, a domain the account side no longer
//! admits, and a local account's messages and presence for remote JIDs
//! once the account side is attached, refused for a retired account and
//! beyond the account's share of the pair queues.
//!
//! `home-a.test` runs the router; `home-b.test` sends through its own
//! connection manager and keeps what it receives. No PostgreSQL, SpiceDB or
//! second process is involved.

// Each suite uses a different part of the shared fixture.
#[allow(dead_code, unused_imports)]
#[path = "s2s_routing_support/mod.rs"]
mod support;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use jid::{FullJid, Jid};
use minidom::Element;
use support::{
    error_condition, jid, remote, routed_and_capturing, send, wait_for, Routed, ARRIVES, HOME_A,
    HOME_B, QUIET,
};
use waddle_server::destination::{
    offer_stanza, IqOffer, StanzaOffer, UnroutableReason, REMOTE_ROUTE_OFFER_TIMEOUT,
};
use waddle_server::s2s::route::{
    AccountWork, LocalAccounts, RemotePrincipal, RouterLimits, MAX_ACCOUNT_REPLIES,
};
use waddle_xmpp::xep::xep0199::NS_PING;
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const BOB: &str = "bob@home-b.test";
const BOB_PHONE: &str = "bob@home-b.test/phone";
const ALICE: &str = "alice@home-a.test";
const ALICE_PHONE: &str = "alice@home-a.test/phone";

/// An account side that records what reaches it, answers with what a case
/// gives it, admits IQ requests while told to, and takes as long as a case
/// tells it for a given stanza.
#[derive(Default)]
struct RecordingAccounts {
    received: Mutex<Vec<(Jid, Jid, Stanza)>>,
    answers: Mutex<HashMap<String, Vec<Stanza>>>,
    admit: AtomicBool,
    occupant_requests: Mutex<Vec<bool>>,
    delays: Mutex<HashMap<String, Duration>>,
    /// The stanzas whose work the router had abandoned when it ended.
    abandoned: Mutex<Vec<String>>,
    /// The sender's domain is no longer admitted.
    domain_refused: AtomicBool,
    /// The local account has been retired.
    retired: AtomicBool,
}

impl RecordingAccounts {
    fn received(&self) -> Vec<(Jid, Jid, Stanza)> {
        match self.received.lock() {
            Ok(received) => received.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn received_ids(&self) -> Vec<String> {
        self.received()
            .into_iter()
            .filter_map(|(_, _, stanza)| stanza_id(&stanza))
            .collect()
    }

    fn answer(&self, id: &str, answers: Vec<Stanza>) {
        match self.answers.lock() {
            Ok(mut map) => map.insert(id.to_owned(), answers),
            Err(poisoned) => poisoned.into_inner().insert(id.to_owned(), answers),
        };
    }

    fn delay(&self, id: &str, delay: Duration) {
        match self.delays.lock() {
            Ok(mut delays) => delays.insert(id.to_owned(), delay),
            Err(poisoned) => poisoned.into_inner().insert(id.to_owned(), delay),
        };
    }

    fn abandoned(&self) -> Vec<String> {
        match self.abandoned.lock() {
            Ok(abandoned) => abandoned.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

fn stanza_id(stanza: &Stanza) -> Option<String> {
    match stanza {
        Stanza::Message(message) => message.id.as_ref().map(|id| id.0.clone()),
        Stanza::Presence(presence) => presence.id.clone(),
        Stanza::Iq(iq) => Some(iq.id().to_owned()),
    }
}

#[async_trait]
impl LocalAccounts for RecordingAccounts {
    fn admits(&self, _principal: &RemotePrincipal) -> bool {
        !self.domain_refused.load(Ordering::SeqCst)
    }

    async fn receive(
        &self,
        principal: &RemotePrincipal,
        to: &Jid,
        stanza: Stanza,
        work: &AccountWork,
    ) -> Vec<Stanza> {
        let id = stanza_id(&stanza).unwrap_or_default();
        let delay = match self.delays.lock() {
            Ok(delays) => delays.get(&id).copied(),
            Err(poisoned) => poisoned.into_inner().get(&id).copied(),
        };
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
        if work.abandoned() {
            match self.abandoned.lock() {
                Ok(mut abandoned) => abandoned.push(id.clone()),
                Err(poisoned) => poisoned.into_inner().push(id.clone()),
            }
        }
        match self.received.lock() {
            Ok(mut received) => received.push((principal.address().clone(), to.clone(), stanza)),
            Err(poisoned) => {
                poisoned
                    .into_inner()
                    .push((principal.address().clone(), to.clone(), stanza))
            }
        }
        match self.answers.lock() {
            Ok(mut map) => map.remove(&id).unwrap_or_default(),
            Err(poisoned) => poisoned.into_inner().remove(&id).unwrap_or_default(),
        }
    }

    async fn admits_request(
        &self,
        _principal: &RemotePrincipal,
        _to: &FullJid,
        room_occupant: bool,
    ) -> bool {
        match self.occupant_requests.lock() {
            Ok(mut seen) => seen.push(room_occupant),
            Err(poisoned) => poisoned.into_inner().push(room_occupant),
        }
        self.admit.load(Ordering::SeqCst)
    }

    async fn may_send(&self, _account: &jid::BareJid) -> bool {
        !self.retired.load(Ordering::SeqCst)
    }
}

async fn with_accounts(
    limits: RouterLimits,
) -> (
    support::transport::Pki,
    Routed,
    support::Deployment,
    Arc<support::CapturingSink>,
    Arc<RecordingAccounts>,
) {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, limits, |_| {}).await;
    let accounts = Arc::new(RecordingAccounts::default());
    assert!(a.router.attach_accounts(accounts.clone()));
    (pki, a, b, captured, accounts)
}

fn chat(from: &str, to: &str, id: &str) -> Message {
    let mut message = Message::new_with_type(MessageType::Chat, Some(jid(to)));
    message.from = Some(jid(from));
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), "hi".to_owned());
    message
}

fn presence(from: &str, to: &str, type_: PresenceType, id: &str) -> Presence {
    let mut presence = Presence::new(type_);
    presence.from = Some(jid(from));
    presence.to = Some(jid(to));
    presence.id = Some(id.to_owned());
    presence
}

fn ping(from: &str, to: &str, id: &str) -> Iq {
    Iq::Get {
        from: Some(jid(from)),
        to: Some(jid(to)),
        id: id.to_owned(),
        payload: Element::builder("ping", NS_PING).build(),
    }
}

async fn offer(a: &Routed, stanza: Stanza) -> StanzaOffer {
    let to = match &stanza {
        Stanza::Message(message) => message.to.clone(),
        Stanza::Presence(presence) => presence.to.clone(),
        Stanza::Iq(iq) => iq.to().cloned(),
    }
    .expect("an addressed stanza");
    offer_stanza(
        &a.router,
        remote(&a.served, to.as_str()),
        stanza,
        a.served.local(),
        REMOTE_ROUTE_OFFER_TIMEOUT,
    )
    .await
}

#[tokio::test]
async fn account_traffic_reaches_the_account_side_with_its_principal_in_the_senders_order() {
    let (_pki, _a, b, _captured, accounts) = with_accounts(RouterLimits::default()).await;

    send(
        &b,
        HOME_B,
        HOME_A,
        presence(BOB, ALICE, PresenceType::Subscribe, "o1"),
    )
    .await;
    send(&b, HOME_B, HOME_A, chat(BOB_PHONE, ALICE, "o2")).await;
    send(&b, HOME_B, HOME_A, chat(BOB_PHONE, ALICE_PHONE, "o3")).await;
    send(
        &b,
        HOME_B,
        HOME_A,
        presence(BOB_PHONE, ALICE_PHONE, PresenceType::None, "o4"),
    )
    .await;

    let ids = wait_for(ARRIVES, || {
        let ids = accounts.received_ids();
        (ids.len() == 4).then_some(ids)
    })
    .await
    .expect("every stanza reaches the account side");
    assert_eq!(ids, vec!["o1", "o2", "o3", "o4"]);
    let received = accounts.received();
    assert_eq!(received[0].0, jid(BOB));
    assert_eq!(received[0].1, jid(ALICE));
    assert_eq!(received[2].0, jid(BOB_PHONE));
    assert_eq!(received[2].1, jid(ALICE_PHONE));
}

#[tokio::test]
async fn the_account_sides_answers_go_back_to_the_sender_within_the_bound() {
    let (_pki, _a, b, captured, accounts) = with_accounts(RouterLimits::default()).await;
    let answers: Vec<Stanza> = (0..MAX_ACCOUNT_REPLIES + 4)
        .map(|index| {
            let mut answer = Presence::new(PresenceType::None);
            answer.from = Some(jid(&["alice@home-a.test/r", &index.to_string()].concat()));
            answer.to = Some(jid(BOB));
            answer.id = Some("answer".to_owned());
            Stanza::Presence(answer)
        })
        .collect();
    accounts.answer("probe", answers);

    send(
        &b,
        HOME_B,
        HOME_A,
        presence(BOB, ALICE, PresenceType::Probe, "probe"),
    )
    .await;

    wait_for(ARRIVES, || {
        (captured.with_id("answer").len() == MAX_ACCOUNT_REPLIES).then_some(())
    })
    .await
    .expect("the answers within the bound arrive");
    tokio::time::sleep(QUIET).await;
    assert_eq!(captured.with_id("answer").len(), MAX_ACCOUNT_REPLIES);
}

#[tokio::test]
async fn an_iq_request_to_a_bare_jid_never_reaches_the_account_side() {
    let (_pki, _a, b, captured, accounts) = with_accounts(RouterLimits::default()).await;
    accounts.admit.store(true, Ordering::SeqCst);

    send(&b, HOME_B, HOME_A, ping(BOB_PHONE, ALICE, "bare")).await;

    let refusal = wait_for(ARRIVES, || captured.with_id("bare").into_iter().next())
        .await
        .expect("the request is answered");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(accounts.received().is_empty());
    let seen = match accounts.occupant_requests.lock() {
        Ok(seen) => seen.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    assert!(seen.is_empty());
}

#[tokio::test]
async fn an_admitted_request_reaches_its_resource_and_only_that_resources_answer_leaves() {
    let (_pki, a, b, captured, accounts) = with_accounts(RouterLimits::default()).await;
    accounts.admit.store(true, Ordering::SeqCst);

    send(&b, HOME_B, HOME_A, ping(BOB_PHONE, ALICE_PHONE, "full")).await;
    let delivered = wait_for(ARRIVES, || a.delivery.iqs("full").into_iter().next())
        .await
        .expect("the request reaches the resource");
    assert_eq!(delivered.0.as_str(), ALICE_PHONE);

    // Another resource's answer completes nothing.
    let other = a
        .offer_iq(
            BOB_PHONE,
            Iq::Result {
                from: Some(jid("alice@home-a.test/laptop")),
                to: Some(jid(BOB_PHONE)),
                id: "full".to_owned(),
                payload: None,
            },
        )
        .await;
    assert!(matches!(other, IqOffer::Accepted));
    tokio::time::sleep(QUIET).await;
    assert!(captured.with_id("full").is_empty());

    let answer = Iq::Result {
        from: Some(jid(ALICE_PHONE)),
        to: Some(jid(BOB_PHONE)),
        id: "full".to_owned(),
        payload: None,
    };
    assert!(matches!(
        a.offer_iq(BOB_PHONE, answer.clone()).await,
        IqOffer::Accepted
    ));
    let left = wait_for(ARRIVES, || captured.with_id("full").into_iter().next())
        .await
        .expect("the resource's answer leaves");
    assert_eq!(left.attr("type"), Some("result"));
    // A second answer completes nothing.
    assert!(matches!(
        a.offer_iq(BOB_PHONE, answer).await,
        IqOffer::Accepted
    ));
    tokio::time::sleep(QUIET).await;
    assert_eq!(captured.with_id("full").len(), 1);
}

#[tokio::test]
async fn a_request_the_account_side_does_not_admit_is_refused_and_not_delivered() {
    let (_pki, a, b, captured, _accounts) = with_accounts(RouterLimits::default()).await;

    send(&b, HOME_B, HOME_A, ping(BOB_PHONE, ALICE_PHONE, "refused")).await;

    let refusal = wait_for(ARRIVES, || captured.with_id("refused").into_iter().next())
        .await
        .expect("the request is answered");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
    assert!(a.delivery.iqs("refused").is_empty());
}

#[tokio::test]
async fn remote_requests_beyond_a_domains_share_are_refused_with_resource_constraint() {
    let limits = RouterLimits {
        account_requests_per_peer: 2,
        ..RouterLimits::default()
    };
    let (_pki, _a, b, captured, accounts) = with_accounts(limits).await;
    accounts.admit.store(true, Ordering::SeqCst);

    for id in ["r1", "r2", "r3"] {
        send(&b, HOME_B, HOME_A, ping(BOB_PHONE, ALICE_PHONE, id)).await;
    }

    let refusal = wait_for(ARRIVES, || captured.with_id("r3").into_iter().next())
        .await
        .expect("the third request is refused");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("resource-constraint")
    );
    assert!(captured.with_id("r1").is_empty());
    assert!(captured.with_id("r2").is_empty());
}

#[tokio::test]
async fn a_slow_account_side_is_abandoned_at_its_deadline_and_the_sender_moves_on() {
    let limits = RouterLimits {
        account_timeout: Duration::from_millis(200),
        ..RouterLimits::default()
    };
    let (_pki, _a, b, captured, accounts) = with_accounts(limits).await;
    let slow = Duration::from_secs(3);
    accounts.delay("slow", slow);
    accounts.answer(
        "slow",
        vec![Stanza::Presence(presence(
            ALICE,
            BOB,
            PresenceType::Subscribed,
            "late",
        ))],
    );

    let started = tokio::time::Instant::now();
    send(
        &b,
        HOME_B,
        HOME_A,
        presence(BOB, ALICE, PresenceType::Subscribe, "slow"),
    )
    .await;
    // The same sender's next stanza is decided after the slow one's
    // deadline, long before its work ends.
    send(&b, HOME_B, HOME_A, chat(BOB_PHONE, ALICE, "next")).await;
    wait_for(ARRIVES, || {
        accounts
            .received_ids()
            .contains(&"next".to_owned())
            .then_some(())
    })
    .await
    .expect("the next stanza reaches the account side");
    assert!(
        started.elapsed() < slow,
        "the worker waited for the slow stanza: {:?}",
        started.elapsed()
    );
    assert!(!accounts.received_ids().contains(&"slow".to_owned()));

    // The slow work ends abandoned, after the next stanza, and its answer
    // never leaves.
    wait_for(ARRIVES, || {
        accounts
            .abandoned()
            .contains(&"slow".to_owned())
            .then_some(())
    })
    .await
    .expect("the slow work sees that it was abandoned");
    assert_eq!(accounts.received_ids(), vec!["next", "slow"]);
    let control = ping(BOB_PHONE, HOME_A, "after-slow");
    send(&b, HOME_B, HOME_A, control).await;
    wait_for(ARRIVES, || {
        captured.with_id("after-slow").into_iter().next()
    })
    .await
    .expect("the server answers a ping on the same pair after the slow work ended");
    assert!(captured.with_id("late").is_empty());
}

#[tokio::test]
async fn a_request_from_a_domain_the_account_side_no_longer_admits_is_not_answered() {
    let (_pki, a, b, captured, accounts) = with_accounts(RouterLimits::default()).await;
    accounts.admit.store(true, Ordering::SeqCst);
    accounts.domain_refused.store(true, Ordering::SeqCst);

    send(
        &b,
        HOME_B,
        HOME_A,
        ping(BOB_PHONE, ALICE_PHONE, "unadmitted"),
    )
    .await;
    send(
        &b,
        HOME_B,
        HOME_A,
        ping(BOB_PHONE, HOME_A, "unadmitted-control"),
    )
    .await;
    wait_for(ARRIVES, || {
        captured.with_id("unadmitted-control").into_iter().next()
    })
    .await
    .expect("the server answers a ping on the same pair");
    tokio::time::sleep(QUIET).await;

    assert!(captured.with_id("unadmitted").is_empty());
    assert!(a.delivery.iqs("unadmitted").is_empty());
    let asked = match accounts.occupant_requests.lock() {
        Ok(seen) => seen.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    assert!(asked.is_empty(), "the request was decided: {asked:?}");
}

#[tokio::test]
async fn with_an_account_side_a_local_accounts_direct_message_and_subscription_leave() {
    let (_pki, a, _b, captured, _accounts) = with_accounts(RouterLimits::default()).await;

    let accepted = offer(&a, Stanza::Message(chat(ALICE_PHONE, BOB, "dm"))).await;
    assert!(matches!(accepted, StanzaOffer::Accepted), "{accepted:?}");
    let subscribe = offer(
        &a,
        Stanza::Presence(presence(ALICE, BOB, PresenceType::Subscribe, "sub")),
    )
    .await;
    assert!(matches!(subscribe, StanzaOffer::Accepted), "{subscribe:?}");
    let directed = offer(
        &a,
        Stanza::Presence(presence(
            ALICE_PHONE,
            BOB_PHONE,
            PresenceType::Unavailable,
            "gone",
        )),
    )
    .await;
    assert!(matches!(directed, StanzaOffer::Accepted), "{directed:?}");

    for id in ["dm", "sub", "gone"] {
        wait_for(ARRIVES, || captured.with_id(id).into_iter().next())
            .await
            .unwrap_or_else(|| panic!("`{id}` leaves"));
    }
}

#[tokio::test]
async fn a_retired_accounts_messages_and_presence_never_leave() {
    let (_pki, a, b, captured, accounts) = with_accounts(RouterLimits::default()).await;
    accounts.retired.store(true, Ordering::SeqCst);

    for stanza in [
        Stanza::Message(chat(ALICE_PHONE, BOB, "retired-dm")),
        Stanza::Presence(presence(ALICE, BOB, PresenceType::Subscribe, "retired-sub")),
        Stanza::Presence(presence(ALICE, BOB, PresenceType::Probe, "retired-probe")),
        Stanza::Presence(presence(
            ALICE_PHONE,
            BOB,
            PresenceType::None,
            "retired-broadcast",
        )),
    ] {
        let refused = offer(&a, stanza).await;
        assert!(
            matches!(
                refused,
                StanzaOffer::Refused {
                    reason: UnroutableReason::RefusedByPolicy,
                    ..
                }
            ),
            "{refused:?}"
        );
    }
    send(
        &b,
        HOME_B,
        HOME_A,
        ping(BOB_PHONE, HOME_A, "retired-control"),
    )
    .await;
    wait_for(ARRIVES, || {
        captured.with_id("retired-control").into_iter().next()
    })
    .await
    .expect("the server answers a ping on the same pair");
    tokio::time::sleep(QUIET).await;
    assert!(captured
        .stanzas()
        .iter()
        .all(|stanza| stanza.attr("id") == Some("retired-control")));
}

#[tokio::test]
async fn an_account_beyond_its_share_of_the_pair_queues_is_refused_as_a_full_queue_refuses() {
    let limits = RouterLimits {
        account_queue_per_sender: 0,
        ..RouterLimits::default()
    };
    let (_pki, a, _b, _captured, _accounts) = with_accounts(limits).await;

    let refused = offer(&a, Stanza::Message(chat(ALICE_PHONE, BOB, "share"))).await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::EgressBusy,
                ..
            }
        ),
        "{refused:?}"
    );
    let refused = offer(
        &a,
        Stanza::Presence(presence(ALICE, BOB, PresenceType::Probe, "share-probe")),
    )
    .await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::EgressBusy,
                ..
            }
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn groupchat_outside_a_joined_room_and_the_dm_pin_marker_never_leave() {
    let (_pki, a, _b, _captured, _accounts) = with_accounts(RouterLimits::default()).await;

    let mut groupchat = chat(ALICE_PHONE, "room@muc.home-b.test", "gc");
    groupchat.type_ = MessageType::Groupchat;
    let refused = offer(&a, Stanza::Message(groupchat)).await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::RefusedByPolicy,
                ..
            }
        ),
        "{refused:?}"
    );

    let mut pin = chat(ALICE_PHONE, BOB, "pin");
    pin.payloads.push(
        Element::builder("pinned", waddle_xmpp::xep::xep_waddle_pin::NS_WADDLE_PIN_V0).build(),
    );
    let refused = offer(&a, Stanza::Message(pin)).await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::RefusedByPolicy,
                ..
            }
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn without_an_account_side_account_traffic_stays_refused_in_both_directions() {
    let pki = support::Pki::new();
    let (a, b, captured) = routed_and_capturing(&pki, RouterLimits::default(), |_| {}).await;

    let refused = offer(&a, Stanza::Message(chat(ALICE_PHONE, BOB, "dm"))).await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::RefusedByPolicy,
                ..
            }
        ),
        "{refused:?}"
    );
    let refused = offer(
        &a,
        Stanza::Presence(presence(ALICE, BOB, PresenceType::Subscribe, "sub")),
    )
    .await;
    assert!(
        matches!(
            refused,
            StanzaOffer::Refused {
                reason: UnroutableReason::RefusedByPolicy,
                ..
            }
        ),
        "{refused:?}"
    );

    send(&b, HOME_B, HOME_A, chat(BOB_PHONE, ALICE, "in")).await;
    let refusal = wait_for(ARRIVES, || captured.with_id("in").into_iter().next())
        .await
        .expect("the message is refused");
    assert_eq!(
        error_condition(&refusal).as_deref(),
        Some("service-unavailable")
    );
}
