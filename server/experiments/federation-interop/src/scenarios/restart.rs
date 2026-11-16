//! Step 8 (failure and restart) and the first part of step 6 (S2S off).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::net::http;
use crate::process;
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::services::waddle::S2s;
use crate::topology::{Home, Server, HOME_A, HOME_B};
use crate::xml::stanza;

/// How long S2S may take to recover after a restart or a healed
/// partition, backoff included.
const RECOVERY: Duration = Duration::from_secs(180);

async fn across(h: &Harness, peer: Peer, outcome: &mut Outcome, label: &str) -> Result<()> {
    let out =
        support::ping_until_result(h, peer, true, Harness::peer_domain(peer), RECOVERY).await?;
    outcome.check(
        [label, ": home-a -> ", peer.label()].concat(),
        is_result(&out),
        describe(&out),
    );
    let back = support::ping_until_result(h, peer, false, HOME_A, RECOVERY).await?;
    outcome.check(
        [label, ": ", peer.label(), " -> home-a"].concat(),
        is_result(&back),
        describe(&back),
    );
    Ok(())
}

pub async fn each_side(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(["home-a and ", peer.label(), " restarted in turn"].concat());
    across(h, peer, &mut outcome, "before").await?;
    let profile = h.profile(Home::A, true);
    h.start_waddle(&profile).await?;
    across(h, peer, &mut outcome, "after restarting home-a").await?;
    match peer {
        Peer::Waddle => {
            let profile = h.profile(Home::B, true);
            h.start_waddle(&profile).await?;
            across(h, peer, &mut outcome, "after restarting home-b").await?;
        }
        Peer::Prosody => {
            let variant = h.prosody_variant();
            h.prosody.restart(&variant).await?;
            across(h, peer, &mut outcome, "after restarting Prosody").await?;
        }
        Peer::Ejabberd => {
            let variant = h.ejabberd_variant();
            h.dirty.insert(Component::Ejabberd);
            h.ejabberd.restart_node(1, &variant).await?;
            let mut session = h.login_ejabberd(2, "alice", &support::id("node2")).await?;
            let answer = ask(&mut session, stanza::ping(&support::id("node2"), HOME_A)).await;
            outcome.check(
                "node 2 serves S2S while node 1 restarts",
                answer.as_ref().is_ok_and(is_result),
                answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string()),
            );
            session.close().await;
            across(h, peer, &mut outcome, "after restarting ejabberd node 1").await?;
        }
    }
    Ok(outcome)
}

pub async fn partition(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new(["relays between home-a and ", peer.label(), " cut both ways"].concat());
    across(h, peer, &mut outcome, "before").await?;
    let paths = [
        (Server::Waddle(Home::A), Harness::peer_server(peer)),
        (Harness::peer_server(peer), Server::Waddle(Home::A)),
    ];
    for (from, to) in paths {
        h.relay(from, to)?.cut();
    }
    let outage = support::ping_from_waddle(h, Home::A, "alice", Harness::peer_domain(peer)).await;
    let outage = outage
        .map(|answer| describe(&answer))
        .unwrap_or_else(|error| error.to_string());
    let denial = support::ping_from_waddle(h, Home::A, "alice", "not-admitted.test").await;
    let denial = denial
        .map(|answer| describe(&answer))
        .unwrap_or_else(|error| error.to_string());
    outcome.check(
        "a remote outage is distinct from a denial",
        outage != denial && !outage.contains("result"),
        ["outage: ", &outage, "; denial: ", &denial].concat(),
    );
    let mut alice = h
        .login_waddle(Home::A, "alice", &support::id("local"))
        .await?;
    let mut bob = h
        .login_waddle(Home::A, "bob", &support::id("local"))
        .await?;
    let local_id = support::id("local");
    alice
        .send(&stanza::chat(
            &local_id,
            &["bob@", HOME_A].concat(),
            "local during partition",
        ))
        .await?;
    let delivered = bob
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(local_id.as_str())
        })
        .await;
    outcome.check(
        "local traffic keeps flowing",
        delivered.is_ok(),
        "direct message between two home-a accounts",
    );
    alice.close().await;
    bob.close().await;
    for (from, to) in paths {
        h.relay(from, to)?.restore();
    }
    across(h, peer, &mut outcome, "after the partition heals").await?;
    Ok(outcome)
}

