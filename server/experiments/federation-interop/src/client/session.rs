//! A logged-in XMPP client session: over WSS (RFC 7395) to a Waddle
//! deployment's TLS front, or over TCP with STARTTLS (RFC 6120) to Prosody
//! or ejabberd. Both authenticate with SASL and bind a resource; after
//! that every stanza is a `minidom::Element`, and elements that arrive
//! while waiting for something else are kept for later waits.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use minidom::rxml::xml_ncname;
use minidom::Element;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::client::sasl::{self, Scram};
use crate::pki::{self, Pki};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

/// How long one login may take.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(20);

/// Where and as whom a client logs in.
#[derive(Debug, Clone)]
pub struct Account {
    pub domain: String,
    pub username: String,
    pub password: String,
}

impl Account {
    pub fn bare(&self) -> String {
        [self.username.as_str(), "@", self.domain.as_str()].concat()
    }
}

trait TcpIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> TcpIo for T {}

enum Io {
    Tcp {
        tls: Box<dyn TcpIo>,
        framer: Box<Framer>,
    },
    Wss {
        ws: Box<WebSocketStream<TlsStream<TcpStream>>>,
    },
}

pub struct Session {
    pub label: String,
    /// The full JID the server bound.
    pub jid: String,
    io: Io,
    backlog: VecDeque<Element>,
    ended: bool,
}

async fn tls_connect(pki: &Pki, port: u16, server_name: &str) -> Result<TlsStream<TcpStream>> {
    let socket = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect 127.0.0.1:{port}"))?;
    tls_upgrade(pki, socket, server_name).await
}

async fn tls_upgrade(
    pki: &Pki,
    socket: TcpStream,
    server_name: &str,
) -> Result<TlsStream<TcpStream>> {
    let connector = TlsConnector::from(Arc::new(pki.client_config(None, &pki::all_versions())?));
    let name = ServerName::try_from(server_name.to_owned())?;
    Ok(connector.connect(name, socket).await?)
}

/// STARTTLS to a third-party server's C2S port; see
/// [`pki::c2s_client_config_for_third_party`].
async fn tls_upgrade_third_party(
    socket: TcpStream,
    server_name: &str,
) -> Result<TlsStream<TcpStream>> {
    let connector = TlsConnector::from(Arc::new(pki::c2s_client_config_for_third_party()?));
    let name = ServerName::try_from(server_name.to_owned())?;
    Ok(connector.connect(name, socket).await?)
}

/// Read framed items from `socket` until `done` accepts one.
async fn read_until<S: AsyncRead + Unpin>(
    socket: &mut S,
    framer: &mut Framer,
    deadline: Instant,
    done: impl Fn(&Item) -> bool,
) -> Result<Item> {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        while let Some(item) = framer.next_item()? {
            if let Item::Element(element) = &item {
                if let Some(condition) = stanza::stream_error_condition(element) {
                    bail!("stream error: {condition}");
                }
            }
            if matches!(item, Item::Close) {
                bail!("the server closed the stream");
            }
            if done(&item) {
                return Ok(item);
            }
        }
        let read = tokio::time::timeout_at(deadline, socket.read(&mut chunk))
            .await
            .context("timed out")??;
        if read == 0 {
            bail!("connection closed");
        }
        framer.push(chunk.get(..read).unwrap_or_default());
    }
}

async fn write_element<S: AsyncWrite + Unpin>(socket: &mut S, element: &Element) -> Result<()> {
    socket.write_all(&header::element_bytes(element)).await?;
    socket.flush().await?;
    Ok(())
}

fn is_element(item: &Item, name: &str) -> bool {
    matches!(item, Item::Element(element) if element.name() == name)
}

fn element(item: Item) -> Option<Element> {
    match item {
        Item::Element(element) => Some(element),
        _ => None,
    }
}

fn mechanisms(features: &Element) -> Vec<String> {
    features
        .children()
        .filter(|child| child.name() == "mechanisms")
        .flat_map(|mechanisms| mechanisms.children().map(Element::text))
        .collect()
}

fn bind_request(resource: &str) -> Element {
    let bind = Element::builder("bind", ns::BIND)
        .append(
            Element::builder("resource", ns::BIND)
                .append(resource)
                .build(),
        )
        .build();
    stanza::set("bind-1", None, bind)
}

fn bound_jid(response: &Element) -> Option<String> {
    response
        .get_child("bind", ns::BIND)?
        .get_child("jid", ns::BIND)
        .map(Element::text)
}

impl Session {
    /// Log in over TCP with STARTTLS and SASL PLAIN, as to Prosody or
    /// ejabberd.
    pub async fn tcp(port: u16, account: &Account, resource: &str, label: &str) -> Result<Self> {
        Self::tcp_fixture(port, account, resource, label, true).await
    }

    /// Log in to a disposable test listener without changing its certificate inventory.
    pub async fn tcp_loopback_plain(
        port: u16,
        account: &Account,
        resource: &str,
        label: &str,
    ) -> Result<Self> {
        Self::tcp_fixture(port, account, resource, label, false).await
    }

