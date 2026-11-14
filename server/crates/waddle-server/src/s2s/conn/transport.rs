//! The socket of one S2S connection, plaintext or TLS.
//!
//! rustls calls the certificate verifier synchronously from whichever
//! thread processes handshake records, so every handshake
//! `process_new_packets` call runs on the bounded verifier workers, never
//! on the async runtime. After the handshake no record can reach the
//! verifier again, and records are processed inline.
//!
//! Reads are rate limited per socket and globally: a peer over its rate is
//! read more slowly, which its deadlines still bound. Only the socket read
//! awaits; every other step is synchronous, so a read cancelled by a
//! deadline or a shutdown loses no input.

use std::io::{ErrorKind, Read, Write};
use std::sync::Arc;
use std::time::Instant;

use rustls::Connection;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::capacity::{Capacity, TokenBucket};
use super::registry::Activity;
use crate::s2s::config::RateLimit;
use crate::s2s::verify::{VerifierWorkers, WorkerError};

/// Bytes read from the socket at once. One TLS record is at most about
/// 16 KiB, and a plaintext read stays below the codec's per-push bound.
const READ_CHUNK: usize = 16 * 1024;

/// Plaintext rustls may buffer for writing before a write fails; the
/// codec's output for one batch is far smaller.
const TLS_SEND_BUFFER: usize = 64 * 1024;

