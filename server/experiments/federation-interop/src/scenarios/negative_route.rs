//! Negative rows on routing, local services (D10) and resources.
//!
//! The G20 rows put the peer's own authenticated stream to home-a through
//! the TLS tap and write the offending stanza into it, between the peer's
//! own writes: the stream really is the stock server's, so home-a's answer
//! is to that server, while no stock server would produce the stanza
//! itself. The other rows use the harness raw peer, whose receiving
//! server shows what home-a sends back.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::process;
use crate::s2s::initiator::{self, Plan, RawStream};
use crate::scenarios::harness::{Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result, newest_closed, waddle_said};
use crate::topology::{Home, Server, HOME_A, RAW_PEER};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

fn tap_driven(peer: Peer) -> String {
    [
        "tap injection into ",
        peer.label(),
        "'s authenticated stream to home-a",
    ]
    .concat()
}

/// Whether home-a ended the tapped stream, and what it said.
async fn stream_refused(relay: &crate::net::relay::Relay) -> (bool, String) {
    let ended = process::eventually(QUIET, || async { newest_closed(relay) }).await;
    let said = waddle_said(relay);
    let condition = [
        "improper-addressing",
        "invalid-from",
        "policy-violation",
        "not-authorized",
        "host-unknown",
        "restricted-xml",
        "bad-format",
        "resource-constraint",
    ]
    .into_iter()
    .find(|condition| said.contains(condition))
    .unwrap_or("none");
    (
        ended || condition != "none",
        [
            "stream error ",
            condition,
            if ended { ", closed" } else { "" },
        ]
        .concat(),
    )
}

pub async fn spoofed_from(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let relay = support::tap_peer_stream(h, peer).await?;
    let spoof = ["mallory@", support::unserved_for(peer), "/spoof"].concat();
    let iq = stanza::iq(
        ns::SERVER,
        "get",
        &support::id("spoof"),
        Some(&spoof),
        Some(HOME_A),
        Some(stanza::ping_payload()),
    );
    relay.inject(header::element_bytes(&iq))?;
    let (refused, detail) = stream_refused(relay).await;
    outcome.check("a stanza from another domain is refused", refused, detail);
    let recovered =
        support::ping_until_result(h, peer, false, HOME_A, Duration::from_secs(60)).await?;
    outcome.check(
        "unrelated traffic recovers",
        is_result(&recovered),
        describe(&recovered),
    );
    Ok(outcome)
}

pub async fn forged_iq_result(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let relay = support::tap_peer_stream(h, peer).await?;
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("forged"))
        .await?;
    let forged_id = support::id("forged");
    let forged = stanza::iq(
        ns::SERVER,
        "result",
        &forged_id,
        Some(Harness::peer_domain(peer)),
        Some(&session.jid),
        None,
    );
    relay.inject(header::element_bytes(&forged))?;
    let absent = session
        .absent(QUIET, |element| {
            element.attr("id") == Some(forged_id.as_str())
        })
        .await?;
    outcome.check(
        "the forged result reaches no local resource",
        absent,
        "watched the addressed resource",
    );
    session.close().await;
    Ok(outcome)
}

pub async fn replayed_iq_result(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let relay = support::tap_peer_stream(h, peer).await?;
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("replay"))
        .await?;
    let ping_id = support::id("replay");
    let first = ask(
        &mut session,
        stanza::ping(&ping_id, Harness::peer_domain(peer)),
    )
    .await?;
    outcome.check(
        "the genuine result arrives",
        is_result(&first),
        describe(&first),
    );
    let replay = stanza::iq(
        ns::SERVER,
        "result",
        &ping_id,
        Some(Harness::peer_domain(peer)),
        Some(&session.jid),
        None,
    );
    relay.inject(header::element_bytes(&replay))?;
    let absent = session
        .absent(QUIET, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await?;
    outcome.check(
        "the replayed result is not delivered again",
        absent,
        "watched the requesting resource",
    );
    session.close().await;
    Ok(outcome)
}

