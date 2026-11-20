//! Negative rows on admission (D1): denylist, modes, list scope and
//! capacity. Each restarts home-a on a changed configuration snapshot
//! (snapshot, not reload: a change takes effect at restart) and leaves it
//! marked for the runner to restore.

use std::time::Duration;

use anyhow::Result;

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::pki::LeafSpec;
use crate::process;
use crate::s2s::initiator::{self, Attempt, Plan, Stage};
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, describe, is_result};
use crate::services::waddle_config::{Mode, Profile};
use crate::topology::{
    self, Home, Server, DENIED, DENIED_ALABEL, DENIED_ULABEL, FLOOD_PEERS, HOME_A, RAW_PEER,
};
use crate::xml::{ns, stanza};

async fn restart_a(h: &mut Harness, profile: &Profile) -> Result<()> {
    h.dirty.insert(Component::Waddle(Home::A));
    h.start_waddle(profile).await
}

/// A raw attempt claiming `from`, with the leaf of `leaf_domain`.
async fn claim(h: &Harness, from: &str, leaf_domain: &str) -> Result<Attempt> {
    let mut plan = Plan::external(from, HOME_A, &h.leaf(leaf_domain)?);
    plan.open.from = Some(from.to_owned());
    initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), &plan).await
}

/// Refused before TLS with `policy-violation`, as every pre-TLS refusal
/// of a pair is.
fn refused_at_open(attempt: &Attempt) -> bool {
    attempt.stage == Stage::BeforeTls
        && attempt.stream_error.as_deref() == Some(support::PRE_TLS_REFUSAL)
}

/// Establish a raw stream and prove it carries traffic.
async fn working_stream(
    h: &Harness,
    outcome: &mut Outcome,
) -> Result<crate::s2s::initiator::RawStream> {
    let attempt = claim(h, RAW_PEER, RAW_PEER).await?;
    let mut stream = attempt
        .stream
        .ok_or_else(|| anyhow::anyhow!("raw peer could not establish"))?;
    h.receiver.clear();
    let ping_id = support::id("admit");
    stream
        .send(&stanza::iq(
            ns::SERVER,
            "get",
            &ping_id,
            Some(&["x@", RAW_PEER, "/r"].concat()),
            Some(HOME_A),
            Some(stanza::ping_payload()),
        ))
        .await?;
    let answer = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await;
    outcome.check(
        "control: the stream carries traffic before the change",
        answer.as_ref().is_some_and(is_result),
        answer
            .map(|element| describe(&element))
            .unwrap_or_else(|| "no answer".to_owned()),
    );
    Ok(stream)
}

/// After a snapshot change: the old stream is gone, a new one is refused
/// and queued work towards the domain is refused without dialling it.
async fn no_longer_admitted(
    h: &Harness,
    mut old: crate::s2s::initiator::RawStream,
    outcome: &mut Outcome,
) -> Result<()> {
    let ending = old.watch(QUIET).await;
    outcome.check(
        "the open stream ended",
        ending.closed,
        format!("stream error {:?}", ending.stream_error),
    );
    let attempt = claim(h, RAW_PEER, RAW_PEER).await?;
    outcome.check(
        "a new stream is refused at open",
        refused_at_open(&attempt),
        attempt.describe(),
    );
    let before = h.receiver.inbound().len();
    let answer = support::ping_from_waddle(h, Home::A, "alice", RAW_PEER).await;
    tokio::time::sleep(QUIET).await;
    let dialled = h.receiver.inbound().len() - before;
    outcome.check(
        "queued work towards the domain is refused without dialling",
        dialled == 0 && answer.as_ref().map_or(true, |answer| !is_result(answer)),
        format!(
            "dialled {dialled}; client answer {}",
            answer
                .map(|answer| describe(&answer))
                .unwrap_or_else(|error| error.to_string())
        ),
    );
    Ok(())
}

pub async fn denylisted_with_open_stream(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer; home-a restarted with peer-raw.test denylisted");
    let stream = working_stream(h, &mut outcome).await?;
    let profile = h.profile(Home::A, true).denying(RAW_PEER);
    restart_a(h, &profile).await?;
    no_longer_admitted(h, stream, &mut outcome).await?;
    Ok(outcome)
}

