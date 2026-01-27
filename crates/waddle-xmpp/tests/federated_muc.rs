//! Federated MUC (Multi-User Chat) Routing Tests
//!
//! These tests verify that MUC rooms properly handle federated occupants:
//! - Remote users can join local rooms
//! - Messages are broadcast to both local and remote occupants
//! - Presence updates are routed via S2S for remote users
//!
//! Note: Full end-to-end federation testing requires two server instances.
//! These tests verify the MUC federation routing logic itself.
//!
//! Run with: `cargo test -p waddle-xmpp --test federated_muc`

mod common;

use jid::FullJid;
use xmpp_parsers::message::{Message, MessageType};

use waddle_xmpp::muc::federation::{
    FederatedMessage, FederatedMessageSet, FederatedPresence, FederatedPresenceSet,
    MessageDeliveryTarget, PresenceDeliveryTarget,
};
use waddle_xmpp::muc::{MucRoom, Occupant, OutboundMucMessage, OutboundMucPresence, RoomConfig};
use waddle_xmpp::{Affiliation, Role};

/// Initialize test environment.
fn init_test() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        common::install_crypto_provider();
        let _ = tracing_subscriber::fmt()
            .with_env_filter("debug")
            .with_test_writer()
            .try_init();
    });
}

/// Create a test MUC room.
fn create_test_room() -> MucRoom {
    MucRoom::new(
        "testroom@muc.example.com".parse().unwrap(),
        "test-waddle-id".to_string(),
        "test-channel-id".to_string(),
        RoomConfig::default(),
    )
}

