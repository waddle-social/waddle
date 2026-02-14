//! MUC Federation Routing
//!
//! This module provides types and functions for routing MUC presence updates
//! and messages to federated occupants via S2S (server-to-server) connections.
//!
//! ## Overview
//!
//! When a user joins, leaves, or updates their presence in a MUC room that has
//! occupants from remote servers, presence must be routed:
//! - **Locally**: Via direct ConnectionRegistry delivery to C2S connections
//! - **Remotely**: Via S2S connections to other XMPP servers
//!
//! Similarly, when a user sends a message to a MUC room, the message must be
//! broadcast to all occupants, routing appropriately based on whether they
//! are local or remote.
//!
//! This module provides `FederatedPresenceSet` and `FederatedMessageSet` types
//! which group stanzas by their delivery mechanism, making it easy to route them correctly.

use std::collections::HashMap;

use jid::{BareJid, FullJid, Jid};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::presence::Presence;

use super::{
    build_leave_presence, build_occupant_presence, MucRoom, Occupant, OutboundMucMessage,
    OutboundMucPresence,
};
use crate::types::{Affiliation, Role};

/// Represents the delivery target for MUC presence stanzas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresenceDeliveryTarget {
    /// Deliver via local C2S connection (ConnectionRegistry)
    Local,
    /// Deliver via S2S to the specified remote domain
    Remote(String),
}

impl PresenceDeliveryTarget {
    /// Returns true if this is a local delivery target.
    pub fn is_local(&self) -> bool {
        matches!(self, PresenceDeliveryTarget::Local)
    }

    /// Returns true if this is a remote delivery target.
    pub fn is_remote(&self) -> bool {
        matches!(self, PresenceDeliveryTarget::Remote(_))
    }

    /// Get the remote domain if this is a remote target.
    pub fn remote_domain(&self) -> Option<&str> {
        match self {
            PresenceDeliveryTarget::Local => None,
            PresenceDeliveryTarget::Remote(domain) => Some(domain),
        }
    }
}

/// An outbound presence stanza with its delivery target.
#[derive(Debug, Clone)]
pub struct FederatedPresence {
    /// The delivery target (local or remote domain)
    pub target: PresenceDeliveryTarget,
    /// The recipient's full JID
    pub to: FullJid,
    /// The presence stanza to deliver
    pub presence: Presence,
}

impl FederatedPresence {
    /// Create a new federated presence for local delivery.
    pub fn local(to: FullJid, presence: Presence) -> Self {
        Self {
            target: PresenceDeliveryTarget::Local,
            to,
            presence,
        }
    }

    /// Create a new federated presence for remote delivery.
    pub fn remote(domain: String, to: FullJid, presence: Presence) -> Self {
        Self {
            target: PresenceDeliveryTarget::Remote(domain),
            to,
            presence,
        }
    }

    /// Convert to OutboundMucPresence (for local delivery).
    pub fn into_outbound_presence(self) -> OutboundMucPresence {
        OutboundMucPresence::new(self.to, self.presence)
    }
}

/// A set of presence stanzas grouped by delivery target.
///
/// This is the result of `MucRoom::broadcast_presence_federated()` and groups
/// presence stanzas so they can be efficiently routed:
/// - Local stanzas go through the ConnectionRegistry
/// - Remote stanzas are batched by domain and sent via S2S
#[derive(Debug, Clone, Default)]
pub struct FederatedPresenceSet {
    /// Presence stanzas for local occupants (deliver via C2S)
    pub local: Vec<OutboundMucPresence>,
    /// Presence stanzas for remote occupants, grouped by domain (deliver via S2S)
    pub remote: HashMap<String, Vec<OutboundMucPresence>>,
}

impl FederatedPresenceSet {
    /// Create a new empty presence set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a presence for local delivery.
    pub fn add_local(&mut self, presence: OutboundMucPresence) {
        self.local.push(presence);
    }

    /// Add a presence for remote delivery to a specific domain.
    pub fn add_remote(&mut self, domain: String, presence: OutboundMucPresence) {
        self.remote.entry(domain).or_default().push(presence);
    }

    /// Get the total number of presence stanzas.
    pub fn total_count(&self) -> usize {
        self.local.len() + self.remote.values().map(|v| v.len()).sum::<usize>()
    }

    /// Get the number of local presence stanzas.
    pub fn local_count(&self) -> usize {
        self.local.len()
    }

    /// Get the number of remote domains.
    pub fn remote_domain_count(&self) -> usize {
        self.remote.len()
    }

    /// Get the total number of remote presence stanzas.
    pub fn remote_count(&self) -> usize {
        self.remote.values().map(|v| v.len()).sum()
    }

