//! Negative rows on addressing: the domain forms the identity profile
//! refuses, in stream headers, as outbound destinations and in stanza
//! addresses, and `see-other-host`, which is never followed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::s2s::initiator::{self, Plan, Stage};
use crate::s2s::receiver::Behaviour;
use crate::scenarios::harness::{Component, Harness, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::topology::{Home, Server, HOME_A, RAW_PEER, REDIRECT};
use crate::xml::{ns, stanza};

/// The refused forms: a bracketed IP-literal, an IPv4address, the
/// numeric, hexadecimal and abbreviated IPv4 forms and a single-label
/// name.
pub const REFUSED_FORMS: [&str; 6] = [
    "[::1]",
    "127.0.0.1",
    "2130706433",
    "0x7f.1",
    "127.1",
    "localhost",
];

pub async fn header_forms(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer sending each refused form as the header's `from`");
    support::raw_control(h).await?;
    for form in REFUSED_FORMS {
        let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
        plan.open.from = Some(form.to_owned());
        let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
        outcome.check(
            ["`from` ", form, " refused before TLS with a stream error"].concat(),
            attempt.stage == Stage::BeforeTls && attempt.stream_error.is_some(),
            attempt.describe(),
        );
    }
    Ok(outcome)
}

/// The default XMPP server port on both loopback addresses.
const DEFAULT_PORT_ADDRESSES: [&str; 2] = ["127.0.0.1:5269", "[::1]:5269"];

/// Counts TCP connections to the default XMPP server port on loopback,
/// where a dial to any of the refused forms would land when no SRV record
/// or endpoint applies.
struct DefaultPortListener {
    accepted: Arc<AtomicUsize>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    bound: Vec<String>,
}

impl DefaultPortListener {
    async fn start() -> Self {
        let accepted = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        let mut bound = Vec::new();
        for address in DEFAULT_PORT_ADDRESSES {
            let Ok(listener) = TcpListener::bind(address).await else {
                continue;
            };
            bound.push(address.to_owned());
            let accepted = Arc::clone(&accepted);
            tasks.push(tokio::spawn(async move {
                while listener.accept().await.is_ok() {
                    accepted.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        Self {
            accepted,
            tasks,
            bound,
        }
    }

    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for DefaultPortListener {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Connections home-a made to anything the harness listens on.
fn harness_dials(h: &Harness, default_port: &DefaultPortListener) -> usize {
    h.relays
        .iter()
        .filter(|(link, _)| link.from == Server::Waddle(Home::A))
        .map(|(_, relay)| relay.connection_count())
        .sum::<usize>()
        + h.receiver.inbound().len()
        + h.redirect.inbound().len()
        + default_port.accepted()
}

pub async fn outbound_destinations(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "a home-a client sending a ping to peer-raw.test (control), then to each refused form; dials counted on every harness listener and on loopback port 5269",
    );
    let default_port = DefaultPortListener::start().await;
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("forms"))
        .await?;
    // Control: a good destination is dialled and seen by the counters.
    h.receiver.disconnect();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    h.receiver.clear();
    let control_id = support::id("form-control");
    session.send(&stanza::ping(&control_id, RAW_PEER)).await?;
    let delivered = h
        .receiver
        .wait_stanza(crate::scenarios::harness::ANSWER, |element| {
            element.attr("id") == Some(control_id.as_str())
        })
        .await
        .is_some();
    let control_dials = h.receiver.inbound().len();
    if !(delivered && control_dials > 0) {
        session.close().await;
        return Err(Unverifiable(format!(
            "control: home-a did not dial peer-raw.test (dials {control_dials}, delivered {delivered})"
        ))
        .into());
    }
    outcome.check(
        "control: a good destination is dialled and delivered",
        true,
        format!("{control_dials} dials to the harness receiving server"),
    );
    let unbound: Vec<&str> = DEFAULT_PORT_ADDRESSES
        .iter()
        .copied()
        .filter(|address| !default_port.bound.iter().any(|bound| bound == address))
        .collect();
    if !unbound.is_empty() {
        outcome.unverified(
            "dials to loopback port 5269 are counted",
            format!("could not listen on {unbound:?}, so a dial there would not be seen"),
        );
    }
    for form in REFUSED_FORMS {
        let before = harness_dials(h, &default_port);
        let answer = ask(&mut session, stanza::ping(&support::id("form"), form)).await;
        tokio::time::sleep(QUIET).await;
        let dials = harness_dials(h, &default_port) - before;
        outcome.check(
            ["destination ", form, " refused without dialling"].concat(),
            dials == 0
                && answer
                    .as_ref()
                    .is_ok_and(|answer| answer.attr("type") == Some("error")),
            format!(
                "{}; dials {dials}",
                answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string())
            ),
        );
    }
    session.close().await;
    Ok(outcome)
}

pub async fn stanza_addresses(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer on an authenticated peer-raw.test stream");
    support::raw_control(h).await?;
    let local = ["x@", RAW_PEER].concat();
    for form in REFUSED_FORMS {
        let refused = ["alice@", form].concat();
        for (side, from, to) in [
            ("`to`", local.as_str(), refused.as_str()),
            ("`from`", refused.as_str(), HOME_A),
        ] {
            let plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
            let attempt = initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await?;
            let detail = attempt.describe();
            let Some(mut stream) = attempt.stream else {
                outcome.check(form, false, ["could not establish: ", &detail].concat());
                continue;
            };
            h.receiver.clear();
            let ping_id = support::id("addr");
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
            let malformed = answer
                .as_ref()
                .and_then(stanza::error_condition)
                .is_some_and(|condition| condition == "jid-malformed");
            // A domain outside the stream's pair is a stream error; a refused
            // spelling of the pair's own domain is a jid-malformed error.
            outcome.check(
                ["stanza ", side, " ", &refused, " refused"].concat(),
                !answer.as_ref().is_some_and(is_result)
                    && (ending.stream_error.is_some() || malformed),
                format!(
                    "stream error {:?}, closed {}, answer {}",
                    ending.stream_error,
                    ending.closed,
                    answer
                        .map(|element| describe(&element))
                        .unwrap_or_else(|| "none".to_owned())
                ),
            );
            stream.close().await;
        }
    }
    Ok(outcome)
}

pub async fn see_other_host(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("the redirect.test receiving server answering see-other-host");
    let target = ["127.0.0.1:", &h.ports.redirect_target().to_string()].concat();
    for (name, behaviour) in [
        ("before TLS", Behaviour::RedirectBeforeTls(target.clone())),
        ("after TLS", Behaviour::RedirectAfterTls(target.clone())),
    ] {
        h.redirect.set_behaviour(behaviour);
        let mut emitted = false;
        let mut logged = 0;
        let mut detail = String::new();
        // A failed attempt puts redirect.test in backoff; restarting home-a
        // clears it, so each case gets a dial of its own.
        for attempt in 0..2 {
            if attempt > 0 {
                h.dirty.insert(Component::Waddle(Home::A));
                let profile = h.profile(Home::A, true);
                h.start_waddle(&profile).await?;
            }
            h.redirect.clear();
            h.redirect_target.reset_records();
            let redirected =
                support::LogMark::new(h, Home::A, &["s2s connection ended", "\"redirected\""]);
            let answer = support::ping_from_waddle(h, Home::A, "alice", REDIRECT).await;
            tokio::time::sleep(QUIET).await;
            emitted = h
                .redirect
                .inbound()
                .iter()
                .any(|inbound| inbound.redirected);
            logged = redirected.added(h);
            detail = format!(
                "dials to redirect.test: {}; see-other-host sent: {emitted}; home-a logged {logged} connections ended as redirected; connections to the redirect target: {}; client answer: {}",
                h.redirect.inbound().len(),
                h.redirect_target.connection_count(),
                answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string())
            );
            if emitted {
                break;
            }
        }
        let name = ["see-other-host ", name, " not followed"].concat();
        if emitted {
            outcome.check(
                name,
                h.redirect_target.connection_count() == 0 && logged > 0,
                detail,
            );
        } else {
            outcome.unverified(
                name,
                ["the harness never sent see-other-host to home-a: ", &detail].concat(),
            );
        }
    }
    h.redirect
        .set_behaviour(Behaviour::RedirectBeforeTls(target));
    Ok(outcome)
}
