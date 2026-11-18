//! Account-traffic flows (W6c): presence subscription and presence, and
//! direct messages with Carbons on the Waddle side, in both directions.

use anyhow::Result;
use minidom::Element;

use crate::client::session::Session;
use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::scenarios::harness::{Harness, ANSWER};
use crate::scenarios::support::{self, ask, describe, is_result, peer_alice};
use crate::topology::{Home, HOME_A};
use crate::xml::{ns, stanza};

fn presence_of(kind: Option<&'static str>, from: String) -> impl Fn(&Element) -> bool {
    move |element| {
        element.name() == "presence"
            && element.attr("type") == kind
            && element
                .attr("from")
                .is_some_and(|value| value.split('/').next() == Some(from.as_str()))
    }
}

async fn subscribe(
    from: &mut Session,
    to: &mut Session,
    to_bare: &str,
    from_bare: &str,
    outcome: &mut Outcome,
    label: &str,
) -> Result<()> {
    from.send(&stanza::presence(
        ns::CLIENT,
        Some(to_bare),
        Some("subscribe"),
    ))
    .await?;
    let request = to
        .wait_for(ANSWER, presence_of(Some("subscribe"), from_bare.to_owned()))
        .await;
    outcome.check(
        [label, ": subscription request delivered"].concat(),
        request.is_ok(),
        "",
    );
    to.send(&stanza::presence(
        ns::CLIENT,
        Some(from_bare),
        Some("subscribed"),
    ))
    .await?;
    let approved = from
        .wait_for(ANSWER, presence_of(Some("subscribed"), to_bare.to_owned()))
        .await;
    outcome.check(
        [label, ": approval delivered"].concat(),
        approved.is_ok(),
        "",
    );
    Ok(())
}

pub async fn presence(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(["clients of home-a and ", peer.label()].concat());
    let mut local = h
        .login_waddle(Home::A, "carol", &support::id("presence"))
        .await?;
    let mut remote = h
        .login_peer(peer, "alice", &support::id("presence"))
        .await?;
    let local_bare = local.bare();
    let remote_bare = peer_alice(peer);
    // Each client requests its roster first, as a client does at login:
    // RFC 6121 section 3.1.6 delivers an approval to the user's interested
    // resources, those that have requested the roster.
    for session in [&mut local, &mut remote] {
        let roster = ask(session, stanza::roster_get(&support::id("roster"))).await?;
        anyhow::ensure!(
            is_result(&roster),
            "roster request refused: {}",
            describe(&roster)
        );
    }
    subscribe(
        &mut local,
        &mut remote,
        &remote_bare,
        &local_bare,
        &mut outcome,
        "home-a -> peer",
    )
    .await?;
    subscribe(
        &mut remote,
        &mut local,
        &local_bare,
        &remote_bare,
        &mut outcome,
        "peer -> home-a",
    )
    .await?;
    local
        .send(&stanza::presence(ns::CLIENT, None, None))
        .await?;
    remote
        .send(&stanza::presence(ns::CLIENT, None, None))
        .await?;
    let seen_by_remote = remote
        .wait_for(ANSWER, presence_of(None, local_bare.clone()))
        .await;
    let seen_by_local = local
        .wait_for(ANSWER, presence_of(None, remote_bare.clone()))
        .await;
    outcome.check("peer sees home-a's presence", seen_by_remote.is_ok(), "");
    outcome.check("home-a sees the peer's presence", seen_by_local.is_ok(), "");
    let roster = ask(&mut local, stanza::roster_get(&support::id("roster"))).await?;
    let item = roster
        .get_child("query", ns::ROSTER)
        .and_then(|query| {
            query
                .children()
                .find(|item| item.attr("jid") == Some(remote_bare.as_str()))
        })
        .cloned();
    outcome.check(
        "home-a roster holds the remote contact with a mutual subscription",
        is_result(&roster)
            && item
                .as_ref()
                .is_some_and(|item| item.attr("subscription") == Some("both")),
        [
            describe(&roster),
            ", subscription ".to_owned(),
            item.as_ref()
                .and_then(|item| item.attr("subscription"))
                .unwrap_or("none")
                .to_owned(),
        ]
        .concat(),
    );
    local.close().await;
    remote.close().await;
    Ok(outcome)
}

pub async fn direct_messages(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "clients of home-a (two resources, Carbons on) and ",
            peer.label(),
        ]
        .concat(),
    );
    let mut first = h.login_waddle(Home::A, "alice", "dm-1").await?;
    let mut second = h.login_waddle(Home::A, "alice", "dm-2").await?;
    for session in [&mut first, &mut second] {
        let enabled = ask(session, stanza::carbons_enable(&support::id("carbons"))).await?;
        outcome.check("Carbons enabled", is_result(&enabled), describe(&enabled));
    }
    let mut remote = h.login_peer(peer, "alice", &support::id("dm")).await?;
    let out_id = support::id("dm-out");
    first
        .send(&stanza::chat(
            &out_id,
            &peer_alice(peer),
            "hello from home-a",
        ))
        .await?;
    let delivered = remote
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(out_id.as_str())
        })
        .await;
    outcome.check("home-a -> peer delivered", delivered.is_ok(), "");
    let carbon = second
        .wait_for(ANSWER, |element| {
            element
                .children()
                .any(|child| child.is("sent", ns::CARBONS))
        })
        .await;
    outcome.check(
        "the other home-a resource gets a sent carbon",
        carbon.is_ok(),
        "",
    );
    let back_id = support::id("dm-back");
    remote
        .send(&stanza::chat(
            &back_id,
            &["alice@", HOME_A].concat(),
            "hello from the peer",
        ))
        .await?;
    let on_first = first
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(back_id.as_str())
                || element
                    .children()
                    .any(|child| child.is("received", ns::CARBONS))
        })
        .await;
    let on_second = second
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(back_id.as_str())
                || element
                    .children()
                    .any(|child| child.is("received", ns::CARBONS))
        })
        .await;
    outcome.check(
        "peer -> home-a reaches both resources",
        on_first.is_ok() && on_second.is_ok(),
        "",
    );
    first.close().await;
    second.close().await;
    remote.close().await;
    Ok(outcome)
}