pub async fn removal_and_closed(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "harness raw peer; home-a restarted without peer-raw.test, then in closed mode",
    );
    let stream = working_stream(h, &mut outcome).await?;
    let profile = h.profile(Home::A, true).without(RAW_PEER);
    restart_a(h, &profile).await?;
    no_longer_admitted(h, stream, &mut outcome).await?;
    let standard = h.profile(Home::A, true);
    restart_a(h, &standard).await?;
    let stream = working_stream(h, &mut outcome).await?;
    let closed = h.profile(Home::A, true).with_mode(Mode::Closed);
    restart_a(h, &closed).await?;
    no_longer_admitted(h, stream, &mut outcome).await?;
    let mut no_endpoints = standard.clone();
    no_endpoints.endpoints.clear();
    let mut empty_allowlist = standard;
    empty_allowlist.allow.clear();
    for (configuration, profile) in [
        ("closed", None),
        ("allowlisted without endpoints", Some(no_endpoints)),
        ("empty allowlist", Some(empty_allowlist)),
    ] {
        if let Some(profile) = profile {
            restart_a(h, &profile).await?;
        }
        let mut local = h
            .login_waddle(Home::A, "alice", &support::id("admission-disco"))
            .await?;
        let answer = support::ask(
            &mut local,
            stanza::disco_info(&support::id("admission-info"), HOME_A),
        )
        .await?;
        let advertises_federation =
            answer
                .children()
                .flat_map(|query| query.children())
                .any(|feature| {
                    feature.name() == "feature" && feature.attr("var") == Some("urn:waddle:s2s:0")
                });
        outcome.check(
            format!("{configuration} home server omits federation from its client discovery"),
            is_result(&answer) && !advertises_federation,
            describe(&answer),
        );
        local.close().await;
    }
    Ok(outcome)
}

pub async fn denylisted_subdomain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer; home-a with deny.test denylisted");
    let profile = h.profile(Home::A, true).with_default_deny();
    restart_a(h, &profile).await?;
    support::raw_control(h).await?;
    for from in [DENIED, "sub.deny.test", "a.b.deny.test"] {
        let attempt = claim(h, from, DENIED).await?;
        outcome.check(
            [from, " refused at open"].concat(),
            refused_at_open(&attempt),
            attempt.describe(),
        );
    }
    Ok(outcome)
}

pub async fn denylisted_spellings(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer; home-a denylisting deny.test and xn--bcher-kva.test");
    let mut profile = h.profile(Home::A, true).with_default_deny();
    profile.deny.push(DENIED_ALABEL.to_owned());
    restart_a(h, &profile).await?;
    support::raw_control(h).await?;
    for from in [
        "deny.test.",
        "DENY.test",
        "Deny.Test",
        DENIED_ULABEL,
        "B\u{fc}cher.test",
        "XN--BCHER-KVA.test",
    ] {
        let attempt = claim(h, from, DENIED).await?;
        outcome.check(
            [from, " refused at open"].concat(),
            refused_at_open(&attempt),
            attempt.describe(),
        );
    }
    Ok(outcome)
}

pub async fn denylisted_each_mode(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer; home-a restarted in each mode with deny.test denylisted");
    for (name, mode) in [
        ("allowlist", Mode::Allowlist),
        ("open", Mode::Open),
        ("closed", Mode::Closed),
    ] {
        let profile = h.profile(Home::A, true).with_default_deny().with_mode(mode);
        restart_a(h, &profile).await?;
        if mode != Mode::Closed {
            support::raw_control(h).await?;
        }
        let attempt = claim(h, DENIED, DENIED).await?;
        outcome.check(
            [name, " mode: deny.test refused at open"].concat(),
            refused_at_open(&attempt),
            attempt.describe(),
        );
    }
    Ok(outcome)
}

