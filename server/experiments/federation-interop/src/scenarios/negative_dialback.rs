//! Negative rows on dialback (XEP-0220 under D2), driven by the harness
//! raw peer against home-a, whose dialback list names peer-raw.test but
//! not redirect.test. Each row first shows dialback for the listed
//! peer-raw.test establishing, and ties its refusal to what home-a did:
//! no verification request to the authoritative server, and home-a's own
//! log line for the refusal.

use anyhow::Result;
use minidom::rxml::xml_ncname;
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::pki::LeafSpec;
use crate::s2s::initiator::{self, Auth, Plan, RawStream, Stage};
use crate::s2s::receiver::{Behaviour, Receiver};
use crate::scenarios::harness::{Harness, QUIET};
use crate::scenarios::support::{self, describe, is_result, PAIR_RELEASE};
use crate::topology::{Home, HOME_A, HOME_B, RAW_PEER, REDIRECT};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

fn dialback_plan(h: &Harness, from: &str, key: &str) -> Result<Plan> {
    let mut plan = Plan::external(from, HOME_A, &h.leaf(RAW_PEER)?);
    plan.leaf = None;
    plan.open = plan.open.with_dialback();
    plan.auth = Auth::Dialback(key.to_owned());
    Ok(plan)
}

/// Verification requests home-a sent to a harness authoritative server.
fn verifications(receiver: &Receiver) -> usize {
    receiver
        .inbound()
        .iter()
        .map(|inbound| inbound.verification_requests)
        .sum()
}

/// The in-row control: inbound dialback without a certificate for
/// peer-raw.test, which is dialback-listed, establishes, and home-a
/// verified the key with the harness's authoritative server.
async fn dialback_control(h: &Harness, outcome: &mut Outcome) -> Result<()> {
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.receiver.issue_key(&key);
    h.receiver.clear();
    let attempt = initiator::attempt(
        &h.pki,
        h.ports.waddle_s2s(Home::A),
        &dialback_plan(h, RAW_PEER, &key)?,
    )
    .await?;
    let verified = verifications(&h.receiver);
    let detail = format!("{}; verification requests {verified}", attempt.describe());
    let established = attempt.stage == Stage::Established && verified > 0;
    if let Some(stream) = attempt.stream {
        stream.close().await;
        tokio::time::sleep(PAIR_RELEASE).await;
    }
    if !established {
        return Err(Unverifiable(
            [
                "control: dialback for the listed peer-raw.test did not establish: ",
                &detail,
            ]
            .concat(),
        )
        .into());
    }
    outcome.check(
        "control: dialback for the listed peer-raw.test establishes",
        true,
        detail,
    );
    Ok(())
}

pub async fn unlisted_domain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer offering dialback for redirect.test, which is admitted but not dialback-listed, with redirect.test's authoritative server answering verifications");
    dialback_control(h, &mut outcome).await?;
    // redirect.test's authoritative server would verify the key, so a
    // refusal comes from the dialback list, not from a failed verification.
    h.redirect.set_behaviour(Behaviour::External);
    h.redirect.clear();
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.redirect.issue_key(&key);
    // redirect.test is admitted, so the refusal is not an admission
    // outcome: home-a ends the stream with its own stream error, which it
    // logs as the `Protocol` outcome.
    let ended = support::LogMark::new(h, Home::A, &["s2s connection ended", "\"protocol\""]);
    let attempt = initiator::attempt(
        &h.pki,
        h.ports.waddle_s2s(Home::A),
        &dialback_plan(h, REDIRECT, &key)?,
    )
    .await?;
    let verified = verifications(&h.redirect);
    let logged = ended.settled(h, 1).await;
    // Admitted, so the refusal comes after TLS, when dialback is offered.
    outcome.check(
        "dialback refused after TLS by the list, before any verification",
        attempt.stage == Stage::Authentication
            && attempt.stream_error.as_deref() == Some("policy-violation")
            && verified == 0
            && logged > 0,
        format!(
            "{}; verification requests to redirect.test {verified}; home-a logged {logged} connections ended with its own stream error",
            attempt.describe()
        ),
    );
    Ok(outcome)
}

