//! MIX channel state.
//!
//! Each `MixChannel` is the in-memory projection of a single Waddle channel
//! hosted at `<channel>@mix.<domain>`. Persistent admission continues to be
//! sourced from the Zanzibar permission model (see
//! `server/crates/waddle-server/src/db/migrations.rs`), so this module is
//! intentionally lightweight.

use std::collections::{HashMap, HashSet};

use jid::BareJid;
use serde::{Deserialize, Serialize};

use crate::types::Affiliation;

/// Leaf nodes a MIX channel exposes over PubSub (XEP-0369 §5, §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MixLeaf {
    Messages,
    Participants,
    Info,
    Config,
    Allowed,
    Banned,
    Avatar,
}

impl MixLeaf {
    pub fn as_node_name(&self) -> &'static str {
        match self {
            MixLeaf::Messages => "urn:xmpp:mix:nodes:messages",
            MixLeaf::Participants => "urn:xmpp:mix:nodes:participants",
            MixLeaf::Info => "urn:xmpp:mix:nodes:info",
            MixLeaf::Config => "urn:xmpp:mix:nodes:config",
            MixLeaf::Allowed => "urn:xmpp:mix:nodes:allowed",
            MixLeaf::Banned => "urn:xmpp:mix:nodes:banned",
            MixLeaf::Avatar => "urn:xmpp:mix:nodes:avatar",
        }
    }

    pub fn default_subscribable() -> &'static [MixLeaf] {
        &[MixLeaf::Messages, MixLeaf::Participants, MixLeaf::Info]
    }

    pub fn from_node_name(name: &str) -> Option<MixLeaf> {
        Some(match name {
            "urn:xmpp:mix:nodes:messages" => MixLeaf::Messages,
            "urn:xmpp:mix:nodes:participants" => MixLeaf::Participants,
            "urn:xmpp:mix:nodes:info" => MixLeaf::Info,
            "urn:xmpp:mix:nodes:config" => MixLeaf::Config,
            "urn:xmpp:mix:nodes:allowed" => MixLeaf::Allowed,
            "urn:xmpp:mix:nodes:banned" => MixLeaf::Banned,
            "urn:xmpp:mix:nodes:avatar" => MixLeaf::Avatar,
            _ => return None,
        })
    }
}

/// A single participant's subscription state for a channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParticipantSubscription {
    /// Leaf nodes the participant has asked to subscribe to.
    pub leaves: HashSet<String>,
}

impl ParticipantSubscription {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_default_leaves() -> Self {
        let leaves = MixLeaf::default_subscribable()
            .iter()
            .map(|l| l.as_node_name().to_string())
            .collect();
        Self { leaves }
    }

    pub fn contains(&self, leaf: MixLeaf) -> bool {
        self.leaves.contains(leaf.as_node_name())
    }

    pub fn add(&mut self, leaf: MixLeaf) {
        self.leaves.insert(leaf.as_node_name().to_string());
    }

    pub fn remove(&mut self, leaf: MixLeaf) {
        self.leaves.remove(leaf.as_node_name());
    }
}

/// A MIX channel participant.
///
/// Unlike MUC, MIX exposes a stable participant identity per channel that
/// outlives any single presence session. `real_jid` is the bare JID of the
/// participant; `nick` is their chosen display name for the channel.
#[derive(Debug, Clone)]
pub struct Participant {
    /// Stable bare JID of the participant.
    pub real_jid: BareJid,
    /// Display name within the channel.
    pub nick: String,
    /// Current affiliation (reused from `crate::types::Affiliation`).
    pub affiliation: Affiliation,
    /// Which leaves this participant has subscribed to.
    pub subscription: ParticipantSubscription,
}

impl Participant {
    pub fn new(real_jid: BareJid, nick: impl Into<String>) -> Self {
        Self {
            real_jid,
            nick: nick.into(),
            affiliation: Affiliation::Member,
            subscription: ParticipantSubscription::with_default_leaves(),
        }
    }
}

/// Configuration for a MIX channel (XEP-0369 §6).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixChannelConfig {
    pub name: String,
    pub description: Option<String>,
    /// Whether the channel is advertised in disco-items.
    pub public: bool,
    /// Last Message Correction advertised.
    pub last_change_by_allowed: bool,
    /// Retractions allowed.
    pub retraction_allowed: bool,
    /// Maximum participants (0 = unlimited).
    pub max_participants: u32,
}

impl Default for MixChannelConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: None,
            public: true,
            last_change_by_allowed: true,
            retraction_allowed: true,
            max_participants: 0,
        }
    }
}

/// In-memory state of a single MIX channel.
#[derive(Debug, Clone)]
pub struct MixChannel {
    pub channel_jid: BareJid,
    pub waddle_id: String,
    pub channel_id: String,
    pub config: MixChannelConfig,
    participants: HashMap<BareJid, Participant>,
}