    /// Check if there are any presence stanzas to deliver.
    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.remote.is_empty()
    }

    /// Get all remote domains that need S2S delivery.
    pub fn remote_domains(&self) -> Vec<&String> {
        self.remote.keys().collect()
    }

    /// Get presence stanzas for a specific remote domain.
    pub fn get_remote(&self, domain: &str) -> Option<&Vec<OutboundMucPresence>> {
        self.remote.get(domain)
    }

    /// Iterate over all federated presences (both local and remote).
    pub fn iter(&self) -> impl Iterator<Item = FederatedPresence> + '_ {
        let local_iter = self.local.iter().map(|p| FederatedPresence {
            target: PresenceDeliveryTarget::Local,
            to: p.to.clone(),
            presence: p.presence.clone(),
        });

        let remote_iter = self.remote.iter().flat_map(|(domain, presences)| {
            presences.iter().map(move |p| FederatedPresence {
                target: PresenceDeliveryTarget::Remote(domain.clone()),
                to: p.to.clone(),
                presence: p.presence.clone(),
            })
        });

        local_iter.chain(remote_iter)
    }
}

// =============================================================================
// Federated Message Routing
// =============================================================================

/// Represents the delivery target for MUC message stanzas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageDeliveryTarget {
    /// Deliver via local C2S connection (ConnectionRegistry)
    Local,
    /// Deliver via S2S to the specified remote domain
    Remote(String),
}

impl MessageDeliveryTarget {
    /// Returns true if this is a local delivery target.
    pub fn is_local(&self) -> bool {
        matches!(self, MessageDeliveryTarget::Local)
    }

    /// Returns true if this is a remote delivery target.
    pub fn is_remote(&self) -> bool {
        matches!(self, MessageDeliveryTarget::Remote(_))
    }

    /// Get the remote domain if this is a remote target.
    pub fn remote_domain(&self) -> Option<&str> {
        match self {
            MessageDeliveryTarget::Local => None,
            MessageDeliveryTarget::Remote(domain) => Some(domain),
        }
    }
}

/// An outbound message stanza with its delivery target.
#[derive(Debug, Clone)]
pub struct FederatedMessage {
    /// The delivery target (local or remote domain)
    pub target: MessageDeliveryTarget,
    /// The recipient's full JID
    pub to: FullJid,
    /// The message stanza to deliver
    pub message: Message,
}

impl FederatedMessage {
    /// Create a new federated message for local delivery.
    pub fn local(to: FullJid, message: Message) -> Self {
        Self {
            target: MessageDeliveryTarget::Local,
            to,
            message,
        }
    }

    /// Create a new federated message for remote delivery.
    pub fn remote(domain: String, to: FullJid, message: Message) -> Self {
        Self {
            target: MessageDeliveryTarget::Remote(domain),
            to,
            message,
        }
    }

    /// Convert to OutboundMucMessage (for local delivery).
    pub fn into_outbound_message(self) -> OutboundMucMessage {
        OutboundMucMessage::new(self.to, self.message)
    }
}

/// A set of message stanzas grouped by delivery target.
///
/// This is the result of `MucRoom::broadcast_message_federated()` and groups
/// message stanzas so they can be efficiently routed:
/// - Local stanzas go through the ConnectionRegistry
/// - Remote stanzas are batched by domain and sent via S2S
#[derive(Debug, Clone, Default)]
pub struct FederatedMessageSet {
    /// Message stanzas for local occupants (deliver via C2S)
    pub local: Vec<OutboundMucMessage>,
    /// Message stanzas for remote occupants, grouped by domain (deliver via S2S)
    pub remote: HashMap<String, Vec<OutboundMucMessage>>,
}

impl FederatedMessageSet {
    /// Create a new empty message set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a message for local delivery.
    pub fn add_local(&mut self, message: OutboundMucMessage) {
        self.local.push(message);
    }

    /// Add a message for remote delivery to a specific domain.
    pub fn add_remote(&mut self, domain: String, message: OutboundMucMessage) {
        self.remote.entry(domain).or_default().push(message);
    }

    /// Get the total number of message stanzas.
    pub fn total_count(&self) -> usize {
        self.local.len() + self.remote.values().map(|v| v.len()).sum::<usize>()
    }

    /// Get the number of local message stanzas.
    pub fn local_count(&self) -> usize {
        self.local.len()
    }

    /// Get the number of remote domains.
    pub fn remote_domain_count(&self) -> usize {
        self.remote.len()
    }

    /// Get the total number of remote message stanzas.
    pub fn remote_count(&self) -> usize {
        self.remote.values().map(|v| v.len()).sum()
    }

    /// Check if there are any message stanzas to deliver.
    pub fn is_empty(&self) -> bool {
        self.local.is_empty() && self.remote.is_empty()
    }

    /// Get all remote domains that need S2S delivery.
    pub fn remote_domains(&self) -> Vec<&String> {
        self.remote.keys().collect()
    }

    /// Get message stanzas for a specific remote domain.
    pub fn get_remote(&self, domain: &str) -> Option<&Vec<OutboundMucMessage>> {
        self.remote.get(domain)
    }

    /// Iterate over all federated messages (both local and remote).
    pub fn iter(&self) -> impl Iterator<Item = FederatedMessage> + '_ {
        let local_iter = self.local.iter().map(|m| FederatedMessage {
            target: MessageDeliveryTarget::Local,
            to: m.to.clone(),
            message: m.message.clone(),
        });

        let remote_iter = self.remote.iter().flat_map(|(domain, messages)| {
            messages.iter().map(move |m| FederatedMessage {
                target: MessageDeliveryTarget::Remote(domain.clone()),
                to: m.to.clone(),
                message: m.message.clone(),
            })
        });

