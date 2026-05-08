use super::*;

use crate::types::Affiliation;

#[test]
fn test_permission_mapper_default_mappings() {
    let mapper = PermissionMapper::new();

    assert_eq!(mapper.map_relation("owner"), Affiliation::Owner);
    assert_eq!(mapper.map_relation("admin"), Affiliation::Admin);
    assert_eq!(mapper.map_relation("moderator"), Affiliation::Admin);
    assert_eq!(mapper.map_relation("manager"), Affiliation::Admin);
    assert_eq!(mapper.map_relation("member"), Affiliation::Member);
    assert_eq!(mapper.map_relation("writer"), Affiliation::Member);
    assert_eq!(mapper.map_relation("viewer"), Affiliation::Member);
    assert_eq!(mapper.map_relation("unknown"), Affiliation::None);
}

#[test]
fn test_permission_mapper_custom_mapping() {
    let mapper = PermissionMapper::new().with_mapping("super_admin", Affiliation::Owner);

    assert_eq!(mapper.map_relation("super_admin"), Affiliation::Owner);
    assert_eq!(mapper.map_relation("admin"), Affiliation::Admin);
}

#[test]
fn test_permission_mapper_highest_wins() {
    let mapper = PermissionMapper::new();

    // Multiple relations - highest should win
    let relations = vec!["member".to_string(), "admin".to_string()];
    assert_eq!(mapper.map_relations(&relations), Affiliation::Admin);

    // Empty relations
    let empty: Vec<String> = vec![];
    assert_eq!(mapper.map_relations(&empty), Affiliation::None);
}

#[test]
fn test_affiliation_list_basic_operations() {
    let mut list = AffiliationList::new();
    let jid: jid::BareJid = "user@example.com".parse().unwrap();

    // Initially no affiliation
    assert_eq!(list.get(&jid), Affiliation::None);

    // Set member
    let change = list.set(jid.clone(), Affiliation::Member);
    assert!(change.is_some());
    let change = change.unwrap();
    assert_eq!(change.old_affiliation, Affiliation::None);
    assert_eq!(change.new_affiliation, Affiliation::Member);
    assert!(change.is_upgrade());

    // Get should return member
    assert_eq!(list.get(&jid), Affiliation::Member);

    // Upgrade to admin
    let change = list.set(jid.clone(), Affiliation::Admin);
    assert!(change.is_some());
    assert!(change.unwrap().is_upgrade());

    // Setting same value should return None
    let change = list.set(jid.clone(), Affiliation::Admin);
    assert!(change.is_none());

    // Remove
    let change = list.remove(&jid);
    assert!(change.is_some());
    assert_eq!(list.get(&jid), Affiliation::None);
}

#[test]
fn test_affiliation_list_by_affiliation() {
    let mut list = AffiliationList::new();

    let owner: jid::BareJid = "owner@example.com".parse().unwrap();
    let admin1: jid::BareJid = "admin1@example.com".parse().unwrap();
    let admin2: jid::BareJid = "admin2@example.com".parse().unwrap();
    let member: jid::BareJid = "member@example.com".parse().unwrap();

    list.set(owner.clone(), Affiliation::Owner);
    list.set(admin1.clone(), Affiliation::Admin);
    list.set(admin2.clone(), Affiliation::Admin);
    list.set(member.clone(), Affiliation::Member);

    let owners = list.by_affiliation(Affiliation::Owner);
    assert_eq!(owners.len(), 1);
    assert!(owners.contains(&owner));

    let admins = list.by_affiliation(Affiliation::Admin);
    assert_eq!(admins.len(), 2);
    assert!(admins.contains(&admin1));
    assert!(admins.contains(&admin2));

    let members = list.by_affiliation(Affiliation::Member);
    assert_eq!(members.len(), 1);
    assert!(members.contains(&member));
}

#[test]
fn test_affiliation_list_has_at_least() {
    let mut list = AffiliationList::new();
    let jid: jid::BareJid = "user@example.com".parse().unwrap();

    list.set(jid.clone(), Affiliation::Admin);

    assert!(list.has_at_least(&jid, Affiliation::Member));
    assert!(list.has_at_least(&jid, Affiliation::Admin));
    assert!(!list.has_at_least(&jid, Affiliation::Owner));
}

#[test]
fn test_affiliation_list_has_owner() {
    let mut list = AffiliationList::new();

    let admin: jid::BareJid = "admin@example.com".parse().unwrap();
    list.set(admin, Affiliation::Admin);
    assert!(!list.has_owner());

    let owner: jid::BareJid = "owner@example.com".parse().unwrap();
    list.set(owner, Affiliation::Owner);
    assert!(list.has_owner());
}

