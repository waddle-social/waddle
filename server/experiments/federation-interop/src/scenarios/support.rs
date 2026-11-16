//! Helpers shared by the rows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;

use crate::client::session::Session;
use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::net::relay::{Connection, Mode, Relay};
use crate::process;
use crate::s2s::initiator::{self, Plan};
use crate::scenarios::harness::{Harness, ANSWER};
use crate::topology::{Home, Server, HOME_A, RAW_PEER};
use crate::xml::stanza;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A stanza id unique within the run.
pub fn id(prefix: &str) -> String {
    [
        prefix,
        "-",
        &NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string(),
    ]
    .concat()
}

pub fn is_result(element: &Element) -> bool {
    element.attr("type") == Some("result")
}

/// A short description of an answer for the evidence.
pub fn describe(element: &Element) -> String {
    match element.attr("type") {
        Some("error") => [
            "error ",
            &stanza::error_condition(element).unwrap_or_else(|| "without a condition".to_owned()),
        ]
        .concat(),
        Some(kind) => kind.to_owned(),
        None => element.name().to_owned(),
    }
}

/// Send an IQ from `session` and wait for its answer.
pub async fn ask(session: &mut Session, iq: Element) -> Result<Element> {
    session.request(iq, ANSWER).await
}

/// Ping `to` from a fresh session of `account` on `home`.
pub async fn ping_from_waddle(h: &Harness, home: Home, account: &str, to: &str) -> Result<Element> {
    let mut session = h.login_waddle(home, account, &id("ping")).await?;
    let answer = ask(&mut session, stanza::ping(&id("ping"), to)).await;
    session.close().await;
    answer
}

/// Ping `to` from a fresh session of `account` on the peer's server.
pub async fn ping_from_peer(h: &Harness, peer: Peer, account: &str, to: &str) -> Result<Element> {
    let mut session = h.login_peer(peer, account, &id("ping")).await?;
    let answer = ask(&mut session, stanza::ping(&id("ping"), to)).await;
    session.close().await;
    answer
}