        local_iter.chain(remote_iter)
    }
}

/// Build a message stanza for S2S delivery to a remote MUC occupant.
///
/// This creates a properly addressed message stanza for delivery via S2S.
/// The 'from' is the room JID with the sender's nick, and the 'to' is the
/// remote occupant's real JID.
///
/// # Arguments
/// * `room_jid` - The room's bare JID
/// * `sender_nick` - The nickname of the occupant sending the message
/// * `to_occupant` - The remote occupant receiving the message
/// * `original_message` - The original message to broadcast
///
/// # Returns
/// An `OutboundMucMessage` properly addressed for S2S delivery
pub fn build_s2s_muc_message(
    room_jid: &BareJid,
    sender_nick: &str,
    to_occupant: &Occupant,
    original_message: &Message,
) -> Result<OutboundMucMessage, jid::Error> {
    let from_room_jid = room_jid.with_resource_str(sender_nick)?;

    let mut broadcast_msg = original_message.clone();
    broadcast_msg.type_ = MessageType::Groupchat;
    broadcast_msg.from = Some(Jid::from(from_room_jid));
    broadcast_msg.to = Some(Jid::from(to_occupant.real_jid.clone()));

    Ok(OutboundMucMessage::new(
        to_occupant.real_jid.clone(),
        broadcast_msg,
    ))
}

/// Build a presence stanza for a remote occupant via S2S.
///
/// This creates a properly addressed presence stanza for delivery via S2S.
/// The 'from' is the room JID with the announcing occupant's nick, and
/// the 'to' is the remote occupant's real JID.
///
/// # Arguments
/// * `room_jid` - The room's bare JID
/// * `from_nick` - The nickname of the occupant being announced
/// * `to_occupant` - The remote occupant receiving the presence
/// * `affiliation` - Affiliation of the announced occupant
/// * `role` - Role of the announced occupant
/// * `is_self` - True if the presence is about the recipient themselves
/// * `real_jid` - Optional real JID to include (for semi-anonymous rooms)
pub fn build_s2s_occupant_presence(
    room_jid: &BareJid,
    from_nick: &str,
    to_occupant: &Occupant,
    affiliation: Affiliation,
    role: Role,
    is_self: bool,
    real_jid: Option<&FullJid>,
) -> Result<OutboundMucPresence, jid::Error> {
    let from_room_jid = room_jid.with_resource_str(from_nick)?;

    let presence = build_occupant_presence(
        &from_room_jid,
        &to_occupant.real_jid,
        affiliation,
        role,
        is_self,
        real_jid,
    );

    Ok(OutboundMucPresence::new(
        to_occupant.real_jid.clone(),
        presence,
    ))
}

/// Build a leave presence stanza for a remote occupant via S2S.
///
/// Creates an unavailable presence for delivery to a remote occupant
/// when someone leaves the room.
///
/// # Arguments
/// * `room_jid` - The room's bare JID
/// * `leaving_nick` - The nickname of the occupant leaving
/// * `to_occupant` - The remote occupant receiving the presence
/// * `affiliation` - Affiliation of the leaving occupant
/// * `is_self` - True if the presence is about the recipient themselves
pub fn build_s2s_leave_presence(
    room_jid: &BareJid,
    leaving_nick: &str,
    to_occupant: &Occupant,
    affiliation: Affiliation,
    is_self: bool,
) -> Result<OutboundMucPresence, jid::Error> {
    let from_room_jid = room_jid.with_resource_str(leaving_nick)?;

    let presence =
        build_leave_presence(&from_room_jid, &to_occupant.real_jid, affiliation, is_self);

    Ok(OutboundMucPresence::new(
        to_occupant.real_jid.clone(),
        presence,
    ))
}

impl MucRoom {
    /// Broadcast presence to all occupants, grouped by delivery target.
    ///
    /// This is the main entry point for federated presence broadcasting.
    /// It returns a `FederatedPresenceSet` which groups presence stanzas
    /// by their delivery mechanism:
    /// - Local presences should be delivered via ConnectionRegistry
    /// - Remote presences should be delivered via S2S, batched by domain
    ///
    /// # Arguments
    /// * `occupant_nick` - The nickname of the occupant whose presence is being broadcast
    /// * `affiliation` - The occupant's affiliation
    /// * `role` - The occupant's role
    /// * `include_real_jid` - Whether to include the real JID (for non-anonymous rooms)
    ///
    /// # Returns
    /// A `FederatedPresenceSet` with presence stanzas grouped for delivery
    pub fn broadcast_presence_federated(
        &self,
        occupant_nick: &str,
        affiliation: Affiliation,
        role: Role,
        include_real_jid: bool,
    ) -> FederatedPresenceSet {
        let mut result = FederatedPresenceSet::new();

        // Get the occupant being announced
        let announcing_occupant = match self.occupants.get(occupant_nick) {
            Some(occ) => occ,
            None => return result, // Occupant not found, nothing to broadcast
        };

        // Build the from JID: room@domain/nick
        let from_room_jid = match self.room_jid.with_resource_str(occupant_nick) {
            Ok(jid) => jid,
            Err(_) => return result, // Invalid nick
        };

        // The real JID to include in the presence (if room is not anonymous)
        let real_jid = if include_real_jid {
            Some(&announcing_occupant.real_jid)
        } else {
            None
        };

        // Build presence for each occupant
        for recipient in self.occupants.values() {
            let is_self = recipient.nick == occupant_nick;

            let presence = build_occupant_presence(
                &from_room_jid,
                &recipient.real_jid,
                affiliation,
                role,
                is_self,
                real_jid.cloned().as_ref(),
            );

            let outbound = OutboundMucPresence::new(recipient.real_jid.clone(), presence);

            // Route based on whether recipient is local or remote
            if recipient.is_remote {
                let domain = recipient
                    .home_server
                    .clone()
                    .unwrap_or_else(|| recipient.real_jid.domain().as_str().to_string());
                result.add_remote(domain, outbound);
            } else {
                result.add_local(outbound);
            }
        }

        result
    }