#[test]
fn test_affiliation_list_counts() {
    let mut list = AffiliationList::new();

    list.set("owner@example.com".parse().unwrap(), Affiliation::Owner);
    list.set("admin1@example.com".parse().unwrap(), Affiliation::Admin);
    list.set("admin2@example.com".parse().unwrap(), Affiliation::Admin);
    list.set("member1@example.com".parse().unwrap(), Affiliation::Member);
    list.set("member2@example.com".parse().unwrap(), Affiliation::Member);
    list.set("member3@example.com".parse().unwrap(), Affiliation::Member);

    let counts = list.counts();
    assert_eq!(counts.get(&Affiliation::Owner), Some(&1));
    assert_eq!(counts.get(&Affiliation::Admin), Some(&2));
    assert_eq!(counts.get(&Affiliation::Member), Some(&3));
    assert_eq!(counts.get(&Affiliation::None), None);
}

#[test]
fn test_federated_permission_policy_default() {
    let policy = FederatedPermissionPolicy::default();
    assert_eq!(policy, FederatedPermissionPolicy::Open);
    assert!(policy.allows_federation());
    assert!(policy.is_open());
}

#[test]
fn test_federated_permission_policy_helpers() {
    assert!(FederatedPermissionPolicy::Open.allows_federation());
    assert!(FederatedPermissionPolicy::Open.is_open());
    assert!(!FederatedPermissionPolicy::Open.uses_allow_list());
    assert!(!FederatedPermissionPolicy::Open.uses_block_list());

    assert!(FederatedPermissionPolicy::AllowList.allows_federation());
    assert!(!FederatedPermissionPolicy::AllowList.is_open());
    assert!(FederatedPermissionPolicy::AllowList.uses_allow_list());
    assert!(!FederatedPermissionPolicy::AllowList.uses_block_list());

    assert!(FederatedPermissionPolicy::BlockList.allows_federation());
    assert!(!FederatedPermissionPolicy::BlockList.is_open());
    assert!(!FederatedPermissionPolicy::BlockList.uses_allow_list());
    assert!(FederatedPermissionPolicy::BlockList.uses_block_list());

    assert!(!FederatedPermissionPolicy::Closed.allows_federation());
    assert!(!FederatedPermissionPolicy::Closed.is_open());
    assert!(!FederatedPermissionPolicy::Closed.uses_allow_list());
    assert!(!FederatedPermissionPolicy::Closed.uses_block_list());
}

#[test]
fn test_federated_affiliation_config_defaults() {
    let config = FederatedAffiliationConfig::default();
    assert_eq!(config.default_affiliation, Affiliation::None);
    assert!(config.allowed_domains.is_empty());
    assert!(config.blocked_domains.is_empty());

    let open_member = FederatedAffiliationConfig::open_member();
    assert_eq!(open_member.default_affiliation, Affiliation::Member);

    let open_none = FederatedAffiliationConfig::open_none();
    assert_eq!(open_none.default_affiliation, Affiliation::None);
}

#[test]
fn test_federated_affiliation_config_domain_lists() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

    // Add allowed domains
    config.add_allowed_domain("trusted.example.com");
    config.add_allowed_domain("partner.example.org");
    assert!(config.is_domain_allowed("trusted.example.com"));
    assert!(config.is_domain_allowed("partner.example.org"));
    assert!(!config.is_domain_allowed("unknown.example.net"));

    // Remove allowed domain
    assert!(config.remove_allowed_domain("trusted.example.com"));
    assert!(!config.is_domain_allowed("trusted.example.com"));
    assert!(!config.remove_allowed_domain("nonexistent.example.com"));

    // Add blocked domains
    config.add_blocked_domain("spam.example.com");
    assert!(config.is_domain_blocked("spam.example.com"));
    assert!(!config.is_domain_blocked("good.example.com"));

    // Remove blocked domain
    assert!(config.remove_blocked_domain("spam.example.com"));
    assert!(!config.is_domain_blocked("spam.example.com"));
}

#[test]
fn test_federated_affiliation_config_jid_lists() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

    let allowed_jid: jid::BareJid = "allowed@trusted.example.com".parse().unwrap();
    let blocked_jid: jid::BareJid = "spammer@spam.example.com".parse().unwrap();
    let unknown_jid: jid::BareJid = "unknown@example.com".parse().unwrap();

    // Add allowed JID
    config.add_allowed_jid(allowed_jid.clone());
    assert!(config.is_jid_allowed(&allowed_jid));
    assert!(!config.is_jid_allowed(&unknown_jid));

    // Add blocked JID
    config.add_blocked_jid(blocked_jid.clone());
    assert!(config.is_jid_blocked(&blocked_jid));
    assert!(!config.is_jid_blocked(&unknown_jid));

    // Remove JIDs
    assert!(config.remove_allowed_jid(&allowed_jid));
    assert!(!config.is_jid_allowed(&allowed_jid));
    assert!(config.remove_blocked_jid(&blocked_jid));
    assert!(!config.is_jid_blocked(&blocked_jid));
}