/// Add a local occupant to a room.
fn add_local_occupant(room: &mut MucRoom, nick: &str, jid: &str) {
    let real_jid: FullJid = jid.parse().unwrap();
    room.add_occupant(Occupant {
        real_jid,
        nick: nick.to_string(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
        is_remote: false,
        home_server: None,
    });
}

/// Add a remote occupant to a room.
fn add_remote_occupant(room: &mut MucRoom, nick: &str, jid: &str, home_server: &str) {
    let real_jid: FullJid = jid.parse().unwrap();
    room.add_occupant(Occupant {
        real_jid,
        nick: nick.to_string(),
        role: Role::Participant,
        affiliation: Affiliation::Member,
        is_remote: true,
        home_server: Some(home_server.to_string()),
    });
}

/// Create a test message.
fn make_test_message(body: &str) -> Message {
    let mut msg = Message::new(None);
    msg.type_ = MessageType::Groupchat;
    msg.id = Some(format!("msg-{}", uuid::Uuid::new_v4()));
    msg.bodies
        .insert(String::new(), xmpp_parsers::message::Body(body.to_string()));
    msg
}

// =============================================================================
// Test: Presence Delivery Targets
// =============================================================================

/// Test presence delivery target creation and classification.
#[test]
fn test_presence_delivery_target() {
    init_test();

    // Local target
    let local = PresenceDeliveryTarget::Local;
    assert!(local.is_local());
    assert!(!local.is_remote());
    assert!(local.remote_domain().is_none());

    // Remote target
    let remote = PresenceDeliveryTarget::Remote("remote.example.org".to_string());
    assert!(!remote.is_local());
    assert!(remote.is_remote());
    assert_eq!(remote.remote_domain(), Some("remote.example.org"));
}

/// Test message delivery target creation and classification.
#[test]
fn test_message_delivery_target() {
    init_test();

    // Local target
    let local = MessageDeliveryTarget::Local;
    assert!(local.is_local());
    assert!(!local.is_remote());
    assert!(local.remote_domain().is_none());

    // Remote target
    let remote = MessageDeliveryTarget::Remote("remote.example.org".to_string());
    assert!(!remote.is_local());
    assert!(remote.is_remote());
    assert_eq!(remote.remote_domain(), Some("remote.example.org"));
}

// =============================================================================
// Test: Federated Presence Sets
// =============================================================================

/// Test creating and querying federated presence sets.
#[test]
fn test_federated_presence_set() {
    init_test();

    let mut set = FederatedPresenceSet::new();
    assert!(set.is_empty());
    assert_eq!(set.total_count(), 0);

    // Add local presence
    let local_to: FullJid = "local@example.com/desktop".parse().unwrap();
    let local_presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    set.add_local(OutboundMucPresence::new(local_to.clone(), local_presence.clone()));

    assert_eq!(set.local_count(), 1);
    assert_eq!(set.total_count(), 1);

    // Add remote presence
    let remote_to: FullJid = "remote@other.example.org/mobile".parse().unwrap();
    set.add_remote(
        "other.example.org".to_string(),
        OutboundMucPresence::new(remote_to.clone(), local_presence.clone()),
    );

    assert_eq!(set.remote_count(), 1);
    assert_eq!(set.total_count(), 2);
    assert_eq!(set.remote_domain_count(), 1);

    // Check specific remote domain
    let remote_list = set.get_remote("other.example.org");
    assert!(remote_list.is_some());
    assert_eq!(remote_list.unwrap().len(), 1);
}

/// Test federated presence set iteration.
#[test]
fn test_federated_presence_set_iteration() {
    init_test();

    let mut set = FederatedPresenceSet::new();

    // Add mixed presences
    let local_to: FullJid = "alice@example.com/desktop".parse().unwrap();
    let presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    set.add_local(OutboundMucPresence::new(local_to.clone(), presence.clone()));

    let remote_to: FullJid = "bob@remote.org/mobile".parse().unwrap();
    set.add_remote(
        "remote.org".to_string(),
        OutboundMucPresence::new(remote_to.clone(), presence.clone()),
    );

    let items: Vec<_> = set.iter().collect();
    assert_eq!(items.len(), 2);

    // Verify we have both local and remote
    let local_count = items.iter().filter(|p| p.target.is_local()).count();
    let remote_count = items.iter().filter(|p| p.target.is_remote()).count();
    assert_eq!(local_count, 1);
    assert_eq!(remote_count, 1);
}

// =============================================================================
// Test: Federated Message Sets
// =============================================================================

/// Test creating and querying federated message sets.
#[test]
fn test_federated_message_set() {
    init_test();

    let mut set = FederatedMessageSet::new();
    assert!(set.is_empty());
    assert_eq!(set.total_count(), 0);

    // Add local message
    let local_to: FullJid = "alice@example.com/desktop".parse().unwrap();
    let message = make_test_message("Hello local!");
    set.add_local(OutboundMucMessage::new(local_to.clone(), message.clone()));

    assert_eq!(set.local_count(), 1);
    assert_eq!(set.total_count(), 1);

    // Add remote messages to different domains
    let remote1_to: FullJid = "bob@remote1.org/client".parse().unwrap();
    set.add_remote(
        "remote1.org".to_string(),
        OutboundMucMessage::new(remote1_to.clone(), message.clone()),
    );

    let remote2_to: FullJid = "charlie@remote2.org/app".parse().unwrap();
    set.add_remote(
        "remote2.org".to_string(),
        OutboundMucMessage::new(remote2_to.clone(), message.clone()),
    );

    assert_eq!(set.remote_count(), 2);
    assert_eq!(set.total_count(), 3);
    assert_eq!(set.remote_domain_count(), 2);
}

/// Test federated message set with multiple occupants per domain.
#[test]
fn test_federated_message_set_multiple_per_domain() {
    init_test();

    let mut set = FederatedMessageSet::new();

    let message = make_test_message("Broadcast message");

    // Add two occupants from the same remote domain
    let remote1_to: FullJid = "user1@remote.org/client1".parse().unwrap();
    let remote2_to: FullJid = "user2@remote.org/client2".parse().unwrap();

    set.add_remote(
        "remote.org".to_string(),
        OutboundMucMessage::new(remote1_to.clone(), message.clone()),
    );
    set.add_remote(
        "remote.org".to_string(),
        OutboundMucMessage::new(remote2_to.clone(), message.clone()),
    );

    // Should have 2 remote messages but only 1 domain
    assert_eq!(set.remote_count(), 2);
    assert_eq!(set.remote_domain_count(), 1);

    // The domain should have 2 messages
    let remote_list = set.get_remote("remote.org");
    assert!(remote_list.is_some());
    assert_eq!(remote_list.unwrap().len(), 2);
}

// =============================================================================
// Test: MUC Room Federation Broadcast
// =============================================================================

/// Test message broadcast with only local occupants.
#[test]
fn test_broadcast_message_local_only() {
    init_test();

    let mut room = create_test_room();
    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_local_occupant(&mut room, "bob", "bob@example.com/mobile");

    let message = make_test_message("Hello local occupants!");
    let result = room.broadcast_message_federated("alice", &message);

    // Both occupants should receive (including echo to sender)
    assert_eq!(result.total_count(), 2);
    assert_eq!(result.local_count(), 2);
    assert_eq!(result.remote_count(), 0);
}

/// Test message broadcast with only remote occupants.
#[test]
fn test_broadcast_message_remote_only() {
    init_test();

    let mut room = create_test_room();
    add_remote_occupant(&mut room, "charlie", "charlie@remote1.org/client", "remote1.org");
    add_remote_occupant(&mut room, "diana", "diana@remote2.org/app", "remote2.org");

    let message = make_test_message("Hello remote occupants!");
    let result = room.broadcast_message_federated("charlie", &message);

    assert_eq!(result.total_count(), 2);
    assert_eq!(result.local_count(), 0);
    assert_eq!(result.remote_count(), 2);
    assert_eq!(result.remote_domain_count(), 2);
}

/// Test message broadcast with mixed local and remote occupants.
#[test]
fn test_broadcast_message_mixed() {
    init_test();

    let mut room = create_test_room();

    // Local occupants
    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_local_occupant(&mut room, "bob", "bob@example.com/mobile");

    // Remote occupants (some on same domain)
    add_remote_occupant(&mut room, "charlie", "charlie@remote.org/client1", "remote.org");
    add_remote_occupant(&mut room, "diana", "diana@remote.org/client2", "remote.org");
    add_remote_occupant(&mut room, "eve", "eve@other.org/app", "other.org");

    let message = make_test_message("Hello everyone!");
    let result = room.broadcast_message_federated("alice", &message);

    // 5 total: 2 local + 3 remote
    assert_eq!(result.total_count(), 5);
    assert_eq!(result.local_count(), 2);
    assert_eq!(result.remote_count(), 3);
    // 2 remote domains
    assert_eq!(result.remote_domain_count(), 2);
}

/// Test that nonexistent sender produces no messages.
#[test]
fn test_broadcast_message_nonexistent_sender() {
    init_test();

    let mut room = create_test_room();
    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");

    let message = make_test_message("This shouldn't be sent");
    let result = room.broadcast_message_federated("nonexistent", &message);

    assert!(result.is_empty());
}

// =============================================================================
// Test: MUC Room Presence Broadcast
// =============================================================================

/// Test presence broadcast to mixed occupants.
#[test]
fn test_broadcast_presence_mixed() {
    init_test();

    let mut room = create_test_room();

    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_remote_occupant(&mut room, "bob", "bob@remote.org/client", "remote.org");

    // broadcast_presence_federated takes (nick, affiliation, role, include_real_jid)
    let result =
        room.broadcast_presence_federated("alice", Affiliation::Member, Role::Participant, false);

    // Both occupants should receive presence
    assert_eq!(result.total_count(), 2);
    assert_eq!(result.local_count(), 1);
    assert_eq!(result.remote_count(), 1);
}

/// Test join presence is sent to all existing occupants.
#[test]
fn test_broadcast_join_presence() {
    init_test();

    let mut room = create_test_room();

    // Existing occupants
    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_remote_occupant(&mut room, "bob", "bob@remote.org/client", "remote.org");

    // New user joins
    add_local_occupant(&mut room, "charlie", "charlie@example.com/mobile");

    let result =
        room.broadcast_presence_federated("charlie", Affiliation::Member, Role::Participant, false);

    // All 3 occupants should receive the new user's presence
    assert_eq!(result.total_count(), 3);
}

/// Test leave presence broadcast to remaining occupants.
#[test]
fn test_broadcast_leave_presence() {
    init_test();

    let mut room = create_test_room();

    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_local_occupant(&mut room, "bob", "bob@example.com/mobile");
    add_remote_occupant(&mut room, "charlie", "charlie@remote.org/client", "remote.org");

    // Alice is leaving - broadcast to bob and charlie (not alice herself)
    let result = room.broadcast_leave_presence_federated("alice", Affiliation::Member);

    // Should go to bob (local) and charlie (remote), but not alice
    assert_eq!(result.total_count(), 2);
    assert_eq!(result.local_count(), 1);
    assert_eq!(result.remote_count(), 1);
}

// =============================================================================
// Test: FederatedMessage and FederatedPresence Helpers
// =============================================================================

/// Test FederatedMessage creation.
#[test]
fn test_federated_message_creation() {
    init_test();

    let to: FullJid = "user@example.com/resource".parse().unwrap();
    let message = make_test_message("Test");

    // Local message
    let local_msg = FederatedMessage::local(to.clone(), message.clone());
    assert!(local_msg.target.is_local());
    assert_eq!(local_msg.to, to);

    // Remote message
    let remote_msg =
        FederatedMessage::remote("remote.org".to_string(), to.clone(), message.clone());
    assert!(remote_msg.target.is_remote());
    assert_eq!(remote_msg.target.remote_domain(), Some("remote.org"));
}

/// Test FederatedPresence creation.
#[test]
fn test_federated_presence_creation() {
    init_test();

    let to: FullJid = "user@example.com/resource".parse().unwrap();
    let presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);

    // Local presence
    let local_pres = FederatedPresence::local(to.clone(), presence.clone());
    assert!(local_pres.target.is_local());
    assert_eq!(local_pres.to, to);

    // Remote presence
    let remote_pres =
        FederatedPresence::remote("remote.org".to_string(), to.clone(), presence.clone());
    assert!(remote_pres.target.is_remote());
    assert_eq!(remote_pres.target.remote_domain(), Some("remote.org"));
}

