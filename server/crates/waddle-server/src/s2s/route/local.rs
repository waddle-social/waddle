//! Delivery of stanzas from S2S streams to local entities: correlated
//! responses, the errors that answer local requests, and the traffic of
//! remote rooms to their local occupants.

use async_trait::async_trait;
use jid::FullJid;
use waddle_xmpp::Stanza;

/// Whether a stanza reached a local resource's connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalDeliveryOutcome {
    /// Handed to the resource's connection or its stream-management
    /// custody, on this replica or another. Not proof that the client read
    /// it.
    Delivered,
    /// No such resource is bound, the recipient's rules dropped it, or the
    /// hand-off failed.
    Unavailable,
}

/// Who generated a stanza the router delivers to a local resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOrigin {
    /// The remote entity's own response. The recipient's rules apply to it
    /// as to any stanza from another entity: a response from an address on
    /// the recipient's XEP-0191 blocking list is dropped.
    Remote,
    /// A stanza this server generated: an error about a request or a
    /// room stanza that could not be sent, a request with no response in
    /// time or with one this server refuses, or the unavailable presence
    /// ending an occupancy whose stream failed. It names the remote entity
    /// as `from` (RFC 6120 §8.3.1) but the remote entity did not send it,
    /// so the recipient's blocking list does not drop it.
    Server,
}

/// Where the router delivers what a local resource is owed: the response
/// that completes its request, the error that answers it, or the traffic of
/// a remote room the resource occupies.
///
/// The router delivers only to the full JID that sent the request or
/// joined the room, never to another resource or account. It keeps nothing for later itself: what
/// the delivery path does with a stanza for a detached resource, such as
/// holding it in that resource's stream-management custody, is the
/// delivery path's own behaviour.
#[async_trait]
pub trait LocalDelivery: Send + Sync {
    async fn deliver(
        &self,
        to: &FullJid,
        stanza: Stanza,
        origin: LocalOrigin,
    ) -> LocalDeliveryOutcome;
}
