//! XEP-0060 §4.5.1 `presence` and `roster` access-model decisions that PEP
//! reads and subscriptions (XEP-0163) apply to a non-owner entity.

use waddle_server::pubsub_authz::{contact_decision, ContactStanding, ReadDecision};
use waddle_xmpp::pubsub::{AccessModel, NodeConfig};

fn standing(presence: bool, groups: &[&str]) -> ContactStanding {
    ContactStanding {
        receives_owner_presence: presence,
        groups: groups.iter().map(|g| (*g).to_string()).collect(),
    }
}

fn roster_node() -> NodeConfig {
    NodeConfig {
        access_model: AccessModel::Roster,
        ..NodeConfig::pep_default()
    }
}

#[test]
fn presence_model_admits_presence_subscriber_only() {
    let cfg = NodeConfig::pep_default();
    assert_eq!(cfg.access_model, AccessModel::Presence);
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &[]))),
        ReadDecision::Allowed
    );
    assert_eq!(
        contact_decision(&cfg, Some(&standing(false, &["Family"]))),
        ReadDecision::PresenceSubscriptionRequired
    );
    assert_eq!(
        contact_decision(&cfg, None),
        ReadDecision::PresenceSubscriptionRequired
    );
}

#[test]
fn roster_model_refuses_non_owner_with_not_in_roster_group() {
    // No roster groups are configurable, so the allowed-group list is
    // empty and no non-owner is in an allowed group (XEP-0060 6.1.3.3).
    let cfg = roster_node();
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &["Family"]))),
        ReadDecision::NotInRosterGroup
    );
    assert_eq!(contact_decision(&cfg, None), ReadDecision::NotInRosterGroup);
}