/// Test conversion to outbound types.
#[test]
fn test_federated_to_outbound_conversion() {
    init_test();

    let to: FullJid = "user@example.com/resource".parse().unwrap();
    let message = make_test_message("Test");
    let presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);

    // Message conversion
    let fed_msg = FederatedMessage::local(to.clone(), message.clone());
    let outbound_msg = fed_msg.into_outbound_message();
    assert_eq!(outbound_msg.to, to);

    // Presence conversion
    let fed_pres = FederatedPresence::local(to.clone(), presence.clone());
    let outbound_pres = fed_pres.into_outbound_presence();
    assert_eq!(outbound_pres.to, to);
}

// =============================================================================
// Test: Room State After Federation Operations
// =============================================================================

/// Test that room state is consistent after adding mixed occupants.
#[test]
fn test_room_state_with_federation() {
    init_test();

    let mut room = create_test_room();

    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_remote_occupant(&mut room, "bob", "bob@remote.org/client", "remote.org");

    // Verify occupant counts
    assert_eq!(room.occupant_count(), 2);

    // Verify we can get both occupants
    assert!(room.get_occupant("alice").is_some());
    assert!(room.get_occupant("bob").is_some());

    // Verify remote flag
    let alice = room.get_occupant("alice").unwrap();
    let bob = room.get_occupant("bob").unwrap();

    assert!(!alice.is_remote);
    assert!(bob.is_remote);
    assert_eq!(bob.home_server, Some("remote.org".to_string()));
}