    async fn tcp_fixture(
        port: u16,
        account: &Account,
        resource: &str,
        label: &str,
        starttls: bool,
    ) -> Result<Self> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let mut socket = TcpStream::connect(("127.0.0.1", port))
            .await
            .with_context(|| format!("connect {label}"))?;
        let open = StreamOpen::client(&account.domain).encode()?;
        let mut tls: Box<dyn TcpIo> = if starttls {
            socket.write_all(&open).await?;
            let mut framer = Framer::new();
            read_until(&mut socket, &mut framer, deadline, |item| {
                is_element(item, "features")
            })
            .await?;
            write_element(&mut socket, &Element::builder("starttls", ns::TLS).build()).await?;
            read_until(&mut socket, &mut framer, deadline, |item| {
                is_element(item, "proceed")
            })
            .await?;
            Box::new(tls_upgrade_third_party(socket, &account.domain).await?)
        } else {
            Box::new(socket)
        };

        tls.write_all(&open).await?;
        let mut framer = Framer::new();
        let features = read_until(&mut tls, &mut framer, deadline, |item| {
            is_element(item, "features")
        })
        .await
        .map(element)?
        .context("features")?;
        anyhow::ensure!(
            mechanisms(&features).iter().any(|name| name == "PLAIN"),
            "{label} offers no PLAIN mechanism"
        );
        let auth = Element::builder("auth", ns::SASL)
            .attr(xml_ncname!("mechanism").to_owned(), "PLAIN")
            .append(sasl::plain(&account.username, &account.password))
            .build();
        write_element(&mut tls, &auth).await?;
        let outcome = read_until(&mut tls, &mut framer, deadline, |item| {
            is_element(item, "success") || is_element(item, "failure")
        })
        .await?;
        anyhow::ensure!(is_element(&outcome, "success"), "{label}: SASL failed");