/// Ping until an answer is a result or `timeout` passes; S2S may need a
/// moment after a restart or a backoff.
pub async fn ping_until_result(
    h: &Harness,
    from: Peer,
    from_home_a: bool,
    to: &str,
    timeout: Duration,
) -> Result<Element> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = if from_home_a {
            ping_from_waddle(h, Home::A, "alice", to).await
        } else {
            ping_from_peer(h, from, "alice", to).await
        };
        match answer {
            Ok(element) if is_result(&element) => return Ok(element),
            Ok(element) if tokio::time::Instant::now() >= deadline => return Ok(element),
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

/// How many lines of the deployment's current log contain every needle,
/// compared without case. Waddle's `/metrics` endpoint is a liveness stub
/// (its metrics leave by OTLP), so the S2S debug log is the harness's
/// record of what each stream did: `s2s stream established` with its
/// direction and strength, `s2s peer certificate refused`, `s2s peer not
/// admitted`, `s2s service started`.
pub fn log_count(h: &Harness, home: Home, needles: &[&str]) -> usize {
    let Some(path) = h.waddle.get(&home).and_then(|waddle| waddle.log_path()) else {
        return 0;
    };
    let text = std::fs::read_to_string(path)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let needles: Vec<String> = needles
        .iter()
        .map(|needle| needle.to_ascii_lowercase())
        .collect();
    text.lines()
        .filter(|line| needles.iter().all(|needle| line.contains(needle.as_str())))
        .count()
}

/// Streams the deployment established, by direction (`inbound` or
/// `outbound`) and strength (`pkix` or `dialback`), from its log.
pub async fn established(h: &Harness, home: Home, direction: &str, strength: &str) -> usize {
    log_count(h, home, &["s2s stream established", direction, strength])
}

/// Peer certificates the deployment refused, from its log.
pub fn certificate_refusals(h: &Harness, home: Home) -> usize {
    log_count(h, home, &["s2s peer certificate refused"])
}

/// Check the newest relayed connection's negotiated TLS version: one the
/// G1 profile allows, or exactly `expected` when given.
pub fn check_tls(outcome: &mut Outcome, relay: &Relay, expected: Option<u16>) {
    let name = [relay.label.as_str(), ": negotiated TLS version"].concat();
    match relay
        .connections()
        .last()
        .and_then(Connection::server_hello)
    {
        Some(hello) => {
            let allowed = matches!(hello.version, 0x0303 | 0x0304);
            outcome.check(
                name,
                allowed && expected.is_none_or(|expected| expected == hello.version),
                format!(
                    "{} cipher suite 0x{:04x}",
                    hello.version_name(),
                    hello.cipher_suite
                ),
            );
        }
        None => outcome.unverified(name, "no ServerHello seen on the relay"),
    }
}

/// The pre-TLS refusal condition: every refusal of a pair before TLS is
/// `policy-violation` (s2s-connection-manager-2047.md, Admission).
pub const PRE_TLS_REFUSAL: &str = "policy-violation";

/// A known-good stream from the harness peer must establish first, so a
/// refusal the row then sees is Waddle's and not a harness defect.
pub async fn raw_control(h: &Harness) -> Result<()> {
    let plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    let detail = attempt.describe();
    match attempt.stream {
        Some(stream) => {
            stream.close().await;
            Ok(())
        }
        None => Err(Unverifiable(
            [
                "the harness peer cannot establish a control stream: ",
                &detail,
            ]
            .concat(),
        )
        .into()),
    }
}

/// Put the relay from the peer's server to home-a into tap mode and make
/// the peer open a fresh stream through it, so the harness holds the
/// peer's authenticated stream in clear.
pub async fn tap_peer_stream(h: &Harness, peer: Peer) -> Result<&Relay> {
    let relay = h.relay(Harness::peer_server(peer), Server::Waddle(Home::A))?;
    relay.cut();
    tokio::time::sleep(Duration::from_millis(500)).await;
    relay.set_mode(Mode::Tap {
        pki: std::sync::Arc::clone(&h.pki),
        identities: h.tap_identities(),
        rewrite: None,
    });
    relay.reset_records();
    let answer = ping_until_result(h, peer, false, HOME_A, Duration::from_secs(60)).await?;
    anyhow::ensure!(
        is_result(&answer),
        "the peer could not reach home-a through the tap: {}",
        describe(&answer)
    );
    let tapped = process::eventually(Duration::from_secs(10), || async { relay.tapped() }).await;
    anyhow::ensure!(tapped, "no tapped stream on {}", relay.label);
    Ok(relay)
}

/// The newest tapped connection's decrypted bytes from Waddle.
pub fn waddle_said(relay: &Relay) -> String {
    relay
        .connections()
        .last()
        .map(|connection| String::from_utf8_lossy(&connection.receiver_tls).into_owned())
        .unwrap_or_default()
}

/// Whether the newest relayed connection has closed.
pub fn newest_closed(relay: &Relay) -> bool {
    relay
        .connections()
        .last()
        .is_some_and(|connection| connection.closed_after.is_some())
}

/// The bare JID `alice@<peer domain>`.
pub fn peer_alice(peer: Peer) -> String {
    ["alice@", Harness::peer_domain(peer)].concat()
}

/// A domain on the far side of `peer` that home-a does not serve.
pub fn unserved_for(peer: Peer) -> &'static str {
    match peer {
        Peer::Waddle => crate::topology::PROSODY,
        Peer::Prosody => crate::topology::EJABBERD,
        Peer::Ejabberd => crate::topology::PROSODY,
    }
}

pub fn context_for(peer: Peer) -> String {
    ["peer ", peer.label()].concat()
}

/// Fail with context when an answer is missing.
pub fn require<T>(value: Option<T>, what: &str) -> Result<T> {
    value.with_context(|| what.to_owned())
}