/// Test room methods for querying remote occupants.
#[test]
fn test_room_remote_occupant_queries() {
    init_test();

    let mut room = create_test_room();

    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_local_occupant(&mut room, "bob", "bob@example.com/mobile");
    add_remote_occupant(&mut room, "charlie", "charlie@remote1.org/client", "remote1.org");
    add_remote_occupant(&mut room, "diana", "diana@remote2.org/app", "remote2.org");
    add_remote_occupant(&mut room, "eve", "eve@remote1.org/phone", "remote1.org");

    // Test remote occupant count
    assert_eq!(room.remote_occupant_count(), 3);
    assert_eq!(room.local_occupant_count(), 2);

    // Test getting remote occupants
    let remote_occupants = room.get_remote_occupants();
    assert_eq!(remote_occupants.len(), 3);

    // Test getting remote domains
    let domains = room.get_remote_domains();
    assert_eq!(domains.len(), 2);
    assert!(domains.contains(&"remote1.org".to_string()));
    assert!(domains.contains(&"remote2.org".to_string()));

    // Test getting occupants by domain
    let remote1_occupants = room.get_occupants_for_domain("remote1.org");
    assert_eq!(remote1_occupants.len(), 2);

    let remote2_occupants = room.get_occupants_for_domain("remote2.org");
    assert_eq!(remote2_occupants.len(), 1);

    let local_occupants = room.get_occupants_for_domain("local");
    assert_eq!(local_occupants.len(), 2);
}