#[test]
fn test_federated_affiliation_config_affiliation_overrides() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

    let user_jid: jid::BareJid = "user@partner.example.org".parse().unwrap();
    let admin_jid: jid::BareJid = "admin@partner.example.org".parse().unwrap();
    let normal_jid: jid::BareJid = "user@other.example.com".parse().unwrap();

    // Set domain-level override
    config.set_domain_affiliation("partner.example.org", Affiliation::Admin);

    // Set JID-level override (takes precedence)
    config.set_jid_affiliation(admin_jid.clone(), Affiliation::Owner);

    // Check affiliation resolution priority:
    // 1. JID-specific override wins
    assert_eq!(
        config.get_affiliation_for_jid(&admin_jid),
        Affiliation::Owner
    );
    // 2. Domain-specific override
    assert_eq!(
        config.get_affiliation_for_jid(&user_jid),
        Affiliation::Admin
    );
    // 3. Default affiliation
    assert_eq!(
        config.get_affiliation_for_jid(&normal_jid),
        Affiliation::Member
    );

    // Remove overrides
    assert_eq!(
        config.remove_jid_affiliation(&admin_jid),
        Some(Affiliation::Owner)
    );
    assert_eq!(
        config.get_affiliation_for_jid(&admin_jid),
        Affiliation::Admin
    );

    assert_eq!(
        config.remove_domain_affiliation("partner.example.org"),
        Some(Affiliation::Admin)
    );
    assert_eq!(
        config.get_affiliation_for_jid(&user_jid),
        Affiliation::Member
    );
}

#[test]
fn test_federated_policy_open_allows_all() {
    let config = FederatedAffiliationConfig::open_member();

    let jid1: jid::BareJid = "user@server1.example.com".parse().unwrap();
    let jid2: jid::BareJid = "user@server2.example.org".parse().unwrap();
    let jid3: jid::BareJid = "user@any.domain.net".parse().unwrap();

    // Open policy allows all JIDs
    assert!(config.is_allowed_by_policy(&jid1, FederatedPermissionPolicy::Open));
    assert!(config.is_allowed_by_policy(&jid2, FederatedPermissionPolicy::Open));
    assert!(config.is_allowed_by_policy(&jid3, FederatedPermissionPolicy::Open));
}

#[test]
fn test_federated_policy_closed_blocks_all() {
    let config = FederatedAffiliationConfig::open_member();

    let jid1: jid::BareJid = "user@server1.example.com".parse().unwrap();
    let jid2: jid::BareJid = "user@server2.example.org".parse().unwrap();

    // Closed policy blocks all JIDs
    assert!(!config.is_allowed_by_policy(&jid1, FederatedPermissionPolicy::Closed));
    assert!(!config.is_allowed_by_policy(&jid2, FederatedPermissionPolicy::Closed));
}

#[test]
fn test_federated_policy_allowlist_domain() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
    config.add_allowed_domain("trusted.example.com");
    config.add_allowed_domain("partner.example.org");

    let allowed_jid: jid::BareJid = "user@trusted.example.com".parse().unwrap();
    let partner_jid: jid::BareJid = "user@partner.example.org".parse().unwrap();
    let blocked_jid: jid::BareJid = "user@unknown.example.net".parse().unwrap();

    // AllowList policy: only allowed domains pass
    assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::AllowList));
    assert!(config.is_allowed_by_policy(&partner_jid, FederatedPermissionPolicy::AllowList));
    assert!(!config.is_allowed_by_policy(&blocked_jid, FederatedPermissionPolicy::AllowList));
}

#[test]
fn test_federated_policy_allowlist_jid() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
    // Don't add any allowed domains

    let specific_jid: jid::BareJid = "special@any.example.com".parse().unwrap();
    let other_jid: jid::BareJid = "other@any.example.com".parse().unwrap();

    // Add specific JID to allow list
    config.add_allowed_jid(specific_jid.clone());

    // AllowList: JID-specific allows work even if domain isn't allowed
    assert!(config.is_allowed_by_policy(&specific_jid, FederatedPermissionPolicy::AllowList));
    assert!(!config.is_allowed_by_policy(&other_jid, FederatedPermissionPolicy::AllowList));
}

