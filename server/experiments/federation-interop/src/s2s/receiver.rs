//! The harness as a receiving and authoritative server for its own
//! domains ([`crate::topology::RAW_PEER`], [`crate::topology::REDIRECT`]):
//! it accepts Waddle's outbound streams, offers SASL EXTERNAL or only
//! dialback, answers dialback verification requests for keys the harness
//! issued, redirects with `see-other-host` when told to, and keeps every
//! stanza it receives for the checks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use minidom::rxml::xml_ncname;
use minidom::Element;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::pki::{self, Leaf, Pki};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

/// What the receiving server offers after TLS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Behaviour {
    /// SASL EXTERNAL, and the dialback feature.
    External,
    /// Only dialback: the harness verifies the key with Waddle as the
    /// Authoritative Server.
    DialbackOnly,
    /// `see-other-host` naming this address, before STARTTLS.
    RedirectBeforeTls(String),
    /// `see-other-host` naming this address, after TLS.
    RedirectAfterTls(String),
}

/// One inbound connection from Waddle.
#[derive(Debug, Clone, Default)]
pub struct Inbound {
    pub header_from: Option<String>,
    pub header_to: Option<String>,
    pub client_certificate: bool,
    pub authenticated: Option<String>,
    pub verification_requests: usize,
    pub stanzas: Vec<Element>,
    pub error: Option<String>,
}

pub struct Settings {
    pub behaviour: Behaviour,
    /// The TLS versions the receiving server accepts.
    pub versions: Vec<&'static rustls::SupportedProtocolVersion>,
    /// The harness leaves by domain, and Waddle listeners by domain for
    /// dialback verification.
    pub leaves: BTreeMap<String, Leaf>,
    pub authoritative: BTreeMap<String, u16>,
}

struct Shared {
    settings: Mutex<Settings>,
    inbound: Mutex<Vec<Inbound>>,
    /// Dialback keys the harness sent as an initiating server; a
    /// verification request for one of them is valid.
    issued_keys: Mutex<BTreeSet<String>>,
}

pub struct Receiver {
    shared: Arc<Shared>,
    pki: Arc<Pki>,
    task: JoinHandle<()>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

const STEP: Duration = Duration::from_secs(15);

impl Receiver {
    pub async fn start(pki: Arc<Pki>, port: u16, settings: Settings) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind harness receiver on {port}"))?;
        let shared = Arc::new(Shared {
            settings: Mutex::new(settings),
            inbound: Mutex::new(Vec::new()),
            issued_keys: Mutex::new(BTreeSet::new()),
        });
        let accept_shared = Arc::clone(&shared);
        let accept_pki = Arc::clone(&pki);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let shared = Arc::clone(&accept_shared);
                let pki = Arc::clone(&accept_pki);
                tokio::spawn(async move {
                    let index = {
                        let mut inbound = lock(&shared.inbound);
                        inbound.push(Inbound::default());
                        inbound.len() - 1
                    };
                    if let Err(error) = serve(&shared, &pki, socket, index).await {
                        if let Some(record) = lock(&shared.inbound).get_mut(index) {
                            record.error = Some(format!("{error:#}"));
                        }
                    }
                });
            }
        });
        Ok(Self { shared, pki, task })
    }

    pub fn set_behaviour(&self, behaviour: Behaviour) {
        lock(&self.shared.settings).behaviour = behaviour;
    }

    /// Present `leaf` for `domain` from now on.
    pub fn set_leaf(&self, domain: &str, leaf: Leaf) {
        lock(&self.shared.settings)
            .leaves
            .insert(domain.to_owned(), leaf);
    }

    pub fn set_versions(&self, versions: Vec<&'static rustls::SupportedProtocolVersion>) {
        lock(&self.shared.settings).versions = versions;
    }

    /// Remember a dialback key the harness is about to send.
    pub fn issue_key(&self, key: &str) {
        lock(&self.shared.issued_keys).insert(key.to_owned());
    }

    pub fn inbound(&self) -> Vec<Inbound> {
        lock(&self.shared.inbound).clone()
    }

    pub fn clear(&self) {
        lock(&self.shared.inbound).clear();
    }

    /// Every stanza received so far.
    pub fn stanzas(&self) -> Vec<Element> {
        lock(&self.shared.inbound)
            .iter()
            .flat_map(|inbound| inbound.stanzas.iter().cloned())
            .collect()
    }

    /// Wait up to `timeout` for a stanza `matches` accepts.
    pub async fn wait_stanza(
        &self,
        timeout: Duration,
        matches: impl Fn(&Element) -> bool,
    ) -> Option<Element> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(found) = self.stanzas().into_iter().find(&matches) {
                return Some(found);
            }
            if Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub fn pki(&self) -> &Arc<Pki> {
        &self.pki
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

enum Read {
    Item(Item),
    End,
}

async fn next<S: AsyncRead + Unpin>(socket: &mut S, framer: &mut Framer) -> Result<Read> {
    let deadline = Instant::now() + STEP;
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match framer.next_item()? {
            Some(Item::Close) => return Ok(Read::End),
            Some(item) => return Ok(Read::Item(item)),
            None => {}
        }
        let read = tokio::time::timeout_at(deadline, socket.read(&mut chunk))
            .await
            .context("peer idle")??;
        if read == 0 {
            return Ok(Read::End);
        }
        framer.push(chunk.get(..read).unwrap_or_default());
    }
}

