//! Admin V1 — community-owner-gated operations exposed via XEP-0050
//! ad-hoc commands.
//!
//! Hard rules:
//!
//! - **No REST**: admin actions flow over XMPP, specifically XEP-0050.
//! - **Owner gate**: the helper [`is_community_owner`] is the single
//!   source of truth for "may this JID see admin surfaces?" — the
//!   `urn:waddle:admin:*` command handlers MUST call it before
//!   doing anything else and refuse non-owners with `<forbidden/>`.
//! - **Custom namespace**: no XEP defines "list users with prefix
//!   search," so the V1 command lives under
//!   `urn:waddle:admin:users:list:0` per the
//!   "Waddle-namespace only when needed" rule.
//!
//! "Community owner" maps onto the server-owner JID set configured
//! via `WADDLE_SERVER_OWNER_LOCALPARTS` and resolved at startup into
//! [`crate::server::AppState::server_owner_jids`]. Waddle is
//! intentionally a single-community deployment in V1; the
//! XEP-0317-hat layer is descriptive social metadata only (see the
//! module docs on `waddle_xmpp::xep::xep0317`), so authority is read
//! from the authoritative server-owner set rather than from
//! decorative hat URIs.

pub mod users_list;

use jid::BareJid;

use crate::server::AppState;

/// `true` iff `jid` (interpreted as a bare JID) is in the configured
/// community-owner set. Returns `false` for non-bare or unknown JIDs.
///
/// This is the single chokepoint for admin authorization in V1. New
/// admin commands MUST call this before performing any work.
pub fn is_community_owner(state: &AppState, jid: &BareJid) -> bool {
    is_owner_in(&state.server_owner_jids, jid)
}

/// Pure helper used by tests and the public [`is_community_owner`]
/// entry point. Walks `owners` and returns `true` on the first JID
/// equal to `jid`.
pub fn is_owner_in(owners: &[BareJid], jid: &BareJid) -> bool {
    owners.iter().any(|owner| owner == jid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jid(s: &str) -> BareJid {
        s.parse().expect("test jid parses")
    }

    #[test]
    fn owner_jid_returns_true() {
        let owners = vec![jid("admin@localhost")];
        assert!(is_owner_in(&owners, &jid("admin@localhost")));
    }

    #[test]
    fn non_owner_jid_returns_false() {
        let owners = vec![jid("admin@localhost")];
        assert!(!is_owner_in(&owners, &jid("alice@localhost")));
    }

    #[test]
    fn empty_owner_set_returns_false_for_anyone() {
        let owners: Vec<BareJid> = vec![];
        assert!(!is_owner_in(&owners, &jid("admin@localhost")));
    }

    #[test]
    fn multiple_owners_match_each_one() {
        let owners = vec![jid("admin@localhost"), jid("root@localhost")];
        assert!(is_owner_in(&owners, &jid("admin@localhost")));
        assert!(is_owner_in(&owners, &jid("root@localhost")));
        assert!(!is_owner_in(&owners, &jid("alice@localhost")));
    }
}