/// Why the transport failed.
#[derive(Debug, Error)]
pub(super) enum TransportError {
    #[error("socket I/O failed")]
    Io(#[from] std::io::Error),
    #[error("TLS failed")]
    Tls(#[from] rustls::Error),
    #[error("the peer closed the connection during the TLS handshake")]
    HandshakeEof,
    #[error("no verifier worker could take the handshake")]
    Workers(#[from] WorkerError),
    #[error("the verifier worker ended without a result")]
    WorkerLost,
}

/// One connection's socket and, after STARTTLS or a direct TLS connect,
/// its TLS session.
pub(super) struct Transport {
    socket: TcpStream,
    tls: Option<Connection>,
    /// Ciphertext read from the socket that rustls has not taken yet.
    pending: Vec<u8>,
    bucket: TokenBucket,
    capacity: Arc<Capacity>,
    activity: Arc<Activity>,
}

impl Transport {
    pub(super) fn new(
        socket: TcpStream,
        rate: RateLimit,
        capacity: Arc<Capacity>,
        activity: Arc<Activity>,
    ) -> Self {
        Self {
            socket,
            tls: None,
            pending: Vec::new(),
            bucket: TokenBucket::new(rate, Instant::now()),
            capacity,
            activity,
        }
    }

    /// The TLS session once its handshake has completed.
    pub(super) fn tls(&self) -> Option<&Connection> {
        self.tls.as_ref()
    }

    /// Read decrypted or plaintext input into `buffer`. `Ok(0)` means the
    /// peer closed the connection or sent a TLS close_notify.
    pub(super) async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        loop {
            if let Some(tls) = self.tls.as_mut() {
                match tls.reader().read(buffer) {
                    Ok(read) => return Ok(read),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    // The peer closed the socket without close_notify.
                    Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(0),
                    Err(error) => return Err(TransportError::Io(error)),
                }
                if !self.pending.is_empty() {
                    feed(tls, &mut self.pending)?;
                    tls.process_new_packets()?;
                    continue;
                }
                let mut chunk = [0u8; READ_CHUNK];
                let read = self.read_socket(&mut chunk).await?;
                if read == 0 {
                    return Ok(0);
                }
                self.pending.extend_from_slice(&chunk[..read]);
            } else {
                let limit = buffer.len().min(READ_CHUNK);
                return self.read_socket(&mut buffer[..limit]).await;
            }
        }
    }

    /// One rate-limited socket read.
    async fn read_socket(&mut self, buffer: &mut [u8]) -> Result<usize, TransportError> {
        let delay = self
            .bucket
            .delay(Instant::now())
            .max(self.capacity.input_delay());
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let read = self.socket.read(buffer).await?;
        if read > 0 {
            self.bucket.consume(read, Instant::now());
            self.capacity.consume_input(read);
            self.activity.touch();
        }
        Ok(read)
    }

    /// Write `bytes` on the current layer.
    pub(super) async fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        match self.tls.as_mut() {
            Some(tls) => {
                tls.writer().write_all(bytes)?;
                self.flush_tls().await
            }
            None => {
                self.socket.write_all(bytes).await?;
                Ok(())
            }
        }
    }

    async fn flush_tls(&mut self) -> Result<(), TransportError> {
        if let Some(tls) = self.tls.as_mut() {
            flush(tls, &mut self.socket).await?;
        }
        Ok(())
    }

    /// Run a TLS handshake on this socket. `handoff` holds ciphertext that
    /// arrived with the plaintext stream after `<proceed/>`; it is given to
    /// rustls before the socket is read again. On success the transport
    /// encrypts everything from then on. On failure the alert rustls
    /// produced is sent when possible.
    pub(super) async fn handshake(
        &mut self,
        mut tls: Connection,
        handoff: Vec<u8>,
        workers: &VerifierWorkers,
    ) -> Result<(), TransportError> {
        tls.set_buffer_limit(Some(TLS_SEND_BUFFER));
        let mut leftover = handoff;
        leftover.append(&mut self.pending);
        self.pending = leftover;
        loop {
            flush(&mut tls, &mut self.socket).await?;
            if !tls.is_handshaking() {
                self.tls = Some(tls);
                return Ok(());
            }
            if self.pending.is_empty() {
                let mut chunk = [0u8; READ_CHUNK];
                let read = self.read_socket(&mut chunk).await?;
                if read == 0 {
                    return Err(TransportError::HandshakeEof);
                }
                self.pending.extend_from_slice(&chunk[..read]);
            }
            feed(&mut tls, &mut self.pending)?;
            let completion = workers
                .run(move || {
                    let result = tls.process_new_packets().map(|_| ());
                    (tls, result)
                })
                .await?;
            let (returned, result) = completion.await.map_err(|_| TransportError::WorkerLost)?;
            tls = returned;
            if let Err(error) = result {
                // Best effort: tell the peer why before closing.
                let _ = flush(&mut tls, &mut self.socket).await;
                return Err(TransportError::Tls(error));
            }
        }
    }

    /// End the connection: TLS close_notify when TLS is up, then a TCP
    /// shutdown. Errors are ignored; the socket is dropped afterwards.
    pub(super) async fn close(&mut self) {
        if let Some(tls) = self.tls.as_mut() {
            tls.send_close_notify();
            let _ = flush(tls, &mut self.socket).await;
        }
        let _ = self.socket.shutdown().await;
    }
}

/// Give rustls as much pending ciphertext as it takes.
fn feed(tls: &mut Connection, pending: &mut Vec<u8>) -> Result<(), TransportError> {
    let mut unread: &[u8] = pending.as_slice();
    let taken = tls.read_tls(&mut unread)?;
    if taken == 0 && !pending.is_empty() {
        // rustls takes nothing only when its own buffer is full, which
        // processing the records it already holds would have emptied.
        return Err(TransportError::Io(std::io::Error::new(
            ErrorKind::InvalidData,
            "TLS input buffer full",
        )));
    }
    pending.drain(..taken);
    Ok(())
}

/// Write every pending TLS record to the socket.
async fn flush(tls: &mut Connection, socket: &mut TcpStream) -> Result<(), TransportError> {
    while tls.wants_write() {
        let mut records = Vec::new();
        tls.write_tls(&mut records)?;
        if records.is_empty() {
            break;
        }
        socket.write_all(&records).await?;
    }
    Ok(())
}
