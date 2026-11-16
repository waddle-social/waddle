//! Negative rows on addressing: the domain forms the identity profile
//! refuses, in stream headers, as outbound destinations and in stanza
//! addresses, and `see-other-host`, which is never followed.

use anyhow::Result;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::s2s::initiator::{self, Plan, Stage};
use crate::s2s::receiver::Behaviour;
use crate::scenarios::harness::{Component, Harness, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::topology::{Home, HOME_A, RAW_PEER, REDIRECT};
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

pub async fn outbound_destinations(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("a home-a client sending a ping to each refused form");
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("forms"))
        .await?;
    for form in REFUSED_FORMS {
        let answer = ask(&mut session, stanza::ping(&support::id("form"), form)).await;
        outcome.check(
            ["destination ", form, " refused"].concat(),
            answer
                .as_ref()
                .is_ok_and(|answer| answer.attr("type") == Some("error")),
            answer
                .map(|answer| describe(&answer))
                .unwrap_or_else(|error| error.to_string()),
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
        let mut dialled = false;
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
            let answer = support::ping_from_waddle(h, Home::A, "alice", REDIRECT).await;
            tokio::time::sleep(QUIET).await;
            dialled = !h.redirect.inbound().is_empty();
            detail = format!(
                "dials to redirect.test: {}; connections to the redirect target: {}; client answer: {}",
                h.redirect.inbound().len(),
                h.redirect_target.connection_count(),
                answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string())
            );
            if dialled {
                break;
            }
        }
        let name = ["see-other-host ", name, " not followed"].concat();
        if dialled {
            outcome.check(name, h.redirect_target.connection_count() == 0, detail);
        } else {
            outcome.unverified(
                name,
                ["home-a never dialled redirect.test: ", &detail].concat(),
            );
        }
    }
    h.redirect
        .set_behaviour(Behaviour::RedirectBeforeTls(target));
    Ok(outcome)
}
