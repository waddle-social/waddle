//! Flow rows on the transport: TLS with SASL EXTERNAL at each allowed TLS
//! version, dialback under D2, and server-domain disco and ping, in both
//! directions. Until account traffic (W6c) lands, server-domain IQs are
//! what crosses S2S in both directions: a local account's IQ to a remote
//! entity is routed, and a remote IQ to the server domain is answered.

use std::time::Duration;

use anyhow::Result;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::pki::{self, LeafSpec};
use crate::s2s::initiator::{self, Auth, Plan, Stage};
use crate::s2s::receiver::Behaviour;
use crate::scenarios::harness::{Component, Harness, ANSWER};
use crate::scenarios::support::{self, ask, check_tls, describe, established, is_result};
use crate::services::{ejabberd, prosody};
use crate::topology::{Home, Server, EJABBERD_INSECURE, HOME_A, PROSODY_INSECURE, RAW_PEER};
use crate::xml::stanza;

/// Ping across in both directions over fresh streams, and check the TLS
/// version each relay saw and that home-a's log shows pkix streams
/// established in both directions.
async fn both_directions(
    h: &Harness,
    peer: Peer,
    outcome: &mut Outcome,
    label: &str,
    expected_tls: Option<u16>,
) -> Result<()> {
    let peer_domain = Harness::peer_domain(peer);
    let before_out = established(h, Home::A, "outbound", "pkix").await;
    let before_in = established(h, Home::A, "inbound", "pkix").await;
    let out =
        support::ping_until_result(h, peer, true, peer_domain, Duration::from_secs(60)).await?;
    outcome.check(
        [label, ": home-a -> ", peer.label()].concat(),
        is_result(&out),
        describe(&out),
    );
    let back = support::ping_until_result(h, peer, false, HOME_A, Duration::from_secs(60)).await?;
    outcome.check(
        [label, ": ", peer.label(), " -> home-a"].concat(),
        is_result(&back),
        describe(&back),
    );
    for (from, to) in [
        (Server::Waddle(Home::A), Harness::peer_server(peer)),
        (Harness::peer_server(peer), Server::Waddle(Home::A)),
    ] {
        match h.relay(from, to) {
            Ok(relay) => check_tls(outcome, relay, expected_tls),
            Err(error) => outcome.unverified("relay", error.to_string()),
        }
    }
    let after_out = established(h, Home::A, "outbound", "pkix").await;
    let after_in = established(h, Home::A, "inbound", "pkix").await;
    outcome.check(
        [
            label,
            ": home-a established pkix streams in both directions",
        ]
        .concat(),
        after_out > before_out && after_in > before_in,
        format!(
            "outbound {before_out} -> {after_out}, inbound {before_in} -> {after_in} (home-a log)"
        ),
    );
    Ok(())
}

/// Cut every relay of the pair so the next traffic opens new streams.
fn reset_pair(h: &Harness, peer: Peer) {
    for (from, to) in [
        (Server::Waddle(Home::A), Harness::peer_server(peer)),
        (Harness::peer_server(peer), Server::Waddle(Home::A)),
    ] {
        if let Ok(relay) = h.relay(from, to) {
            relay.cut();
            relay.restore();
            relay.reset_records();
        }
    }
}