pub async fn relay_unserved(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let relay = support::tap_peer_stream(h, peer).await?;
    let third = support::unserved_for(peer);
    let target_peer = match third {
        crate::topology::PROSODY => Peer::Prosody,
        _ => Peer::Ejabberd,
    };
    let mut watcher = h
        .login_peer(target_peer, "alice", &support::id("relay"))
        .await
        .ok();
    let message_id = support::id("relay");
    let message = stanza::message(
        ns::SERVER,
        "chat",
        &message_id,
        Some(&[support::peer_alice(peer).as_str(), "/relay"].concat()),
        &["alice@", third].concat(),
        Some("relay through home-a"),
    );
    relay.inject(header::element_bytes(&message))?;
    let (refused, detail) = stream_refused(relay).await;
    match watcher.as_mut() {
        Some(watcher) => {
            let absent = watcher
                .absent(QUIET, |element| {
                    element.attr("id") == Some(message_id.as_str())
                })
                .await?;
            outcome.check(
                ["nothing relayed to ", third].concat(),
                absent,
                detail.clone(),
            );
        }
        None => outcome.unverified(
            ["nothing relayed to ", third].concat(),
            "the third server is not running",
        ),
    }
    outcome.note(
        [
            "home-a's handling: ",
            &detail,
            if refused { "" } else { " (stanza dropped)" },
        ]
        .concat(),
    );
    if let Some(watcher) = watcher {
        watcher.close().await;
    }
    Ok(outcome)
}

async fn raw_stream(h: &Harness) -> Result<RawStream> {
    let plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    let detail = attempt.describe();
    attempt
        .stream
        .ok_or_else(|| anyhow::anyhow!("raw peer could not establish: {detail}"))
}

fn from_raw() -> String {
    ["x@", RAW_PEER, "/r"].concat()
}

pub async fn no_error_to_error(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer and its receiving server");
    let mut stream = raw_stream(h).await?;
    h.receiver.clear();
    let control_id = support::id("control");
    stream
        .send(&stanza::message(
            ns::SERVER,
            "normal",
            &control_id,
            Some(&from_raw()),
            HOME_A,
            Some("hello"),
        ))
        .await?;
    let control = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(control_id.as_str())
        })
        .await;
    outcome.check(
        "control: a message to the server domain is answered with an error",
        control
            .as_ref()
            .is_some_and(|element| element.attr("type") == Some("error")),
        control
            .map(|element| describe(&element))
            .unwrap_or_else(|| "no answer".to_owned()),
    );
    let error_id = support::id("error");
    let mut error = stanza::message(
        ns::SERVER,
        "error",
        &error_id,
        Some(&from_raw()),
        HOME_A,
        None,
    );
    error.append_child(
        Element::builder("error", ns::SERVER)
            .append(Element::builder("service-unavailable", ns::STANZAS).build())
            .build(),
    );
    stream.send(&error).await?;
    tokio::time::sleep(QUIET).await;
    let answered = h
        .receiver
        .stanzas()
        .iter()
        .any(|element| element.attr("id") == Some(error_id.as_str()));
    outcome.check(
        "an error stanza is not answered",
        !answered,
        "watched the receiving server",
    );
    stream.close().await;
    Ok(outcome)
}