async fn write<S: AsyncWrite + Unpin>(socket: &mut S, bytes: &[u8]) -> Result<()> {
    socket.write_all(bytes).await?;
    socket.flush().await?;
    Ok(())
}

fn features(children: Vec<Element>) -> Element {
    let mut builder = Element::builder("features", ns::STREAM);
    for child in children {
        builder = builder.append(child);
    }
    builder.build()
}

fn starttls_required() -> Element {
    Element::builder("starttls", ns::TLS)
        .append(Element::builder("required", ns::TLS).build())
        .build()
}

fn see_other_host(address: &str) -> Element {
    Element::builder("error", ns::STREAM)
        .append(
            Element::builder("see-other-host", ns::STREAM_ERRORS)
                .append(address)
                .build(),
        )
        .build()
}

fn response_header(local: &str, remote: Option<&str>, id: &str) -> Result<Vec<u8>> {
    let mut open = StreamOpen::server(Some(local), remote.unwrap_or_default()).with_dialback();
    open.to = remote.map(str::to_owned);
    Ok(open.with_id(id).encode()?)
}

async fn serve(shared: &Arc<Shared>, pki: &Pki, mut socket: TcpStream, index: usize) -> Result<()> {
    let mut framer = Framer::new();
    let Read::Item(Item::Header(opening)) = next(&mut socket, &mut framer).await? else {
        anyhow::bail!("no stream header");
    };
    let to = opening.attribute("to").unwrap_or_default();
    let from = opening.attribute("from");
    if let Some(record) = lock(&shared.inbound).get_mut(index) {
        record.header_from = from.clone();
        record.header_to = Some(to.clone());
    }
    let (behaviour, leaf, versions) = {
        let settings = lock(&shared.settings);
        (
            settings.behaviour.clone(),
            settings.leaves.get(&to).cloned(),
            settings.versions.clone(),
        )
    };
    let leaf = leaf.with_context(|| format!("no harness leaf for {to}"))?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    write(&mut socket, &response_header(&to, from.as_deref(), &id)?).await?;
    if let Behaviour::RedirectBeforeTls(address) = &behaviour {
        write(
            &mut socket,
            &header::element_bytes(&see_other_host(address)),
        )
        .await?;
        write(&mut socket, header::STREAM_CLOSE).await?;
        return Ok(());
    }
    write(
        &mut socket,
        &header::element_bytes(&features(vec![starttls_required()])),
    )
    .await?;
    loop {
        match next(&mut socket, &mut framer).await? {
            Read::Item(Item::Element(element)) if element.name() == "starttls" => break,
            Read::Item(_) => {}
            Read::End => return Ok(()),
        }
    }
    write(
        &mut socket,
        &header::element_bytes(&Element::builder("proceed", ns::TLS).build()),
    )
    .await?;
    let acceptor = TlsAcceptor::from(Arc::new(pki.server_config(&leaf, true, &versions)?));
    let mut tls = acceptor.accept(socket).await.context("accept TLS")?;
    let client_certificate = tls
        .get_ref()
        .1
        .peer_certificates()
        .is_some_and(|certificates| !certificates.is_empty());
    if let Some(record) = lock(&shared.inbound).get_mut(index) {
        record.client_certificate = client_certificate;
    }
    let mut framer = Framer::new();
    let Read::Item(Item::Header(_)) = next(&mut tls, &mut framer).await? else {
        anyhow::bail!("no stream header after TLS");
    };
    write(&mut tls, &response_header(&to, from.as_deref(), &id)?).await?;
    if let Behaviour::RedirectAfterTls(address) = &behaviour {
        write(&mut tls, &header::element_bytes(&see_other_host(address))).await?;
        write(&mut tls, header::STREAM_CLOSE).await?;
        return Ok(());
    }
    let mut offered = vec![Element::builder("dialback", ns::DIALBACK_FEATURE).build()];
    if behaviour == Behaviour::External && client_certificate {
        offered.push(
            Element::builder("mechanisms", ns::SASL)
                .append(
                    Element::builder("mechanism", ns::SASL)
                        .append("EXTERNAL")
                        .build(),
                )
                .build(),
        );
    }
    write(&mut tls, &header::element_bytes(&features(offered))).await?;
    loop {
        let element = match next(&mut tls, &mut framer).await? {
            Read::Item(Item::Element(element)) => element,
            Read::Item(Item::Header(_)) => {
                // A restart after SASL success.
                write(&mut tls, &response_header(&to, from.as_deref(), &id)?).await?;
                write(&mut tls, &header::element_bytes(&features(Vec::new()))).await?;
                continue;
            }
            Read::Item(Item::Close) | Read::End => return Ok(()),
        };
        if element.is("auth", ns::SASL) {
            if behaviour == Behaviour::External && client_certificate {
                write(
                    &mut tls,
                    &header::element_bytes(&Element::builder("success", ns::SASL).build()),
                )
                .await?;
                framer = Framer::new();
                if let Some(record) = lock(&shared.inbound).get_mut(index) {
                    record.authenticated = Some("external".to_owned());
                }
            } else {
                let failure = Element::builder("failure", ns::SASL)
                    .append(Element::builder("not-authorized", ns::SASL).build())
                    .build();
                write(&mut tls, &header::element_bytes(&failure)).await?;
            }
        } else if element.is("verify", ns::DIALBACK) {
            // Waddle, as a Receiving Server, asks whether the harness issued a key.
            let key = element.text();
            let valid = lock(&shared.issued_keys).contains(key.trim());
            if let Some(record) = lock(&shared.inbound).get_mut(index) {
                record.verification_requests += 1;
            }
            let answer = Element::builder("verify", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), to.as_str())
                .attr(
                    xml_ncname!("to").to_owned(),
                    element.attr("from").unwrap_or_default(),
                )
                .attr(
                    xml_ncname!("id").to_owned(),
                    element.attr("id").unwrap_or_default(),
                )
                .attr(
                    xml_ncname!("type").to_owned(),
                    if valid { "valid" } else { "invalid" },
                )
                .build();
            write(&mut tls, &header::element_bytes(&answer)).await?;
        } else if element.is("result", ns::DIALBACK) {
            // Waddle, as an Initiating Server, sent its key; verify it with
            // Waddle's own Authoritative Server.
            let originating = element.attr("from").unwrap_or_default().to_owned();
            let port = lock(&shared.settings)
                .authoritative
                .get(&originating)
                .copied();
            let valid = match port {
                Some(port) => verify_with_authoritative(
                    pki,
                    &leaf,
                    port,
                    &to,
                    &originating,
                    &id,
                    element.text().trim(),
                )
                .await
                .unwrap_or(false),
                None => false,
            };
            let answer = Element::builder("result", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), to.as_str())
                .attr(xml_ncname!("to").to_owned(), originating.as_str())
                .attr(
                    xml_ncname!("type").to_owned(),
                    if valid { "valid" } else { "invalid" },
                )
                .build();
            write(&mut tls, &header::element_bytes(&answer)).await?;
            if valid {
                if let Some(record) = lock(&shared.inbound).get_mut(index) {
                    record.authenticated = Some("dialback".to_owned());
                }
            }
        } else if let Some(record) = lock(&shared.inbound).get_mut(index) {
            record.stanzas.push(element);
        }
    }
}