pub async fn tls_external(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(match peer {
        Peer::Waddle => "home-a and home-b; TLS 1.2 inbound and outbound by the harness raw peer",
        Peer::Prosody => "stock Prosody, default and TLS-1.2-only configurations",
        Peer::Ejabberd => "stock ejabberd cluster, default and TLS-1.2-only configurations",
    });
    reset_pair(h, peer);
    both_directions(h, peer, &mut outcome, "default TLS", None).await?;
    match peer {
        Peer::Waddle => {
            // Waddle negotiates its highest version with itself; TLS 1.2
            // in each role is shown against the harness peer.
            for (name, version) in [
                ("TLS 1.2", &rustls::version::TLS12),
                ("TLS 1.3", &rustls::version::TLS13),
            ] {
                let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
                plan.versions = vec![version];
                let attempt =
                    initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
                outcome.check(
                    [name, " inbound (raw peer -> home-a)"].concat(),
                    attempt.stage == Stage::Established,
                    attempt.describe(),
                );
                if let Some(stream) = attempt.stream {
                    stream.close().await;
                }
                h.receiver.set_versions(vec![version]);
                h.receiver.clear();
                let answer = support::ping_from_waddle(h, Home::A, "alice", RAW_PEER).await;
                let received = h
                    .receiver
                    .wait_stanza(ANSWER, |element| element.name() == "iq")
                    .await;
                outcome.check(
                    [name, " outbound (home-a -> raw receiver)"].concat(),
                    received.is_some(),
                    format!(
                        "receiver got the ping: {}; client answer: {}",
                        received.is_some(),
                        answer
                            .map(|element| describe(&element))
                            .unwrap_or_else(|error| error.to_string())
                    ),
                );
            }
            h.receiver.set_versions(pki::all_versions());
        }
        Peer::Prosody => {
            let mut variant = h.prosody_variant();
            variant.tls = prosody::TlsProfile::Tls12Only;
            h.dirty.insert(Component::Prosody);
            h.prosody.restart(&variant).await?;
            reset_pair(h, peer);
            both_directions(h, peer, &mut outcome, "TLS 1.2 only", Some(0x0303)).await?;
        }
        Peer::Ejabberd => {
            let mut variant = h.ejabberd_variant();
            variant.tls = ejabberd::TlsProfile::Tls12Only;
            h.dirty.insert(Component::Ejabberd);
            if let Err(error) = h.ejabberd.restart(&variant).await {
                outcome.unverified(
                    "TLS 1.2 only",
                    format!("ejabberd did not start with s2s_protocol_options limiting TLS to 1.2: {error:#}"),
                );
                return Ok(outcome);
            }
            reset_pair(h, peer);
            both_directions(h, peer, &mut outcome, "TLS 1.2 only", Some(0x0303)).await?;
        }
    }
    Ok(outcome)
}

pub async fn dialback(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    match peer {
        Peer::Waddle => dialback_with_raw_peer(h).await,
        Peer::Prosody => dialback_with_stock(h, peer, PROSODY_INSECURE).await,
        Peer::Ejabberd => dialback_with_stock(h, peer, EJABBERD_INSECURE).await,
    }
}

/// No Waddle negotiates dialback with a Waddle whose certificate it
/// verifies, so the harness plays the dialback-only peer.
async fn dialback_with_raw_peer(h: &mut Harness) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer for peer-raw.test, which is on home-a's dialback list");
    // Inbound: Waddle as Receiving Server, the harness as Authoritative.
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.receiver.issue_key(&key);
    let before = established(h, Home::A, "inbound", "dialback").await;
    let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    plan.leaf = None;
    plan.open = plan.open.with_dialback();
    plan.auth = Auth::Dialback(key);
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    outcome.check(
        "inbound dialback for a listed domain",
        attempt.stage == Stage::Established,
        attempt.describe(),
    );
    let verifications: usize = h
        .receiver
        .inbound()
        .iter()
        .map(|inbound| inbound.verification_requests)
        .sum();
    outcome.check(
        "home-a verified the key with the Authoritative Server over TLS",
        verifications > 0,
        format!("{verifications} verification requests"),
    );
    if let Some(stream) = attempt.stream {
        stream.close().await;
    }
    let after = established(h, Home::A, "inbound", "dialback").await;
    outcome.note(format!(
        "home-a inbound dialback streams {before} -> {after}"
    ));

    // Outbound: Waddle as Initiating Server; the harness verifies the key
    // with Waddle as Authoritative Server.
    h.receiver.set_behaviour(Behaviour::DialbackOnly);
    h.receiver.clear();
    let mut session = h.login_waddle(Home::A, "alice", &support::id("db")).await?;
    let ping_id = support::id("db-ping");
    session.send(&stanza::ping(&ping_id, RAW_PEER)).await?;
    let received = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await;
    let dialback_streams = h
        .receiver
        .inbound()
        .iter()
        .filter(|inbound| inbound.authenticated.as_deref() == Some("dialback"))
        .count();
    outcome.check(
        "outbound dialback: Initiating and Authoritative roles",
        received.is_some() && dialback_streams > 0,
        format!(
            "ping delivered: {}, dialback-authenticated streams: {dialback_streams}",
            received.is_some()
        ),
    );
    session.close().await;
    h.receiver.set_behaviour(Behaviour::External);
    Ok(outcome)
}