/// Test occupants by domain grouping.
#[test]
fn test_room_occupants_by_domain() {
    init_test();

    let mut room = create_test_room();

    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_remote_occupant(&mut room, "bob", "bob@remote.org/client", "remote.org");
    add_remote_occupant(&mut room, "charlie", "charlie@remote.org/mobile", "remote.org");

    let by_domain = room.get_occupants_by_domain();

    // Should have 2 groups: "local" and "remote.org"
    assert_eq!(by_domain.len(), 2);
    assert_eq!(by_domain.get("local").map(|v| v.len()), Some(1));
    assert_eq!(by_domain.get("remote.org").map(|v| v.len()), Some(2));
}

/// Test self-leave presence generation.
#[test]
fn test_self_leave_presence() {
    init_test();

    let room = create_test_room();
    let leaving_jid: FullJid = "alice@example.com/desktop".parse().unwrap();

    let result = room.build_self_leave_presence(&leaving_jid, "alice", Affiliation::Member);

    assert_eq!(result.to, leaving_jid);
    assert_eq!(
        result.presence.type_,
        xmpp_parsers::presence::Type::Unavailable
    );
}

// =============================================================================
// Test: Message Addressing Verification
// =============================================================================

/// Test that broadcast messages have correct addressing.
#[test]
fn test_broadcast_message_addressing() {
    init_test();

    let mut room = create_test_room();
    add_local_occupant(&mut room, "alice", "alice@example.com/desktop");
    add_remote_occupant(&mut room, "bob", "bob@remote.org/client", "remote.org");

    let message = make_test_message("Test message");
    let result = room.broadcast_message_federated("alice", &message);

    // Verify message addressing for all recipients
    for fed_msg in result.iter() {
        // All messages should be groupchat type
        assert_eq!(fed_msg.message.type_, MessageType::Groupchat);

        // From should be room@muc.example.com/alice
        let from = fed_msg.message.from.as_ref().unwrap();
        assert!(from.to_string().contains("testroom@muc.example.com/alice"));

        // To should be the recipient's real JID
        let to = fed_msg.message.to.as_ref().unwrap();
        assert_eq!(to.to_string(), fed_msg.to.to_string());
    }
}

// =============================================================================
// Note: Full E2E Federation Tests
// =============================================================================

// Full end-to-end federated MUC testing would require:
// 1. Two XMPP server instances (server A and server B)
// 2. S2S connections established between them
// 3. User from server B joining a room hosted on server A
// 4. Verifying messages flow in both directions
//
// This would look like:
// ```ignore
// async fn test_federated_muc_e2e() {
//     let server_a = start_server("alpha.local");
//     let server_b = start_server("beta.local");
//     establish_s2s(&server_a, &server_b).await;
//
//     let alice = connect_user(&server_a, "alice@alpha.local");
//     let bob = connect_user(&server_b, "bob@beta.local");
//
//     alice.create_room("room@muc.alpha.local").await;
//     bob.join_room("room@muc.alpha.local").await;  // Federated join
//
//     alice.send_message("Hello Bob!").await;
//     let msg = bob.receive_message().await;  // Via S2S
//     assert!(msg.body.contains("Hello Bob!"));
// }
// ```