    /// Broadcast leave presence to all remaining occupants.
    ///
    /// This should be called when an occupant leaves the room. It creates
    /// unavailable presence stanzas for all remaining occupants, grouped
    /// by delivery target.
    ///
    /// # Arguments
    /// * `leaving_nick` - The nickname of the occupant who is leaving
    /// * `affiliation` - The leaving occupant's affiliation
    ///
    /// # Returns
    /// A `FederatedPresenceSet` with leave presence stanzas grouped for delivery
    pub fn broadcast_leave_presence_federated(
        &self,
        leaving_nick: &str,
        affiliation: Affiliation,
    ) -> FederatedPresenceSet {
        let mut result = FederatedPresenceSet::new();

        // Build the from JID: room@domain/leaving_nick
        let from_room_jid = match self.room_jid.with_resource_str(leaving_nick) {
            Ok(jid) => jid,
            Err(_) => return result,
        };

        // Build leave presence for each remaining occupant
        for recipient in self.occupants.values() {
            // Skip the leaving occupant themselves - they'll get special self-presence
            if recipient.nick == leaving_nick {
                continue;
            }

            let presence = build_leave_presence(
                &from_room_jid,
                &recipient.real_jid,
                affiliation,
                false, // not self
            );

            let outbound = OutboundMucPresence::new(recipient.real_jid.clone(), presence);

            if recipient.is_remote {
                let domain = recipient
                    .home_server
                    .clone()
                    .unwrap_or_else(|| recipient.real_jid.domain().as_str().to_string());
                result.add_remote(domain, outbound);
            } else {
                result.add_local(outbound);
            }
        }

        result
    }

    /// Build self-presence for a user who is leaving.
    ///
    /// This creates the special presence that goes back to the user who
    /// initiated the leave, with status code 110 (self-presence).
    ///
    /// # Arguments
    /// * `leaving_jid` - The full JID of the user leaving
    /// * `nick` - Their nickname in the room
    /// * `affiliation` - Their affiliation
    ///
    /// # Returns
    /// An `OutboundMucPresence` with the self-leave presence
    pub fn build_self_leave_presence(
        &self,
        leaving_jid: &FullJid,
        nick: &str,
        affiliation: Affiliation,
    ) -> Result<OutboundMucPresence, jid::Error> {
        let from_room_jid = self.room_jid.with_resource_str(nick)?;

        let presence = build_leave_presence(&from_room_jid, leaving_jid, affiliation, true);

        Ok(OutboundMucPresence::new(leaving_jid.clone(), presence))
    }

    // === Federated Message Broadcasting ===