pub async fn listed_fails_to_load(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("home-a started on configurations that must not load");
    let base = || h.profile(Home::A, true).with_default_deny();
    let mut allowed = base();
    allowed.allow.push(DENIED.to_owned());
    let mut allowed_sub = base();
    allowed_sub.allow.push("sub.deny.test".to_owned());
    let mut dialback = base();
    dialback.dialback.push("sub.deny.test".to_owned());
    let mut trusted = base();
    trusted.trusted_homes.push(DENIED.to_owned());
    let cases = [
        ("a denylisted domain allowed", allowed),
        ("a subdomain of a denylisted domain allowed", allowed_sub),
        (
            "a subdomain of a denylisted domain dialback-listed",
            dialback,
        ),
        ("a denylisted domain as trusted home", trusted),
    ];
    h.dirty.insert(Component::Waddle(Home::A));
    let ports = h.ports;
    h.waddle(Home::A)?.stop().await?;
    for (name, profile) in cases {
        let config = h.write_profile(&profile)?;
        let result = h
            .waddle(Home::A)?
            .start_expecting_failure(ports, config)
            .await;
        // ConfigError::Denied: "entry N of `<section>` is covered by the denylist".
        let refused = result
            .as_ref()
            .is_ok_and(|tail| tail.contains("covered by the denylist"));
        outcome.check(
            [name, ": refused to load"].concat(),
            refused,
            match result {
                Ok(tail) => tail
                    .lines()
                    .rev()
                    .find(|line| line.to_ascii_lowercase().contains("error"))
                    .unwrap_or_default()
                    .to_owned(),
                Err(error) => error.to_string(),
            },
        );
    }
    Ok(outcome)
}

pub async fn listed_subdomain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer claiming subdomains of admitted domains");
    support::raw_control(h).await?;
    for from in [
        "sub.peer-raw.test",
        "sub.home-b.test",
        "sub.insecure.prosody.test",
    ] {
        let attempt = claim(h, from, RAW_PEER).await?;
        outcome.check(
            [from, " refused at open"].concat(),
            refused_at_open(&attempt),
            attempt.describe(),
        );
    }
    Ok(outcome)
}

