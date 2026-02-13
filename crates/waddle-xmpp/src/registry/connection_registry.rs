//! Connection Registry implementation.
//!
//! Tracks active XMPP connections by their full JID for message routing.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use jid::{BareJid, FullJid};
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

/// Connection state stored in the registry.
///
/// Contains the outbound sender and shared state that can be queried
/// by the registry (like carbons_enabled status for XEP-0280).
#[derive(Debug)]
pub struct ConnectionEntry {
    /// Channel to send stanzas to this connection
    pub sender: mpsc::Sender<OutboundStanza>,
    /// Whether XEP-0280 Message Carbons is enabled for this connection
    pub carbons_enabled: Arc<AtomicBool>,
    /// Whether this resource is currently available (presence type != unavailable)
    pub presence_available: Arc<AtomicBool>,
    /// Last advertised priority for this resource (-128..127)
    pub presence_priority: Arc<std::sync::atomic::AtomicI8>,
}

impl ConnectionEntry {
    /// Create a new connection entry with carbons disabled by default.
    pub fn new(sender: mpsc::Sender<OutboundStanza>) -> Self {
        Self {
            sender,
            carbons_enabled: Arc::new(AtomicBool::new(false)),
            presence_available: Arc::new(AtomicBool::new(false)),
            presence_priority: Arc::new(std::sync::atomic::AtomicI8::new(0)),
        }
    }