    /// Broadcast a message to all occupants, grouped by delivery target.
    ///
    /// This is the main entry point for federated message broadcasting.
    /// It returns a `FederatedMessageSet` which groups message stanzas
    /// by their delivery mechanism:
    /// - Local messages should be delivered via ConnectionRegistry
    /// - Remote messages should be delivered via S2S, batched by domain
    ///
    /// Unlike the non-federated `broadcast_message()`, this method does not
    /// return an error for permission checks - the caller should verify
    /// permissions before calling this method.
    ///
    /// # Arguments
    /// * `sender_nick` - The nickname of the occupant sending the message
    /// * `message` - The original message to broadcast
    ///
    /// # Returns
    /// A `FederatedMessageSet` with message stanzas grouped for delivery.
    /// Returns an empty set if the sender is not found.
    pub fn broadcast_message_federated(
        &self,
        sender_nick: &str,
        message: &Message,
    ) -> FederatedMessageSet {
        let mut result = FederatedMessageSet::new();

        // Verify sender is an occupant
        if self.occupants.get(sender_nick).is_none() {
            return result; // Sender not found, nothing to broadcast
        }

        // Build the 'from' JID: room@domain/sender_nick
        let from_room_jid = match self.room_jid.with_resource_str(sender_nick) {
            Ok(jid) => jid,
            Err(_) => return result, // Invalid nick
        };

        // Build message for each occupant
        for recipient in self.occupants.values() {
            let mut broadcast_msg = message.clone();
            broadcast_msg.type_ = MessageType::Groupchat;
            broadcast_msg.from = Some(Jid::from(from_room_jid.clone()));
            broadcast_msg.to = Some(Jid::from(recipient.real_jid.clone()));

            let outbound = OutboundMucMessage::new(recipient.real_jid.clone(), broadcast_msg);

            // Route based on whether recipient is local or remote
            if recipient.is_remote {
                let domain = recipient
                    .home_server
                    .clone()
                    .unwrap_or_else(|| recipient.real_jid.domain().as_str().to_string());
                result.add_remote(domain, outbound);
            } else {
                result.add_local(outbound);
            }
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::muc::RoomConfig;
    use xmpp_parsers::message::Body;
    use xmpp_parsers::presence::Type as PresenceType;

    fn create_test_room() -> MucRoom {
        MucRoom::new(
            "testroom@muc.example.com".parse().unwrap(),
            "test-waddle-id".to_string(),
            "test-channel-id".to_string(),
            RoomConfig::default(),
        )
    }

    fn add_local_occupant(room: &mut MucRoom, nick: &str, jid: &str) {
        room.add_occupant(Occupant {
            real_jid: jid.parse().unwrap(),
            nick: nick.to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: false,
            home_server: None,
        });
    }

    fn add_remote_occupant(room: &mut MucRoom, nick: &str, jid: &str, home_server: &str) {
        room.add_occupant(Occupant {
            real_jid: jid.parse().unwrap(),
            nick: nick.to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some(home_server.to_string()),
        });
    }

    #[test]
    fn test_presence_delivery_target() {
        let local = PresenceDeliveryTarget::Local;
        assert!(local.is_local());
        assert!(!local.is_remote());
        assert_eq!(local.remote_domain(), None);

        let remote = PresenceDeliveryTarget::Remote("remote.example.com".to_string());
        assert!(!remote.is_local());
        assert!(remote.is_remote());
        assert_eq!(remote.remote_domain(), Some("remote.example.com"));
    }

    #[test]
    fn test_federated_presence_set_empty() {
        let set = FederatedPresenceSet::new();
        assert!(set.is_empty());
        assert_eq!(set.total_count(), 0);
        assert_eq!(set.local_count(), 0);
        assert_eq!(set.remote_count(), 0);
        assert_eq!(set.remote_domain_count(), 0);
    }

    #[test]
    fn test_federated_presence_set_local_only() {
        let mut set = FederatedPresenceSet::new();

        let to: FullJid = "user@local.example.com/res".parse().unwrap();
        let presence = Presence::new(PresenceType::None);
        let outbound = OutboundMucPresence::new(to, presence);

        set.add_local(outbound);

        assert!(!set.is_empty());
        assert_eq!(set.total_count(), 1);
        assert_eq!(set.local_count(), 1);
        assert_eq!(set.remote_count(), 0);
    }

    #[test]
    fn test_federated_presence_set_remote_only() {
        let mut set = FederatedPresenceSet::new();

        let to: FullJid = "user@remote.example.com/res".parse().unwrap();
        let presence = Presence::new(PresenceType::None);
        let outbound = OutboundMucPresence::new(to, presence);

        set.add_remote("remote.example.com".to_string(), outbound);

        assert!(!set.is_empty());
        assert_eq!(set.total_count(), 1);
        assert_eq!(set.local_count(), 0);
        assert_eq!(set.remote_count(), 1);
        assert_eq!(set.remote_domain_count(), 1);
        assert!(set
            .remote_domains()
            .contains(&&"remote.example.com".to_string()));
    }

    #[test]
    fn test_federated_presence_set_mixed() {
        let mut set = FederatedPresenceSet::new();

        // Add local
        let local_to: FullJid = "user1@local.example.com/res".parse().unwrap();
        set.add_local(OutboundMucPresence::new(
            local_to,
            Presence::new(PresenceType::None),
        ));

        // Add remote from domain A
        let remote_a: FullJid = "user2@remote-a.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-a.example.com".to_string(),
            OutboundMucPresence::new(remote_a, Presence::new(PresenceType::None)),
        );

        // Add remote from domain B
        let remote_b: FullJid = "user3@remote-b.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-b.example.com".to_string(),
            OutboundMucPresence::new(remote_b, Presence::new(PresenceType::None)),
        );

        // Add another from domain A
        let remote_a2: FullJid = "user4@remote-a.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-a.example.com".to_string(),
            OutboundMucPresence::new(remote_a2, Presence::new(PresenceType::None)),
        );

        assert_eq!(set.total_count(), 4);
        assert_eq!(set.local_count(), 1);
        assert_eq!(set.remote_count(), 3);
        assert_eq!(set.remote_domain_count(), 2);
        assert_eq!(
            set.get_remote("remote-a.example.com").map(|v| v.len()),
            Some(2)
        );
        assert_eq!(
            set.get_remote("remote-b.example.com").map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn test_broadcast_presence_federated_local_only() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_local_occupant(&mut room, "bob", "bob@example.com/mobile");

        let result = room.broadcast_presence_federated(
            "alice",
            Affiliation::Member,
            Role::Participant,
            false,
        );

        // Both occupants should receive presence (including alice herself)
        assert_eq!(result.total_count(), 2);
        assert_eq!(result.local_count(), 2);
        assert_eq!(result.remote_count(), 0);
    }

