//! A recording TCP relay for one directed S2S path.
//!
//! In `Forward` mode the relay copies bytes unchanged and keeps the first
//! [`RECORD_LIMIT`] bytes of each direction: the plaintext before STARTTLS
//! (the stream headers, features and `<proceed/>`) and the TLS handshake's
//! first records, from which the negotiated version is read. `Cut` refuses
//! new connections and ends open ones, which is the partition of step 8.
//! `Tap` terminates TLS on both sides with leaves of the `.test` CA, which
//! the harness owns, records the decrypted stream and can rewrite one byte
//! sequence into another of the same length; it is used only for the
//! observation rows, never for the flows.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::net::tls_hello::{self, ServerHello};
use crate::pki::{self, Leaf, Pki};
use crate::xml::framer::{Framer, Item};

/// Bytes kept per direction and connection.
pub const RECORD_LIMIT: usize = 256 * 1024;

/// Leaves the tap presents, by domain.
#[derive(Clone, Default)]
pub struct TapIdentities {
    pub leaves: BTreeMap<String, Leaf>,
}

/// A rewrite the tap applies to the initiating side's decrypted bytes.
#[derive(Debug, Clone)]
pub struct Rewrite {
    pub from: Vec<u8>,
    pub to: Vec<u8>,
}

#[derive(Clone)]
pub enum Mode {
    Forward,
    Cut,
    /// Accept connections and never forward or answer: a peer that hangs.
    Stall,
    Tap {
        pki: Arc<Pki>,
        identities: TapIdentities,
        rewrite: Option<Rewrite>,
    },
}

/// What one relayed connection carried.
#[derive(Debug, Clone, Default)]
pub struct Connection {
    pub opened_after: Duration,
    pub closed_after: Option<Duration>,
    /// Bytes from the initiating server, as relayed (ciphertext after
    /// STARTTLS) or, under the tap, before STARTTLS only.
    pub initiator: Vec<u8>,
    pub receiver: Vec<u8>,
    /// Under the tap: the decrypted stream after STARTTLS.
    pub initiator_tls: Vec<u8>,
    pub receiver_tls: Vec<u8>,
    pub tapped: bool,
    pub rewrites: usize,
    pub error: Option<String>,
}

impl Connection {
    /// The initiator's bytes before its first TLS record.
    pub fn initiator_plaintext(&self) -> &[u8] {
        before_tls(&self.initiator)
    }

    pub fn receiver_plaintext(&self) -> &[u8] {
        before_tls(&self.receiver)
    }

    /// The receiving side's ServerHello, if the relay saw one in clear.
    pub fn server_hello(&self) -> Option<ServerHello> {
        let start = self.receiver.iter().position(|byte| *byte == 0x16)?;
        tls_hello::parse_server_hello(self.receiver.get(start..)?)
    }
}

fn before_tls(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|byte| *byte == 0x16) {
        Some(end) => bytes.get(..end).unwrap_or(bytes),
        None => bytes,
    }
}

struct Shared {
    mode: Mutex<Mode>,
    connections: Mutex<Vec<Connection>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    started: Instant,
    cut: watch::Sender<u64>,
    /// Bytes to write into the newest tapped stream, towards the receiver.
    inject: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
}

/// A running relay.
pub struct Relay {
    pub label: String,
    pub port: u16,
    shared: Arc<Shared>,
    task: JoinHandle<()>,
}

