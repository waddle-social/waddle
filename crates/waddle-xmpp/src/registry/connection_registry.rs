//! Connection Registry implementation.
//!
//! Tracks active XMPP connections by their full JID for message routing.

use std::fmt;

use dashmap::DashMap;
use jid::FullJid;
use tokio::sync::mpsc;
use tracing::{debug, info, instrument, warn};

use crate::connection::Stanza;

/// A stanza to be sent to a connection.
///
/// This is the message type sent through the outbound channel to
/// deliver stanzas to connected clients.
#[derive(Debug, Clone)]
pub struct OutboundStanza {
    /// The stanza to send
    pub stanza: Stanza,
}

impl OutboundStanza {
    /// Create a new outbound stanza.
    pub fn new(stanza: Stanza) -> Self {
        Self { stanza }
    }
}

/// Result of attempting to send a message to a connection.
#[derive(Debug)]
pub enum SendResult {
    /// Message was successfully queued for delivery
    Sent,
    /// The recipient is not currently connected
    NotConnected,
    /// The channel to the recipient is full (backpressure)
    ChannelFull,
    /// The channel to the recipient is closed
    ChannelClosed,
}

/// Registry for tracking active XMPP connections.
///
/// Thread-safe registry that maps full JIDs to outbound message channels.
/// Uses DashMap for concurrent access without explicit locking.
///
/// ## Usage
///
/// ```ignore
/// let registry = ConnectionRegistry::new();
///
/// // When a connection is established:
/// let (tx, rx) = mpsc::channel(256);
/// registry.register(full_jid.clone(), tx);
///
/// // When routing a message:
/// let result = registry.send_to(&recipient_jid, stanza).await;
///
/// // When a connection closes:
/// registry.unregister(&full_jid);
/// ```
pub struct ConnectionRegistry {
    /// Map of full JID to outbound channel sender
    connections: DashMap<FullJid, mpsc::Sender<OutboundStanza>>,
}

impl ConnectionRegistry {
    /// Create a new connection registry.
    pub fn new() -> Self {
        info!("Creating connection registry");
        Self {
            connections: DashMap::new(),
        }
    }

    /// Register a connection with its outbound channel.
    ///
    /// If a connection with the same JID already exists, it will be replaced.
    /// This handles reconnection scenarios where a client reconnects with
    /// the same resource before the old connection is cleaned up.
    #[instrument(skip(self, sender), fields(jid = %jid))]
    pub fn register(&self, jid: FullJid, sender: mpsc::Sender<OutboundStanza>) {
        let existing = self.connections.insert(jid.clone(), sender);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            debug!("Registered new connection");
        }
    }

    /// Unregister a connection.
    ///
    /// Returns the sender if the connection was registered, None otherwise.
    #[instrument(skip(self), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<mpsc::Sender<OutboundStanza>> {
        let removed = self.connections.remove(jid);
        if removed.is_some() {
            debug!("Unregistered connection");
        } else {
            debug!("Connection was not registered");
        }
        removed.map(|(_, sender)| sender)
    }

    /// Check if a JID is currently connected.
    pub fn is_connected(&self, jid: &FullJid) -> bool {
        self.connections.contains_key(jid)
    }

    /// Get the number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Send a stanza to a connected user.
    ///
    /// Returns the result of the send operation.
    #[instrument(skip(self, stanza), fields(to = %jid))]
    pub async fn send_to(&self, jid: &FullJid, stanza: Stanza) -> SendResult {
        let sender = match self.connections.get(jid) {
            Some(entry) => entry.value().clone(),
            None => {
                debug!("Recipient not connected");
                return SendResult::NotConnected;
            }
        };

        let outbound = OutboundStanza::new(stanza);

        match sender.try_send(outbound) {
            Ok(()) => {
                debug!("Stanza queued for delivery");
                SendResult::Sent
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("Outbound channel full, applying backpressure");
                SendResult::ChannelFull
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                debug!("Outbound channel closed, connection may have dropped");
                // Remove the stale entry
                self.connections.remove(jid);
                SendResult::ChannelClosed
            }
        }
    }

    /// Send a stanza to multiple recipients.
    ///
    /// Returns a vector of (jid, result) pairs for each recipient.
    pub async fn send_to_many<'a, I>(&self, recipients: I, stanza: Stanza) -> Vec<(FullJid, SendResult)>
    where
        I: IntoIterator<Item = &'a FullJid>,
    {
        let mut results = Vec::new();

        for jid in recipients {
            let result = self.send_to(jid, stanza.clone()).await;
            results.push((jid.clone(), result));
        }

        results
    }

    /// List all connected JIDs.
    ///
    /// Useful for debugging and monitoring.
    pub fn list_connections(&self) -> Vec<FullJid> {
        self.connections.iter().map(|r| r.key().clone()).collect()
    }

    /// Remove all stale connections (those with closed channels).
    ///
    /// This can be called periodically to clean up connections that
    /// were not properly unregistered.
    pub fn cleanup_stale(&self) -> usize {
        let mut removed = 0;
        let stale: Vec<FullJid> = self
            .connections
            .iter()
            .filter(|entry| entry.value().is_closed())
            .map(|entry| entry.key().clone())
            .collect();

        for jid in stale {
            if self.connections.remove(&jid).is_some() {
                debug!(jid = %jid, "Removed stale connection");
                removed += 1;
            }
        }

        if removed > 0 {
            info!(count = removed, "Cleaned up stale connections");
        }

        removed
    }
}