pub async fn crash(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("home-b killed without shutdown and started again");
    h.dirty.insert(Component::Waddle(Home::B));
    h.waddle(Home::B)?.crash().await?;
    let during = support::ping_from_waddle(h, Home::A, "alice", HOME_B).await;
    outcome.note(format!(
        "home-a -> home-b while home-b is down: {}",
        during
            .map(|answer| describe(&answer))
            .unwrap_or_else(|error| error.to_string())
    ));
    let profile = h.profile(Home::B, true);
    h.start_waddle(&profile).await?;
    across(h, Peer::Waddle, &mut outcome, "after home-b's crash").await?;
    Ok(outcome)
}

/// Step 6, first part: home-a with S2S off. Its S2S configuration path
/// names a file that does not exist, so a read would fail startup.
pub async fn s2s_disabled(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("home-a started with WADDLE_S2S_ENABLED unset");
    let unread = h.root.join("never-written-s2s.toml");
    anyhow::ensure!(!unread.exists(), "the unread path exists");
    let ports = h.ports;
    h.dirty.insert(Component::Waddle(Home::A));
    let started = h
        .waddle(Home::A)?
        .start(
            ports,
            S2s::Disabled {
                unread: unread.clone(),
            },
        )
        .await;
    outcome.check(
        "starts and becomes ready without reading the S2S configuration",
        started.is_ok() && !unread.exists(),
        started
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default(),
    );
    outcome.check(
        "no s2s listener",
        process::port_closed(ports.waddle_s2s(Home::A)).await,
        [
            "nothing on 127.0.0.1:",
            &ports.waddle_s2s(Home::A).to_string(),
        ]
        .concat(),
    );
    let relay = h.relay(Server::Waddle(Home::A), Server::Waddle(Home::B))?;
    relay.reset_records();
    h.receiver.clear();
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("off"))
        .await?;
    let mut answers = Vec::new();
    for target in [HOME_B, crate::topology::RAW_PEER] {
        let answer = ask(&mut session, stanza::ping(&support::id("off"), target)).await;
        answers.push(
            [
                target,
                ": ",
                &answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string()),
            ]
            .concat(),
        );
    }
    tokio::time::sleep(QUIET).await;
    outcome.check(
        "no dial",
        relay.connection_count() == 0 && h.receiver.inbound().is_empty(),
        answers.join("; "),
    );
    let features = ask(
        &mut session,
        stanza::disco_info(&support::id("off"), HOME_A),
    )
    .await?;
    let advertised: Vec<String> = features
        .children()
        .flat_map(|query| query.children())
        .filter(|child| child.name() == "feature")
        .filter_map(|feature| feature.attr("var").map(str::to_owned))
        .filter(|var| var.contains("s2s") || var.contains("dialback") || var.contains("server"))
        .collect();
    outcome.check(
        "no S2S advertisement in disco#info",
        advertised.is_empty(),
        format!("{advertised:?}"),
    );
    session.close().await;
    let metrics = http::get(ports.waddle_http(Home::A), "/metrics").await?;
    let s2s_metrics = http::metric_lines(&metrics.body, "waddle_s2s_");
    outcome.check(
        "no S2S connection metrics",
        s2s_metrics
            .iter()
            .all(|line| line.starts_with('#') || line.ends_with(" 0")),
        format!("{s2s_metrics:?}"),
    );
    h.waddle(Home::A)?.stop().await?;
    Ok(outcome)
}

/// The log of a deployment's current process.
pub fn log_of(h: &Harness, home: Home) -> Option<PathBuf> {
    h.waddle.get(&home).and_then(|waddle| waddle.log_path())
}
