use waddle_xmpp::disco::info::{muc_room_features, server_features, Feature};

#[test]
fn server_root_disco_does_not_advertise_a_domain_archive() {
    let features = server_features();

    assert!(!features.contains(&Feature::mam()));
    assert!(!features.contains(&Feature::mam_extended()));
}

#[test]
fn muc_room_disco_advertises_mam_extended_for_supported_id_filters() {
    let features = muc_room_features(true, true, true, false, false);

    assert!(features.contains(&Feature::mam()));
    assert!(features.contains(&Feature::mam_extended()));
}

// ── §Security "Sender Impersonation" + "MUC message spoofing" +
//    §MUC Archives (PR for #1250 / #1251 / #1268) ────────────────────

use jid::{BareJid, FullJid, Jid};
use waddle_xmpp::protocol::event::OutboundEvent;
use waddle_xmpp::protocol::id_gen::FixedIdGenerator;
use waddle_xmpp::protocol::room::archive::MucArchiveHandler;
use waddle_xmpp::protocol::room::canonicalize::MucCanonicalizeHandler;
use waddle_xmpp::protocol::room::context::{OccupantSnapshot, RoomContext};
use waddle_xmpp::protocol::room::traits::{RoomHandler, RoomHandlerOutcome};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OCCUPANT_ID_SECRET_MIN_BYTES};
use waddle_xmpp_core::types::{Affiliation, Role};
use xmpp_parsers::message::{Message, MessageType};

const MUC_USER_NS: &str = "http://jabber.org/protocol/muc#user";

fn secret() -> OccupantIdSecret {
    OccupantIdSecret::new(vec![7u8; OCCUPANT_ID_SECRET_MIN_BYTES]).expect("valid secret")
}

fn groupchat(room: &BareJid, sender: &FullJid, body: &str) -> Message {
    let mut m = Message::new(Some(Jid::from(room.clone())));
    m.from = Some(Jid::from(sender.clone()));
    m.type_ = MessageType::Groupchat;
    m.bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    m
}

fn run_chain(
    room: &BareJid,
    sender: &FullJid,
    nick: &str,
    msg: &mut Message,
) -> Vec<OutboundEvent> {
    let occupants = vec![OccupantSnapshot {
        full_jid: sender.clone(),
        nick: nick.to_string(),
        affiliation: Affiliation::Member,
        role: Role::Participant,
    }];
    let id_gen = FixedIdGenerator("fixed-stanza-id".to_string());
    let secret = secret();
    let ctx = RoomContext {
        room,
        sender_full: sender,
        occupants: &occupants,
        durable_recipient_bare_jids: &[],
        managed_room_forbidden: false,
        room_moderated: false,
        room_occupants_may_change_subject: false,
        room_members_only: false,
        pin_permission: waddle_xmpp::muc::PinPermission::default(),
        id_gen: &id_gen,
        occupant_id_secret: &secret,
        sender_nickname_generation: 0,
        project_sender_inbox: true,
        synthetic_sender_authority: None,
        dispatch_timestamp: 0,
    };
    let mut events = Vec::new();
    for handler in [
        &MucCanonicalizeHandler as &dyn RoomHandler,
        &MucArchiveHandler as &dyn RoomHandler,
    ] {
        match handler.handle(msg, &ctx) {
            RoomHandlerOutcome::Continue(e) => events.extend(e),
            RoomHandlerOutcome::Halt(_) => panic!("chain must not halt"),
        }
    }
    events
}

