//! Negative rows on admission (D1): denylist, modes, list scope and
//! capacity. Each restarts home-a on a changed configuration snapshot
//! (snapshot, not reload: a change takes effect at restart) and leaves it
//! marked for the runner to restore.

use std::time::Duration;

use anyhow::Result;

use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::s2s::initiator::{self, Attempt, Plan, Stage};
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, describe, is_result};
use crate::services::waddle_config::{Mode, Profile};
use crate::topology::{Home, DENIED, DENIED_ALABEL, DENIED_ULABEL, FLOOD_PEERS, HOME_A, RAW_PEER};
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

fn refused_at_open(attempt: &Attempt) -> bool {
    attempt.stage == Stage::BeforeTls
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
    Ok(outcome)
}

pub async fn denylisted_subdomain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer; home-a with deny.test denylisted");
    let profile = h.profile(Home::A, true).with_default_deny();
    restart_a(h, &profile).await?;
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
    dialback.allow.push("sub.deny.test".to_owned());
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
        outcome.check(
            [name, ": refused to load"].concat(),
            result.is_ok(),
            match result {
                Ok(tail) => tail.lines().last().unwrap_or_default().to_owned(),
                Err(error) => error.to_string(),
            },
        );
    }
    Ok(outcome)
}

pub async fn listed_subdomain(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer claiming subdomains of admitted domains");
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
    let mut outcome =
        Outcome::new("harness raw peers from six domains; home-a in open mode with max_peers 2");
    let profile = h
        .profile(Home::A, true)
        .with_mode(Mode::Open)
        .admitting(&FLOOD_PEERS)
        .with_limit("max_peers", 2);
    restart_a(h, &profile).await?;
    let mut streams = Vec::new();
    let mut refused = Vec::new();
    for domain in FLOOD_PEERS {
        let attempt = claim(h, domain, domain).await?;
        match attempt.stream {
            Some(stream) => streams.push((domain, stream)),
            None => refused.push([domain, ": ", &attempt.describe()].concat()),
        }
    }
    outcome.check(
        "peers beyond the cap are refused",
        streams.len() <= 2 && !refused.is_empty(),
        format!("{} established, refused: {refused:?}", streams.len()),
    );
    let mut alive = 0;
    for (_, stream) in &mut streams {
        let ending = stream.watch(Duration::from_secs(1)).await;
        alive += usize::from(!ending.closed);
    }
    outcome.note(format!(
        "{alive} admitted streams still open after the refusals"
    ));
    let local = support::ping_from_waddle(h, Home::A, "alice", HOME_A).await;
    outcome.check(
        "home-a keeps serving local traffic",
        local.as_ref().is_ok_and(is_result),
        local
            .map(|answer| describe(&answer))
            .unwrap_or_else(|error| error.to_string()),
    );
    for (_, stream) in streams {
        stream.close().await;
    }
    Ok(outcome)
}
