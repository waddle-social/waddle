use waddle_xmpp::disco::info::{muc_room_features, server_features, Feature};
use waddle_xmpp::mam::{InMemoryMamStorage, MamArchiveKind, MamStorage};
use waddle_xmpp_core::mam::{ArchivedMessage, MamQuery};

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

/// XEP-0313 §4.3.1: querying a personal archive with `with` equal to
/// the archive owner's bare JID is a self-chat query. Both archived
/// endpoints must match the owner's bare JID; ordinary contacts and
/// unrelated rows must not leak into the result set or its RSM count.
#[tokio::test]
async fn owner_with_filters_self_chat_before_rsm_pagination() {
    let storage = InMemoryMamStorage::new();
    let archive: BareJid = "juliet@example.com".parse().unwrap();

    for (id, from, to) in [
        (
            "self-one",
            "juliet@example.com/balcony",
            "juliet@example.com/chamber",
        ),
        (
            "ordinary-contact",
            "romeo@example.com/phone",
            "juliet@example.com/chamber",
        ),
        (
            "self-two",
            "juliet@example.com/tablet",
            "juliet@example.com",
        ),
        (
            "unrelated",
            "mercutio@example.com/phone",
            "benvolio@example.com/tablet",
        ),
    ] {
        storage
            .store_message(
                &archive,
                &ArchivedMessage {
                    id: id.to_string(),
                    ..ArchivedMessage::for_test(from.parse().unwrap(), to.parse().unwrap())
                },
            )
            .await
            .unwrap();
    }

    let first = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some("juliet@example.com".parse().unwrap()),
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.messages[0].id, "self-one");
    assert_eq!(first.count, Some(2));
    assert!(!first.complete);

    let second = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some("juliet@example.com".parse().unwrap()),
                after_id: first.last_id,
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.messages[0].id, "self-two");
    assert_eq!(second.count, Some(2));
    assert!(second.complete);

    let empty = storage
        .query_messages(
            &archive,
            MamArchiveKind::Personal,
            &MamQuery {
                with: Some("juliet@example.com".parse().unwrap()),
                after_id: second.last_id,
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(empty.messages.is_empty());
    assert_eq!(empty.count, Some(2));
    assert!(empty.complete);
}

/// XEP-0313 §4.3.1 ordinary `with` semantics still apply to room
/// archives. A full occupant JID is an exact publisher filter; it is
/// not the personal-archive owner-self special case merely because its
/// bare form equals the archive JID.
#[tokio::test]
async fn room_archive_full_occupant_with_is_exact_before_rsm_pagination() {
    let storage = InMemoryMamStorage::new();
    let room: BareJid = "room@example.com".parse().unwrap();
    let room_jid: Jid = "room@example.com".parse().unwrap();
    let alice: Jid = "room@example.com/alice".parse().unwrap();
    let base = chrono::DateTime::parse_from_rfc3339("2026-07-02T12:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    for (id, seconds, from) in [
        ("alice-one", 0, alice.clone()),
        ("bob-one", 1, "room@example.com/bob".parse().unwrap()),
        ("alice-two", 2, alice.clone()),
    ] {
        storage
            .store_message(
                &room,
                &ArchivedMessage {
                    id: id.to_string(),
                    timestamp: base + chrono::Duration::seconds(seconds),
                    ..ArchivedMessage::for_test(from, room_jid.clone())
                },
            )
            .await
            .unwrap();
    }

    let first = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice.clone()),
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(first.messages[0].id, "alice-one");
    assert_eq!(first.count, Some(2));
    assert!(!first.complete);

    let second = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(alice),
                after_id: first.last_id,
                max: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.messages[0].id, "alice-two");
    assert_eq!(second.count, Some(2));
    assert!(second.complete);

    let bare_room = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some(room_jid),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(bare_room.messages.len(), 3);

    let empty = storage
        .query_messages(
            &room,
            MamArchiveKind::Room,
            &MamQuery {
                with: Some("room@example.com/charlie".parse().unwrap()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(empty.messages.is_empty());
    assert_eq!(empty.count, Some(0));
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

/// XEP-0313 archive writes can participate in the ingress transaction: an
/// uncommitted row is not visible after the caller rolls the transaction back.
#[tokio::test]
async fn xep0313_tx_archive_write_rolls_back_with_caller_transaction() {
    use sqlx::postgres::PgPoolOptions;
    use waddle_xmpp::mam::{
        store_archived_message_on_connection, ArchiveExpectation, MamTxStoreOutcome, SqlxMamStorage,
    };
    use waddle_xmpp_core::mam::{ArchivedMucSender, ArchivedRichMessage};
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;

    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping XEP-0313 Postgres tx-write test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    SqlxMamStorage::open(&url)
        .await
        .expect("initialize MAM schema");
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect Postgres");
    let archive: BareJid = format!("mam-tx-{}@conference.example.com", uuid::Uuid::now_v7())
        .parse()
        .expect("archive JID");
    let id = format!("mam-tx-{}", uuid::Uuid::now_v7());
    let message = ArchivedMessage {
        id: id.clone(),
        body: Some("transactional archive".to_string()),
        origin_id: Some(OriginId::new("mam-tx-origin")),
        message_type: MessageType::Groupchat,
        rich: Some(ArchivedRichMessage {
            muc_sender: Some(ArchivedMucSender {
                jid: "alice@example.com/session".parse().expect("sender JID"),
                affiliation: Affiliation::Member,
                role: Role::Participant,
            }),
            ..ArchivedRichMessage::default()
        }),
        ..ArchivedMessage::for_test(
            format!("{archive}/alice").parse().expect("occupant JID"),
            Jid::from(archive.clone()),
        )
    };
    let mut tx = pool.begin().await.expect("begin transaction");
    assert!(matches!(
        store_archived_message_on_connection(&mut tx, &archive, &message, ArchiveExpectation::Fresh)
            .await
            .expect("store archive row"),
        MamTxStoreOutcome::Inserted(ref stanza_id)
            if stanza_id.id == id && stanza_id.by == archive
    ));
    tx.rollback().await.expect("roll back transaction");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mam_messages WHERE id = $1")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .expect("count rolled-back row");
    assert_eq!(count, 0);
}