    #[test]
    fn test_broadcast_presence_federated_mixed_occupants() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_local_occupant(&mut room, "bob", "bob@example.com/mobile");
        add_remote_occupant(
            &mut room,
            "charlie",
            "charlie@remote.example.org/client",
            "remote.example.org",
        );
        add_remote_occupant(
            &mut room,
            "diana",
            "diana@other.example.net/app",
            "other.example.net",
        );
        add_remote_occupant(
            &mut room,
            "eve",
            "eve@remote.example.org/phone",
            "remote.example.org",
        );

        let result = room.broadcast_presence_federated(
            "alice",
            Affiliation::Member,
            Role::Participant,
            false,
        );

        // 5 total occupants receive presence
        assert_eq!(result.total_count(), 5);
        // 2 local (alice and bob)
        assert_eq!(result.local_count(), 2);
        // 3 remote across 2 domains
        assert_eq!(result.remote_count(), 3);
        assert_eq!(result.remote_domain_count(), 2);
        // 2 occupants on remote.example.org (charlie and eve)
        assert_eq!(
            result.get_remote("remote.example.org").map(|v| v.len()),
            Some(2)
        );
        // 1 occupant on other.example.net (diana)
        assert_eq!(
            result.get_remote("other.example.net").map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn test_broadcast_leave_presence_federated() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_local_occupant(&mut room, "bob", "bob@example.com/mobile");
        add_remote_occupant(
            &mut room,
            "charlie",
            "charlie@remote.example.org/client",
            "remote.example.org",
        );

        let result = room.broadcast_leave_presence_federated("alice", Affiliation::Member);

        // Leave should go to bob and charlie, but not alice herself
        assert_eq!(result.total_count(), 2);
        assert_eq!(result.local_count(), 1); // bob
        assert_eq!(result.remote_count(), 1); // charlie
    }

    #[test]
    fn test_broadcast_presence_nonexistent_occupant() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");

        let result = room.broadcast_presence_federated(
            "nonexistent",
            Affiliation::Member,
            Role::Participant,
            false,
        );