/// XEP-0313 §Security "MUC message spoofing" (#1251): a forged
/// occupant-supplied `<x xmlns='muc#user'>` never reaches the archive
/// event or the reflected message.
#[test]
fn xep0313_forged_muc_user_x_never_reaches_archive_or_reflection() {
    let room: BareJid = "coven@chat.shakespeare.lit".parse().unwrap();
    let sender: FullJid = "mallory@shakespeare.lit/web".parse().unwrap();
    let mut msg = groupchat(&room, &sender, "innocent-looking message");
    msg.payloads.push(
        minidom::Element::builder("x", MUC_USER_NS)
            .append(
                minidom::Element::builder("item", MUC_USER_NS)
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        "victim@shakespeare.lit",
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("affiliation").to_owned(),
                        "owner",
                    )
                    .build(),
            )
            .build(),
    );

    let events = run_chain(&room, &sender, "mallory", &mut msg);

    // Reflected (in-flight) message is clean.
    assert!(
        !msg.payloads.iter().any(|p| p.ns() == MUC_USER_NS),
        "reflection must not carry the forged muc#user <x>"
    );
    // Archived copy is clean of the forgery too; the only muc#user
    // content the interpreter may add later is the room-authored
    // real-JID item derived from `sender_item`.
    let archived = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::ArchiveGroupchat {
                message,
                sender_item,
                ..
            } => Some((message, sender_item)),
            _ => None,
        })
        .expect("archive event emitted");
    assert!(
        !archived.0.payloads.iter().any(|p| p.ns() == MUC_USER_NS),
        "archive event message must not carry the forged muc#user <x>"
    );
    let sender_item = archived.1.as_ref().expect("sender_item captured");
    assert_eq!(
        sender_item.jid.to_string(),
        "mallory@shakespeare.lit/web",
        "sender_item must carry the real sender, not the forged victim"
    );
}

/// XEP-0313 §MUC Archives (#1268): the archive event carries the
/// sender's typed authority snapshot (real full JID + affiliation +
/// role) so the interpreter can bake the non-anonymous real-JID
/// disclosure into the archived copy.
#[test]
fn xep0313_archive_event_captures_sender_real_jid_item() {
    let room: BareJid = "coven@chat.shakespeare.lit".parse().unwrap();
    let sender: FullJid = "crone1@shakespeare.lit/desktop".parse().unwrap();
    let mut msg = groupchat(&room, &sender, "Thrice the brinded cat hath mew'd.");

    let events = run_chain(&room, &sender, "firstwitch", &mut msg);

    let sender_item = events
        .iter()
        .find_map(|e| match e {
            OutboundEvent::ArchiveGroupchat { sender_item, .. } => sender_item.as_ref(),
            _ => None,
        })
        .expect("sender_item captured");
    assert_eq!(
        sender_item.jid.to_string(),
        "crone1@shakespeare.lit/desktop"
    );
    assert_eq!(sender_item.affiliation, Affiliation::Member);
    assert_eq!(sender_item.role, Role::Participant);

    // And the wire builder produces the XEP-0313 §MUC Archives shape.
    let x = waddle_xmpp_core::mam::build_archived_muc_sender_x(sender_item);
    assert_eq!(x.name(), "x");
    assert_eq!(x.ns(), MUC_USER_NS);
    let item = x.get_child("item", MUC_USER_NS).expect("item child");
    assert_eq!(item.attr("jid"), Some("crone1@shakespeare.lit/desktop"));
    assert_eq!(item.attr("affiliation"), Some("member"));
    assert_eq!(item.attr("role"), Some("participant"));
}

/// XEP-0313 §Security "Sender Impersonation" (#1250): result envelopes
/// carry `from` = the queried archive JID (the room bare JID for MUC
/// archives) so strict clients accept them against their open query.
#[test]
fn xep0313_result_envelope_from_is_the_room_jid() {
    let archive: Jid = "coven@chat.shakespeare.lit".parse().unwrap();
    let requester: Jid = "hag66@shakespeare.lit/pda".parse().unwrap();
    let row = waddle_xmpp_core::mam::ArchivedMessage {
        id: "row-1".to_string(),
        body: Some("hello".to_string()),
        message_type: MessageType::Groupchat,
        ..waddle_xmpp_core::mam::ArchivedMessage::for_test(
            "coven@chat.shakespeare.lit/firstwitch".parse().unwrap(),
            archive.clone(),
        )
    };

    let envelopes =
        waddle_xmpp_core::mam::build_result_messages("q1", &archive, &requester, &[row]);

    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0].from.as_ref(), Some(&archive));
    assert_eq!(envelopes[0].to.as_ref(), Some(&requester));
}
