//! Step 8 (failure and restart) and the first part of step 6 (S2S off).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::pki::LeafSpec;
use crate::process;
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::services::waddle::S2s;
use crate::topology::{self, Home, Server, HOME_A, HOME_B, RAW_PEER};
use crate::watch::OpenWatch;
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

/// What home-a did with one S2S configuration file in place.
struct Probe {
    listening: bool,
    relay_dials: usize,
    receiver_dials: usize,
    s2s_lines: usize,
    answers: String,
}

impl Probe {
    fn describe(&self) -> String {
        format!(
            "S2S port listening: {}; connections to home-b's relay: {}; to the harness receiving server: {}; log lines from waddle_server::s2s: {}; client answers: {}",
            self.listening, self.relay_dials, self.receiver_dials, self.s2s_lines, self.answers
        )
    }
}

/// Ping home-b and peer-raw.test from a home-a client, whose
/// configuration routes them to home-b's relay and to the harness
/// receiving server, and record what home-a did.
async fn probe(h: &Harness) -> Result<Probe> {
    let listening = !process::port_closed(h.ports.waddle_s2s(Home::A)).await;
    let relay = h.relay(Server::Waddle(Home::A), Server::Waddle(Home::B))?;
    relay.reset_records();
    h.receiver.disconnect();
    h.receiver.clear();
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("off"))
        .await?;
    let mut answers = Vec::new();
    for target in [HOME_B, RAW_PEER] {
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
    session.close().await;
    tokio::time::sleep(QUIET).await;
    Ok(Probe {
        listening,
        relay_dials: relay.connection_count(),
        receiver_dials: h.receiver.inbound().len(),
        s2s_lines: support::log_count(h, Home::A, &[S2S_TARGET]),
        answers: answers.join("; "),
    })
}

/// The tracing target prefix of every S2S log line, as Waddle's JSON log
/// writes it.
const S2S_TARGET: &str = "\"waddle_server::s2s";

/// Step 6, first part. home-a is started twice on one S2S configuration
/// file whose endpoints send home-b to its relay and peer-raw.test to the
/// harness receiving server: first with S2S enabled, the control that
/// this file makes home-a listen, dial both and log S2S lines; then with
/// `WADDLE_S2S_ENABLED` unset and `WADDLE_S2S_CONFIG_FILE` still naming
/// the readable file. The file, the dialback secret and the certificate
/// chains and keys it names are this row's own copies, so an inotify
/// watch on them sees only home-a.
pub async fn s2s_disabled(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "home-a started with S2S enabled (control), then with WADDLE_S2S_ENABLED unset, on the same readable S2S configuration",
    );
    let mut material = h
        .material
        .get(&Home::A)
        .cloned()
        .context("home-a material")?;
    let mut watched = Vec::new();
    for domain in topology::waddle_domains(HOME_A) {
        let leaf = h.issue(&LeafSpec::new(
            &["s2s-off-", &domain].concat(),
            &[domain.as_str()],
        ))?;
        watched.push(leaf.certificate_path.clone());
        watched.push(leaf.key_path.clone());
        material
            .credentials
            .insert(domain, (leaf.certificate_path, leaf.key_path));
    }
    let secret = h.root.join("s2s-off-dialback-secret");
    process::write_private(
        &secret,
        uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .repeat(2)
            .as_bytes(),
    )?;
    material.dialback_secret = secret.clone();
    watched.push(secret);
    let config = h
        .profile(Home::A, true)
        .write(&h.root, h.ports, &material)?;
    watched.push(config.clone());
    let ports = h.ports;
    h.dirty.insert(Component::Waddle(Home::A));

    // Control: the same file with S2S enabled.
    h.waddle(Home::A)?.stop().await?;
    h.waddle(Home::A)?
        .start(
            ports,
            S2s::Enabled {
                config: config.clone(),
            },
        )
        .await
        .map_err(|error| {
            Unverifiable(format!(
                "control: home-a did not start with S2S enabled on this configuration: {error:#}"
            ))
        })?;
    let control = probe(h).await?;
    h.waddle(Home::A)?.stop().await?;
    if !(control.listening
        && control.relay_dials > 0
        && control.receiver_dials > 0
        && control.s2s_lines > 0)
    {
        return Err(Unverifiable(
            [
                "control: with S2S enabled this configuration did not listen, dial and log: ",
                &control.describe(),
            ]
            .concat(),
        )
        .into());
    }
    outcome.check(
        "control: with S2S enabled the same file listens, dials home-b's relay and the harness receiving server, and logs S2S lines",
        true,
        control.describe(),
    );

    // S2S off on the same, readable file.
    let mut watch = match OpenWatch::new(&watched) {
        Ok(watch) => Some(watch),
        Err(error) => {
            outcome.unverified(
                "the S2S configuration and its credentials are not opened",
                format!("inotify is unavailable: {error:#}"),
            );
            None
        }
    };
    let started = h
        .waddle(Home::A)?
        .start(
            ports,
            S2s::Disabled {
                config: config.clone(),
            },
        )
        .await;
    outcome.check(
        "starts and becomes ready with S2S off",
        started.is_ok(),
        started
            .as_ref()
            .err()
            .map(|error| format!("{error:#}"))
            .unwrap_or_default(),
    );
    if started.is_err() {
        return Ok(outcome);
    }
    let off = probe(h).await?;
    outcome.check(
        "no S2S listener",
        !off.listening,
        [
            "nothing on 127.0.0.1:",
            &ports.waddle_s2s(Home::A).to_string(),
        ]
        .concat(),
    );
    outcome.check(
        "no dial to the endpoints the configuration names",
        off.relay_dials == 0 && off.receiver_dials == 0,
        off.describe(),
    );
    outcome.check(
        "no S2S log line",
        off.s2s_lines == 0,
        format!("{} lines from waddle_server::s2s", off.s2s_lines),
    );
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("off"))
        .await?;
    let features = ask(
        &mut session,
        stanza::disco_info(&support::id("off"), HOME_A),
    )
    .await?;
    session.close().await;
    let advertised: Vec<String> = features
        .children()
        .flat_map(|query| query.children())
        .filter(|child| child.name() == "feature")
        .filter_map(|feature| feature.attr("var").map(str::to_owned))
        .filter(|var| var.contains("s2s") || var.contains("dialback") || var.contains("server"))
        .collect();
    outcome.check(
        "no S2S advertisement in disco#info",
        is_result(&features) && advertised.is_empty(),
        format!("{} with S2S features {advertised:?}", describe(&features)),
    );
    if let Some(watch) = watch.as_mut() {
        let touched = watch.touched()?;
        outcome.check(
            "the S2S configuration and its credentials are not opened",
            touched.is_empty(),
            format!("opened or read while S2S was off: {touched:?}"),
        );
    }
    h.waddle(Home::A)?.stop().await?;
    Ok(outcome)
}

/// The log of a deployment's current process.
pub fn log_of(h: &Harness, home: Home) -> Option<PathBuf> {
    h.waddle.get(&home).and_then(|waddle| waddle.log_path())
}
