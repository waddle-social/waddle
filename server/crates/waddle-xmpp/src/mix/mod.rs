//! Mediated Information eXchange (MIX) — XEP-0369 and companions.
//!
//! MIX replaces Multi-User Chat (XEP-0045) with a cleaner channel model built
//! on PubSub. A Waddle channel is a MIX channel at `<channel>@mix.<domain>`.
//! Participants subscribe to node-leafs (messages, participants, info, config,
//! allowed, banned) and receive messages as PubSub notifications rather than
//! through presence-driven occupant lists.
//!
//! ## Scope
//!
//! This module hosts MIX's server-side surface alongside `crate::muc`. MUC is
//! retained while clients are migrated. See the plan in
//! `/root/.claude/plans/based-on-this-research-tidy-wave.md`.
//!
//! ## Layout
//!
//! - [`channel`] — in-memory channel model: config, participants, admission.
//! - [`registry`] — concurrent registry of live channels, keyed by bare JID.
//! - [`stanzas`] — typed builders/parsers for `urn:xmpp:mix:*` payloads (no
//!   ad-hoc `format!` XML; all `minidom::Element`).
//! - [`pam`] — XEP-0405 Participant Server Requirements integration.
//! - [`federation`] — server-to-server delivery of MIX messages.

pub mod channel;
pub mod federation;
pub mod pam;
pub mod registry;
pub mod stanzas;

pub use channel::{MixChannel, MixChannelConfig, Participant, ParticipantSubscription};
pub use registry::{MixChannelHandle, MixChannelInfo, MixChannelRegistry};
pub use stanzas::{
    build_join_result, build_leave_result, build_setnick_result, build_update_subscription_result,
    parse_join, parse_leave, parse_setnick, parse_update_subscription, JoinRequest, LeaveRequest,
    MixError, MixLeafNode, SetnickRequest, UpdateSubscriptionRequest, NS_MIX_CORE, NS_MIX_MISC,
    NS_MIX_PAM,
};

/// Check whether a bare JID lives on the MIX subdomain.
pub fn is_mix_jid(jid: &jid::BareJid, mix_domain: &str) -> bool {
    jid.domain().as_str() == mix_domain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_mix_jid() {
        let channel: jid::BareJid = "general@mix.example.com".parse().unwrap();
        let user: jid::BareJid = "alice@example.com".parse().unwrap();
        assert!(is_mix_jid(&channel, "mix.example.com"));
        assert!(!is_mix_jid(&user, "mix.example.com"));
    }
}