pub async fn after_certificate_failure(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer presenting a wrong-name certificate, then offering dialback for peer-raw.test");
    dialback_control(h, &mut outcome).await?;
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.receiver.issue_key(&key);
    h.receiver.clear();
    let mut plan = dialback_plan(h, RAW_PEER, &key)?;
    plan.leaf = Some(h.issue(&LeafSpec::new(
        "dialback-wrong-name",
        &["unrelated-name.test"],
    ))?);
    let refusals = support::LogMark::new(
        h,
        Home::A,
        &["s2s peer certificate refused", "\"identity_mismatch\""],
    );
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    let verified = verifications(&h.receiver);
    let logged = refusals.settled(h, 1).await;
    outcome.check(
        "dialback never offered after the active certificate failure",
        matches!(attempt.stage, Stage::Tls | Stage::Authentication)
            && attempt.features_after_tls.is_none()
            && verified == 0
            && logged > 0,
        format!(
            "{}; verification requests {verified}; home-a logged {logged} identity_mismatch certificate refusals",
            attempt.describe()
        ),
    );
    Ok(outcome)
}

pub async fn unsolicited(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer on an EXTERNAL-authenticated stream");
    support::raw_control(h).await?;
    // An unsolicited request and an answer nothing asked for: both end the
    // stream (XEP-0220 sections 2.2 and 4.1).
    for (name, element) in [
        (
            "unsolicited db:result",
            Element::builder("result", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), RAW_PEER)
                .attr(xml_ncname!("to").to_owned(), HOME_A)
                .append("0123456789abcdef")
                .build(),
        ),
        (
            "uncorrelated db:verify answer",
            Element::builder("verify", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), RAW_PEER)
                .attr(xml_ncname!("to").to_owned(), HOME_A)
                .attr(xml_ncname!("id").to_owned(), "no-such-request")
                .attr(xml_ncname!("type").to_owned(), "valid")
                .build(),
        ),
    ] {
        let Some(mut stream) = external_stream(h, &mut outcome, name).await? else {
            continue;
        };
        stream.send(&element).await?;
        let ending = stream.watch(QUIET).await;
        let answered_valid = ending
            .elements
            .iter()
            .any(|element| element.attr("type") == Some("valid"));
        outcome.check(
            [name, " refused"].concat(),
            !answered_valid && ending.closed && ending.stream_error.is_some(),
            format!(
                "stream error {:?}, closed {}, answers {}",
                ending.stream_error,
                ending.closed,
                ending.elements.len()
            ),
        );
        stream.close().await;
        tokio::time::sleep(PAIR_RELEASE).await;
    }
    // A verification request is a request home-a answers as the
    // Authoritative Server on any stream from the pair; it holds no state
    // to correlate it with (XEP-0185 keys), so a key it did not issue is
    // answered `invalid`, never `valid`.
    let name = "a verification request for a key home-a did not issue";
    if let Some(mut stream) = external_stream(h, &mut outcome, name).await? {
        let request = Element::builder("verify", ns::DIALBACK)
            .attr(xml_ncname!("from").to_owned(), RAW_PEER)
            .attr(xml_ncname!("to").to_owned(), HOME_A)
            .attr(xml_ncname!("id").to_owned(), "no-such-stream")
            .append("0123456789abcdef")
            .build();
        stream.send(&request).await?;
        let ending = stream.watch(QUIET).await;
        let answers: Vec<Option<&str>> = ending
            .elements
            .iter()
            .filter(|element| element.is("verify", ns::DIALBACK))
            .map(|element| element.attr("type"))
            .collect();
        outcome.check(
            [name, " is answered invalid"].concat(),
            answers == [Some("invalid")],
            format!(
                "answers {answers:?}, stream error {:?}, closed {}",
                ending.stream_error, ending.closed
            ),
        );
        stream.close().await;
        tokio::time::sleep(PAIR_RELEASE).await;
    }
    Ok(outcome)
}

/// An EXTERNAL-authenticated peer-raw.test stream that declares the
/// dialback namespace, or a failed check named `name` when it does not
/// establish.
async fn external_stream(
    h: &Harness,
    outcome: &mut Outcome,
    name: &str,
) -> Result<Option<RawStream>> {
    let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    plan.open = plan.open.with_dialback();
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    if attempt.stream.is_none() {
        outcome.check(
            name,
            false,
            ["could not establish: ", &attempt.describe()].concat(),
        );
    }
    Ok(attempt.stream)
}

/// A stream of each kind the row covers, authenticated as peer-raw.test.
async fn authenticated_stream(h: &Harness, dialback: bool) -> Result<Option<RawStream>> {
    let plan = if dialback {
        let key = uuid::Uuid::new_v4().simple().to_string();
        h.receiver.issue_key(&key);
        dialback_plan(h, RAW_PEER, &key)?
    } else {
        Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?)
    };
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    Ok(attempt.stream)
}

