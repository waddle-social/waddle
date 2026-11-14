//! Delivery of stanzas from S2S streams to local entities.

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
    /// No such resource is bound, or the hand-off failed.
    Unavailable,
}

/// Where the router delivers what a local resource is owed: the response
/// that completes its request, or the error that answers it.
///
/// The router delivers only to the full JID that sent the request, never to
/// another resource or account, and never stores the stanza for later.
#[async_trait]
pub trait LocalDelivery: Send + Sync {
    async fn deliver(&self, to: &FullJid, stanza: Stanza) -> LocalDeliveryOutcome;
}
