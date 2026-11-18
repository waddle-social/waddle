//! The harness as an initiating server (RFC 6120, XEP-0178, XEP-0220):
//! a raw peer that negotiates STARTTLS, then SASL EXTERNAL or dialback,
//! with a chosen certificate, TLS versions and stream header, and stops
//! wherever the receiving server refuses it. It drives the negative rows
//! no stock server would produce.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use minidom::rxml::xml_ncname;
use minidom::Element;
use rustls::pki_types::ServerName;
use rustls::SupportedProtocolVersion;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use crate::pki::{self, Leaf, Pki};
use crate::xml::framer::{Framer, Item};
use crate::xml::header::{self, StreamOpen};
use crate::xml::{ns, stanza};

const STEP_TIMEOUT: Duration = Duration::from_secs(10);

/// How the raw peer authenticates after TLS.
#[derive(Debug, Clone)]
pub enum Auth {
    External,
    /// Send this dialback key (XEP-0220 section 2.1.1).
    Dialback(String),
    /// Stop after the post-TLS features.
    None,
}

/// What the raw peer does.
#[derive(Clone)]
pub struct Plan {
    pub open: StreamOpen,
    /// Bytes written before the first stream header.
    pub leading: Vec<u8>,
    /// Bytes written before the post-TLS stream header.
    pub leading_after_tls: Vec<u8>,
    pub leaf: Option<Leaf>,
    pub versions: Vec<&'static SupportedProtocolVersion>,
    pub auth: Auth,
    /// Write a TLS ClientHello at once instead of a stream header
    /// (inbound direct TLS).
    pub direct_tls: bool,
}

impl Plan {
    /// Authenticate as `from` to `to` with `leaf` and SASL EXTERNAL.
    pub fn external(from: &str, to: &str, leaf: &Leaf) -> Self {
        Self {
            open: StreamOpen::server(Some(from), to),
            leading: Vec::new(),
            leading_after_tls: Vec::new(),
            leaf: Some(leaf.clone()),
            versions: pki::all_versions(),
            auth: Auth::External,
            direct_tls: false,
        }
    }
}

/// How far the attempt got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Refused or closed before STARTTLS.
    BeforeTls,
    /// The TLS handshake failed.
    Tls,
    /// TLS succeeded; authentication was refused or not attempted.
    Authentication,
    /// Authenticated; the stream is open for stanzas.
    Established,
}

pub struct Attempt {
    pub stage: Stage,
    /// The stream error condition the receiving server sent, if any.
    pub stream_error: Option<String>,
    pub detail: String,
    pub features_before_tls: Option<Element>,
    pub features_after_tls: Option<Element>,
    pub stream_id: Option<String>,
    pub stream: Option<RawStream>,
}

impl Attempt {
    fn stopped(stage: Stage, stream_error: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            stage,
            stream_error,
            detail: detail.into(),
            features_before_tls: None,
            features_after_tls: None,
            stream_id: None,
            stream: None,
        }
    }

    pub fn offers(&self, name: &str, namespace: &str) -> bool {
        self.features_after_tls
            .as_ref()
            .is_some_and(|features| features.children().any(|child| child.is(name, namespace)))
    }

    pub fn describe(&self) -> String {
        format!(
            "stage {:?}, stream error {:?}, {}",
            self.stage, self.stream_error, self.detail
        )
    }
}

/// What reading a step produced.
enum Step {
    Item(Item),
    Error(String),
    Closed,
    TimedOut,
}

async fn step<S: AsyncRead + Unpin>(
    socket: &mut S,
    framer: &mut Framer,
    deadline: Instant,
    accept: impl Fn(&Item) -> bool,
) -> Step {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match framer.next_item() {
            Ok(Some(item)) => {
                if let Item::Element(element) = &item {
                    if let Some(condition) = stanza::stream_error_condition(element) {
                        return Step::Error(condition);
                    }
                }
                if matches!(item, Item::Close) {
                    return Step::Closed;
                }
                if accept(&item) {
                    return Step::Item(item);
                }
                // A receiving server often writes its response header and
                // what follows it (features, or a stream error) in one
                // write: take every buffered item before reading again.
                continue;
            }
            Ok(None) => {}
            Err(_) => return Step::Closed,
        }
        match tokio::time::timeout_at(deadline, socket.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(_)) => return Step::Closed,
            Ok(Ok(read)) => framer.push(chunk.get(..read).unwrap_or_default()),
            Err(_) => return Step::TimedOut,
        }
    }
}

fn named(item: &Item, name: &str) -> bool {
    matches!(item, Item::Element(element) if element.name() == name)
}

