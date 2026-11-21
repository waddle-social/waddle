//! Negative rows on certificates and trust. In the Waddle column the
//! harness presents the bad certificate itself: as an initiating server
//! claiming home-b.test (inbound) and as the receiving server of
//! peer-raw.test (outbound). In the Prosody and ejabberd columns (G20) the
//! stock server is restarted presenting the bad certificate, and traffic
//! in both directions must fail.
//!
//! Every refusal is tied to home-a's own record of it: its log line `s2s
//! peer certificate refused` with the verifier's code. Each row first shows
//! the same path working with the good certificate, and after a restart
//! that the stock server is serving again, so a refusal cannot be a path
//! that never worked.

use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::net::relay::Relay;
use crate::pki::{Issuer, Leaf, LeafSpec};
use crate::process;
use crate::s2s::initiator::{self, Attempt, Auth, Plan, Stage};
use crate::scenarios::harness::{Component, Harness, ANSWER};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::scenarios::transport;
use crate::topology::{
    self, Home, Server, EJABBERD, EJABBERD_MUC, HOME_A, HOME_B, PROSODY, PROSODY_MUC,
    PUBLIC_PROFILE, RAW_PEER,
};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

/// Which bad certificate a row uses.
#[derive(Debug, Clone, Copy)]
pub enum Bad {
    WrongName,
    UnknownIssuer,
    Expired,
}

impl Bad {
    /// The code home-a's verifier logs for the refusal: a name that does
    /// not match the reference identity is `identity_mismatch`; an
    /// untrusted issuer and an expired certificate fail path building,
    /// `path`.
    fn code(self) -> &'static str {
        match self {
            Self::WrongName => IDENTITY_MISMATCH,
            Self::UnknownIssuer | Self::Expired => "path",
        }
    }
}

const IDENTITY_MISMATCH: &str = "identity_mismatch";

fn bad_leaf(h: &Harness, bad: Bad, domain: &str) -> Result<Leaf> {
    let stem = |kind: &str| [kind, "-", domain].concat();
    match bad {
        Bad::WrongName => h.issue(&LeafSpec::new(
            &stem("wrong-name"),
            &["unrelated-name.test"],
        )),
        Bad::UnknownIssuer => {
            h.issue(&LeafSpec::new(&stem("unknown-issuer"), &[domain]).issued_by(Issuer::Other))
        }
        Bad::Expired => h.issue(&LeafSpec::new(&stem("expired"), &[domain]).expired()),
    }
}

async fn raw(h: &Harness, plan: &Plan) -> Result<Attempt> {
    initiator::attempt(&h.pki, h.ports.waddle_s2s(Home::A), plan).await
}

/// Refused at the TLS handshake after admission: the pair was admitted
/// before TLS, and no post-TLS features were sent. Under TLS 1.3 the
/// client learns of a refused client certificate only when it reads after
/// the handshake, so the stage may be `Authentication`.
fn refused_at_tls(attempt: &Attempt) -> bool {
    matches!(attempt.stage, Stage::Tls | Stage::Authentication)
        && attempt.features_after_tls.is_none()
}

/// An inbound attempt by the raw peer, which must be refused at TLS with
/// a certificate refusal in home-a's log (with `code` when given).
async fn inbound_refused(
    h: &Harness,
    plan: &Plan,
    code: Option<&str>,
    outcome: &mut Outcome,
    name: &str,
) -> Result<()> {
    let refusals = Refusals::mark(h, code);
    let attempt = raw(h, plan).await?;
    let (coded, any) = refusals.settled(h).await;
    outcome.check(
        name,
        refused_at_tls(&attempt) && coded > 0,
        format!("{}; {}", attempt.describe(), refusals.describe(coded, any)),
    );
    if let Some(stream) = attempt.stream {
        stream.close().await;
    }
    Ok(())
}

/// home-a's certificate refusals since a mark: those with the expected
/// code, and all of them.
struct Refusals {
    code: Option<String>,
    coded: support::LogMark,
    any: support::LogMark,
}