pub async fn open_mode_exhaustion(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "harness raw peers flood-1 to flood-4.test, admitted by open mode alone (outside the reserve); home-a in open mode with max_peers 2, eviction after 5 s without traffic and a 60 s first outbound backoff; every relay into home-a cut so no other peer holds a slot",
    );
    let profile = h
        .profile(Home::A, true)
        .with_mode(Mode::Open)
        .on_test_ca(&FLOOD_PEERS)
        .with_limit("max_peers", 2)
        .with_limit("eviction_idle_ms", 5_000)
        .with_limit("backoff_initial_ms", 60_000);
    restart_a(h, &profile).await?;
    // The runner restores every relay after the row.
    for link in topology::RELAYS {
        if link.to == Server::Waddle(Home::A) {
            if let Some(relay) = h.relays.get(&link) {
                relay.cut();
            }
        }
    }
    let [first, second, third, fourth, ..] = FLOOD_PEERS;

    // The global peer cap: two peers are admitted, the third is refused
    // for capacity.
    let established = support::LogMark::new(h, Home::A, &["s2s stream established", "inbound"]);
    let mut admitted = Vec::new();
    for domain in [first, second] {
        let attempt = claim(h, domain, domain).await?;
        let detail = attempt.describe();
        match attempt.stream {
            Some(stream) => admitted.push((domain, stream)),
            None => {
                return Err(Unverifiable(format!(
                    "control: {domain} was not admitted below the cap: {detail}"
                ))
                .into())
            }
        }
    }
    let streams = established.settled(h, 2).await;
    outcome.check(
        "control: exactly two peers admitted below the cap",
        admitted.len() == 2 && streams == 2,
        format!("home-a logged {streams} inbound streams established"),
    );
    let capacity = support::LogMark::new(h, Home::A, &["s2s connection ended", "\"capacity\""]);
    let refused = claim(h, third, third).await?;
    let refusals = capacity.settled(h, 1).await;
    outcome.check(
        "a third peer is refused for capacity",
        refused.stream.is_none()
            && refused.stream_error.as_deref() == Some("resource-constraint")
            && refusals > 0,
        format!(
            "{}; home-a logged {refusals} connections ended for capacity",
            refused.describe()
        ),
    );
    for (domain, stream) in &mut admitted {
        let ending = stream.watch(Duration::from_millis(500)).await;
        outcome.check(
            [domain, "'s admitted stream survives the refusal"].concat(),
            !ending.closed,
            format!("stream error {:?}", ending.stream_error),
        );
    }

    // Eviction: once both admitted streams have carried nothing for the
    // eviction idle time, a new peer evicts exactly one of them.
    tokio::time::sleep(Duration::from_secs(7)).await;
    let evictions = support::LogMark::new(h, Home::A, &["s2s connection ended", "\"evicted\""]);
    let newcomer = claim(h, fourth, fourth).await?;
    let mut ended = Vec::new();
    for (domain, stream) in &mut admitted {
        let ending = stream.watch(Duration::from_secs(2)).await;
        if ending.closed {
            ended.push(format!("{domain}: {:?}", ending.stream_error));
        }
    }
    let evicted = evictions.settled(h, 1).await;
    outcome.check(
        "after the eviction idle time a new peer evicts exactly one idle peer",
        newcomer.stage == Stage::Established
            && ended.len() == 1
            && ended
                .iter()
                .all(|ending| ending.contains("resource-constraint"))
            && evicted == 1,
        format!(
            "newcomer {}; ended {ended:?}; home-a logged {evicted} connections ended as evicted",
            newcomer.describe()
        ),
    );
    if let Some(stream) = newcomer.stream {
        stream.close().await;
    }
    for (_, stream) in admitted {
        stream.close().await;
    }
    tokio::time::sleep(support::PAIR_RELEASE).await;

    let local = support::ping_from_waddle(h, Home::A, "alice", HOME_A).await;
    outcome.check(
        "home-a keeps serving local traffic",
        local.as_ref().is_ok_and(is_result),
        local
            .map(|answer| describe(&answer))
            .unwrap_or_else(|error| error.to_string()),
    );

    // Backoff: a destination whose attempt failed is not dialled again
    // within the first backoff (60 s); the window measured is a few
    // seconds right after the failure.
    h.receiver.set_leaf(
        RAW_PEER,
        h.issue(&LeafSpec::new(
            "backoff-wrong-name",
            &["unrelated-name.test"],
        ))?,
    );
    h.receiver.disconnect();
    tokio::time::sleep(Duration::from_secs(1)).await;
    h.receiver.clear();
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("backoff"))
        .await?;
    let refusals = support::LogMark::new(h, Home::A, &["s2s peer certificate refused"]);
    session
        .send(&stanza::ping(&support::id("backoff"), RAW_PEER))
        .await?;
    let dialled = process::eventually(Duration::from_secs(10), || async {
        !h.receiver.inbound().is_empty()
    })
    .await;
    let failed = refusals.settled(h, 1).await;
    h.receiver.clear();
    let held_id = support::id("backoff");
    session.send(&stanza::ping(&held_id, RAW_PEER)).await?;
    let held = session
        .wait_for(QUIET, |element| {
            element.attr("id") == Some(held_id.as_str())
        })
        .await
        .ok();
    let redials = h.receiver.inbound().len();
    session.close().await;
    let detail = format!(
        "first attempt dialled: {dialled}, certificate refusals logged {failed}; within the backoff: {redials} dials, answer {}",
        held.as_ref()
            .map(describe)
            .unwrap_or_else(|| "none yet".to_owned())
    );
    if dialled && failed > 0 {
        outcome.check(
            "a failed destination is not dialled again within the backoff",
            redials == 0 && !held.as_ref().is_some_and(is_result),
            detail,
        );
    } else {
        outcome.unverified(
            "a failed destination is not dialled again within the backoff",
            [
                "control: the first attempt did not dial and fail: ",
                &detail,
            ]
            .concat(),
        );
    }
    h.receiver.set_leaf(RAW_PEER, h.leaf(RAW_PEER)?);
    Ok(outcome)
}