        tls.write_all(&open).await?;
        let mut framer = Framer::new();
        read_until(&mut tls, &mut framer, deadline, |item| {
            is_element(item, "features")
        })
        .await?;
        write_element(&mut tls, &bind_request(resource)).await?;
        let bound = read_until(
            &mut tls,
            &mut framer,
            deadline,
            |item| matches!(item, Item::Element(element) if element.name() == "iq"),
        )
        .await
        .map(element)?
        .context("bind response")?;
        let jid = bound_jid(&bound).context("bind response without a JID")?;
        let mut session = Self {
            label: label.to_owned(),
            jid,
            io: Io::Tcp {
                tls,
                framer: Box::new(framer),
            },
            backlog: VecDeque::new(),
            ended: false,
        };
        session
            .send(&stanza::presence(ns::CLIENT, None, None))
            .await?;
        Ok(session)
    }

    /// Log in over WSS with SASL SCRAM-SHA-256, as to a Waddle deployment.
    pub async fn wss(
        pki: &Pki,
        port: u16,
        account: &Account,
        resource: &str,
        label: &str,
    ) -> Result<Self> {
        let deadline = Instant::now() + LOGIN_TIMEOUT;
        let tls = tls_connect(pki, port, &account.domain).await?;
        let url = ["wss://", &account.domain, ":", &port.to_string(), "/ws"].concat();
        let mut request = url.into_client_request()?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("xmpp"));
        let (ws, _) = tokio_tungstenite::client_async(request, tls)
            .await
            .with_context(|| format!("{label}: WebSocket handshake"))?;
        let mut session = Self {
            label: label.to_owned(),
            jid: String::new(),
            io: Io::Wss { ws: Box::new(ws) },
            backlog: VecDeque::new(),
            ended: false,
        };
        let open = Element::builder("open", ns::FRAMING)
            .attr(xml_ncname!("to").to_owned(), account.domain.as_str())
            .attr(xml_ncname!("version").to_owned(), "1.0")
            .build();
        session.send(&open).await?;
        let features = session
            .wait_until(deadline, |element| element.name() == "features")
            .await?;
        anyhow::ensure!(
            mechanisms(&features)
                .iter()
                .any(|name| name == "SCRAM-SHA-256"),
            "{label} offers no SCRAM-SHA-256 mechanism"
        );
        let scram = Scram::new(&account.username, &account.password);
        let auth = Element::builder("auth", ns::SASL)
            .attr(xml_ncname!("mechanism").to_owned(), "SCRAM-SHA-256")
            .append(scram.first())
            .build();
        session.send(&auth).await?;
        let challenge = session
            .wait_until(deadline, |element| {
                element.name() == "challenge" || element.name() == "failure"
            })
            .await?;
        anyhow::ensure!(challenge.name() == "challenge", "{label}: SASL failed");
        let response = Element::builder("response", ns::SASL)
            .append(scram.respond(&challenge.text())?)
            .build();
        session.send(&response).await?;
        let outcome = session
            .wait_until(deadline, |element| {
                element.name() == "success" || element.name() == "failure"
            })
            .await?;
        anyhow::ensure!(outcome.name() == "success", "{label}: SASL failed");
        session.send(&open).await?;
        session
            .wait_until(deadline, |element| element.name() == "features")
            .await?;
        session.send(&bind_request(resource)).await?;
        let bound = session
            .wait_until(deadline, |element| element.name() == "iq")
            .await?;
        session.jid = bound_jid(&bound).context("bind response without a JID")?;
        session
            .send(&stanza::presence(ns::CLIENT, None, None))
            .await?;
        Ok(session)
    }

    /// The bare JID of the session.
    pub fn bare(&self) -> String {
        self.jid.split('/').next().unwrap_or_default().to_owned()
    }

    pub async fn send(&mut self, element: &Element) -> Result<()> {
        match &mut self.io {
            Io::Tcp { tls, .. } => write_element(tls, element).await,
            Io::Wss { ws } => {
                let text = String::from_utf8(header::element_bytes(element))?;
                ws.send(Message::Text(text.into())).await?;
                Ok(())
            }
        }
    }

    /// Read the next element from the wire, or `None` at `deadline`.
    async fn read(&mut self, deadline: Instant) -> Result<Option<Element>> {
        if self.ended {
            bail!("{}: the stream has ended", self.label);
        }
        match &mut self.io {
            Io::Tcp { tls, framer } => {
                let mut chunk = vec![0u8; 16 * 1024];
                loop {
                    match framer.next_item()? {
                        Some(Item::Element(element)) => return Ok(Some(element)),
                        Some(Item::Close) => {
                            self.ended = true;
                            bail!("{}: the server closed the stream", self.label);
                        }
                        Some(Item::Header(_)) => continue,
                        None => {}
                    }
                    let read = match tokio::time::timeout_at(deadline, tls.read(&mut chunk)).await {
                        Ok(read) => read?,
                        Err(_) => return Ok(None),
                    };
                    if read == 0 {
                        self.ended = true;
                        bail!("{}: connection closed", self.label);
                    }
                    framer.push(chunk.get(..read).unwrap_or_default());
                }
            }
            Io::Wss { ws } => loop {
                let message = match tokio::time::timeout_at(deadline, ws.next()).await {
                    Ok(Some(message)) => message?,
                    Ok(None) => {
                        self.ended = true;
                        bail!("{}: WebSocket closed", self.label);
                    }
                    Err(_) => return Ok(None),
                };
                if let Message::Text(text) = message {
                    let element: Element = text.as_str().parse()?;
                    if element.name() == "close" && element.ns() == ns::FRAMING {
                        self.ended = true;
                        bail!("{}: the server closed the stream", self.label);
                    }
                    return Ok(Some(element));
                }
            },
        }
    }

    async fn wait_until(
        &mut self,
        deadline: Instant,
        matches: impl Fn(&Element) -> bool,
    ) -> Result<Element> {
        if let Some(index) = self.backlog.iter().position(&matches) {
            if let Some(element) = self.backlog.remove(index) {
                return Ok(element);
            }
        }
        loop {
            match self.read(deadline).await? {
                Some(element) if matches(&element) => return Ok(element),
                Some(element) => {
                    if let Some(condition) = stanza::stream_error_condition(&element) {
                        bail!("{}: stream error {condition}", self.label);
                    }
                    self.backlog.push_back(element);
                }
                None => bail!("{}: timed out waiting", self.label),
            }
        }
    }

    /// Wait up to `timeout` for an element `matches` accepts.
    pub async fn wait_for(
        &mut self,
        timeout: Duration,
        matches: impl Fn(&Element) -> bool,
    ) -> Result<Element> {
        self.wait_until(Instant::now() + timeout, matches).await
    }

    /// Whether no element `matches` accepts arrives within `window`.
    pub async fn absent(
        &mut self,
        window: Duration,
        matches: impl Fn(&Element) -> bool,
    ) -> Result<bool> {
        match self.wait_until(Instant::now() + window, matches).await {
            Ok(_) => Ok(false),
            Err(error) if error.to_string().contains("timed out waiting") => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// Send an IQ and wait for its result or error.
    pub async fn request(&mut self, iq: Element, timeout: Duration) -> Result<Element> {
        let id = iq.attr("id").context("an IQ without an id")?.to_owned();
        self.send(&iq).await?;
        self.wait_for(timeout, |element| {
            element.name() == "iq"
                && element.attr("id") == Some(id.as_str())
                && matches!(element.attr("type"), Some("result") | Some("error"))
        })
        .await
    }

    /// Everything received and not yet consumed.
    pub fn drain_backlog(&mut self) -> Vec<Element> {
        self.backlog.drain(..).collect()
    }

    pub async fn close(mut self) {
        match &mut self.io {
            Io::Tcp { tls, .. } => {
                let _ = tls.write_all(header::STREAM_CLOSE).await;
                let _ = tls.shutdown().await;
            }
            Io::Wss { ws } => {
                let close = Element::builder("close", ns::FRAMING).build();
                if let Ok(text) = String::from_utf8(header::element_bytes(&close)) {
                    let _ = ws.send(Message::Text(text.into())).await;
                }
                let _ = ws.close(None).await;
            }
        }
    }
}