/// Ask Waddle, as the Authoritative Server for `originating`, whether it
/// issued `key` for the stream `id` (XEP-0220 section 2.2), over TLS.
async fn verify_with_authoritative(
    pki: &Pki,
    leaf: &Leaf,
    port: u16,
    local: &str,
    originating: &str,
    id: &str,
    key: &str,
) -> Result<bool> {
    let mut socket = TcpStream::connect(("127.0.0.1", port)).await?;
    let opening = StreamOpen::server(Some(local), originating)
        .with_dialback()
        .encode()?;
    write(&mut socket, &opening).await?;
    let mut framer = Framer::new();
    loop {
        match next(&mut socket, &mut framer).await? {
            Read::Item(Item::Element(element)) if element.name() == "features" => break,
            Read::Item(_) => {}
            Read::End => anyhow::bail!("authoritative server closed"),
        }
    }
    write(
        &mut socket,
        &header::element_bytes(&Element::builder("starttls", ns::TLS).build()),
    )
    .await?;
    loop {
        match next(&mut socket, &mut framer).await? {
            Read::Item(Item::Element(element)) if element.name() == "proceed" => break,
            Read::Item(_) => {}
            Read::End => anyhow::bail!("authoritative server refused STARTTLS"),
        }
    }
    let connector = TlsConnector::from(Arc::new(
        pki.client_config(Some(leaf), &pki::all_versions())?,
    ));
    let mut tls = connector
        .connect(ServerName::try_from(originating.to_owned())?, socket)
        .await?;
    write(&mut tls, &opening).await?;
    let mut framer = Framer::new();
    loop {
        match next(&mut tls, &mut framer).await? {
            Read::Item(Item::Element(element)) if element.name() == "features" => break,
            Read::Item(_) => {}
            Read::End => anyhow::bail!("authoritative server closed after TLS"),
        }
    }
    let request = Element::builder("verify", ns::DIALBACK)
        .attr(xml_ncname!("from").to_owned(), local)
        .attr(xml_ncname!("to").to_owned(), originating)
        .attr(xml_ncname!("id").to_owned(), id)
        .append(key)
        .build();
    write(&mut tls, &header::element_bytes(&request)).await?;
    loop {
        match next(&mut tls, &mut framer).await? {
            Read::Item(Item::Element(element)) if element.is("verify", ns::DIALBACK) => {
                return Ok(element.attr("type") == Some("valid"));
            }
            Read::Item(Item::Element(element)) => {
                if let Some(condition) = stanza::stream_error_condition(&element) {
                    anyhow::bail!("authoritative server: {condition}");
                }
            }
            Read::Item(_) => {}
            Read::End => anyhow::bail!("authoritative server closed before answering"),
        }
    }
}