impl Refusals {
    fn mark(h: &Harness, code: Option<&str>) -> Self {
        let quoted = code.map(|code| ["\"", code, "\""].concat());
        let mut needles = vec!["s2s peer certificate refused"];
        if let Some(quoted) = &quoted {
            needles.push(quoted.as_str());
        }
        Self {
            code: code.map(str::to_owned),
            coded: support::LogMark::new(h, Home::A, &needles),
            any: support::LogMark::new(h, Home::A, &["s2s peer certificate refused"]),
        }
    }

    /// The counts once a refusal has been logged, or after a few seconds.
    async fn settled(&self, h: &Harness) -> (usize, usize) {
        let any = self.any.settled(h, 1).await;
        (self.coded.added(h), any)
    }

    fn describe(&self, coded: usize, any: usize) -> String {
        match &self.code {
            Some(code) => {
                format!("home-a logged {any} certificate refusals, {coded} with code {code}")
            }
            None => format!("home-a logged {any} certificate refusals"),
        }
    }
}

/// Send a ping from a home-a client to the harness receiving server and
/// report whether it arrived.
async fn ping_raw_receiver(h: &Harness) -> Result<(bool, String)> {
    let mut session = h
        .login_waddle(Home::A, "alice", &support::id("cert-out"))
        .await?;
    let ping_id = support::id("cert-out");
    session.send(&stanza::ping(&ping_id, RAW_PEER)).await?;
    let received = h
        .receiver
        .wait_stanza(ANSWER, |element| {
            element.attr("id") == Some(ping_id.as_str())
        })
        .await;
    session.close().await;
    Ok((received.is_some(), ping_id))
}

/// Close home-a's streams to the harness receiving server and give home-a
/// a moment to notice, so its next stanza for peer-raw.test dials anew.
async fn fresh_receiver_stream(h: &Harness) {
    h.receiver.disconnect();
    tokio::time::sleep(Duration::from_secs(1)).await;
    h.receiver.clear();
}

/// The outbound control of the Waddle column: with the good certificate,
/// home-a dials the harness receiving server and delivers. A destination
/// still in backoff from an earlier row is retried.
async fn outbound_control(h: &Harness, outcome: &mut Outcome) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        fresh_receiver_stream(h).await;
        let (delivered, _) = ping_raw_receiver(h).await?;
        let dialled = !h.receiver.inbound().is_empty();
        if delivered && dialled {
            outcome.check(
                "control: home-a dials and delivers to peer-raw.test with the good certificate",
                true,
                "the receiving server got the ping on a new stream",
            );
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Unverifiable(format!(
                "outbound control failed: home-a dialled {dialled}, delivered {delivered} with the good certificate"
            ))
            .into());
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Outbound with a bad certificate on the harness receiving server: home-a
/// must dial, send nothing and log the refusal with `code`.
async fn outbound_refused(
    h: &Harness,
    leaf: Leaf,
    code: &str,
    outcome: &mut Outcome,
) -> Result<()> {
    h.receiver.set_leaf(RAW_PEER, leaf);
    fresh_receiver_stream(h).await;
    let refusals = Refusals::mark(h, Some(code));
    let (delivered, _) = ping_raw_receiver(h).await?;
    let dialled = !h.receiver.inbound().is_empty();
    let (coded, any) = refusals.settled(h).await;
    let detail = format!(
        "dialled: {dialled}; stanzas delivered: {delivered}; {}",
        refusals.describe(coded, any)
    );
    if dialled {
        outcome.check(
            "outbound: refused for the receiving server's certificate, nothing sent",
            !delivered && h.receiver.stanzas().is_empty() && coded > 0,
            detail,
        );
    } else {
        outcome.unverified("outbound", ["home-a did not dial: ", &detail].concat());
    }
    h.receiver.set_leaf(RAW_PEER, h.leaf(RAW_PEER)?);
    Ok(())
}

/// Record a refusal of the stock column. The stock server's certificate
/// is the one under test only if home-a refused it with the expected
/// code; a refusal with another code means the server presented something
/// else (UNVERIFIED), and no refusal at all fails the row.
fn judge_stock(
    outcome: &mut Outcome,
    name: String,
    failed_across: bool,
    refusals: &Refusals,
    (coded, any): (usize, usize),
    detail: String,
) {
    let detail = [detail, "; ".to_owned(), refusals.describe(coded, any)].concat();
    if coded > 0 || any == 0 {
        outcome.check(name, failed_across && coded > 0, detail);
    } else {
        outcome.unverified(
            name,
            [
                "home-a refused a certificate with another code, so the peer may not have presented the one under test: ",
                &detail,
            ]
            .concat(),
        );
    }
}

