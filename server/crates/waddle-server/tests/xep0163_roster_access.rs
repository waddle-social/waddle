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

fn roster_node_for(groups: &[&str]) -> NodeConfig {
    NodeConfig {
        roster_groups_allowed: groups.iter().map(|g| (*g).to_string()).collect(),
        ..roster_node()
    }
}

#[test]
fn roster_model_without_allowed_groups_admits_no_contact() {
    // An empty pubsub#roster_groups_allowed leaves no group a contact
    // could be in (XEP-0060 6.1.3.3).
    let cfg = roster_node();
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &["Family"]))),
        ReadDecision::NotInRosterGroup
    );
    assert_eq!(contact_decision(&cfg, None), ReadDecision::NotInRosterGroup);
}

#[test]
fn roster_model_admits_presence_subscribers_in_an_allowed_group() {
    let cfg = roster_node_for(&["Friends", "Family"]);
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &["Work", "Family"]))),
        ReadDecision::Allowed
    );
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &["Work"]))),
        ReadDecision::NotInRosterGroup
    );
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &[]))),
        ReadDecision::NotInRosterGroup
    );
}

#[test]
fn roster_model_requires_a_presence_subscription_as_well_as_the_group() {
    // A `to`/`none` item filed under an allowed group does not receive the
    // owner's presence, so it is not a contact the model admits.
    let cfg = roster_node_for(&["Friends"]);
    assert_eq!(
        contact_decision(&cfg, Some(&standing(false, &["Friends"]))),
        ReadDecision::NotInRosterGroup
    );
    assert_eq!(contact_decision(&cfg, None), ReadDecision::NotInRosterGroup);
}

#[test]
fn group_names_match_exactly() {
    let cfg = roster_node_for(&["Friends"]);
    assert_eq!(
        contact_decision(&cfg, Some(&standing(true, &["friends"]))),
        ReadDecision::NotInRosterGroup
    );
}