/// Send a ping on `stream` and return home-a's answer, as the harness
/// receiving server got it, with the stream's ending.
async fn ping_on(
    h: &Harness,
    stream: &mut RawStream,
    from: &str,
    to: &str,
) -> Result<(Option<Element>, crate::s2s::initiator::Ending)> {
    let ping_id = support::id("extra");
    stream
        .send(&stanza::iq(
            ns::SERVER,
            "get",
            &ping_id,
            Some(from),
            Some(to),
            Some(stanza::ping_payload()),
        ))
        .await?;
    let ending = stream.watch(QUIET).await;
    let answer = h
        .receiver
        .wait_stanza(QUIET, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await;
    Ok((answer, ending))
}

pub async fn extra_domains(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "harness raw peer on EXTERNAL- and dialback-authenticated peer-raw.test -> home-a.test streams",
    );
    let own = ["x@", RAW_PEER, "/r"].concat();
    for (kind, dialback) in [("EXTERNAL", false), ("dialback", true)] {
        for (name, from, to) in [
            ("extra sender domain", ["x@", REDIRECT].concat(), HOME_A),
            ("extra target domain", own.clone(), HOME_B),
        ] {
            let name = [kind, " stream: ", name, " refused"].concat();
            h.receiver.clear();
            let Some(mut stream) = authenticated_stream(h, dialback).await? else {
                outcome.unverified(name, "the stream did not authenticate");
                continue;
            };
            // Control on the same stream: the pair's own domains are
            // answered.
            let (control, _) = ping_on(h, &mut stream, &own, HOME_A).await?;
            if !control.as_ref().is_some_and(is_result) {
                outcome.unverified(
                    name,
                    format!(
                        "control: a ping within the pair was not answered on this stream: {}",
                        control
                            .map(|element| describe(&element))
                            .unwrap_or_else(|| "no answer".to_owned())
                    ),
                );
                stream.close().await;
                tokio::time::sleep(PAIR_RELEASE).await;
                continue;
            }
            let (answer, ending) = ping_on(h, &mut stream, &from, to).await?;
            let answered = answer.as_ref().is_some_and(is_result);
            // A domain outside the stream's pair is a stream error
            // (s2s-handshake-2047.md, decisions after the G4 review).
            outcome.check(
                name,
                !answered && ending.stream_error.is_some() && ending.closed,
                format!(
                    "control within the pair answered; stream error {:?}, closed {}, result delivered {answered}",
                    ending.stream_error, ending.closed
                ),
            );
            stream.close().await;
            // The next stream is for the same pair; let home-a unregister
            // this one first, or the new one would be refused as a
            // conflict.
            tokio::time::sleep(PAIR_RELEASE).await;
        }
    }
    Ok(outcome)
}

pub async fn verify_over_plaintext(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer sending db:verify before STARTTLS");
    support::raw_control(h).await?;
    let mut socket = initiator::connect(h.ports.waddle_s2s(Home::A)).await?;
    socket
        .write_all(
            &StreamOpen::server(Some(RAW_PEER), HOME_A)
                .with_dialback()
                .encode()?,
        )
        .await?;
    let verify = Element::builder("verify", ns::DIALBACK)
        .attr(xml_ncname!("from").to_owned(), RAW_PEER)
        .attr(xml_ncname!("to").to_owned(), HOME_A)
        .attr(xml_ncname!("id").to_owned(), "plaintext-check")
        .append("0123456789abcdef")
        .build();
    socket.write_all(&header::element_bytes(&verify)).await?;
    let mut framer = Framer::new();
    let mut chunk = vec![0u8; 8192];
    let mut answered = false;
    let mut stream_error = None;
    let mut closed = false;
    let deadline = tokio::time::Instant::now() + QUIET;
    loop {
        while let Ok(Some(item)) = framer.next_item() {
            if let Item::Element(element) = item {
                answered |= element.is("verify", ns::DIALBACK);
                stream_error = stream_error.or_else(|| stanza::stream_error_condition(&element));
            }
        }
        match tokio::time::timeout_at(deadline, socket.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => {
                closed = true;
                break;
            }
            Err(_) => break,
            Ok(Ok(read)) => framer.push(chunk.get(..read).unwrap_or_default()),
        }
    }
    outcome.check(
        "no verification answer over plaintext; the stream is refused",
        !answered && (closed || stream_error.is_some()),
        format!("stream error {stream_error:?}, closed {closed}"),
    );
    Ok(outcome)
}