impl Default for ConnectionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ConnectionRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectionRegistry")
            .field("connection_count", &self.connections.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::message::{Message, MessageType};
    use jid::Jid;

    fn test_jid(user: &str) -> FullJid {
        format!("{}@example.com/resource", user).parse().unwrap()
    }

    fn make_test_message(to: &str) -> Message {
        let bare_jid: jid::BareJid = to.parse().unwrap();
        let mut msg = Message::new(Some(Jid::from(bare_jid)));
        msg.type_ = MessageType::Chat;
        msg
    }

    #[test]
    fn test_registry_creation() {
        let registry = ConnectionRegistry::new();
        assert_eq!(registry.connection_count(), 0);
    }

    #[test]
    fn test_register_connection() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        assert!(registry.is_connected(&jid));
        assert_eq!(registry.connection_count(), 1);
    }

    #[test]
    fn test_register_replaces_existing() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);

        registry.register(jid.clone(), tx1);
        registry.register(jid.clone(), tx2);

        // Should still only have one connection
        assert_eq!(registry.connection_count(), 1);
    }

    #[test]
    fn test_unregister_connection() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);
        assert!(registry.is_connected(&jid));

        let removed = registry.unregister(&jid);
        assert!(removed.is_some());
        assert!(!registry.is_connected(&jid));
        assert_eq!(registry.connection_count(), 0);
    }

    #[test]
    fn test_unregister_nonexistent() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let removed = registry.unregister(&jid);
        assert!(removed.is_none());
    }

    #[tokio::test]
    async fn test_send_to_connected_user() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, mut rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::Sent));

        // Verify the message was received
        let received = rx.recv().await;
        assert!(received.is_some());
    }

    #[tokio::test]
    async fn test_send_to_disconnected_user() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::NotConnected));
    }

    #[tokio::test]
    async fn test_send_to_closed_channel() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);

        // Drop the receiver to close the channel
        drop(rx);

        let msg = make_test_message("user1@example.com");
        let stanza = Stanza::Message(msg);

        let result = registry.send_to(&jid, stanza).await;
        assert!(matches!(result, SendResult::ChannelClosed));

        // Connection should have been removed
        assert!(!registry.is_connected(&jid));
    }

    #[tokio::test]
    async fn test_send_to_full_channel() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, _rx) = mpsc::channel(1); // Very small buffer

        registry.register(jid.clone(), tx);

        // Fill the channel
        let msg1 = make_test_message("user1@example.com");
        let _ = registry.send_to(&jid, Stanza::Message(msg1)).await;

        // This should hit backpressure
        let msg2 = make_test_message("user1@example.com");
        let result = registry.send_to(&jid, Stanza::Message(msg2)).await;
        assert!(matches!(result, SendResult::ChannelFull));
    }

    #[test]
    fn test_list_connections() {
        let registry = ConnectionRegistry::new();

        let jid1 = test_jid("user1");
        let jid2 = test_jid("user2");

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);

        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        let connections = registry.list_connections();
        assert_eq!(connections.len(), 2);
        assert!(connections.contains(&jid1));
        assert!(connections.contains(&jid2));
    }

    #[test]
    fn test_cleanup_stale() {
        let registry = ConnectionRegistry::new();
        let jid = test_jid("user1");
        let (tx, rx) = mpsc::channel(16);

        registry.register(jid.clone(), tx);
        assert!(registry.is_connected(&jid));

        // Drop the receiver to make the channel stale
        drop(rx);

        let removed = registry.cleanup_stale();
        assert_eq!(removed, 1);
        assert!(!registry.is_connected(&jid));
    }

    #[tokio::test]
    async fn test_send_to_many() {
        let registry = ConnectionRegistry::new();

        let jid1 = test_jid("user1");
        let jid2 = test_jid("user2");
        let jid3 = test_jid("user3"); // Not registered

        let (tx1, mut rx1) = mpsc::channel(16);
        let (tx2, mut rx2) = mpsc::channel(16);

        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        let msg = make_test_message("room@muc.example.com");
        let stanza = Stanza::Message(msg);

        let recipients = vec![&jid1, &jid2, &jid3];
        let results = registry.send_to_many(recipients, stanza).await;

        assert_eq!(results.len(), 3);

        // Check results
        let result_map: std::collections::HashMap<_, _> = results.into_iter().collect();
        assert!(matches!(result_map.get(&jid1), Some(SendResult::Sent)));
        assert!(matches!(result_map.get(&jid2), Some(SendResult::Sent)));
        assert!(matches!(result_map.get(&jid3), Some(SendResult::NotConnected)));

        // Verify messages were received
        assert!(rx1.recv().await.is_some());
        assert!(rx2.recv().await.is_some());
    }
}