async fn dialback_with_stock(h: &mut Harness, peer: Peer, host: &str) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "stock ",
            peer.label(),
            " host ",
            host,
            ", whose S2S trust excludes the .test CA",
        ]
        .concat(),
    );
    let before_out = established(h, Home::A, "outbound", "dialback").await;
    let out = support::ping_until_result(h, peer, true, host, Duration::from_secs(60)).await?;
    let after_out = established(h, Home::A, "outbound", "dialback").await;
    outcome.check(
        ["outbound to ", host].concat(),
        is_result(&out),
        describe(&out),
    );
    outcome.check(
        "home-a authenticated by dialback (Initiating role; the peer verified with home-a's Authoritative role)",
        after_out > before_out,
        format!("outbound dialback streams {before_out} -> {after_out}"),
    );
    // Inbound from the dialback host: a stock server that presents a
    // certificate home-a verifies authenticates with SASL EXTERNAL, so
    // inbound dialback is exercised only if it presents none.
    let before_in = established(h, Home::A, "inbound", "dialback").await;
    let before_pkix = established(h, Home::A, "inbound", "pkix").await;
    let account = crate::client::session::Account {
        domain: host.to_owned(),
        username: "alice".to_owned(),
        password: match peer {
            Peer::Prosody => h
                .prosody
                .passwords
                .get("alice")
                .cloned()
                .unwrap_or_default(),
            _ => h
                .ejabberd
                .passwords
                .get("alice")
                .cloned()
                .unwrap_or_default(),
        },
    };
    let port = match peer {
        Peer::Prosody => h.ports.prosody_c2s(),
        _ => h.ports.ejabberd_c2s(1),
    };
    let mut session =
        crate::client::session::Session::tcp(port, &account, "db", "dialback host").await?;
    let back = ask(&mut session, stanza::ping(&support::id("db-in"), HOME_A)).await?;
    session.close().await;
    let after_in = established(h, Home::A, "inbound", "dialback").await;
    let after_pkix = established(h, Home::A, "inbound", "pkix").await;
    outcome.check(
        ["inbound from ", host].concat(),
        is_result(&back),
        describe(&back),
    );
    if after_in > before_in {
        outcome.check(
            "inbound by dialback for a listed domain",
            true,
            "dialback stream established",
        );
    } else {
        outcome.unverified(
            "inbound by dialback for a listed domain",
            format!(
                "{} authenticated with SASL EXTERNAL (inbound pkix {before_pkix} -> {after_pkix}); the stock server presents its certificate, so inbound dialback is shown only by the harness peer (Waddle column)",
                peer.label()
            ),
        );
    }
    Ok(outcome)
}

pub async fn disco_ping(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(match peer {
        Peer::Waddle => "clients of home-a and home-b",
        Peer::Prosody => "clients of home-a and stock Prosody",
        Peer::Ejabberd => "clients of home-a and the stock ejabberd cluster",
    });
    let peer_domain = Harness::peer_domain(peer);
    let mut local = h
        .login_waddle(Home::A, "alice", &support::id("disco"))
        .await?;
    for (name, request) in [
        (
            "disco#info",
            stanza::disco_info(&support::id("info"), peer_domain),
        ),
        (
            "disco#items",
            stanza::disco_items(&support::id("items"), peer_domain),
        ),
        ("ping", stanza::ping(&support::id("ping"), peer_domain)),
    ] {
        let answer = ask(&mut local, request).await?;
        outcome.check(
            [name, " home-a -> ", peer.label()].concat(),
            is_result(&answer),
            describe(&answer),
        );
    }
    local.close().await;
    let mut remote = h.login_peer(peer, "alice", &support::id("disco")).await?;
    for (name, request) in [
        (
            "disco#info",
            stanza::disco_info(&support::id("info"), HOME_A),
        ),
        (
            "disco#items",
            stanza::disco_items(&support::id("items"), HOME_A),
        ),
        ("ping", stanza::ping(&support::id("ping"), HOME_A)),
    ] {
        let answer = ask(&mut remote, request).await?;
        if name == "disco#info" {
            let features: Vec<String> = answer
                .children()
                .flat_map(|query| query.children())
                .filter(|child| child.name() == "feature")
                .filter_map(|feature| feature.attr("var").map(str::to_owned))
                .collect();
            outcome.note(format!("home-a features for remote askers: {features:?}"));
        }
        outcome.check(
            [name, " ", peer.label(), " -> home-a"].concat(),
            is_result(&answer),
            describe(&answer),
        );
    }
    remote.close().await;
    Ok(outcome)
}

/// The leaf of `domain` with only serverAuth.
pub fn server_only(h: &Harness, domain: &str) -> Result<crate::pki::Leaf> {
    h.issue(&LeafSpec::new(&["server-only-", domain].concat(), &[domain]).server_only())
}
