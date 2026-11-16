//! Negative rows on certificates and trust. In the Waddle column the
//! harness presents the bad certificate itself: as an initiating server
//! claiming home-b.test (inbound) and as the receiving server of
//! peer-raw.test (outbound). In the Prosody and ejabberd columns (G20) the
//! stock server is restarted presenting the bad certificate, and traffic
//! in both directions must fail.

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::pki::{Issuer, Leaf, LeafSpec};
use crate::s2s::initiator::{self, Attempt, Auth, Plan, Stage};
use crate::scenarios::harness::{Component, Harness, ANSWER};
use crate::scenarios::support::{self, describe, is_result};
use crate::scenarios::transport;
use crate::topology::{
    self, Home, EJABBERD, EJABBERD_MUC, HOME_A, HOME_B, PROSODY, PROSODY_MUC, PUBLIC_PROFILE,
    RAW_PEER,
};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::ns;

/// Which bad certificate a row uses.
#[derive(Debug, Clone, Copy)]
pub enum Bad {
    WrongName,
    UnknownIssuer,
    Expired,
}

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

fn refused(attempt: &Attempt) -> bool {
    attempt.stage != Stage::Established
}

/// Traffic in both directions between home-a and the peer must fail.
async fn both_directions_fail(
    h: &Harness,
    peer: Peer,
    outcome: &mut Outcome,
    peer_host: &str,
) -> Result<()> {
    let out = support::ping_from_waddle(h, Home::A, "alice", peer_host).await;
    outcome.check(
        ["home-a -> ", peer_host, " refused"].concat(),
        out.as_ref().map_or(true, |answer| !is_result(answer)),
        out.map(|answer| describe(&answer))
            .unwrap_or_else(|error| error.to_string()),
    );
    if peer_host == Harness::peer_domain(peer) {
        let back = support::ping_from_peer(h, peer, "alice", HOME_A).await;
        outcome.check(
            [peer.label(), " -> home-a refused"].concat(),
            back.as_ref().map_or(true, |answer| !is_result(answer)),
            back.map(|answer| describe(&answer))
                .unwrap_or_else(|error| error.to_string()),
        );
    }
    Ok(())
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

/// Cut both relays of the pair, so the next traffic opens new streams.
fn fresh_streams(h: &Harness, peer: Peer) {
    for (from, to) in [
        (
            topology::Server::Waddle(Home::A),
            Harness::peer_server(peer),
        ),
        (
            Harness::peer_server(peer),
            topology::Server::Waddle(Home::A),
        ),
    ] {
        if let Ok(relay) = h.relay(from, to) {
            relay.cut();
            relay.restore();
        }
    }
}

pub async fn bad_certificate(h: &mut Harness, peer: Peer, bad: Bad) -> Result<Outcome> {
    match peer {
        Peer::Waddle => {
            let mut outcome = Outcome::new(
                "harness raw peer claiming home-b.test (inbound) and the peer-raw.test receiving server (outbound)",
            );
            let plan = Plan::external(HOME_B, HOME_A, &bad_leaf(h, bad, HOME_B)?);
            let attempt = raw(h, &plan).await?;
            outcome.check("inbound refused", refused(&attempt), attempt.describe());
            h.receiver.set_leaf(RAW_PEER, bad_leaf(h, bad, RAW_PEER)?);
            h.receiver.clear();
            let answer = support::ping_from_waddle(h, Home::A, "alice", RAW_PEER).await;
            let delivered = !h.receiver.stanzas().is_empty();
            outcome.check(
                "outbound: home-a sends nothing to a receiving server with the bad certificate",
                !delivered,
                format!(
                    "stanzas delivered: {delivered}; client answer: {}",
                    answer
                        .map(|answer| describe(&answer))
                        .unwrap_or_else(|error| error.to_string())
                ),
            );
            h.receiver.set_leaf(RAW_PEER, h.leaf(RAW_PEER)?);
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
            swap_stock_certificate(h, peer, host, bad_leaf(h, bad, host)?).await?;
            fresh_streams(h, peer);
            both_directions_fail(h, peer, &mut outcome, host).await?;
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
            let muc = topology::muc_domain(HOME_B);
            let plan = Plan::external(&muc, HOME_A, &h.leaf(HOME_B)?);
            let attempt = raw(h, &plan).await?;
            outcome.check("inbound refused", refused(&attempt), attempt.describe());
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
            swap_stock_certificate(h, peer, component, parent_leaf).await?;
            fresh_streams(h, peer);
            let mut session = h
                .login_waddle(Home::A, "alice", &support::id("parent"))
                .await?;
            let answer = support::ask(
                &mut session,
                crate::xml::stanza::disco_info(&support::id("parent"), component),
            )
            .await;
            outcome.check(
                ["home-a -> ", component, " refused"].concat(),
                answer.as_ref().map_or(true, |answer| !is_result(answer)),
                answer
                    .map(|answer| describe(&answer))
                    .unwrap_or_else(|error| error.to_string()),
            );
            session.close().await;
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
    let plan = Plan::external(PUBLIC_PROFILE, HOME_A, &h.leaf(PUBLIC_PROFILE)?);
    let attempt = raw(h, &plan).await?;
    outcome.check("refused", refused(&attempt), attempt.describe());
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
/// s_client` restricted to an old TLS version through a loopback bridge.
pub async fn tls_version_outside_profile(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "OpenSSL s_client offering only TLS 1.1, then only TLS 1.0, after the harness's STARTTLS",
    );
    for (name, flag) in [("TLS 1.1", "-tls1_1"), ("TLS 1.0", "-tls1")] {
        let mut socket = TcpStream::connect(("127.0.0.1", h.ports.waddle_s2s(Home::A))).await?;
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
                                &minidom::Element::builder("starttls", ns::TLS).build(),
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
        match spliced {
            Ok((0, _)) | Err(_) => outcome.unverified(
                name,
                format!(
                    "OpenSSL sent no ClientHello for {name}; it may not support the version: {}",
                    text.trim()
                ),
            ),
            Ok(_) => outcome.check(
                [name, " refused"].concat(),
                !output.status.success()
                    && !text.contains("Protocol version: TLSv1.1")
                    && !text.contains("Protocol version: TLSv1\n"),
                text.lines().take(6).collect::<Vec<_>>().join(" | "),
            ),
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