        // No presence should be generated
        assert!(result.is_empty());
    }

    #[test]
    fn test_build_self_leave_presence() {
        let room = create_test_room();
        let leaving_jid: FullJid = "alice@example.com/desktop".parse().unwrap();

        let result = room
            .build_self_leave_presence(&leaving_jid, "alice", Affiliation::Member)
            .expect("valid nick should succeed");

        assert_eq!(result.to, leaving_jid);
        assert_eq!(result.presence.type_, PresenceType::Unavailable);
    }

    #[test]
    fn test_federated_presence_set_iter() {
        let mut set = FederatedPresenceSet::new();

        let local_to: FullJid = "local@example.com/res".parse().unwrap();
        set.add_local(OutboundMucPresence::new(
            local_to.clone(),
            Presence::new(PresenceType::None),
        ));

        let remote_to: FullJid = "remote@other.com/res".parse().unwrap();
        set.add_remote(
            "other.com".to_string(),
            OutboundMucPresence::new(remote_to.clone(), Presence::new(PresenceType::None)),
        );

        let items: Vec<_> = set.iter().collect();
        assert_eq!(items.len(), 2);

        // Check we have one local and one remote
        let local_count = items.iter().filter(|p| p.target.is_local()).count();
        let remote_count = items.iter().filter(|p| p.target.is_remote()).count();
        assert_eq!(local_count, 1);
        assert_eq!(remote_count, 1);
    }

    #[test]
    fn test_build_s2s_occupant_presence() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_occupant = Occupant {
            real_jid: "user@remote.example.org/client".parse().unwrap(),
            nick: "remote_user".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some("remote.example.org".to_string()),
        };

        let result = build_s2s_occupant_presence(
            &room_jid,
            "announcing_nick",
            &to_occupant,
            Affiliation::Member,
            Role::Participant,
            false,
            None,
        )
        .expect("valid nick should succeed");

        assert_eq!(result.to, to_occupant.real_jid);
        assert_eq!(result.presence.type_, PresenceType::None);
    }

    #[test]
    fn test_build_s2s_leave_presence() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_occupant = Occupant {
            real_jid: "user@remote.example.org/client".parse().unwrap(),
            nick: "remote_user".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some("remote.example.org".to_string()),
        };

        let result = build_s2s_leave_presence(
            &room_jid,
            "leaving_nick",
            &to_occupant,
            Affiliation::Member,
            false,
        )
        .expect("valid nick should succeed");

        assert_eq!(result.to, to_occupant.real_jid);
        assert_eq!(result.presence.type_, PresenceType::Unavailable);
    }

    // =============================================================================
    // Message Federation Tests
    // =============================================================================

    fn make_test_message(body: &str) -> Message {
        let mut msg = Message::new(None::<Jid>);
        msg.type_ = MessageType::Groupchat;
        msg.id = Some("test-msg-1".to_string());
        msg.bodies.insert(String::new(), Body(body.to_string()));
        msg
    }

    #[test]
    fn test_message_delivery_target() {
        let local = MessageDeliveryTarget::Local;
        assert!(local.is_local());
        assert!(!local.is_remote());
        assert_eq!(local.remote_domain(), None);

        let remote = MessageDeliveryTarget::Remote("remote.example.com".to_string());
        assert!(!remote.is_local());
        assert!(remote.is_remote());
        assert_eq!(remote.remote_domain(), Some("remote.example.com"));
    }

    #[test]
    fn test_federated_message_set_empty() {
        let set = FederatedMessageSet::new();
        assert!(set.is_empty());
        assert_eq!(set.total_count(), 0);
        assert_eq!(set.local_count(), 0);
        assert_eq!(set.remote_count(), 0);
        assert_eq!(set.remote_domain_count(), 0);
    }

    #[test]
    fn test_federated_message_set_local_only() {
        let mut set = FederatedMessageSet::new();

        let to: FullJid = "user@local.example.com/res".parse().unwrap();
        let message = make_test_message("Hello!");
        let outbound = OutboundMucMessage::new(to, message);

        set.add_local(outbound);

        assert!(!set.is_empty());
        assert_eq!(set.total_count(), 1);
        assert_eq!(set.local_count(), 1);
        assert_eq!(set.remote_count(), 0);
    }

    #[test]
    fn test_federated_message_set_remote_only() {
        let mut set = FederatedMessageSet::new();

        let to: FullJid = "user@remote.example.com/res".parse().unwrap();
        let message = make_test_message("Hello!");
        let outbound = OutboundMucMessage::new(to, message);

        set.add_remote("remote.example.com".to_string(), outbound);

        assert!(!set.is_empty());
        assert_eq!(set.total_count(), 1);
        assert_eq!(set.local_count(), 0);
        assert_eq!(set.remote_count(), 1);
        assert_eq!(set.remote_domain_count(), 1);
        assert!(set
            .remote_domains()
            .contains(&&"remote.example.com".to_string()));
    }

    #[test]
    fn test_federated_message_set_mixed() {
        let mut set = FederatedMessageSet::new();

        // Add local
        let local_to: FullJid = "user1@local.example.com/res".parse().unwrap();
        set.add_local(OutboundMucMessage::new(local_to, make_test_message("msg1")));

        // Add remote from domain A
        let remote_a: FullJid = "user2@remote-a.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-a.example.com".to_string(),
            OutboundMucMessage::new(remote_a, make_test_message("msg2")),
        );

        // Add remote from domain B
        let remote_b: FullJid = "user3@remote-b.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-b.example.com".to_string(),
            OutboundMucMessage::new(remote_b, make_test_message("msg3")),
        );

        // Add another from domain A
        let remote_a2: FullJid = "user4@remote-a.example.com/res".parse().unwrap();
        set.add_remote(
            "remote-a.example.com".to_string(),
            OutboundMucMessage::new(remote_a2, make_test_message("msg4")),
        );

        assert_eq!(set.total_count(), 4);
        assert_eq!(set.local_count(), 1);
        assert_eq!(set.remote_count(), 3);
        assert_eq!(set.remote_domain_count(), 2);
        assert_eq!(
            set.get_remote("remote-a.example.com").map(|v| v.len()),
            Some(2)
        );
        assert_eq!(
            set.get_remote("remote-b.example.com").map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn test_federated_message_set_iter() {
        let mut set = FederatedMessageSet::new();

        let local_to: FullJid = "local@example.com/res".parse().unwrap();
        set.add_local(OutboundMucMessage::new(
            local_to.clone(),
            make_test_message("local msg"),
        ));

        let remote_to: FullJid = "remote@other.com/res".parse().unwrap();
        set.add_remote(
            "other.com".to_string(),
            OutboundMucMessage::new(remote_to.clone(), make_test_message("remote msg")),
        );

        let items: Vec<_> = set.iter().collect();
        assert_eq!(items.len(), 2);

        // Check we have one local and one remote
        let local_count = items.iter().filter(|m| m.target.is_local()).count();
        let remote_count = items.iter().filter(|m| m.target.is_remote()).count();
        assert_eq!(local_count, 1);
        assert_eq!(remote_count, 1);
    }

    #[test]
    fn test_broadcast_message_federated_local_only() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_local_occupant(&mut room, "bob", "bob@example.com/mobile");

        let message = make_test_message("Hello everyone!");
        let result = room.broadcast_message_federated("alice", &message);

        // Both occupants should receive the message (including alice as echo)
        assert_eq!(result.total_count(), 2);
        assert_eq!(result.local_count(), 2);
        assert_eq!(result.remote_count(), 0);
    }

    #[test]
    fn test_broadcast_message_federated_remote_only() {
        let mut room = create_test_room();
        add_remote_occupant(
            &mut room,
            "charlie",
            "charlie@remote.example.org/client",
            "remote.example.org",
        );
        add_remote_occupant(
            &mut room,
            "diana",
            "diana@other.example.net/app",
            "other.example.net",
        );

        let message = make_test_message("Hello remote users!");
        let result = room.broadcast_message_federated("charlie", &message);

        // Both remote occupants should receive the message
        assert_eq!(result.total_count(), 2);
        assert_eq!(result.local_count(), 0);
        assert_eq!(result.remote_count(), 2);
        assert_eq!(result.remote_domain_count(), 2);
    }

    #[test]
    fn test_broadcast_message_federated_mixed_occupants() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_local_occupant(&mut room, "bob", "bob@example.com/mobile");
        add_remote_occupant(
            &mut room,
            "charlie",
            "charlie@remote.example.org/client",
            "remote.example.org",
        );
        add_remote_occupant(
            &mut room,
            "diana",
            "diana@other.example.net/app",
            "other.example.net",
        );
        add_remote_occupant(
            &mut room,
            "eve",
            "eve@remote.example.org/phone",
            "remote.example.org",
        );

        let message = make_test_message("Hello everyone!");
        let result = room.broadcast_message_federated("alice", &message);

        // 5 total occupants receive the message
        assert_eq!(result.total_count(), 5);
        // 2 local (alice and bob)
        assert_eq!(result.local_count(), 2);
        // 3 remote across 2 domains
        assert_eq!(result.remote_count(), 3);
        assert_eq!(result.remote_domain_count(), 2);
        // 2 occupants on remote.example.org (charlie and eve)
        assert_eq!(
            result.get_remote("remote.example.org").map(|v| v.len()),
            Some(2)
        );
        // 1 occupant on other.example.net (diana)
        assert_eq!(
            result.get_remote("other.example.net").map(|v| v.len()),
            Some(1)
        );
    }

    #[test]
    fn test_broadcast_message_federated_nonexistent_sender() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");

        let message = make_test_message("Hello!");
        let result = room.broadcast_message_federated("nonexistent", &message);

        // No messages should be generated
        assert!(result.is_empty());
    }

    #[test]
    fn test_broadcast_message_federated_message_addressing() {
        let mut room = create_test_room();
        add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
        add_remote_occupant(
            &mut room,
            "bob",
            "bob@remote.example.org/client",
            "remote.example.org",
        );

        let message = make_test_message("Test message");
        let result = room.broadcast_message_federated("alice", &message);

        // Verify message addressing is correct
        for msg in result.iter() {
            // All messages should be groupchat type
            assert_eq!(msg.message.type_, MessageType::Groupchat);

            // From should be room@muc.example.com/alice
            let from = msg.message.from.as_ref().unwrap();
            assert!(from.to_string().contains("testroom@muc.example.com/alice"));

            // To should be the recipient's real JID
            let to = msg.message.to.as_ref().unwrap();
            assert_eq!(to.to_string(), msg.to.to_string());
        }
    }

    #[test]
    fn test_build_s2s_muc_message() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_occupant = Occupant {
            real_jid: "user@remote.example.org/client".parse().unwrap(),
            nick: "remote_user".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some("remote.example.org".to_string()),
        };
        let original_message = make_test_message("Hello from sender!");

        let result =
            build_s2s_muc_message(&room_jid, "sender_nick", &to_occupant, &original_message)
                .expect("valid nick should succeed");

        assert_eq!(result.to, to_occupant.real_jid);
        assert_eq!(result.message.type_, MessageType::Groupchat);

        // Verify from is room@muc.example.com/sender_nick
        let from = result.message.from.as_ref().unwrap();
        assert_eq!(from.to_string(), "room@muc.example.com/sender_nick");

        // Verify to is the occupant's real JID
        let to = result.message.to.as_ref().unwrap();
        assert_eq!(to.to_string(), "user@remote.example.org/client");
    }

    #[test]
    fn test_federated_message_into_outbound() {
        let to: FullJid = "user@example.com/res".parse().unwrap();
        let message = make_test_message("Hello!");

        let federated = FederatedMessage::local(to.clone(), message.clone());
        let outbound = federated.into_outbound_message();

        assert_eq!(outbound.to, to);
        assert_eq!(outbound.message.id, message.id);
    }

    #[test]
    fn test_build_s2s_muc_message_invalid_nick() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_occupant = Occupant {
            real_jid: "user@remote.example.org/client".parse().unwrap(),
            nick: "remote_user".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some("remote.example.org".to_string()),
        };
        let original_message = make_test_message("Hello!");

        // Empty nick is not a valid resource
        let result = build_s2s_muc_message(&room_jid, "", &to_occupant, &original_message);
        assert!(result.is_err(), "Empty nick should return Err");
    }

    #[test]
    fn test_build_s2s_occupant_presence_invalid_nick() {
        let room_jid: BareJid = "room@muc.example.com".parse().unwrap();
        let to_occupant = Occupant {
            real_jid: "user@remote.example.org/client".parse().unwrap(),
            nick: "remote_user".to_string(),
            role: Role::Participant,
            affiliation: Affiliation::Member,
            is_remote: true,
            home_server: Some("remote.example.org".to_string()),
        };

        let result = build_s2s_occupant_presence(
            &room_jid,
            "",
            &to_occupant,
            Affiliation::Member,
            Role::Participant,
            false,
            None,
        );
        assert!(result.is_err(), "Empty nick should return Err");
    }
}