/// Ask `to` for disco#info from a home-a client, retrying until a result
/// or `timeout`; one attempt when `timeout` is zero. Disco#info is
/// answered by a server domain and a MUC host alike, so the control and
/// the probe ask the same question.
async fn disco_from_home_a(h: &Harness, to: &str, timeout: Duration) -> Result<Element> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let mut session = h
            .login_waddle(Home::A, "alice", &support::id("cert"))
            .await?;
        let answer = ask(&mut session, stanza::disco_info(&support::id("cert"), to)).await;
        session.close().await;
        let late = tokio::time::Instant::now() >= deadline;
        match answer {
            Ok(element) if is_result(&element) || late => return Ok(element),
            Err(error) if late => return Err(error),
            _ => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
}

/// Every relay home-a may use towards the peer: both ejabberd nodes are
/// endpoints of the ejabberd domains.
fn outbound_relays(h: &Harness, peer: Peer) -> Vec<&Relay> {
    let targets = match peer {
        Peer::Ejabberd => vec![Server::Ejabberd(1), Server::Ejabberd(2)],
        other => vec![Harness::peer_server(other)],
    };
    targets
        .into_iter()
        .filter_map(|to| h.relay(Server::Waddle(Home::A), to).ok())
        .collect()
}

fn dials(relays: &[&Relay]) -> usize {
    relays.iter().map(|relay| relay.connection_count()).sum()
}

/// Before the swap: the same paths work with the good certificate. A
/// control that fails for any reason makes the row UNVERIFIED.
async fn stock_control(h: &Harness, peer: Peer, host: &str, outcome: &mut Outcome) -> Result<()> {
    fresh_streams(h, peer);
    let out = disco_from_home_a(h, host, Duration::from_secs(60)).await;
    match out {
        Ok(answer) if is_result(&answer) => outcome.check(
            ["control: home-a -> ", host, " with the good certificate"].concat(),
            true,
            describe(&answer),
        ),
        other => {
            return Err(Unverifiable(format!(
                "control: home-a -> {host} with the good certificate was not answered: {}",
                other
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| format!("{error:#}"))
            ))
            .into())
        }
    }
    if host == Harness::peer_domain(peer) {
        let back =
            support::ping_until_result(h, peer, false, HOME_A, Duration::from_secs(60)).await;
        match back {
            Ok(answer) if is_result(&answer) => outcome.check(
                [
                    "control: ",
                    peer.label(),
                    " -> home-a with the good certificate",
                ]
                .concat(),
                true,
                describe(&answer),
            ),
            other => {
                return Err(Unverifiable(format!(
                    "control: {} -> home-a with the good certificate was not answered: {}",
                    peer.label(),
                    other
                        .map(|answer| describe(&answer))
                        .unwrap_or_else(|error| format!("{error:#}"))
                ))
                .into())
            }
        }
    }
    Ok(())
}

/// After the swap: the stock server listens for S2S and serves its own
/// clients, so a failure across S2S is not a server that did not come up.
async fn stock_serving(h: &Harness, peer: Peer, outcome: &mut Outcome) -> Result<()> {
    let port = match peer {
        Peer::Ejabberd => h.ports.ejabberd_s2s(1),
        _ => h.ports.prosody_s2s(),
    };
    process::wait_tcp(peer.label(), port, Duration::from_secs(30))
        .await
        .map_err(|error| Unverifiable(format!("after the swap: {error:#}")))?;
    let local = support::ping_from_peer(h, peer, "alice", Harness::peer_domain(peer)).await;
    match local {
        Ok(answer) if is_result(&answer) => {
            outcome.check(
                "control: after the swap the peer listens for S2S and answers its own client",
                true,
                describe(&answer),
            );
            Ok(())
        }
        other => Err(Unverifiable(format!(
            "after the swap the peer does not serve its own client: {}",
            other
                .map(|answer| describe(&answer))
                .unwrap_or_else(|error| error.to_string())
        ))
        .into()),
    }
}