impl MixChannel {
    pub fn new(
        channel_jid: BareJid,
        waddle_id: String,
        channel_id: String,
        config: MixChannelConfig,
    ) -> Self {
        Self {
            channel_jid,
            waddle_id,
            channel_id,
            config,
            participants: HashMap::new(),
        }
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    pub fn is_full(&self) -> bool {
        if self.config.max_participants == 0 {
            false
        } else {
            self.participants.len() >= self.config.max_participants as usize
        }
    }

    pub fn get_participant(&self, jid: &BareJid) -> Option<&Participant> {
        self.participants.get(jid)
    }

    pub fn has_participant(&self, jid: &BareJid) -> bool {
        self.participants.contains_key(jid)
    }

    pub fn participants(&self) -> impl Iterator<Item = &Participant> {
        self.participants.values()
    }

    /// Add (or replace) a participant. Returns the previous state if any.
    pub fn upsert_participant(&mut self, participant: Participant) -> Option<Participant> {
        self.participants
            .insert(participant.real_jid.clone(), participant)
    }

    pub fn remove_participant(&mut self, jid: &BareJid) -> Option<Participant> {
        self.participants.remove(jid)
    }

    pub fn set_nick(&mut self, jid: &BareJid, nick: String) -> Option<&Participant> {
        if let Some(p) = self.participants.get_mut(jid) {
            p.nick = nick;
            Some(&*p)
        } else {
            None
        }
    }

    pub fn update_subscription(
        &mut self,
        jid: &BareJid,
        add: &[MixLeaf],
        remove: &[MixLeaf],
    ) -> Option<&ParticipantSubscription> {
        let p = self.participants.get_mut(jid)?;
        for l in add {
            p.subscription.add(*l);
        }
        for l in remove {
            p.subscription.remove(*l);
        }
        Some(&p.subscription)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_jid() -> BareJid {
        "general@mix.example.com".parse().unwrap()
    }
    fn user_jid(local: &str) -> BareJid {
        format!("{}@example.com", local).parse().unwrap()
    }

    #[test]
    fn test_join_and_leave() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "waddle-1".into(),
            "channel-1".into(),
            MixChannelConfig::default(),
        );
        let alice = Participant::new(user_jid("alice"), "Alice");
        assert!(ch.upsert_participant(alice).is_none());
        assert_eq!(ch.participant_count(), 1);
        assert!(ch.has_participant(&user_jid("alice")));

        assert!(ch.remove_participant(&user_jid("alice")).is_some());
        assert_eq!(ch.participant_count(), 0);
    }

    #[test]
    fn test_setnick() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "waddle-1".into(),
            "channel-1".into(),
            MixChannelConfig::default(),
        );
        ch.upsert_participant(Participant::new(user_jid("alice"), "Alice"));
        let p = ch.set_nick(&user_jid("alice"), "Ally".into()).unwrap();
        assert_eq!(p.nick, "Ally");
    }

    #[test]
    fn test_default_subscription_contains_core_leaves() {
        let ch = MixChannel::new(
            channel_jid(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        );
        let _ = ch;
        let sub = ParticipantSubscription::with_default_leaves();
        assert!(sub.contains(MixLeaf::Messages));
        assert!(sub.contains(MixLeaf::Participants));
        assert!(sub.contains(MixLeaf::Info));
        assert!(!sub.contains(MixLeaf::Config));
    }

    #[test]
    fn test_update_subscription_add_remove() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "w".into(),
            "c".into(),
            MixChannelConfig::default(),
        );
        ch.upsert_participant(Participant::new(user_jid("alice"), "Alice"));
        let sub = ch
            .update_subscription(&user_jid("alice"), &[MixLeaf::Config], &[MixLeaf::Info])
            .unwrap();
        assert!(sub.contains(MixLeaf::Messages));
        assert!(sub.contains(MixLeaf::Participants));
        assert!(sub.contains(MixLeaf::Config));
        assert!(!sub.contains(MixLeaf::Info));
    }

    #[test]
    fn test_leaf_round_trip() {
        for leaf in [
            MixLeaf::Messages,
            MixLeaf::Participants,
            MixLeaf::Info,
            MixLeaf::Config,
            MixLeaf::Allowed,
            MixLeaf::Banned,
            MixLeaf::Avatar,
        ] {
            assert_eq!(MixLeaf::from_node_name(leaf.as_node_name()), Some(leaf));
        }
    }

    #[test]
    fn test_capacity() {
        let mut ch = MixChannel::new(
            channel_jid(),
            "w".into(),
            "c".into(),
            MixChannelConfig {
                max_participants: 2,
                ..MixChannelConfig::default()
            },
        );
        ch.upsert_participant(Participant::new(user_jid("a"), "a"));
        ch.upsert_participant(Participant::new(user_jid("b"), "b"));
        assert!(ch.is_full());
    }
}
