//! Stanzas handed to an established outbound stream.
//!
//! Each outbound stream has a channel of capacity one: a sender waits while
//! the stream writes the stanza before it, so a slow or stalled peer pushes
//! back on its sender instead of queueing inside the connection. The
//! connection reports, for each stanza it took, whether its bytes were
//! written to the TLS session within the write deadline. A written stanza
//! is not a delivered one: the peer may still drop it, and nothing on the
//! stream acknowledges it.

use std::sync::Arc;

use minidom::Element;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use waddle_xmpp::s2s::SendError;

use super::peer_stream::VerifiedPeerStream;
use super::registry::Registry;
use crate::permissions::host_grant::ManagedSpaceGrant;

/// How many stanzas an outbound stream holds before its sender waits: the
/// one being written.
pub(super) const OUTBOX_CAPACITY: usize = 1;

/// One stanza for an outbound stream, and where to report its write.
#[derive(Debug)]
pub(super) struct Transmission {
    pub(super) stanza: Element,
    pub(super) space_grant: Option<Arc<ManagedSpaceGrant>>,
    pub(super) written: oneshot::Sender<Result<(), Untransmitted>>,
}

/// Why an outbound stream did not write a stanza it took.
#[derive(Debug)]
pub(super) enum Untransmitted {
    /// The codec refused the stanza before producing a byte.
    Refused(SendError),
    AuthorizationExpired,
    /// The stream began to close before it wrote the stanza.
    Closing(Element),
}

/// The sending half of an outbound stream's channel.
pub(super) type Outbox = mpsc::Sender<Transmission>;

/// The receiving half, owned by the connection.
pub(super) type OutboxReceiver = mpsc::Receiver<Transmission>;

/// A new channel for an outbound stream.
pub(super) fn outbox() -> (Outbox, OutboxReceiver) {
    mpsc::channel(OUTBOX_CAPACITY)
}

/// What became of a stanza offered to [`super::S2sHandle::transmit`].
#[derive(Debug, Error)]
pub enum TransmitError {
    /// The stream did not take the stanza, which was never written and is
    /// returned unchanged: the stream had ended or was closing. Sending it
    /// on another stream for the same pair cannot duplicate it.
    #[error("the stream did not take the stanza")]
    NotTaken(Element),
    /// The codec refused the stanza, which was never written: it is not a
    /// stanza, or its addresses are outside the stream's domain pair or the
    /// identity profile.
    #[error("the stream refused the stanza: {0}")]
    Refused(SendError),
    /// The protected stanza lost its grant before entering TLS.
    #[error("space authorization expired before transmission")]
    AuthorizationExpired,
    /// The stream took the stanza and ended before its write completed or
    /// within it: the stanza may or may not have reached the peer.
    #[error("the stream ended while writing the stanza")]
    Unknown,
}

/// Hand `stanza` to the established outbound `stream` and wait until it is
/// written or refused.
pub(super) async fn transmit(
    registry: &Registry,
    stream: &VerifiedPeerStream,
    stanza: Element,
    space_grant: Option<Arc<ManagedSpaceGrant>>,
) -> Result<(), TransmitError> {
    let Some(outbox) = registry.outbox(stream) else {
        return Err(TransmitError::NotTaken(stanza));
    };
    let (written, result) = oneshot::channel();
    if let Err(refused) = outbox
        .send(Transmission {
            stanza,
            space_grant,
            written,
        })
        .await
    {
        return Err(TransmitError::NotTaken(refused.0.stanza));
    }
    match result.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(Untransmitted::Refused(error))) => Err(TransmitError::Refused(error)),
        Ok(Err(Untransmitted::AuthorizationExpired)) => Err(TransmitError::AuthorizationExpired),
        Ok(Err(Untransmitted::Closing(stanza))) => Err(TransmitError::NotTaken(stanza)),
        Err(_) => Err(TransmitError::Unknown),
    }
}

/// Return every stanza still waiting in a closing stream's channel to its
/// sender, unwritten.
pub(super) fn drain(receiver: &mut OutboxReceiver) {
    receiver.close();
    while let Ok(transmission) = receiver.try_recv() {
        let _ = transmission
            .written
            .send(Err(Untransmitted::Closing(transmission.stanza)));
    }
}
