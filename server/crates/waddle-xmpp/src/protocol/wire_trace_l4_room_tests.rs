//! L4 wire-trace tests for the MUC room handler chain (#229 PR17).
//!
//! These tests exercise the room chain end-to-end — frozen
//! [`super::room::RoomContext`], all four handlers in registration
//! order, deterministic [`super::id_gen::FixedIdGenerator`] — and assert
//! on the exact event shape the chain produces.
//!
//! The chain is the locked Q7 option C order:
//!
//! 1. `OccupancyValidationHandler` (XEP-0045 §7.4 + managed-room policy)
//! 2. `MucCanonicalizeHandler` (XEP-0359 strip+stamp `by=room`,
//!    XEP-0421 occupant-id, `from='room/nick'`)
//! 3. `MucArchiveHandler` (XEP-0313 §5.1.3 → `ArchiveGroupchat`)
//! 4. `ReflectorHandler` (per-occupant `RouteToConnection`)

use super::event::OutboundEvent;
use super::id_gen::FixedIdGenerator;
use super::room::{default_room_dispatcher, OccupantSnapshot, RoomContext};
use crate::types::{Affiliation, Role};
use crate::xep::xep0421::{
    extract_occupant_id_from_message, generate_occupant_id, OccupantIdSecret,
};
use crate::Stanza;
use jid::{BareJid, FullJid, Jid};
use waddle_xmpp_core::xep0359::{extract_stanza_ids, NS_SID};
use xmpp_parsers::message::{Message, MessageType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

fn test_occupant_id_secret() -> OccupantIdSecret {
    // ≥32 bytes so the value also passes the production length floor;
    // the L4 wire-trace fixture mirrors what a real deployment would carry.
    OccupantIdSecret::new(b"l4-wire-trace-occupant-id-secret-32b".to_vec())
        .expect("test secret meets length floor")
}

fn full(s: &str) -> FullJid {
    s.parse().expect("valid full jid")
}
fn bare(s: &str) -> BareJid {
    s.parse().expect("valid bare jid")
}

fn occ(full_jid: FullJid, nick: &str) -> OccupantSnapshot {
    OccupantSnapshot {
        full_jid,
        nick: nick.to_string(),
        affiliation: Affiliation::Member,
        role: Role::Participant,
    }
}

fn groupchat(room: &BareJid, sender: &FullJid, body: &str) -> Message {
    let mut m = Message::new(Some(Jid::from(room.clone())));
    m.from = Some(Jid::from(sender.clone()));
    m.type_ = MessageType::Groupchat;
    m.id = Some(xmpp_parsers::message::Id("client-msg-id".to_string()));
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

#[test]
fn xep_0045_groupchat_dispatches_through_handler_chain_with_canonical_stamps() {
    // Two occupants in `team@conf.example.com`: alice (sender) and bob.
    // Alice sends `<message type='groupchat' to='room' body='hi'>`.
    //
    // Run the full room chain. Assert on:
    //   - Each occupant gets a `RouteToConnection` (XEP-0045 §7.2.13
    //     fan-out includes sender for echo).
    //   - Each routed copy carries:
    //       - `from='team@conf.example.com/alice-nick'` (XEP-0045)
    //       - `<stanza-id by='team@conf.example.com'>` (XEP-0359)
    //       - `<occupant-id>` matching the deterministic HMAC for
    //         (alice@example.com, team@conf.example.com)
    //   - `ArchiveGroupchat` emitted with `room=team@conf.example.com`
    //     and the sender's full JID.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");
    let bob = full("bob@example.com/desk");

    let occupants = vec![
        occ(alice.clone(), "alice-nick"),
        occ(bob.clone(), "bob-nick"),
    ];
    let id_gen = FixedIdGenerator("room-archive-id-1".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };

    let mut msg = groupchat(&room, &alice, "hi everyone");
    let dispatcher = default_room_dispatcher();
    let outcome = dispatcher.dispatch(&mut msg, &ctx);

    assert!(!outcome.halted, "occupant sender must not halt");

    // Archive event present.
    let archive_events: Vec<_> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ArchiveGroupchat { room, sender, .. } => {
                Some((room.clone(), sender.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(archive_events.len(), 1, "exactly one archive write");
    assert_eq!(archive_events[0].0, room);
    assert_eq!(archive_events[0].1, alice);

    // Fan-out targets — both alice and bob in occupant order.
    let routes: Vec<&Message> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::RouteToConnection { stanza, .. } => match stanza.as_ref() {
                Stanza::Message(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(routes.len(), 2, "fan-out to both occupants");

    // Every reflected copy carries the canonical stamps.
    let expected_occupant_id = generate_occupant_id(&alice.to_bare(), &room, &secret);
    for route in &routes {
        // from='room/alice-nick'
        assert_eq!(
            route.from.as_ref().map(|j| j.to_string()),
            Some("team@conf.example.com/alice-nick".to_string()),
            "reflected copy carries XEP-0045 `from='room/nick'`"
        );
        // `<stanza-id by='room' id='room-archive-id-1'>`
        let stamps = extract_stanza_ids(route);
        let room_jid: Jid = "team@conf.example.com".parse().expect("valid jid");
        let room_stamp = stamps
            .iter()
            .find(|s| s.by == room_jid)
            .expect("room-stamped stanza-id present");
        assert_eq!(room_stamp.id, "room-archive-id-1");
        // `<occupant-id id='<HMAC>'>`
        let occupant_id = extract_occupant_id_from_message(route)
            .expect("occupant-id stamped on every reflection");
        assert_eq!(occupant_id, expected_occupant_id);
    }
}

#[test]
fn xep_0045_non_occupant_halts_chain_with_typed_not_acceptable() {
    // Alice is not in the room — `OccupancyValidationHandler` halts the
    // chain with the typed XEP-0045 §7.4 reply. Assert: no
    // `ArchiveGroupchat`, no `RouteToConnection`, exactly one
    // `SendStanza` carrying the typed error.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");

    let id_gen = FixedIdGenerator("ignored".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &[], // empty — alice is NOT a member
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };

    let mut msg = groupchat(&room, &alice, "intruder");
    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(outcome.halted, "non-occupant must halt the chain");

    let archive = outcome
        .events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. }))
        .count();
    let routes = outcome
        .events
        .iter()
        .filter(|e| matches!(e, OutboundEvent::RouteToConnection { .. }))
        .count();
    assert_eq!(archive, 0, "halted chain does not archive");
    assert_eq!(routes, 0, "halted chain does not reflect");

    let send_stanzas: Vec<&Message> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::SendStanza(stanza) => match stanza.as_ref() {
                Stanza::Message(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(send_stanzas.len(), 1, "exactly one error reply");
    assert_eq!(send_stanzas[0].type_, MessageType::Error);
    let err_elem = send_stanzas[0]
        .payloads
        .iter()
        .find(|p| p.name() == "error")
        .expect("error payload");
    let parsed = StanzaError::try_from(err_elem.clone()).expect("typed StanzaError");
    assert_eq!(parsed.type_, ErrorType::Cancel);
    assert_eq!(parsed.defined_condition, DefinedCondition::NotAcceptable);
}

#[test]
fn xep_0359_room_chain_strips_client_spoofed_room_stanza_id() {
    // Q8(a) regression: client tries to spoof `<stanza-id by='room'/>`
    // — the canonicalize handler strips the spoof under the room's
    // typed BareJid equality and stamps the genuine value.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");
    let occupants = vec![occ(alice.clone(), "alice-nick")];
    let id_gen = FixedIdGenerator("genuine-room-id".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };

    let mut msg = groupchat(&room, &alice, "spoof attempt");
    msg.payloads
        .push(waddle_xmpp_core::xep0359::build_stanza_id_element(
            "client-claim",
            &"team@conf.example.com".parse::<Jid>().expect("jid"),
        ));

    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(!outcome.halted);

    // Pull the first reflection and inspect its room-stamped stanza-id.
    let reflected = outcome
        .events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::RouteToConnection { stanza, .. } => match stanza.as_ref() {
                Stanza::Message(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .expect("at least one reflection");
    // Exactly ONE `<stanza-id by='room'/>`, with the genuine id.
    let room_stamps: Vec<_> = reflected
        .payloads
        .iter()
        .filter(|p| p.name() == "stanza-id" && p.ns() == NS_SID)
        .filter(|p| p.attr("by") == Some("team@conf.example.com"))
        .collect();
    assert_eq!(room_stamps.len(), 1, "spoofed stamp stripped");
    assert_eq!(room_stamps[0].attr("id"), Some("genuine-room-id"));
}

#[test]
fn xep_0424_groupchat_retraction_emits_archive_and_tombstone_events() {
    // #229 PR18 regression: a XEP-0424 retraction sent to a room must
    // emit BOTH an `ArchiveGroupchat` (for the retraction event row)
    // and an `ApplyGroupchatRetractionTombstone` (for the original
    // row). The interpreter persists both; the chain handler
    // (`MucArchiveHandler`) is responsible for emitting them.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");
    let occupants = vec![occ(alice.clone(), "alice-nick")];
    let id_gen = FixedIdGenerator("retraction-archive".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };

    let mut msg = Message::new(Some(Jid::from(room.clone())));
    msg.from = Some(Jid::from(alice.clone()));
    msg.type_ = MessageType::Groupchat;
    msg.id = Some(xmpp_parsers::message::Id("retract-1".to_string()));
    msg.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "/me retracted".to_string(),
    );
    msg.payloads.push(
        xmpp_parsers::minidom::Element::builder("retract", "urn:xmpp:message-retract:1")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "target-stanza-id",
            )
            .build(),
    );
    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(!outcome.halted);

    let has_archive = outcome
        .events
        .iter()
        .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. }));
    let has_tombstone = outcome.events.iter().any(|e| {
        matches!(
            e,
            OutboundEvent::ApplyGroupchatRetractionTombstone {
                target_message_id,
                ..
            } if target_message_id == "target-stanza-id"
        )
    });
    assert!(
        has_archive,
        "retraction message itself must be archived as a timeline row"
    );
    assert!(
        has_tombstone,
        "retraction request must emit a tombstone event for the target row"
    );
}

#[test]
fn xep_0430_groupchat_message_emits_durable_recipient_inbox_projection() {
    // #524 compliance: the room chain emits one
    // `ProjectGroupchatInbox` per durable affiliation recipient, with
    // `is_recipient=false` for the sender's own row and
    // `is_recipient=true` for durable recipients.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");
    let bob = full("bob@example.com/desk");
    let charlie_a = full("charlie@example.com/a");
    let charlie_b = full("charlie@example.com/b");
    let occupants = vec![
        occ(alice.clone(), "alice"),
        occ(bob.clone(), "bob"),
        occ(charlie_a.clone(), "charlie"),
        occ(charlie_b.clone(), "charlie"),
    ];
    let durable_recipients = vec![bob.to_bare(), charlie_a.to_bare()];
    let id_gen = FixedIdGenerator("inbox-archive".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &occupants,
        durable_recipient_bare_jids: &durable_recipients,
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };
    let mut msg = groupchat(&room, &alice, "hello inbox");
    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(!outcome.halted);

    let projections: Vec<(BareJid, bool)> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::ProjectGroupchatInbox {
                owner,
                is_recipient,
                ..
            } => Some((owner.clone(), *is_recipient)),
            _ => None,
        })
        .collect();
    // Sender + two durable bare JIDs (charlie's two sessions collapse for
    // the active-channel live flag, not recipient expansion).
    assert_eq!(projections.len(), 3);
    let alice_bare: BareJid = "alice@example.com".parse().unwrap();
    let bob_bare: BareJid = "bob@example.com".parse().unwrap();
    let charlie_bare: BareJid = "charlie@example.com".parse().unwrap();
    let alice_row = projections.iter().find(|(o, _)| o == &alice_bare).unwrap();
    let bob_row = projections.iter().find(|(o, _)| o == &bob_bare).unwrap();
    let charlie_row = projections
        .iter()
        .find(|(o, _)| o == &charlie_bare)
        .unwrap();
    assert!(!alice_row.1, "sender's own row must not bump unread");
    assert!(bob_row.1, "durable recipients get unread bumped");
    assert!(charlie_row.1, "durable recipients get unread bumped");
}

// ── XEP-0045 §8.1 subject change capture ─────────────────────────────────

fn subject_change(room: &BareJid, sender: &FullJid, text: &str) -> Message {
    let mut m = Message::new(Some(Jid::from(room.clone())));
    m.from = Some(Jid::from(sender.clone()));
    m.type_ = MessageType::Groupchat;
    m.subjects
        .insert(xmpp_parsers::message::Lang::new(), text.to_string());
    m
}

fn occ_with_role(full_jid: FullJid, nick: &str, role: Role) -> OccupantSnapshot {
    OccupantSnapshot {
        full_jid,
        nick: nick.to_string(),
        affiliation: Affiliation::Member,
        role,
    }
}

#[test]
fn xep_0045_section_8_1_live_subject_change_chain_stamps_occupant_id() {
    // Alice (moderator) sends a subject change. Chain end-to-end:
    //   - reflected to all occupants with `from='room/alice-nick'`,
    //     `<stanza-id by='room'>`, `<occupant-id>` HMAC of alice.bare.
    //   - `PersistRoomSubject` event emitted with the captured fields.
    //   - `<subject>{text}</subject>` preserved on every reflected copy,
    //     no `<body/>` injected.
    let room = bare("team@conf.example.com");
    let alice = full("alice@example.com/web");
    let bob = full("bob@example.com/desk");
    let occupants = vec![
        occ_with_role(alice.clone(), "alice-nick", Role::Moderator),
        occ_with_role(bob.clone(), "bob-nick", Role::Participant),
    ];
    let id_gen = FixedIdGenerator("subject-stamp-1".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &alice,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 1_700_000_000,
    };

    let mut msg = subject_change(&room, &alice, "New topic");
    let dispatcher = default_room_dispatcher();
    let outcome = dispatcher.dispatch(&mut msg, &ctx);
    assert!(!outcome.halted, "moderator subject change must not halt");

    let persist = outcome
        .events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::PersistRoomSubject {
                texts,
                setter,
                setter_nick,
                ..
            } => Some((texts.clone(), setter.clone(), setter_nick.clone())),
            _ => None,
        })
        .expect("PersistRoomSubject emitted");
    assert_eq!(
        persist.0.get(""),
        Some("New topic"),
        "default-language subject text persisted"
    );
    assert_eq!(persist.1, alice.to_bare());
    assert_eq!(persist.2, "alice-nick");

    let routes: Vec<&Message> = outcome
        .events
        .iter()
        .filter_map(|e| match e {
            OutboundEvent::RouteToConnection { stanza, .. } => match stanza.as_ref() {
                Stanza::Message(m) => Some(m),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        routes.len(),
        2,
        "subject change broadcast to both occupants"
    );

    let expected_occupant_id = generate_occupant_id(&alice.to_bare(), &room, &secret);
    for route in &routes {
        assert_eq!(
            route.from.as_ref().map(|j| j.to_string()),
            Some("team@conf.example.com/alice-nick".to_string()),
        );
        assert!(
            !route.subjects.is_empty(),
            "<subject/> preserved on every reflected copy"
        );
        assert_eq!(
            route.subjects.iter().next().map(|s| s.1.as_str()),
            Some("New topic")
        );
        assert!(route.bodies.is_empty(), "subject change has no <body/>");
        let occupant_id = extract_occupant_id_from_message(route)
            .expect("XEP-0421 occupant-id stamped on subject reflection");
        assert_eq!(occupant_id, expected_occupant_id);
    }
}

#[test]
fn xep_0045_section_8_1_visitor_subject_change_halts_with_forbidden_no_broadcast() {
    // Visitors may never change the subject. Chain halts with a typed
    // `<forbidden/>` reply; no `RouteToConnection`, no `ArchiveGroupchat`,
    // no `PersistRoomSubject`.
    let room = bare("team@conf.example.com");
    let eve = full("eve@example.com/web");
    let bob = full("bob@example.com/desk");
    let occupants = vec![
        occ_with_role(eve.clone(), "eve-nick", Role::Visitor),
        occ_with_role(bob.clone(), "bob-nick", Role::Participant),
    ];
    let id_gen = FixedIdGenerator("ignored".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &eve,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };

    let mut msg = subject_change(&room, &eve, "Forbidden topic");
    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(outcome.halted, "visitor subject change MUST halt the chain");

    assert!(
        outcome
            .events
            .iter()
            .all(|e| !matches!(e, OutboundEvent::PersistRoomSubject { .. })),
        "denied subject change MUST NOT persist"
    );
    assert!(
        outcome
            .events
            .iter()
            .all(|e| !matches!(e, OutboundEvent::RouteToConnection { .. })),
        "denied subject change MUST NOT broadcast"
    );
    assert!(
        outcome
            .events
            .iter()
            .all(|e| !matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
        "denied subject change MUST NOT archive"
    );

    let stanza = outcome
        .events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::SendStanza(s) => Some(s.as_ref()),
            _ => None,
        })
        .expect("typed error reply emitted to sender");
    let Stanza::Message(reply) = stanza else {
        panic!("expected SendStanza(Message), got {stanza:?}");
    };
    assert_eq!(reply.type_, MessageType::Error);
    let error_elem = reply
        .payloads
        .iter()
        .find(|p| p.name() == "error")
        .expect("error payload attached");
    let parsed = StanzaError::try_from(error_elem.clone()).expect("typed StanzaError parse");
    assert_eq!(parsed.type_, ErrorType::Auth, "§8.1 deny is type='auth'");
    assert_eq!(
        parsed.defined_condition,
        DefinedCondition::Forbidden,
        "§8.1 deny carries <forbidden/>"
    );
}

