//! The W10 observations of each peer's wire behavior: a pre-TLS `from`,
//! whitespace before stream headers, ejabberd's effective STARTTLS
//! setting, a mixed-case `to` domain, and the clustered ejabberd's
//! streams against Waddle's one stream per directional pair. An
//! observation row is PASS when the behavior was observed and recorded and
//! Waddle's handling of it was as specified.

use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::net::relay::Connection;
use crate::s2s::initiator::{self, Auth, Plan};
use crate::scenarios::harness::{Harness, ANSWER};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::services::ejabberd::MAX_S2S_CONNECTIONS;
use crate::topology::{Home, Server, EJABBERD, HOME_A, RAW_PEER};
use crate::xml::framer::{Framer, Item};
use crate::xml::{ns, stanza};

/// The first stream header in `bytes`.
fn first_header(bytes: &[u8]) -> Option<crate::xml::framer::Header> {
    let mut framer = Framer::new();
    framer.push(skip_leading_whitespace(bytes));
    loop {
        match framer.next_item() {
            Ok(Some(Item::Header(header))) => return Some(header),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

fn skip_leading_whitespace(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    bytes.get(start..).unwrap_or_default()
}

/// Whitespace found before a stream header: before the first token of
/// the stream, or between the XML declaration and the stream element.
fn whitespace_before_headers(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    if bytes.first().is_some_and(u8::is_ascii_whitespace) {
        found.push("before the first token".to_owned());
    }
    let text = String::from_utf8_lossy(bytes);
    for (index, _) in text.match_indices("<stream:stream") {
        let before = text.get(..index).unwrap_or_default();
        if let Some(declaration_end) = before.rfind("?>") {
            let gap = before.get(declaration_end + 2..).unwrap_or_default();
            if !gap.is_empty() && gap.chars().all(char::is_whitespace) {
                found.push("between the XML declaration and <stream:stream>".to_owned());
            }
        }
    }
    found
}

/// Drive the peer through a fresh connection on its relay to home-a and
/// return that connection's record.
async fn fresh_connection(h: &Harness, peer: Peer) -> Result<Connection> {
    let relay = support::tap_peer_stream(h, peer).await?;
    relay
        .connections()
        .into_iter()
        .rev()
        .find(|connection| connection.tapped)
        .context("no tapped connection recorded")
}

pub async fn pre_tls_from(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new(["stock ", peer.label(), " opening a stream to home-a"].concat());
    let connection = fresh_connection(h, peer).await?;
    let header = first_header(connection.initiator_plaintext());
    match header {
        Some(header) => {
            let from = header.attribute("from");
            outcome.check(
                "pre-TLS stream header recorded",
                true,
                format!(
                    "from: {}; to: {}",
                    from.as_deref().unwrap_or("absent"),
                    header.attribute("to").unwrap_or_default()
                ),
            );
            outcome.note(if from.is_some() {
                "the peer sends a pre-TLS `from`, which Waddle requires".to_owned()
            } else {
                "the peer sends no pre-TLS `from`; Waddle refuses such a stream with bad-format"
                    .to_owned()
            });
        }
        None => outcome.check(
            "pre-TLS stream header recorded",
            false,
            "no header in the relayed bytes",
        ),
    }
    Ok(outcome)
}

pub async fn whitespace(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(["stock ", peer.label(), " through the TLS tap"].concat());
    let connection = fresh_connection(h, peer).await?;
    let before_tls = whitespace_before_headers(connection.initiator_plaintext());
    let after_tls = whitespace_before_headers(&connection.initiator_tls);
    outcome.check(
        "stream headers before and after TLS recorded",
        !connection.initiator_plaintext().is_empty() && !connection.initiator_tls.is_empty(),
        format!("before TLS: {before_tls:?}; after TLS and SASL: {after_tls:?}"),
    );
    outcome.note(if before_tls.is_empty() && after_tls.is_empty() {
        "the peer sends no whitespace before its stream headers".to_owned()
    } else {
        "the peer sends whitespace before a stream header, which Waddle refuses".to_owned()
    });
    Ok(outcome)
}

pub async fn ejabberd_starttls(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "harness raw peer reading ejabberd's stream features, and the relayed features to home-a",
    );
    let mut plan = Plan::external(RAW_PEER, EJABBERD, &h.leaf(RAW_PEER)?);
    plan.auth = Auth::None;
    for node in [1u8, 2] {
        let attempt = initiator::attempt(&h.pki, h.ports.ejabberd_s2s(node), &plan).await?;
        let starttls = attempt
            .features_before_tls
            .as_ref()
            .and_then(|features| features.get_child("starttls", ns::TLS).cloned());
        let required = starttls
            .as_ref()
            .is_some_and(|element| element.get_child("required", ns::TLS).is_some());
        outcome.check(
            format!("node {node} offers STARTTLS as required"),
            required,
            format!(
                "starttls offered: {}, required: {required}",
                starttls.is_some()
            ),
        );
        if let Some(stream) = attempt.stream {
            stream.close().await;
        }
    }
    Ok(outcome)
}

pub async fn mixed_case_to(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(["stock ", peer.label()].concat());
    let mixed = "Home-A.TEST";
    let relay = support::tap_peer_stream(h, peer).await?;
    let mut session = h.login_peer(peer, "alice", &support::id("case")).await?;
    let first_id = support::id("case");
    let answer = ask(&mut session, stanza::ping(&first_id, mixed)).await?;
    let wire = relay
        .connections()
        .last()
        .map(|connection| String::from_utf8_lossy(&connection.initiator_tls).into_owned())
        .unwrap_or_default();
    let preserved = wire.contains(mixed);
    outcome.note(format!(
        "the peer {} the mixed-case `to` it was given",
        if preserved { "kept" } else { "normalized" }
    ));
    if preserved {
        outcome.check(
            "Waddle accepts the mixed-case `to` and answers from the lowercase domain",
            is_result(&answer) && answer.attr("from") == Some(HOME_A),
            [
                describe(&answer),
                " from ".to_owned(),
                answer.attr("from").unwrap_or_default().to_owned(),
            ]
            .concat(),
        );
        outcome.driven_by = ["stock ", peer.label()].concat();
    } else {
        // The peer normalized it; write one into its own authenticated
        // stream instead.
        let injected_id = support::id("case-injected");
        let iq = stanza::iq(
            ns::SERVER,
            "get",
            &injected_id,
            Some(&session.jid),
            Some(mixed),
            Some(stanza::ping_payload()),
        );
        relay.inject(crate::xml::header::element_bytes(&iq))?;
        let reply = session
            .wait_for(ANSWER, |element: &Element| {
                element.attr("id") == Some(injected_id.as_str())
            })
            .await;
        match reply {
            Ok(reply) => outcome.check(
                "Waddle accepts the mixed-case `to` and answers from the lowercase domain",
                is_result(&reply) && reply.attr("from") == Some(HOME_A),
                [
                    describe(&reply),
                    " from ".to_owned(),
                    reply.attr("from").unwrap_or_default().to_owned(),
                ]
                .concat(),
            ),
            Err(error) => outcome.check(
                "Waddle accepts the mixed-case `to`",
                false,
                format!("no answer to the injected stanza: {error:#}"),
            ),
        }
        outcome.driven_by = [
            "tap injection into stock ",
            peer.label(),
            "'s authenticated stream (the peer normalizes `to` itself)",
        ]
        .concat();
    }
    session.close().await;
    Ok(outcome)
}

pub async fn ejabberd_cluster_streams(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "clients on both ejabberd nodes pinging home-a at once (max_s2s_connections 4, 2 per node)",
    );
    let relay = h.relay(Server::Ejabberd(1), Server::Waddle(Home::A))?;
    relay.cut();
    relay.restore();
    relay.reset_records();
    let mut sessions = Vec::new();
    for node in [1u8, 2] {
        for index in 0..2 {
            let resource = ["cluster-", &node.to_string(), "-", &index.to_string()].concat();
            sessions.push(h.login_ejabberd(node, "alice", &resource).await?);
        }
    }
    let mut asked = 0usize;
    let mut answered = 0usize;
    for round in 0..5 {
        let mut waiting = Vec::new();
        for session in &mut sessions {
            let ping_id = support::id(&["cluster-", &round.to_string()].concat());
            session.send(&stanza::ping(&ping_id, HOME_A)).await?;
            waiting.push(ping_id);
            asked += 1;
        }
        for (session, ping_id) in sessions.iter_mut().zip(waiting) {
            if let Ok(answer) = session
                .wait_for(ANSWER, |element| {
                    element.attr("id") == Some(ping_id.as_str())
                })
                .await
            {
                answered += usize::from(is_result(&answer));
            }
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
    }
    for session in sessions {
        session.close().await;
    }
    let connections = relay.connection_count();
    let concurrent = relay.max_active();
    outcome.check(
        "every ping answered while the cluster's streams replace each other",
        answered == asked,
        format!("{answered} of {asked} answered"),
    );
    if concurrent > 1 {
        outcome.check(
            "the cluster opened more than one stream for the pair",
            true,
            format!("{connections} connections, at most {concurrent} open at once (limit {MAX_S2S_CONNECTIONS})"),
        );
    } else {
        outcome.unverified(
            "the cluster opened more than one stream for the pair",
            format!("ejabberd kept one stream ({connections} connections in total); replacement was not exercised"),
        );
    }
    if let Some(waddle) = h.waddle.get(&Home::A) {
        if let Ok(metrics) = waddle.metrics(h.ports).await {
            outcome.note(format!(
                "home-a S2S metrics: {:?}",
                crate::net::http::metric_lines(&metrics, "waddle_s2s_")
            ));
        }
    }
    Ok(outcome)
}