/// How long the outbound probe waits for home-a to dial the stock peer.
/// home-a backs a failing destination off, starting at one second and
/// doubling, and the peer's restart on the bad certificate can fail a dial
/// of home-a's own traffic while the peer is down; the probe waits that
/// out instead of reading a backoff as a refusal.
const DIAL_WAIT: Duration = Duration::from_secs(70);

/// What the peer-to-home-a direction is compared against, taken before the
/// stock peer restarts on the bad certificate: a peer that opens S2S as it
/// starts (ejabberd probes the presence of its accounts' contacts at
/// home-a) is refused then, and backs off before the row's own ping.
struct InboundMark {
    connections: usize,
    streams: usize,
    refusals: Refusals,
}

impl InboundMark {
    async fn take(h: &Harness, peer: Peer, code: &str) -> Result<Self> {
        let inbound = h.relay(Harness::peer_server(peer), Server::Waddle(Home::A))?;
        Ok(Self {
            connections: inbound.connection_count(),
            streams: support::established(h, Home::A, "inbound", "").await,
            refusals: Refusals::mark(h, Some(code)),
        })
    }
}

/// Traffic from the stock peer to home-a must fail if the peer's relay
/// shows that it attempted S2S since `mark`, with home-a logging the
/// certificate refusal with `code`.
async fn peer_to_home_a_fails(
    h: &Harness,
    peer: Peer,
    outcome: &mut Outcome,
    mark: InboundMark,
) -> Result<()> {
    let inbound = h.relay(Harness::peer_server(peer), Server::Waddle(Home::A))?;
    let back = support::ping_from_peer(h, peer, "alice", HOME_A).await;
    let attempted = inbound.connection_count() > mark.connections;
    let counts = mark.refusals.settled(h).await;
    let opened = support::established(h, Home::A, "inbound", "").await > mark.streams;
    let name = [peer.label(), " -> home-a refused for the certificate"].concat();
    let detail = [
        back.as_ref()
            .map(describe)
            .unwrap_or_else(|error| error.to_string()),
        format!("; inbound streams established: {opened}"),
    ]
    .concat();
    if attempted {
        let failed = !opened && back.as_ref().is_ok_and(|answer| !is_result(answer));
        judge_stock(outcome, name, failed, &mark.refusals, counts, detail);
    } else {
        outcome.unverified(name, ["the peer did not attempt S2S: ", &detail].concat());
    }
    Ok(())
}