#[test]
fn xep_0045_section_8_1_participant_allowed_via_changesubject_knob() {
    // §8.1: with `muc#roomconfig_changesubject` enabled, a mere
    // participant may change the subject (#1265 item 8). Chain
    // Continues; PersistRoomSubject emitted.
    let room = bare("team@conf.example.com");
    let bob = full("bob@example.com/desk");
    let alice = full("alice@example.com/web");
    let occupants = vec![
        occ_with_role(bob.clone(), "bob-nick", Role::Participant),
        occ_with_role(alice.clone(), "alice-nick", Role::Participant),
    ];
    let id_gen = FixedIdGenerator("subject-stamp-2".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &bob,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: true,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 1_700_000_000,
    };
    let mut msg = subject_change(&room, &bob, "Bob's topic");
    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(!outcome.halted);
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, OutboundEvent::PersistRoomSubject { .. })),
        "participant with changesubject knob is allowed to change subject"
    );
}

#[test]
fn xep_0045_section_8_1_subject_with_body_is_not_a_subject_change() {
    // §8.1 distinguishes subject changes by `<subject/>` AND no
    // `<body/>`. A message with both is a regular groupchat message
    // and must not be captured as a subject change — otherwise a
    // visitor could write to a moderated room by attaching a
    // `<subject/>` to their `<body/>`.
    let room = bare("team@conf.example.com");
    let eve = full("eve@example.com/web");
    let bob = full("bob@example.com/desk");
    let occupants = vec![
        occ_with_role(eve.clone(), "eve-nick", Role::Participant),
        occ_with_role(bob.clone(), "bob-nick", Role::Participant),
    ];
    let id_gen = FixedIdGenerator("subject-stamp-3".to_string());
    let secret = test_occupant_id_secret();
    let ctx = RoomContext {
        room: &room,
        sender_full: &eve,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: crate::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };
    let mut msg = subject_change(&room, &eve, "topic-ish");
    msg.bodies
        .insert(xmpp_parsers::message::Lang::new(), "hi".to_string());

    let outcome = default_room_dispatcher().dispatch(&mut msg, &ctx);
    assert!(!outcome.halted);
    assert!(
        outcome
            .events
            .iter()
            .all(|e| !matches!(e, OutboundEvent::PersistRoomSubject { .. })),
        "body+subject is not a subject change; no PersistRoomSubject"
    );
    // Regular groupchat fan-out + archive still happen (this confirms
    // the handler is a true passthrough, not a halting filter).
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, OutboundEvent::RouteToConnection { .. })),
        "regular message still fans out"
    );
    assert!(
        outcome
            .events
            .iter()
            .any(|e| matches!(e, OutboundEvent::ArchiveGroupchat { .. })),
        "regular message still archives"
    );
}