async fn write<S: AsyncWrite + Unpin>(socket: &mut S, bytes: &[u8]) -> Result<()> {
    socket.write_all(bytes).await?;
    socket.flush().await?;
    Ok(())
}

fn refused(stage: Stage, outcome: Step, waiting_for: &str) -> Attempt {
    match outcome {
        Step::Error(condition) => Attempt::stopped(
            stage,
            Some(condition),
            format!("stream error while waiting for {waiting_for}"),
        ),
        Step::Closed => Attempt::stopped(stage, None, format!("closed before {waiting_for}")),
        Step::TimedOut => Attempt::stopped(stage, None, format!("no {waiting_for} in time")),
        Step::Item(_) => {
            Attempt::stopped(stage, None, format!("unexpected item before {waiting_for}"))
        }
    }
}

/// Run `plan` against the S2S listener on `port`.
pub async fn attempt(pki: &Pki, port: u16, plan: &Plan) -> Result<Attempt> {
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut socket = TcpStream::connect(("127.0.0.1", port))
        .await
        .with_context(|| format!("connect S2S listener {port}"))?;
    let sni = plan.open.to.clone().unwrap_or_default();
    if plan.direct_tls {
        let connector = TlsConnector::from(Arc::new(
            pki.client_config(plan.leaf.as_ref(), &plan.versions)?,
        ));
        let name = ServerName::try_from(sni)?;
        return Ok(
            match tokio::time::timeout(STEP_TIMEOUT, connector.connect(name, socket)).await {
                Ok(Ok(_)) => Attempt::stopped(Stage::Authentication, None, "direct TLS accepted"),
                Ok(Err(error)) => {
                    Attempt::stopped(Stage::Tls, None, format!("direct TLS refused: {error}"))
                }
                Err(_) => Attempt::stopped(Stage::Tls, None, "direct TLS timed out"),
            },
        );
    }
    let opening = plan.open.encode()?;
    write(
        &mut socket,
        &[plan.leading.as_slice(), opening.as_slice()].concat(),
    )
    .await?;
    let mut framer = Framer::new();
    let features_before = match step(&mut socket, &mut framer, deadline, |item| {
        named(item, "features")
    })
    .await
    {
        Step::Item(Item::Element(features)) => features,
        other => return Ok(refused(Stage::BeforeTls, other, "features")),
    };
    write(
        &mut socket,
        &header::element_bytes(&Element::builder("starttls", ns::TLS).build()),
    )
    .await?;
    match step(&mut socket, &mut framer, deadline, |item| {
        named(item, "proceed")
    })
    .await
    {
        Step::Item(_) => {}
        other => return Ok(refused(Stage::BeforeTls, other, "proceed")),
    }
    let connector = TlsConnector::from(Arc::new(
        pki.client_config(plan.leaf.as_ref(), &plan.versions)?,
    ));
    let name = ServerName::try_from(sni)?;
    let mut tls = match tokio::time::timeout(STEP_TIMEOUT, connector.connect(name, socket)).await {
        Ok(Ok(tls)) => tls,
        Ok(Err(error)) => return Ok(Attempt::stopped(Stage::Tls, None, error.to_string())),
        Err(_) => {
            return Ok(Attempt::stopped(
                Stage::Tls,
                None,
                "TLS handshake timed out",
            ))
        }
    };
    write(
        &mut tls,
        &[plan.leading_after_tls.as_slice(), opening.as_slice()].concat(),
    )
    .await?;
    let mut framer = Framer::new();
    let mut stream_id = None;
    let features_after = match step(&mut tls, &mut framer, deadline, |item| {
        named(item, "features") || matches!(item, Item::Header(_))
    })
    .await
    {
        Step::Item(Item::Header(header)) => {
            stream_id = header.attribute("id");
            match step(&mut tls, &mut framer, deadline, |item| {
                named(item, "features")
            })
            .await
            {
                Step::Item(Item::Element(features)) => features,
                other => return Ok(refused(Stage::Authentication, other, "features after TLS")),
            }
        }
        Step::Item(Item::Element(features)) => features,
        other => return Ok(refused(Stage::Authentication, other, "features after TLS")),
    };
    let mut result = Attempt {
        stage: Stage::Authentication,
        stream_error: None,
        detail: String::new(),
        features_before_tls: Some(features_before),
        features_after_tls: Some(features_after),
        stream_id,
        stream: None,
    };
    match &plan.auth {
        Auth::None => {
            result.detail = "stopped after the post-TLS features".to_owned();
            result.stream = Some(RawStream::new(tls, framer));
        }
        Auth::External => {
            let auth = Element::builder("auth", ns::SASL)
                .attr(xml_ncname!("mechanism").to_owned(), "EXTERNAL")
                .append("=")
                .build();
            write(&mut tls, &header::element_bytes(&auth)).await?;
            match step(&mut tls, &mut framer, deadline, |item| {
                named(item, "success") || named(item, "failure")
            })
            .await
            {
                Step::Item(Item::Element(element)) if element.name() == "success" => {}
                Step::Item(Item::Element(element)) => {
                    let condition = element
                        .children()
                        .next()
                        .map(|child| child.name().to_owned())
                        .unwrap_or_default();
                    result.detail = ["SASL failure ", &condition].concat();
                    return Ok(result);
                }
                other => {
                    let stopped = refused(Stage::Authentication, other, "SASL outcome");
                    result.stream_error = stopped.stream_error;
                    result.detail = stopped.detail;
                    return Ok(result);
                }
            }
            write(&mut tls, &opening).await?;
            let mut framer = Framer::new();
            match step(&mut tls, &mut framer, deadline, |item| {
                named(item, "features")
            })
            .await
            {
                Step::Item(_) => {
                    result.stage = Stage::Established;
                    result.detail = "SASL EXTERNAL succeeded".to_owned();
                    result.stream = Some(RawStream::new(tls, framer));
                }
                other => {
                    let stopped = refused(Stage::Authentication, other, "post-SASL features");
                    result.stream_error = stopped.stream_error;
                    result.detail = stopped.detail;
                }
            }
        }
        Auth::Dialback(key) => {
            let from = plan.open.from.clone().unwrap_or_default();
            let to = plan.open.to.clone().unwrap_or_default();
            let request = Element::builder("result", ns::DIALBACK)
                .attr(xml_ncname!("from").to_owned(), from.as_str())
                .attr(xml_ncname!("to").to_owned(), to.as_str())
                .append(key.as_str())
                .build();
            write(&mut tls, &header::element_bytes(&request)).await?;
            match step(&mut tls, &mut framer, deadline, |item| {
                named(item, "result")
            })
            .await
            {
                Step::Item(Item::Element(answer)) if answer.attr("type") == Some("valid") => {
                    result.stage = Stage::Established;
                    result.detail = "dialback valid".to_owned();
                    result.stream = Some(RawStream::new(tls, framer));
                }
                Step::Item(Item::Element(answer)) => {
                    result.detail = [
                        "dialback answered ",
                        answer.attr("type").unwrap_or("without a type"),
                    ]
                    .concat();
                }
                other => {
                    let stopped = refused(Stage::Authentication, other, "dialback result");
                    result.stream_error = stopped.stream_error;
                    result.detail = stopped.detail;
                }
            }
        }
    }
    Ok(result)
}