    /// Get the carbons_enabled handle for this connection.
    ///
    /// The returned Arc can be used by the ConnectionActor to update
    /// the carbons status when enable/disable IQs are received.
    pub fn carbons_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.carbons_enabled)
    }

    /// Check if carbons is enabled for this connection.
    pub fn is_carbons_enabled(&self) -> bool {
        self.carbons_enabled.load(Ordering::Relaxed)
    }

    /// Check if this resource is currently available.
    pub fn is_presence_available(&self) -> bool {
        self.presence_available.load(Ordering::Relaxed)
    }

    /// Get the last advertised presence priority.
    pub fn presence_priority(&self) -> i8 {
        self.presence_priority.load(Ordering::Relaxed)
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
/// Thread-safe registry that maps full JIDs to connection entries.
/// Uses DashMap for concurrent access without explicit locking.
///
/// ## Usage
///
/// ```ignore
/// let registry = ConnectionRegistry::new();
///
/// // When a connection is established:
/// let (tx, rx) = mpsc::channel(256);
/// let carbons_handle = registry.register(full_jid.clone(), tx);
///
/// // The connection can update carbons_handle when enable/disable IQs are received
/// carbons_handle.store(true, Ordering::Relaxed);
///
/// // When routing a message:
/// let result = registry.send_to(&recipient_jid, stanza).await;
///
/// // When a connection closes:
/// registry.unregister(&full_jid);
/// ```
pub struct ConnectionRegistry {
    /// Map of full JID to connection entry (includes sender and carbons status)
    connections: DashMap<FullJid, ConnectionEntry>,
    /// Pending subscription stanzas for offline users (RFC 6121).
    pending_subscription_stanzas: DashMap<BareJid, Vec<Stanza>>,
}

impl ConnectionRegistry {
    /// Create a new connection registry.
    pub fn new() -> Self {
        info!("Creating connection registry");
        Self {
            connections: DashMap::new(),
            pending_subscription_stanzas: DashMap::new(),
        }
    }

    /// Register a connection with its outbound channel.
    ///
    /// Returns a handle to the carbons_enabled flag that the ConnectionActor
    /// can use to update the carbons status when enable/disable IQs are received.
    ///
    /// If a connection with the same JID already exists, it will be replaced.
    /// This handles reconnection scenarios where a client reconnects with
    /// the same resource before the old connection is cleaned up.
    #[instrument(skip(self, sender), fields(jid = %jid))]
    pub fn register(&self, jid: FullJid, sender: mpsc::Sender<OutboundStanza>) -> Arc<AtomicBool> {
        let entry = ConnectionEntry::new(sender);
        let carbons_handle = entry.carbons_handle();
        let existing = self.connections.insert(jid.clone(), entry);
        if existing.is_some() {
            debug!("Replaced existing connection registration");
        } else {
            debug!("Registered new connection");
        }
        carbons_handle
    }

    /// Unregister a connection.
    ///
    /// Returns the connection entry if the connection was registered, None otherwise.
    #[instrument(skip(self), fields(jid = %jid))]
    pub fn unregister(&self, jid: &FullJid) -> Option<ConnectionEntry> {
        let removed = self.connections.remove(jid);
        if removed.is_some() {
            debug!("Unregistered connection");
        } else {
            debug!("Connection was not registered");
        }
        removed.map(|(_, entry)| entry)
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
            Some(entry) => entry.value().sender.clone(),
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
    pub async fn send_to_many<'a, I>(
        &self,
        recipients: I,
        stanza: Stanza,
    ) -> Vec<(FullJid, SendResult)>
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

    /// Get all connected resources for a bare JID, excluding a specific full JID.
    ///
    /// Used by message carbons to find other connected clients for the same user.
    /// Returns all full JIDs that match the bare JID except the excluded one.
    pub fn get_other_resources_for_user(
        &self,
        bare_jid: &BareJid,
        exclude_jid: &FullJid,
    ) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| {
                let jid = entry.key();
                // Match bare JID but exclude the specific full JID
                jid.to_bare() == *bare_jid && jid != exclude_jid
            })
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get all connected resources for a bare JID.
    ///
    /// Returns all full JIDs that match the given bare JID.
    /// Used for routing messages to all connected clients of a user.
    pub fn get_resources_for_user(&self, bare_jid: &BareJid) -> Vec<FullJid> {
        self.connections
            .iter()
            .filter(|entry| entry.key().to_bare() == *bare_jid)
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Queue a subscription stanza for an offline bare JID.
    ///
    /// These stanzas are delivered when the user next becomes available.
    pub fn queue_pending_subscription_stanza(&self, bare_jid: &BareJid, stanza: Stanza) {
        self.pending_subscription_stanzas
            .entry(bare_jid.clone())
            .or_default()
            .push(stanza);
    }

    /// Drain and return all pending subscription stanzas for a bare JID.
    pub fn drain_pending_subscription_stanzas(&self, bare_jid: &BareJid) -> Vec<Stanza> {
        self.pending_subscription_stanzas
            .remove(bare_jid)
            .map(|(_, stanzas)| stanzas)
            .unwrap_or_default()
    }

    /// Update presence state for a connected resource.
    ///
    /// Returns true if the resource was found and updated.
    pub fn update_presence(&self, jid: &FullJid, available: bool, priority: i8) -> bool {
        if let Some(entry) = self.connections.get(jid) {
            entry
                .value()
                .presence_available
                .store(available, Ordering::Relaxed);
            entry
                .value()
                .presence_priority
                .store(priority, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    /// Get all available resources for a bare JID with their priorities.
    pub fn get_available_resources_for_user(&self, bare_jid: &BareJid) -> Vec<(FullJid, i8)> {
        self.connections
            .iter()
            .filter(|entry| {
                entry.key().to_bare() == *bare_jid && entry.value().is_presence_available()
            })
            .map(|entry| (entry.key().clone(), entry.value().presence_priority()))
            .collect()
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
            .filter(|entry| entry.value().sender.is_closed())
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
    use jid::Jid;
    use xmpp_parsers::message::{Message, MessageType};

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
        assert!(matches!(
            result_map.get(&jid3),
            Some(SendResult::NotConnected)
        ));

        // Verify messages were received
        assert!(rx1.recv().await.is_some());
        assert!(rx2.recv().await.is_some());
    }

    #[test]
    fn test_update_presence_and_get_available_resources() {
        let registry = ConnectionRegistry::new();

        let jid1: FullJid = "user@example.com/one".parse().unwrap();
        let jid2: FullJid = "user@example.com/two".parse().unwrap();
        let bare: BareJid = "user@example.com".parse().unwrap();

        let (tx1, _rx1) = mpsc::channel(16);
        let (tx2, _rx2) = mpsc::channel(16);
        registry.register(jid1.clone(), tx1);
        registry.register(jid2.clone(), tx2);

        // Default is unavailable until initial presence is sent.
        assert!(registry.get_available_resources_for_user(&bare).is_empty());

        assert!(registry.update_presence(&jid1, true, 5));
        assert!(registry.update_presence(&jid2, true, -1));

        let mut resources = registry.get_available_resources_for_user(&bare);
        resources.sort_by(|a, b| a.0.to_string().cmp(&b.0.to_string()));
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].0, jid1);
        assert_eq!(resources[0].1, 5);
        assert_eq!(resources[1].0, jid2);
        assert_eq!(resources[1].1, -1);
    }

    #[test]
    fn test_update_presence_missing_jid_returns_false() {
        let registry = ConnectionRegistry::new();
        let missing: FullJid = "missing@example.com/resource".parse().unwrap();
        assert!(!registry.update_presence(&missing, true, 1));
    }

    #[test]
    fn test_queue_and_drain_pending_subscription_stanzas() {
        let registry = ConnectionRegistry::new();
        let bare: BareJid = "user@example.com".parse().unwrap();

        let mut subscribe = xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::Subscribe,
        );
        subscribe.to = Some(jid::Jid::from(bare.clone()));

        let mut unsubscribed = xmpp_parsers::presence::Presence::new(
            xmpp_parsers::presence::Type::Unsubscribed,
        );
        unsubscribed.to = Some(jid::Jid::from(bare.clone()));

        registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(subscribe));
        registry.queue_pending_subscription_stanza(&bare, Stanza::Presence(unsubscribed));

        let drained = registry.drain_pending_subscription_stanzas(&bare);
        assert_eq!(drained.len(), 2);
        assert!(matches!(&drained[0], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Subscribe));
        assert!(matches!(&drained[1], Stanza::Presence(p) if p.type_ == xmpp_parsers::presence::Type::Unsubscribed));

        // Draining again should be empty.
        assert!(registry
            .drain_pending_subscription_stanzas(&bare)
            .is_empty());
    }
}
