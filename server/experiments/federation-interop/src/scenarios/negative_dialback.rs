//! Negative rows on dialback (XEP-0220 under D2), driven by the harness
//! raw peer against home-a, whose dialback list names peer-raw.test but
//! not redirect.test.

use anyhow::Result;
use minidom::rxml::xml_ncname;
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::pki::LeafSpec;
use crate::s2s::initiator::{self, Auth, Plan, Stage};
use crate::scenarios::harness::{Harness, QUIET};
use crate::scenarios::support;
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

pub async fn unlisted_domain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer offering dialback for redirect.test, which is admitted but not dialback-listed");
    support::raw_control(h).await?;
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.redirect.issue_key(&key);
    let attempt = initiator::attempt(
        &h.pki,
        h.ports.waddle_s2s(Home::A),
        &dialback_plan(h, REDIRECT, &key)?,
    )
    .await?;
    // Admitted, so the refusal comes after TLS, when dialback is offered.
    outcome.check(
        "dialback refused after TLS",
        attempt.stage == Stage::Authentication,
        attempt.describe(),
    );
    Ok(outcome)
}

pub async fn after_certificate_failure(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer presenting a wrong-name certificate, then offering dialback for peer-raw.test");
    support::raw_control(h).await?;
    let key = uuid::Uuid::new_v4().simple().to_string();
    h.receiver.issue_key(&key);
    let mut plan = dialback_plan(h, RAW_PEER, &key)?;
    plan.leaf = Some(h.issue(&LeafSpec::new(
        "dialback-wrong-name",
        &["unrelated-name.test"],
    ))?);
    let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
    outcome.check(
        "dialback refused after the active certificate failure",
        matches!(attempt.stage, Stage::Tls | Stage::Authentication),
        attempt.describe(),
    );
    Ok(outcome)
}

pub async fn unsolicited(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer on an EXTERNAL-authenticated stream");
    support::raw_control(h).await?;
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
            "uncorrelated db:verify",
            Element::builder("verify", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), RAW_PEER)
                .attr(xml_ncname!("to").to_owned(), HOME_A)
                .attr(xml_ncname!("id").to_owned(), "no-such-stream")
                .append("0123456789abcdef")
                .build(),
        ),
    ] {
        let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
        plan.open = plan.open.with_dialback();
        let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
        let Some(mut stream) = attempt.stream else {
            outcome.check(
                name,
                false,
                ["could not establish: ", &attempt.describe()].concat(),
            );
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
    }
    Ok(outcome)
}

pub async fn extra_domains(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "harness raw peer on an EXTERNAL-authenticated peer-raw.test -> home-a.test stream",
    );
    for (name, from, to) in [
        (
            "extra sender domain",
            ["x@", REDIRECT].concat(),
            HOME_A.to_owned(),
        ),
        (
            "extra target domain",
            ["x@", RAW_PEER].concat(),
            HOME_B.to_owned(),
        ),
    ] {
        let plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
        let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
        let Some(mut stream) = attempt.stream else {
            outcome.check(
                name,
                false,
                ["could not establish: ", &attempt.describe()].concat(),
            );
            continue;
        };
        h.receiver.clear();
        let ping_id = support::id("extra");
        stream
            .send(&stanza::iq(
                ns::SERVER,
                "get",
                &ping_id,
                Some(&from),
                Some(&to),
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
        let answered = answer
            .as_ref()
            .is_some_and(|element| element.attr("type") == Some("result"));
        let malformed = answer
            .as_ref()
            .and_then(stanza::error_condition)
            .is_some_and(|condition| condition == "jid-malformed");
        // A domain outside the stream's pair is a stream error
        // (s2s-handshake-2047.md, decisions after the G4 review).
        outcome.check(
            [name, " refused"].concat(),
            !answered && (ending.stream_error.is_some() || malformed),
            format!(
                "stream error {:?}, closed {}, result delivered {answered}",
                ending.stream_error, ending.closed
            ),
        );
        stream.close().await;
    }
    Ok(outcome)
}

pub async fn verify_over_plaintext(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer sending db:verify before STARTTLS");
    support::raw_control(h).await?;
    let mut socket = TcpStream::connect(("127.0.0.1", h.ports.waddle_s2s(Home::A))).await?;
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