/// Traffic from home-a to the stock peer must fail once home-a dials it,
/// with home-a logging the certificate refusal with `code`. A probe home-a
/// answers without dialling (it is backing the peer off) is repeated until
/// [`DIAL_WAIT`] passes.
async fn home_a_to_peer_fails(
    h: &Harness,
    peer: Peer,
    outcome: &mut Outcome,
    peer_host: &str,
    code: &str,
) {
    let outbound = outbound_relays(h, peer);
    let name = ["home-a -> ", peer_host, " refused for the certificate"].concat();
    let deadline = tokio::time::Instant::now() + DIAL_WAIT;
    loop {
        let before = dials(&outbound);
        let refusals = Refusals::mark(h, Some(code));
        let out = disco_from_home_a(h, peer_host, Duration::ZERO).await;
        let dialled = dials(&outbound) > before;
        let detail = out
            .as_ref()
            .map(describe)
            .unwrap_or_else(|error| error.to_string());
        if dialled {
            let counts = refusals.settled(h).await;
            let failed = out.as_ref().is_ok_and(|answer| !is_result(answer));
            judge_stock(outcome, name, failed, &refusals, counts, detail);
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            outcome.unverified(
                name,
                [
                    "home-a did not dial the peer within ",
                    &DIAL_WAIT.as_secs().to_string(),
                    " s: ",
                    &detail,
                ]
                .concat(),
            );
            return;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Restart the stock peer presenting `leaf` for `host`.
async fn swap_stock_certificate(h: &mut Harness, peer: Peer, host: &str, leaf: Leaf) -> Result<()> {
    match peer {
        Peer::Prosody => {
            let mut variant = h.prosody_variant();
            variant.leaves.insert(host.to_owned(), leaf);
            h.dirty.insert(Component::Prosody);
            h.prosody.restart(&variant).await
        }
        Peer::Ejabberd => {
            let mut variant = h.ejabberd_variant();
            variant.leaves.insert(host.to_owned(), leaf);
            h.dirty.insert(Component::Ejabberd);
            h.ejabberd.restart(&variant).await
        }
        Peer::Waddle => {
            Err(Unverifiable("no stock certificate to swap on Waddle".to_owned()).into())
        }
    }
}

/// Cut both relays of the pair (both ejabberd nodes' relays from home-a),
/// so the next traffic opens new streams.
fn fresh_streams(h: &Harness, peer: Peer) {
    let mut relays = outbound_relays(h, peer);
    if let Ok(relay) = h.relay(Harness::peer_server(peer), Server::Waddle(Home::A)) {
        relays.push(relay);
    }
    for relay in relays {
        relay.cut();
        relay.restore();
    }
}

/// The stock-peer column: control with the good certificate, swap, check
/// the peer is serving, then traffic must fail with `code` in each
/// direction in which the peer's relays show an S2S attempt: from the peer
/// (when `host` is its account domain) since before the swap, and from
/// home-a once its backoff lets it dial. After the row the harness restarts
/// the peer on its standard certificate and waits until traffic flows both
/// ways again ([`Harness::restore_standard`]).
async fn stock_bad_certificate(
    h: &mut Harness,
    peer: Peer,
    host: &str,
    leaf: Leaf,
    code: &str,
    outcome: &mut Outcome,
) -> Result<()> {
    stock_control(h, peer, host, outcome).await?;
    let inbound = if host == Harness::peer_domain(peer) {
        Some(InboundMark::take(h, peer, code).await?)
    } else {
        None
    };
    swap_stock_certificate(h, peer, host, leaf).await?;
    stock_serving(h, peer, outcome).await?;
    fresh_streams(h, peer);
    if let Some(mark) = inbound {
        peer_to_home_a_fails(h, peer, outcome, mark).await?;
    }
    home_a_to_peer_fails(h, peer, outcome, host, code).await;
    Ok(())
}

pub async fn bad_certificate(h: &mut Harness, peer: Peer, bad: Bad) -> Result<Outcome> {
    match peer {
        Peer::Waddle => {
            let mut outcome = Outcome::new(
                "harness raw peer claiming home-b.test (inbound) and the peer-raw.test receiving server (outbound)",
            );
            support::raw_control(h).await?;
            let plan = Plan::external(HOME_B, HOME_A, &bad_leaf(h, bad, HOME_B)?);
            inbound_refused(
                h,
                &plan,
                Some(bad.code()),
                &mut outcome,
                "inbound refused at TLS for the certificate",
            )
            .await?;
            outbound_control(h, &mut outcome).await?;
            let leaf = bad_leaf(h, bad, RAW_PEER)?;
            outbound_refused(h, leaf, bad.code(), &mut outcome).await?;
            Ok(outcome)
        }
        Peer::Prosody | Peer::Ejabberd => {
            let host = Harness::peer_domain(peer);
            let mut outcome = Outcome::new(
                [
                    "stock ",
                    peer.label(),
                    " presenting the bad certificate for ",
                    host,
                ]
                .concat(),
            );
            let leaf = bad_leaf(h, bad, host)?;
            stock_bad_certificate(h, peer, host, leaf, bad.code(), &mut outcome).await?;
            Ok(outcome)
        }
    }
}

pub async fn parent_domain(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    match peer {
        Peer::Waddle => {
            let mut outcome = Outcome::new(
                "harness raw peer claiming muc.home-b.test with home-b.test's certificate",
            );
            support::raw_control(h).await?;
            let muc = topology::muc_domain(HOME_B);
            let plan = Plan::external(&muc, HOME_A, &h.leaf(HOME_B)?);
            inbound_refused(
                h,
                &plan,
                Some(IDENTITY_MISMATCH),
                &mut outcome,
                "inbound refused at TLS for the certificate",
            )
            .await?;
            Ok(outcome)
        }
        Peer::Prosody | Peer::Ejabberd => {
            let (parent, component) = match peer {
                Peer::Prosody => (PROSODY, PROSODY_MUC),
                _ => (EJABBERD, EJABBERD_MUC),
            };
            let mut outcome = Outcome::new(
                [
                    "stock ",
                    peer.label(),
                    " presenting ",
                    parent,
                    "'s certificate for ",
                    component,
                ]
                .concat(),
            );
            let parent_leaf = h.issue(&LeafSpec::new(
                &["parent-for-", component].concat(),
                &[parent],
            ))?;
            stock_bad_certificate(
                h,
                peer,
                component,
                parent_leaf,
                IDENTITY_MISMATCH,
                &mut outcome,
            )
            .await?;
            Ok(outcome)
        }
    }
}

pub async fn server_auth_only(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "home-b restarted on serverAuth-only leaves, and the harness raw peer on a serverAuth-only leaf",
    );
    // Waddle with Waddle: home-b presents serverAuth-only certificates in
    // both roles and home-a must accept them.
    let mut material = h
        .material
        .get(&Home::B)
        .cloned()
        .context("home-b material")?;
    for domain in topology::waddle_domains(HOME_B) {
        let leaf = transport::server_only(h, &domain)?;
        material.credentials.insert(
            domain,
            (leaf.certificate_path.clone(), leaf.key_path.clone()),
        );
    }
    let profile = h.profile(Home::B, true);
    h.dirty.insert(Component::Waddle(Home::B));
    h.start_waddle_with(&profile, &material).await?;
    fresh_streams(h, Peer::Waddle);
    let out =
        support::ping_until_result(h, Peer::Waddle, true, HOME_B, Duration::from_secs(60)).await?;
    outcome.check(
        "home-a -> home-b (home-b presents serverAuth-only as receiving server)",
        is_result(&out),
        describe(&out),
    );
    let back =
        support::ping_until_result(h, Peer::Waddle, false, HOME_A, Duration::from_secs(60)).await?;
    outcome.check(
        "home-b -> home-a (home-b presents serverAuth-only as initiating server)",
        is_result(&back),
        describe(&back),
    );
    // The harness peer in the initiating role.
    let plan = Plan::external(RAW_PEER, HOME_A, &transport::server_only(h, RAW_PEER)?);
    let attempt = raw(h, &plan).await?;
    outcome.check(
        "raw peer with a serverAuth-only leaf authenticates inbound",
        attempt.stage == Stage::Established,
        attempt.describe(),
    );
    if let Some(stream) = attempt.stream {
        stream.close().await;
    }
    Ok(outcome)
}

pub async fn private_ca_for_public_profile(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("harness raw peer claiming public-profile.test with a .test CA leaf");
    support::raw_control(h).await?;
    let plan = Plan::external(PUBLIC_PROFILE, HOME_A, &h.leaf(PUBLIC_PROFILE)?);
    inbound_refused(
        h,
        &plan,
        None,
        &mut outcome,
        "refused at TLS for the certificate",
    )
    .await?;
    Ok(outcome)
}

pub async fn no_pre_tls_from(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer sending a stream header without `from`");
    let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    plan.open.from = None;
    let attempt = raw(h, &plan).await?;
    outcome.check(
        "refused before TLS with bad-format",
        attempt.stage == Stage::BeforeTls && attempt.stream_error.as_deref() == Some("bad-format"),
        attempt.describe(),
    );
    Ok(outcome)
}

pub async fn inbound_direct_tls(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("harness raw peer starting TLS at once on the S2S port");
    support::raw_control(h).await?;
    let mut plan = Plan::external(RAW_PEER, HOME_A, &h.leaf(RAW_PEER)?);
    plan.direct_tls = true;
    let attempt = raw(h, &plan).await?;
    outcome.check(
        "direct TLS refused",
        attempt.stage == Stage::Tls,
        attempt.describe(),
    );
    Ok(outcome)
}

/// Negotiate STARTTLS in clear, then hand the socket to `openssl
/// s_client` restricted to one TLS version through a loopback bridge. TLS
/// 1.2 through the same bridge is the control: without it a refusal of
/// 1.1 or 1.0 could be the bridge's failure.
pub async fn tls_version_outside_profile(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "OpenSSL s_client offering only TLS 1.2 (control), then only TLS 1.1, then only TLS 1.0, after the harness's STARTTLS",
    );
    let mut control_negotiated = false;
    for (name, flag, control) in [
        ("TLS 1.2", "-tls1_2", true),
        ("TLS 1.1", "-tls1_1", false),
        ("TLS 1.0", "-tls1", false),
    ] {
        let mut socket = initiator::connect(h.ports.waddle_s2s(Home::A)).await?;
        socket
            .write_all(&StreamOpen::server(Some(RAW_PEER), HOME_A).encode()?)
            .await?;
        let mut framer = Framer::new();
        let mut chunk = vec![0u8; 8192];
        let mut saw_features = false;
        let mut proceed = false;
        let deadline = tokio::time::Instant::now() + ANSWER;
        while !proceed {
            while let Ok(Some(item)) = framer.next_item() {
                if let Item::Element(element) = item {
                    if element.name() == "features" && !saw_features {
                        saw_features = true;
                        socket
                            .write_all(&header::element_bytes(
                                &Element::builder("starttls", ns::TLS).build(),
                            ))
                            .await?;
                    }
                    proceed |= element.name() == "proceed";
                }
            }
            if proceed {
                break;
            }
            let read = tokio::time::timeout_at(deadline, socket.read(&mut chunk))
                .await
                .context("STARTTLS negotiation timed out")??;
            anyhow::ensure!(read > 0, "home-a closed during STARTTLS");
            framer.push(chunk.get(..read).unwrap_or_default());
        }
        let bridge = TcpListener::bind(("127.0.0.1", h.ports.tls_bridge())).await?;
        let mut openssl = Command::new(&h.bins.openssl);
        openssl
            .arg("s_client")
            .arg("-connect")
            .arg(["127.0.0.1:", &h.ports.tls_bridge().to_string()].concat())
            .arg("-servername")
            .arg(HOME_A)
            .arg(flag)
            .arg("-cipher")
            .arg("DEFAULT@SECLEVEL=0")
            .arg("-brief")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let child = openssl.output();
        let splice = async {
            let (mut client, _) = bridge.accept().await?;
            let mut hello = vec![0u8; 4096];
            let read =
                tokio::time::timeout(Duration::from_secs(10), client.read(&mut hello)).await??;
            socket
                .write_all(hello.get(..read).unwrap_or_default())
                .await?;
            let forwarded = tokio::time::timeout(
                Duration::from_secs(10),
                tokio::io::copy_bidirectional(&mut client, &mut socket),
            )
            .await;
            anyhow::Ok((read, forwarded.is_ok()))
        };
        let (output, spliced) = tokio::join!(child, splice);
        let output = output?;
        let text = [
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ]
        .concat();
        let summary = text.lines().take(6).collect::<Vec<_>>().join(" | ");
        let sent_hello = matches!(spliced, Ok((read, _)) if read > 0);
        if control {
            control_negotiated = sent_hello && text.contains("Protocol version: TLSv1.2");
            outcome.check(
                "control: TLS 1.2 through the same bridge is negotiated",
                control_negotiated,
                summary,
            );
        } else if !control_negotiated {
            outcome.unverified(
                [name, " refused"].concat(),
                "the TLS 1.2 control through the bridge did not negotiate",
            );
        } else if !sent_hello {
            outcome.unverified(
                name,
                format!(
                    "OpenSSL sent no ClientHello for {name}; it may not support the version: {}",
                    text.trim()
                ),
            );
        } else {
            outcome.check(
                [name, " refused"].concat(),
                !output.status.success()
                    && !text.contains("Protocol version: TLSv1.1")
                    && !text.contains("Protocol version: TLSv1\n"),
                summary,
            );
        }
    }
    Ok(outcome)
}

/// An attempt as a raw peer, used by other modules.
pub async fn attempt_as(
    h: &Harness,
    from: &str,
    leaf: Option<Leaf>,
    auth: Auth,
) -> Result<Attempt> {
    let mut plan = Plan::external(from, HOME_A, &h.leaf(RAW_PEER)?);
    plan.leaf = leaf;
    plan.auth = auth;
    raw(h, &plan).await
}