pub async fn account_handler_refused(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    match peer {
        Peer::Waddle => {
            let mut outcome = Outcome::new(
                "harness raw peer (home-b's own outbound refusal stops account traffic before S2S)",
            );
            let mut stream = raw_stream(h).await?;
            h.receiver.clear();
            let mut conditions = BTreeMap::new();
            for account in ["alice", "nobody-here"] {
                let message_id = support::id("d7");
                stream
                    .send(&stanza::message(
                        ns::SERVER,
                        "chat",
                        &message_id,
                        Some(&from_raw()),
                        &[account, "@", HOME_A].concat(),
                        Some("hello"),
                    ))
                    .await?;
                let answer = h
                    .receiver
                    .wait_stanza(ANSWER, |element| {
                        element.attr("id") == Some(message_id.as_str())
                    })
                    .await;
                conditions.insert(account, answer.as_ref().and_then(stanza::error_condition));
            }
            stream.close().await;
            compare_conditions(&mut outcome, &conditions);
            Ok(outcome)
        }
        Peer::Prosody | Peer::Ejabberd => {
            let mut outcome = Outcome::new(["a client of stock ", peer.label()].concat());
            let mut session = h.login_peer(peer, "alice", &support::id("d7")).await?;
            let mut conditions = BTreeMap::new();
            for account in ["alice", "nobody-here"] {
                let message_id = support::id("d7");
                session
                    .send(&stanza::chat(
                        &message_id,
                        &[account, "@", HOME_A].concat(),
                        "hello",
                    ))
                    .await?;
                let answer = session
                    .wait_for(ANSWER, |element| {
                        element.attr("id") == Some(message_id.as_str())
                    })
                    .await
                    .ok();
                conditions.insert(account, answer.as_ref().and_then(stanza::error_condition));
            }
            session.close().await;
            compare_conditions(&mut outcome, &conditions);
            Ok(outcome)
        }
    }
}

fn compare_conditions(outcome: &mut Outcome, conditions: &BTreeMap<&str, Option<String>>) {
    let existing = conditions.get("alice").cloned().flatten();
    let missing = conditions.get("nobody-here").cloned().flatten();
    outcome.check(
        "an existing account is refused",
        existing.is_some(),
        format!("{existing:?}"),
    );
    outcome.check(
        "refused exactly as an account that does not exist",
        existing.is_some() && existing == missing,
        format!("existing {existing:?}, missing {missing:?}"),
    );
}

pub async fn d10_services(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer and its receiving server");
    let mut stream = raw_stream(h).await?;
    h.receiver.clear();
    let upload_id = support::id("upload");
    let request = Element::builder("request", ns::HTTP_UPLOAD)
        .attr(minidom::rxml::xml_ncname!("filename").to_owned(), "a.txt")
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "10")
        .build();
    stream
        .send(&stanza::iq(
            ns::SERVER,
            "get",
            &upload_id,
            Some(&from_raw()),
            Some(HOME_A),
            Some(request),
        ))
        .await?;
    let upload = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(upload_id.as_str())
        })
        .await;
    outcome.check(
        "an HTTP upload request receives service-unavailable",
        upload.as_ref().and_then(stanza::error_condition).as_deref() == Some("service-unavailable"),
        upload
            .map(|element| describe(&element))
            .unwrap_or_else(|| "no answer".to_owned()),
    );
    let before = h.receiver.stanzas().len();
    let presence = Element::builder("presence", ns::SERVER)
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), from_raw())
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), HOME_A)
        .build();
    stream.send(&presence).await?;
    tokio::time::sleep(QUIET).await;
    let after = h.receiver.stanzas().len();
    outcome.check(
        "presence to the server domain is ignored",
        after == before,
        format!("{} answers", after - before),
    );
    let ping_id = support::id("d10-ping");
    stream
        .send(&stanza::iq(
            ns::SERVER,
            "get",
            &ping_id,
            Some(&from_raw()),
            Some(HOME_A),
            Some(stanza::ping_payload()),
        ))
        .await?;
    let ping = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await;
    outcome.check(
        "control: ping is answered",
        ping.as_ref().is_some_and(is_result),
        ping.map(|element| describe(&element))
            .unwrap_or_else(|| "no answer".to_owned()),
    );
    stream.close().await;
    Ok(outcome)
}

async fn injected_refusal(
    h: &Harness,
    peer: Peer,
    element: Element,
    outcome: &mut Outcome,
    name: &str,
) -> Result<()> {
    let relay = support::tap_peer_stream(h, peer).await?;
    relay.inject(header::element_bytes(&element))?;
    let (refused, detail) = stream_refused(relay).await;
    outcome.check([name, " refused"].concat(), refused, detail);
    let recovered =
        support::ping_until_result(h, peer, false, HOME_A, Duration::from_secs(60)).await?;
    outcome.check(
        "home-a serves the peer again afterwards",
        is_result(&recovered),
        describe(&recovered),
    );
    Ok(())
}