impl Relay {
    pub async fn start(label: String, port: u16, upstream: u16) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .with_context(|| format!("bind relay {label} on {port}"))?;
        let (cut, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            mode: Mutex::new(Mode::Forward),
            connections: Mutex::new(Vec::new()),
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            started: Instant::now(),
            cut,
            inject: Mutex::new(None),
        });
        let accept_shared = Arc::clone(&shared);
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let mode = lock(&accept_shared.mode).clone();
                if matches!(mode, Mode::Cut) {
                    drop(socket);
                    continue;
                }
                let shared = Arc::clone(&accept_shared);
                tokio::spawn(async move {
                    relay_one(shared, socket, upstream, mode).await;
                });
            }
        });
        Ok(Self {
            label,
            port,
            shared,
            task,
        })
    }

    pub fn set_mode(&self, mode: Mode) {
        let cut = matches!(mode, Mode::Cut);
        *lock(&self.shared.mode) = mode;
        if cut {
            self.shared.cut.send_modify(|generation| *generation += 1);
        }
    }

    /// Cut the path: refuse new connections and end open ones.
    pub fn cut(&self) {
        self.set_mode(Mode::Cut);
    }

    /// End open connections, then return to forwarding.
    pub fn restore(&self) {
        let stalled = matches!(*lock(&self.shared.mode), Mode::Stall);
        self.set_mode(Mode::Forward);
        if stalled {
            self.shared.cut.send_modify(|generation| *generation += 1);
        }
    }

    /// Accept connections but never forward them.
    pub fn stall(&self) {
        self.set_mode(Mode::Cut);
        self.set_mode(Mode::Stall);
    }

    pub fn connections(&self) -> Vec<Connection> {
        lock(&self.shared.connections).clone()
    }

    pub fn connection_count(&self) -> usize {
        lock(&self.shared.connections).len()
    }

    pub fn active(&self) -> usize {
        self.shared.active.load(Ordering::SeqCst)
    }

    /// The most connections open at once since the last reset.
    pub fn max_active(&self) -> usize {
        self.shared.max_active.load(Ordering::SeqCst)
    }

    /// Write `bytes` into the newest tapped stream as if the initiating
    /// server had sent them, between two of its own writes.
    pub fn inject(&self, bytes: Vec<u8>) -> Result<()> {
        let sender = lock(&self.shared.inject)
            .clone()
            .context("no tapped stream on this relay")?;
        sender
            .send(bytes)
            .map_err(|_| anyhow::anyhow!("the tapped stream has ended"))
    }

    /// Whether a tapped stream is open for injection.
    pub fn tapped(&self) -> bool {
        lock(&self.shared.inject)
            .as_ref()
            .is_some_and(|sender| !sender.is_closed())
    }

    pub fn reset_records(&self) {
        lock(&self.shared.connections).clear();
        self.shared
            .max_active
            .store(self.active(), Ordering::SeqCst);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
        self.shared.cut.send_modify(|generation| *generation += 1);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

async fn relay_one(shared: Arc<Shared>, client: TcpStream, upstream: u16, mode: Mode) {
    let index = {
        let mut connections = lock(&shared.connections);
        connections.push(Connection {
            opened_after: shared.started.elapsed(),
            tapped: matches!(mode, Mode::Tap { .. }),
            ..Connection::default()
        });
        connections.len() - 1
    };
    let now_active = shared.active.fetch_add(1, Ordering::SeqCst) + 1;
    shared.max_active.fetch_max(now_active, Ordering::SeqCst);
    let mut cut = shared.cut.subscribe();
    let result = tokio::select! {
        result = carry(&shared, index, client, upstream, mode) => result,
        _ = cut.changed() => Err(anyhow::anyhow!("relay cut")),
    };
    shared.active.fetch_sub(1, Ordering::SeqCst);
    let mut connections = lock(&shared.connections);
    if let Some(record) = connections.get_mut(index) {
        record.closed_after = Some(shared.started.elapsed());
        if let Err(error) = result {
            record.error = Some(format!("{error:#}"));
        }
    }
}

async fn carry(
    shared: &Arc<Shared>,
    index: usize,
    client: TcpStream,
    upstream: u16,
    mode: Mode,
) -> Result<()> {
    let server = TcpStream::connect(("127.0.0.1", upstream))
        .await
        .context("connect upstream")?;
    match mode {
        Mode::Forward | Mode::Cut => forward(shared, index, client, server).await,
        Mode::Stall => {
            // Hold both sockets open until the relay is cut or dropped.
            let _held = (client, server);
            std::future::pending::<()>().await;
            Ok(())
        }
        Mode::Tap {
            pki,
            identities,
            rewrite,
        } => tap(shared, index, client, server, &pki, &identities, rewrite).await,
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Initiator,
    Receiver,
    InitiatorTls,
    ReceiverTls,
}

fn append(shared: &Shared, index: usize, direction: Direction, bytes: &[u8]) {
    let mut connections = lock(&shared.connections);
    let Some(record) = connections.get_mut(index) else {
        return;
    };
    let target = match direction {
        Direction::Initiator => &mut record.initiator,
        Direction::Receiver => &mut record.receiver,
        Direction::InitiatorTls => &mut record.initiator_tls,
        Direction::ReceiverTls => &mut record.receiver_tls,
    };
    let room = RECORD_LIMIT.saturating_sub(target.len());
    target.extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
}

async fn forward(
    shared: &Arc<Shared>,
    index: usize,
    client: TcpStream,
    server: TcpStream,
) -> Result<()> {
    let (client_read, client_write) = client.into_split();
    let (server_read, server_write) = server.into_split();
    let up = pump(
        Arc::clone(shared),
        index,
        Direction::Initiator,
        client_read,
        server_write,
        None,
    );
    let down = pump(
        Arc::clone(shared),
        index,
        Direction::Receiver,
        server_read,
        client_write,
        None,
    );
    let (up, down) = tokio::join!(up, down);
    up.and(down)
}

async fn pump<R, W>(
    shared: Arc<Shared>,
    index: usize,
    direction: Direction,
    mut reader: R,
    mut writer: W,
    rewrite: Option<Rewrite>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut chunk = vec![0u8; 16 * 1024];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            if !pending.is_empty() {
                writer.write_all(&pending).await?;
            }
            let _ = writer.shutdown().await;
            return Ok(());
        }
        let bytes = chunk.get(..read).unwrap_or_default();
        append(&shared, index, direction, bytes);
        match &rewrite {
            Some(rule) if !rule.from.is_empty() => {
                pending.extend_from_slice(bytes);
                let replaced = replace_all(&mut pending, rule);
                if replaced > 0 {
                    let mut connections = lock(&shared.connections);
                    if let Some(record) = connections.get_mut(index) {
                        record.rewrites += replaced;
                    }
                }
                // Hold back a possible prefix of the pattern.
                let keep = rule.from.len() - 1;
                let flush = pending.len().saturating_sub(keep);
                let out: Vec<u8> = pending.drain(..flush).collect();
                writer.write_all(&out).await?;
            }
            _ => writer.write_all(bytes).await?,
        }
        writer.flush().await?;
    }
}