/// An authenticated raw stream.
pub struct RawStream {
    tls: Box<TlsStream<TcpStream>>,
    framer: Framer,
}

/// How a raw stream ended, if it did.
#[derive(Debug, Clone)]
pub struct Ending {
    pub stream_error: Option<String>,
    pub closed: bool,
    pub elements: Vec<Element>,
}

impl RawStream {
    fn new(tls: TlsStream<TcpStream>, framer: Framer) -> Self {
        Self {
            tls: Box::new(tls),
            framer,
        }
    }

    pub async fn send(&mut self, element: &Element) -> Result<()> {
        write(self.tls.as_mut(), &header::element_bytes(element)).await
    }

    pub async fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        write(self.tls.as_mut(), bytes).await
    }

    /// Read for up to `window`; report a stream error, closure and the
    /// elements received.
    pub async fn watch(&mut self, window: Duration) -> Ending {
        let deadline = Instant::now() + window;
        let mut ending = Ending {
            stream_error: None,
            closed: false,
            elements: Vec::new(),
        };
        loop {
            match step(self.tls.as_mut(), &mut self.framer, deadline, |_| true).await {
                Step::Item(Item::Element(element)) => ending.elements.push(element),
                Step::Item(_) => {}
                Step::Error(condition) => {
                    ending.stream_error = Some(condition);
                    ending.closed = true;
                    return ending;
                }
                Step::Closed => {
                    ending.closed = true;
                    return ending;
                }
                Step::TimedOut => return ending,
            }
        }
    }

    pub async fn close(mut self) {
        let _ = self.tls.write_all(header::STREAM_CLOSE).await;
        let _ = self.tls.shutdown().await;
    }
}