#[test]
fn test_federated_policy_blocklist_domain() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
    config.add_blocked_domain("spam.example.com");
    config.add_blocked_domain("abuse.example.org");

    let blocked_jid1: jid::BareJid = "user@spam.example.com".parse().unwrap();
    let blocked_jid2: jid::BareJid = "user@abuse.example.org".parse().unwrap();
    let allowed_jid: jid::BareJid = "user@good.example.net".parse().unwrap();

    // BlockList policy: blocked domains are rejected, others pass
    assert!(!config.is_allowed_by_policy(&blocked_jid1, FederatedPermissionPolicy::BlockList));
    assert!(!config.is_allowed_by_policy(&blocked_jid2, FederatedPermissionPolicy::BlockList));
    assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::BlockList));
}

#[test]
fn test_federated_policy_blocklist_jid() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

    let bad_user: jid::BareJid = "baduser@good.example.com".parse().unwrap();
    let good_user: jid::BareJid = "gooduser@good.example.com".parse().unwrap();

    // Block specific JID
    config.add_blocked_jid(bad_user.clone());

    // BlockList: JID-specific blocks work even if domain is allowed
    assert!(!config.is_allowed_by_policy(&bad_user, FederatedPermissionPolicy::BlockList));
    assert!(config.is_allowed_by_policy(&good_user, FederatedPermissionPolicy::BlockList));
}

#[test]
fn test_federated_policy_blocklist_jid_overrides_domain_block() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);

    // Block the domain
    config.add_blocked_domain("mostly-bad.example.com");

    // But allow one specific JID from that domain
    let exception_jid: jid::BareJid = "good-user@mostly-bad.example.com".parse().unwrap();
    let regular_jid: jid::BareJid = "regular@mostly-bad.example.com".parse().unwrap();
    config.add_allowed_jid(exception_jid.clone());

    // BlockList: JID allow list overrides domain block
    assert!(config.is_allowed_by_policy(&exception_jid, FederatedPermissionPolicy::BlockList));
    assert!(!config.is_allowed_by_policy(&regular_jid, FederatedPermissionPolicy::BlockList));
}

#[test]
fn test_federated_user_join_open_room_open_policy() {
    let config = FederatedAffiliationConfig::open_member();
    let jid: jid::BareJid = "user@remote.example.com".parse().unwrap();

    // Open room, open policy: anyone can join
    let affiliation = config.get_affiliation_for_jid(&jid);
    assert_eq!(affiliation, Affiliation::Member);

    let can_join = config.is_allowed_by_policy(&jid, FederatedPermissionPolicy::Open)
        && affiliation != Affiliation::Outcast;
    assert!(can_join);
}

#[test]
fn test_federated_user_join_members_only_room() {
    let config = FederatedAffiliationConfig::open_member();
    let jid: jid::BareJid = "user@remote.example.com".parse().unwrap();

    // Members-only room: federated users with Member affiliation can join
    let affiliation = config.get_affiliation_for_jid(&jid);
    assert!(affiliation >= Affiliation::Member);

    // With None affiliation, cannot join members-only
    let none_config = FederatedAffiliationConfig::open_none();
    let none_affiliation = none_config.get_affiliation_for_jid(&jid);
    assert!(none_affiliation < Affiliation::Member);
}

#[test]
fn test_federated_user_join_closed_policy() {
    let config = FederatedAffiliationConfig::open_member();
    let jid: jid::BareJid = "user@remote.example.com".parse().unwrap();

    // Closed policy: no one can join regardless of affiliation
    assert!(!config.is_allowed_by_policy(&jid, FederatedPermissionPolicy::Closed));
}

#[test]
fn test_federated_user_join_blocked_domain() {
    let mut config = FederatedAffiliationConfig::open_member();
    config.add_blocked_domain("spam.example.com");

    let blocked_jid: jid::BareJid = "user@spam.example.com".parse().unwrap();
    let allowed_jid: jid::BareJid = "user@good.example.com".parse().unwrap();

    // BlockList policy: blocked domain rejected
    assert!(!config.is_allowed_by_policy(&blocked_jid, FederatedPermissionPolicy::BlockList));
    assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::BlockList));
}

#[test]
fn test_federated_user_join_allowlist_with_accepted_domain() {
    let mut config = FederatedAffiliationConfig::new(Affiliation::Member);
    config.add_allowed_domain("trusted.example.com");

    let allowed_jid: jid::BareJid = "user@trusted.example.com".parse().unwrap();
    let rejected_jid: jid::BareJid = "user@untrusted.example.org".parse().unwrap();

    // AllowList policy: only allowed domain passes
    assert!(config.is_allowed_by_policy(&allowed_jid, FederatedPermissionPolicy::AllowList));
    assert!(!config.is_allowed_by_policy(&rejected_jid, FederatedPermissionPolicy::AllowList));
}