fn replace_all(buffer: &mut [u8], rule: &Rewrite) -> usize {
    if rule.from.len() != rule.to.len() || rule.from.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while start + rule.from.len() <= buffer.len() {
        let end = start + rule.from.len();
        if buffer.get(start..end) == Some(rule.from.as_slice()) {
            if let Some(window) = buffer.get_mut(start..end) {
                window.copy_from_slice(&rule.to);
            }
            count += 1;
            start = end;
        } else {
            start += 1;
        }
    }
    count
}

/// Relay in clear until the receiver sends `<proceed/>`, then terminate
/// TLS on both sides and relay the decrypted streams.
async fn tap(
    shared: &Arc<Shared>,
    index: usize,
    mut client: TcpStream,
    mut server: TcpStream,
    pki: &Pki,
    identities: &TapIdentities,
    rewrite: Option<Rewrite>,
) -> Result<()> {
    let mut header_reader = Framer::new();
    let mut proceed_reader = Framer::new();
    let mut to = None;
    let mut from = None;
    let mut client_chunk = vec![0u8; 16 * 1024];
    let mut server_chunk = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            read = client.read(&mut client_chunk) => {
                let read = read?;
                anyhow::ensure!(read > 0, "initiator closed before STARTTLS");
                let bytes = client_chunk.get(..read).unwrap_or_default();
                append(shared, index, Direction::Initiator, bytes);
                header_reader.push(bytes);
                while let Ok(Some(item)) = header_reader.next_item() {
                    if let Item::Header(header) = item {
                        to = header.attribute("to");
                        from = header.attribute("from");
                    }
                }
                server.write_all(bytes).await?;
            }
            read = server.read(&mut server_chunk) => {
                let read = read?;
                anyhow::ensure!(read > 0, "receiver closed before STARTTLS");
                let bytes = server_chunk.get(..read).unwrap_or_default();
                append(shared, index, Direction::Receiver, bytes);
                client.write_all(bytes).await?;
                proceed_reader.push(bytes);
                let mut proceed = false;
                while let Ok(Some(item)) = proceed_reader.next_item() {
                    if let Item::Element(element) = item {
                        proceed |= element.name() == "proceed";
                    }
                }
                if proceed {
                    break;
                }
            }
        }
    }
    let to = to.context("the initiator sent no `to`")?;
    let receiver_leaf = identities
        .leaves
        .get(&to.to_ascii_lowercase())
        .with_context(|| format!("no tap identity for {to}"))?;
    let initiator_leaf = from
        .as_deref()
        .and_then(|from| identities.leaves.get(&from.to_ascii_lowercase()));
    let acceptor = TlsAcceptor::from(Arc::new(pki.server_config(
        receiver_leaf,
        true,
        &pki::all_versions(),
    )?));
    let connector = TlsConnector::from(Arc::new(
        pki.client_config(initiator_leaf, &pki::all_versions())?,
    ));
    let client_tls = acceptor.accept(client).await.context("tap: accept TLS")?;
    let server_name = ServerName::try_from(to.to_ascii_lowercase())?;
    let server_tls = connector
        .connect(server_name, server)
        .await
        .context("tap: connect TLS")?;
    let (client_read, client_write) = tokio::io::split(client_tls);
    let (server_read, server_write) = tokio::io::split(server_tls);
    let (inject, injected) = mpsc::unbounded_channel();
    *lock(&shared.inject) = Some(inject);
    let up = pump_injecting(
        Arc::clone(shared),
        index,
        client_read,
        server_write,
        rewrite,
        injected,
    );
    let down = pump(
        Arc::clone(shared),
        index,
        Direction::ReceiverTls,
        server_read,
        client_write,
        None,
    );
    let (up, down) = tokio::join!(up, down);
    up.and(down)
}

/// The tap's initiator-to-receiver direction: the initiator's bytes with
/// the rewrite applied, and injected bytes written between reads.
async fn pump_injecting<R, W>(
    shared: Arc<Shared>,
    index: usize,
    mut reader: R,
    mut writer: W,
    rewrite: Option<Rewrite>,
    mut injected: mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let read = read?;
                if read == 0 {
                    let _ = writer.shutdown().await;
                    return Ok(());
                }
                let mut bytes = chunk.get(..read).unwrap_or_default().to_vec();
                append(&shared, index, Direction::InitiatorTls, &bytes);
                if let Some(rule) = &rewrite {
                    let replaced = replace_all(&mut bytes, rule);
                    if replaced > 0 {
                        let mut connections = lock(&shared.connections);
                        if let Some(record) = connections.get_mut(index) {
                            record.rewrites += replaced;
                        }
                    }
                }
                writer.write_all(&bytes).await?;
                writer.flush().await?;
            }
            Some(bytes) = injected.recv() => {
                append(&shared, index, Direction::InitiatorTls, &bytes);
                writer.write_all(&bytes).await?;
                writer.flush().await?;
            }
        }
    }
}
