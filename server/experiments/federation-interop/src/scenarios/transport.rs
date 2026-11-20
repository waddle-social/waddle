//! Flow rows on the transport: TLS with SASL EXTERNAL at each allowed TLS
//! version, dialback under D2, and server-domain disco and ping, in both
//! directions. Until account traffic (W6c) lands, server-domain IQs are
//! what crosses S2S in both directions: a local account's IQ to a remote
//! entity is routed, and a remote IQ to the server domain is answered.

use std::time::Duration;

use anyhow::Result;
use minidom::{rxml::xml_ncname, Element};

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::pki::{self, LeafSpec};
use crate::s2s::initiator::{self, Auth, Plan, Stage};
use crate::s2s::receiver::Behaviour;
use crate::scenarios::harness::{Component, Harness, ANSWER};
use crate::scenarios::support::{self, ask, check_tls, describe, established, is_result};
use crate::services::{ejabberd, prosody};
use crate::topology::{Home, Server, EJABBERD_INSECURE, HOME_A, PROSODY_INSECURE, RAW_PEER};
use crate::xml::{ns, stanza};

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
    // Earlier rows' connections are cleared so only this row's
    // verification requests are counted.
    h.receiver.clear();
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
    check_stock_inbound_dialback(h, peer, host, true, &mut outcome).await?;
    Ok(outcome)
}

pub async fn stock_dialback_wrong_authority(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let host = match peer {
        Peer::Prosody => PROSODY_INSECURE,
        Peer::Ejabberd => EJABBERD_INSECURE,
        Peer::Waddle => anyhow::bail!("a stock dialback peer is required"),
    };
    let mut outcome = Outcome::new(format!(
        "{} with a wrong-name authoritative certificate",
        peer.label()
    ));
    let mut control = Outcome::new("the same stock peer with its valid authority certificate");
    check_stock_inbound_dialback(h, peer, host, true, &mut control).await?;
    let control = control.finish();
    if control.status != crate::evidence::Status::Pass {
        outcome.unverified(
            "the valid authoritative certificate admitted this same peer",
            format!("{:?}", control.checks),
        );
        return Ok(outcome);
    }
    outcome.check(
        "the valid authoritative certificate admitted this same peer",
        true,
        "TLS callback and inbound dialback succeeded, and the client received the ping result",
    );
    check_stock_inbound_dialback(h, peer, host, false, &mut outcome).await?;
    Ok(outcome)
}

