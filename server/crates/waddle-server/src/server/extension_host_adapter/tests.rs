use super::*;
use waddle_xmpp::roster::{AskType, RosterItem, Subscription};
use xmpp_parsers::presence::Show;

#[test]
fn maps_xmpp_presence_show_to_host_enum() {
    assert_eq!(host_presence_show(Show::Chat), HostPresenceShow::Chat);
    assert_eq!(host_presence_show(Show::Away), HostPresenceShow::Away);
    assert_eq!(host_presence_show(Show::Dnd), HostPresenceShow::Dnd);
    assert_eq!(host_presence_show(Show::Xa), HostPresenceShow::Xa);
}

#[test]
fn maps_roster_subscription_without_stringly_state() {
    let item = RosterItem {
        jid: "bob@example.com".parse().expect("jid"),
        name: Some("Bob".to_string()),
        subscription: Subscription::Both,
        ask: Some(AskType::Subscribe),
        approved: false,
        groups: vec!["Friends".to_string()],
    };

    let mapped = host_roster_item(item);
    assert_eq!(mapped.jid.to_string(), "bob@example.com");
    assert_eq!(mapped.subscription, HostRosterSubscription::Both);
    assert_eq!(mapped.ask, Some(HostRosterAsk::Subscribe));
    assert_eq!(mapped.groups, vec!["Friends"]);
}

#[test]
fn detects_roomless_and_cross_room_launches_in_host_sent_envelopes() {
    let room: BareJid = "pub@muc.example.com".parse().expect("room jid");

    let same_room = envelope_with_launch_room(Some("pub@muc.example.com"));
    assert!(!envelope_has_cross_room_launch(&same_room, &room));
    assert!(!envelope_has_roomless_launch(&same_room));

    let other_room = envelope_with_launch_room(Some("other@muc.example.com"));
    assert!(envelope_has_cross_room_launch(&other_room, &room));
    assert!(!envelope_has_roomless_launch(&other_room));

    let roomless = envelope_with_launch_room(None);
    assert!(!envelope_has_cross_room_launch(&roomless, &room));
    assert!(envelope_has_roomless_launch(&roomless));
}

fn envelope_with_launch_room(room: Option<&str>) -> waddle_extensions::ExtensionEnvelope {
    let namespace = waddle_extensions::types::PayloadNamespace::new("urn:waddle:decision-polls:1")
        .expect("namespace");
    waddle_extensions::ExtensionEnvelope::new(vec![waddle_extensions::MessageEnrichment {
        id: waddle_extensions::types::EnrichmentId::new("enrichment-1").expect("enrichment id"),
        plugin: waddle_extensions::PluginId::new("decision-polls").expect("plugin id"),
        capability: waddle_extensions::ExtensionCapability::MessageEnrich,
        payload_namespace: namespace,
        created_at: waddle_extensions::Timestamp::new("2026-04-27T12:00:00Z").expect("timestamp"),
        source: None,
        ui: Vec::new(),
        payloads: Vec::new(),
        launches: vec![waddle_extensions::LaunchDescriptor {
            id: waddle_extensions::LaunchId::new("vote-yes").expect("launch id"),
            plugin: waddle_extensions::PluginId::new("decision-polls").expect("plugin id"),
            action: waddle_extensions::types::ActionId::new("vote").expect("action id"),
            command_node: waddle_extensions::types::CommandNode::invoke(),
            label: waddle_extensions::DisplayText::new("Vote yes").expect("label"),
            context: waddle_extensions::LaunchContext {
                waddle_id: waddle_extensions::WaddleId::new("waddle-1").expect("waddle id"),
                room: room.map(|value| waddle_extensions::RoomJid::new(value).expect("room jid")),
                source_stanza_id: None,
            },
            payloads: Vec::new(),
            fallback: None,
            expires_at: None,
            token: None,
        }],
    }])
}
