//! XEP-0405: MIX Participant Server Requirements (MIX-PAM).
//!
//! When a user joins a MIX channel, their own server records the membership
//! in a client-independent store (the "MIX roster") so messages are
//! delivered to the user even when offline and so clients re-sync across
//! devices on reconnect.
//!
//! The persistent store lives in `waddle-server`'s database as the
//! `mix_subscriptions` table; this module defines the in-process types
//! used by the XMPP crate to stage that state.

use std::collections::HashSet;

use jid::BareJid;
use serde::{Deserialize, Serialize};

/// A single MIX channel the user participates in (XEP-0405).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MixSubscription {
    pub user: BareJid,
    pub channel: BareJid,
    pub participant_id: String,
    pub nick: Option<String>,
    /// Leaf node names the user is subscribed to (may be empty).
    pub nodes: HashSet<String>,
}

impl MixSubscription {
    pub fn new(user: BareJid, channel: BareJid, participant_id: impl Into<String>) -> Self {
        Self {
            user,
            channel,
            participant_id: participant_id.into(),
            nick: None,
            nodes: HashSet::new(),
        }
    }

    pub fn with_nick(mut self, nick: impl Into<String>) -> Self {
        self.nick = Some(nick.into());
        self
    }

    pub fn with_nodes<I: IntoIterator<Item = String>>(mut self, nodes: I) -> Self {
        self.nodes = nodes.into_iter().collect();
        self
    }

    pub fn is_subscribed_to(&self, node_name: &str) -> bool {
        self.nodes.contains(node_name)
    }
}

/// In-memory view of a user's MIX roster (for tests and dispatch).
#[derive(Debug, Default, Clone)]
pub struct MixRoster {
    entries: Vec<MixSubscription>,
}

impl MixRoster {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &MixSubscription> {
        self.entries.iter()
    }

    pub fn contains(&self, channel: &BareJid) -> bool {
        self.entries.iter().any(|e| &e.channel == channel)
    }

    pub fn upsert(&mut self, sub: MixSubscription) {
        if let Some(existing) = self.entries.iter_mut().find(|e| e.channel == sub.channel) {
            *existing = sub;
        } else {
            self.entries.push(sub);
        }
    }

    pub fn remove(&mut self, channel: &BareJid) -> Option<MixSubscription> {
        if let Some(idx) = self.entries.iter().position(|e| &e.channel == channel) {
            Some(self.entries.remove(idx))
        } else {
            None
        }
    }

    pub fn get(&self, channel: &BareJid) -> Option<&MixSubscription> {
        self.entries.iter().find(|e| &e.channel == channel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(name: &str) -> BareJid {
        format!("{}@mix.example.com", name).parse().unwrap()
    }
    fn user(local: &str) -> BareJid {
        format!("{}@example.com", local).parse().unwrap()
    }

    #[test]
    fn test_upsert_new_and_existing() {
        let mut roster = MixRoster::new();
        roster.upsert(
            MixSubscription::new(user("alice"), channel("general"), "pid-1")
                .with_nick("Alice")
                .with_nodes(["urn:xmpp:mix:nodes:messages".into()]),
        );
        assert_eq!(roster.len(), 1);
        assert!(roster.contains(&channel("general")));

        // Update same channel → should replace, not duplicate
        roster.upsert(
            MixSubscription::new(user("alice"), channel("general"), "pid-1")
                .with_nick("Ally")
                .with_nodes(["urn:xmpp:mix:nodes:messages".into()]),
        );
        assert_eq!(roster.len(), 1);
        assert_eq!(
            roster.get(&channel("general")).unwrap().nick.as_deref(),
            Some("Ally")
        );
    }

    #[test]
    fn test_remove() {
        let mut roster = MixRoster::new();
        roster.upsert(MixSubscription::new(user("alice"), channel("a"), "p1"));
        roster.upsert(MixSubscription::new(user("alice"), channel("b"), "p2"));
        assert!(roster.remove(&channel("a")).is_some());
        assert_eq!(roster.len(), 1);
        assert!(roster.remove(&channel("a")).is_none());
    }

    #[test]
    fn test_subscription_node_check() {
        let sub = MixSubscription::new(user("alice"), channel("g"), "p1").with_nodes([
            "urn:xmpp:mix:nodes:messages".into(),
            "urn:xmpp:mix:nodes:participants".into(),
        ]);
        assert!(sub.is_subscribed_to("urn:xmpp:mix:nodes:messages"));
        assert!(!sub.is_subscribed_to("urn:xmpp:mix:nodes:config"));
    }
}