async fn check_stock_inbound_dialback(
    h: &mut Harness,
    peer: Peer,
    host: &str,
    valid_authority: bool,
    outcome: &mut Outcome,
) -> Result<()> {
    use crate::client::session::{Account, Session};
    use crate::xml::framer::{Framer, Item};

    let wrong_leaf = if valid_authority {
        None
    } else {
        Some(h.issue(&LeafSpec::new(
            "wrong-dialback-authority",
            &["unrelated-name.test"],
        ))?)
    };
    h.dirty.insert(Component::Waddle(Home::A));
    let mut profile = h.profile(Home::A, true);
    // Only this peer can establish an inbound stream during the check.
    profile.allow.retain(|domain| domain == host);
    profile.dialback.retain(|domain| domain == host);
    profile.trusted_homes.retain(|domain| domain == host);
    profile.endpoints.retain(|(domain, _)| domain == host);
    let (initiating_server, authority, password) = match peer {
        Peer::Prosody => {
            h.dirty.insert(Component::Prosody);
            let mut variant = h.prosody_variant();
            variant.certificate_less_outbound = Some(prosody::CertificateLessOutbound {
                host: PROSODY_INSECURE,
                target: HOME_A.to_owned(),
            });
            if let Some(leaf) = wrong_leaf {
                variant.leaves.insert(host.to_owned(), leaf);
            }
            h.prosody.restart(&variant).await?;
            (
                Server::Prosody,
                Server::Prosody,
                h.prosody.passwords.get("alice"),
            )
        }
        Peer::Ejabberd => {
            h.dirty.insert(Component::Ejabberd);
            let mut variant = h.ejabberd_variant();
            variant.certificate_less_node_two = true;
            if let Some(leaf) = wrong_leaf {
                variant.leaves.insert(host.to_owned(), leaf);
            }
            h.ejabberd.restart(&variant).await?;
            let authority = Server::Ejabberd(1);
            let link = crate::topology::relay_for(Server::Waddle(Home::A), authority)
                .ok_or_else(|| anyhow::anyhow!("missing ejabberd authoritative relay"))?;
            let port = h.ports.relay(link);
            profile
                .endpoints
                .retain(|(domain, endpoint)| domain != host || *endpoint == port);
            (
                Server::Ejabberd(2),
                authority,
                h.ejabberd.passwords.get("alice"),
            )
        }
        Peer::Waddle => anyhow::bail!("a stock dialback peer is required"),
    };
    let account = Account {
        domain: host.to_owned(),
        username: "alice".to_owned(),
        password: password
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing stock fixture account"))?,
    };
    h.start_waddle(&profile).await?;
    let home = Server::Waddle(Home::A);
    let inbound = h.relay(initiating_server, home)?;
    let callback = h.relay(home, authority)?;
    let inbound_start = inbound.connection_count();
    let callback_start = callback.connection_count();
    let before = established(h, Home::A, "inbound", "dialback").await;
    let before_pkix = established(h, Home::A, "inbound", "pkix").await;
    let refused = support::LogMark::new(
        h,
        Home::A,
        &["s2s peer certificate refused", "identity_mismatch"],
    );
    let dialback_ended = support::LogMark::new(
        h,
        Home::A,
        &[
            "s2s connection ended",
            "\"direction\":\"Inbound\"",
            "\"outcome\":\"Dialback\"",
        ],
    );
    let mut session = match peer {
        Peer::Prosody => {
            Session::tcp(h.ports.prosody_c2s(), &account, "db", "dialback fixture").await?
        }
        _ => {
            Session::tcp_loopback_plain(h.ports.ejabberd_c2s(2), &account, "db", "dialback fixture")
                .await?
        }
    };
    let answer = ask(&mut session, stanza::ping(&support::id("db-in"), HOME_A)).await;
    session.close().await;

    let names_pair = |connection: &crate::net::relay::Connection, from: &str, to: &str| {
        let mut framer = Framer::new();
        framer.push(connection.initiator_plaintext());
        matches!(framer.next_item(), Ok(Some(Item::Header(header)))
            if header.attribute("from").as_deref() == Some(from)
                && header.attribute("to").as_deref() == Some(to))
            && connection.server_hello().is_some()
    };
    let initiates = inbound
        .connections()
        .iter()
        .skip(inbound_start)
        .any(|connection| names_pair(connection, host, HOME_A));
    let called_back = callback
        .connections()
        .iter()
        .skip(callback_start)
        .any(|connection| names_pair(connection, HOME_A, host));
    outcome.check(
        "stock initiator and authoritative callback used TLS for the expected pair",
        initiates && called_back,
        format!("initiating stream: {initiates}; callback: {called_back}"),
    );
    let got_result = answer.as_ref().is_ok_and(is_result);
    if valid_authority {
        let after = established(h, Home::A, "inbound", "dialback").await;
        let result_from_home = answer
            .as_ref()
            .is_ok_and(|iq| is_result(iq) && iq.attr("from") == Some(HOME_A));
        outcome.check(
            "stock inbound dialback established and the client received its ping result",
            result_from_home && after > before,
            format!("ping result from {HOME_A}: {result_from_home}; inbound dialback streams {before} -> {after}"),
        );
    } else {
        let refusals = refused.settled(h, 1).await;
        let rejected_streams = dialback_ended.settled(h, 1).await;
        let after = established(h, Home::A, "inbound", "dialback").await;
        let after_pkix = established(h, Home::A, "inbound", "pkix").await;
        outcome.check(
            "wrong-name authoritative certificate refused before admission or delivery",
            !got_result && after == before && after_pkix == before_pkix && refusals > 0 && rejected_streams > 0,
            format!("ping result: {got_result}; inbound dialback {before} -> {after}; inbound PKIX {before_pkix} -> {after_pkix}; certificate refusals: {refusals}; rejected dialback streams: {rejected_streams}"),
        );
    }
    Ok(())
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
    let home_info = ask(
        &mut local,
        stanza::disco_info(&support::id("home-info"), HOME_A),
    )
    .await?;
    let advertises_federation = |answer: &Element| {
        answer
            .children()
            .flat_map(|query| query.children())
            .any(|feature| {
                feature.name() == "feature" && feature.attr("var") == Some("urn:waddle:s2s:0")
            })
    };
    outcome.check(
        "enabled home server advertises federation to its client",
        is_result(&home_info) && advertises_federation(&home_info),
        describe(&home_info),
    );
    let node_info = ask(
        &mut local,
        stanza::get(
            &support::id("node-info"),
            HOME_A,
            Element::builder("query", ns::DISCO_INFO)
                .attr(
                    xml_ncname!("node").to_owned(),
                    "urn:waddle:interop:unhandled",
                )
                .build(),
        ),
    )
    .await?;
    outcome.check(
        "node-specific discovery omits the federation capability",
        !advertises_federation(&node_info),
        describe(&node_info),
    );
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
