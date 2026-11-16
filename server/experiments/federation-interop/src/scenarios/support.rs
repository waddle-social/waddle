//! Helpers shared by the rows.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;

use crate::client::session::Session;
use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::net::relay::{Connection, Mode, Relay};
use crate::process;
use crate::scenarios::harness::{Harness, ANSWER};
use crate::topology::{Home, Server, HOME_A};
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

/// The sum of metric samples whose name starts with `prefix` and whose
/// labels include every value in `values` (as `="value"`).
pub fn metric(metrics: &str, prefix: &str, values: &[&str]) -> f64 {
    metrics
        .lines()
        .filter(|line| line.starts_with(prefix))
        .filter(|line| {
            values.iter().all(|value| {
                let quoted = ["=\"", value, "\""].concat();
                line.contains(&quoted)
            })
        })
        .filter_map(|line| line.rsplit(' ').next()?.parse::<f64>().ok())
        .sum()
}

/// Established-stream counters of a deployment by direction and strength.
pub async fn established(h: &Harness, home: Home, direction: &str, strength: &str) -> f64 {
    match h.waddle.get(&home) {
        Some(waddle) => match waddle.metrics(h.ports).await {
            Ok(metrics) => metric(&metrics, "waddle_s2s_established", &[direction, strength]),
            Err(_) => f64::NAN,
        },
        None => f64::NAN,
    }
}

/// Record the newest relayed connection's negotiated TLS version.
pub fn note_tls(outcome: &mut Outcome, relay: &Relay) {
    match relay
        .connections()
        .last()
        .and_then(Connection::server_hello)
    {
        Some(hello) => outcome.note(format!(
            "{}: {} cipher suite 0x{:04x}",
            relay.label,
            hello.version_name(),
            hello.cipher_suite
        )),
        None => outcome.note(format!("{}: no ServerHello seen", relay.label)),
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