pub async fn oversized(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let body = stanza::filler(600 * 1024);
    let message = stanza::message(
        ns::SERVER,
        "normal",
        &support::id("big"),
        Some(&[support::peer_alice(peer).as_str(), "/big"].concat()),
        HOME_A,
        Some(&body),
    );
    injected_refusal(h, peer, message, &mut outcome, "a 600 KiB stanza").await?;
    Ok(outcome)
}

pub async fn deep_nesting(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(tap_driven(peer));
    let mut message = stanza::message(
        ns::SERVER,
        "normal",
        &support::id("deep"),
        Some(&[support::peer_alice(peer).as_str(), "/deep"].concat()),
        HOME_A,
        None,
    );
    message.append_child(stanza::nested(64));
    injected_refusal(h, peer, message, &mut outcome, "a stanza nested 64 deep").await?;
    Ok(outcome)
}

pub async fn preauth_large_element(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer writing a large non-control element before TLS");
    let mut socket = TcpStream::connect(("127.0.0.1", h.ports.waddle_s2s(Home::A))).await?;
    socket
        .write_all(&StreamOpen::server(Some(RAW_PEER), HOME_A).encode()?)
        .await?;
    let element = stanza::message(
        ns::SERVER,
        "normal",
        "pre-auth",
        Some(RAW_PEER),
        HOME_A,
        Some(&stanza::filler(64 * 1024)),
    );
    let bytes = header::element_bytes(&element);
    // The write may fail once the server has closed; that is the refusal.
    let _ = socket.write_all(&bytes).await;
    let mut received = Vec::new();
    let _ = tokio::time::timeout(QUIET, socket.read_to_end(&mut received)).await;
    let text = String::from_utf8_lossy(&received);
    let refused = text.contains("policy-violation")
        || text.contains("restricted-xml")
        || text.contains("not-authorized")
        || text.contains("unsupported-stanza-type");
    outcome.check(
        "refused and closed",
        refused || received.len() < bytes.len(),
        text.chars()
            .rev()
            .take(200)
            .collect::<String>()
            .chars()
            .rev()
            .collect::<String>(),
    );
    Ok(outcome)
}

pub async fn queue_exhaustion(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "home-a's outbound path to ",
            peer.label(),
            " cut while a client floods it",
        ]
        .concat(),
    );
    let relay = h.relay(Server::Waddle(Home::A), Harness::peer_server(peer))?;
    relay.cut();
    let peer_domain = Harness::peer_domain(peer);
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("flood"))
        .await?;
    let mut ids = Vec::new();
    for _ in 0..400 {
        let ping_id = support::id("flood");
        session.send(&stanza::ping(&ping_id, peer_domain)).await?;
        ids.push(ping_id);
    }
    let local = ask(&mut session, stanza::ping(&support::id("local"), HOME_A)).await;
    outcome.check(
        "local traffic is answered while the peer's queue is full",
        local.as_ref().is_ok_and(is_result),
        local
            .map(|answer| describe(&answer))
            .unwrap_or_else(|error| error.to_string()),
    );
    let mut conditions: BTreeMap<String, usize> = BTreeMap::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    let mut answered = 0usize;
    while answered < ids.len() && tokio::time::Instant::now() < deadline {
        match session
            .wait_for(Duration::from_secs(5), |element| {
                element.name() == "iq"
                    && element
                        .attr("id")
                        .is_some_and(|id| id.starts_with("flood-"))
            })
            .await
        {
            Ok(answer) => {
                answered += 1;
                *conditions.entry(describe(&answer)).or_insert(0) += 1;
            }
            Err(_) => continue,
        }
    }
    outcome.check(
        "the flood is answered with errors, not held without bound",
        answered > 0 && !conditions.contains_key("result"),
        format!("{answered} of {} answered: {conditions:?}", ids.len()),
    );
    session.close().await;
    relay.restore();
    Ok(outcome)
}
