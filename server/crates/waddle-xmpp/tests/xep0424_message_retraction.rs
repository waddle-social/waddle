//! XEP-0424: Message Retraction — dedicated conformance suite.
//!
//! In-crate `xep::xep0424::tests` covers parsing/building of the
//! `<retract/>` and `<retracted/>` elements. This suite pins the
//! audit-level invariants that crossing the public-API boundary
//! exposes:
//!
//! - §3 namespace string `urn:xmpp:message-retract:1`.
//! - §"Tombstones" MUST: any service that replaces retracted MAM
//!   entries with a tombstone MUST advertise
//!   `urn:xmpp:message-retract:1#tombstone`. Waddle's MAM layer does
//!   exactly that (`mam::storage::tombstone::apply_tombstone` +
//!   `protocol::event::outbound::ReplaceWithTombstone`), so the
//!   advert is mandatory on every disco surface where retraction is
//!   advertised at all — server-wide and per MUC room.
//! - §3 wire shape: `<retract id='ORIGINAL' xmlns='urn:xmpp:message-retract:1'/>`,
//!   with a `<body/>` fallback on the wrapping message (the
//!   builder's responsibility).
//! - §"Tombstones" wire shape: `<retracted id='RETRACTION_MSG_ID'
//!   stamp='…'/>` — the `id` references the retraction message's
//!   `id`, NOT the original. The builder must keep that distinction
//!   so the MAM tombstone can be matched back to its retraction.

use waddle_xmpp::disco::{muc_room_features, server_features, Feature};
use waddle_xmpp::xep::xep0424::{
    build_retract_element, build_retracted_element, build_retraction_message,
    build_tombstone_message, extract_retraction_from_message, is_retract_element,
    is_retracted_element, is_retraction_message, is_tombstone_message, RetractionKind,
    NS_MESSAGE_RETRACT,
};

async fn assert_author_retry_cannot_downgrade_moderation_tombstone<S>(storage: S)
where
    S: waddle_xmpp::mam::MamStorage + Clone,
{
    use jid::{BareJid, Jid};
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedModeration, ArchivedRichPayload, ArchivedTombstone, RichMessageId,
        RichText, StoreOutcome, TerminalTombstoneOutcome,
    };
    use xmpp_parsers::message::MessageType;

    let room = "terminal-tombstone@conference.example.com"
        .parse::<BareJid>()
        .expect("room JID");
    let archive_id = "terminal-tombstone-target";
    let original = ArchivedMessage {
        id: archive_id.to_string(),
        body: Some("moderated content".to_string()),
        message_type: MessageType::Groupchat,
        ..ArchivedMessage::for_test(
            "terminal-tombstone@conference.example.com/alice"
                .parse::<Jid>()
                .expect("occupant JID"),
            Jid::from(room.clone()),
        )
    };
    assert_eq!(
        storage
            .store_message(&room, &original)
            .await
            .expect("store original"),
        StoreOutcome::Stored(archive_id.to_string())
    );

    let moderation = ArchivedTombstone {
        retraction_id: None,
        stamp: chrono::Utc::now(),
        moderation: Some(ArchivedModeration {
            target_id: RichMessageId::new(archive_id).expect("target id"),
            moderated_by: "owner@example.com".parse::<Jid>().expect("moderator JID"),
            stamp: Some(chrono::Utc::now()),
            reason: RichText::new("room policy"),
        }),
        sender_scope: None,
    };
    assert!(storage
        .replace_with_tombstone(archive_id, moderation.clone())
        .await
        .expect("install moderation tombstone"));

    let author_retry = ArchivedTombstone {
        retraction_id: RichMessageId::new("author-retraction-retry"),
        stamp: chrono::Utc::now(),
        moderation: None,
        sender_scope: None,
    };
    assert_eq!(
        storage
            .replace_with_terminal_tombstone(archive_id, author_retry)
            .await
            .expect("apply terminal author retry"),
        TerminalTombstoneOutcome::AlreadyTombstoned
    );

    let retained = storage
        .get_message(archive_id)
        .await
        .expect("lookup retained row")
        .expect("retained row");
    assert_eq!(
        retained.rich.and_then(|rich| rich.payload),
        Some(ArchivedRichPayload::Tombstone(moderation)),
        "an XEP-0424 author retry must not downgrade XEP-0425 attribution or reason"
    );

    let live_id = "terminal-tombstone-live-target";
    let mut live = original;
    live.id = live_id.to_string();
    assert_eq!(
        storage
            .store_message(&room, &live)
            .await
            .expect("store live terminal-replacement target"),
        StoreOutcome::Stored(live_id.to_string())
    );
    let author_tombstone = ArchivedTombstone {
        retraction_id: RichMessageId::new("live-author-retraction"),
        stamp: chrono::Utc::now(),
        moderation: None,
        sender_scope: None,
    };
    assert_eq!(
        storage
            .replace_with_terminal_tombstone(live_id, author_tombstone.clone())
            .await
            .expect("replace live row terminally"),
        TerminalTombstoneOutcome::Replaced
    );
    assert_eq!(
        storage
            .replace_with_terminal_tombstone(live_id, author_tombstone.clone())
            .await
            .expect("repeat terminal replacement"),
        TerminalTombstoneOutcome::AlreadyTombstoned
    );
    assert_eq!(
        storage
            .replace_with_terminal_tombstone("missing-terminal-target", author_tombstone)
            .await
            .expect("missing terminal replacement"),
        TerminalTombstoneOutcome::NotFound
    );
}

#[tokio::test]
async fn xep0424_author_retry_preserves_terminal_moderation_tombstone_in_all_backends() {
    assert_author_retry_cannot_downgrade_moderation_tombstone(
        waddle_xmpp::mam::InMemoryMamStorage::new(),
    )
    .await;
    assert_author_retry_cannot_downgrade_moderation_tombstone(
        waddle_xmpp::mam::SqlxMamStorage::open_in_memory()
            .await
            .expect("SQLite MAM storage"),
    )
    .await;
}

// ── §3 namespace ─────────────────────────────────────────────────────

#[test]
fn xep0424_namespace_matches_spec_v1() {
    // XEP-0424 v1 (the published Proposed-status revision) bumps
    // the namespace from `:0` to `:1`. This test pins the literal so
    // a stray import of an old constant can't silently re-version.
    assert_eq!(NS_MESSAGE_RETRACT, "urn:xmpp:message-retract:1");
}

// ── §"Tombstones" MUST disco advertisement ───────────────────────────

#[test]
fn xep0424_server_advertises_tombstone_feature() {
    // XEP-0424 §"Tombstones": "A service which supports tombstones
    // MUST advertise the 'urn:xmpp:message-retract:1#tombstone'
    // feature in its Service Discovery responses." Waddle's MAM
    // storage replaces retracted entries with tombstones, so the
    // advert is mandatory.
    let feats = server_features();
    let target = Feature::message_retraction_tombstone();
    assert!(
        feats.iter().any(|f| f == &target),
        "server_features() must advertise `urn:xmpp:message-retract:1#tombstone` \
         because the MAM layer rewrites entries with tombstones"
    );
    // Defence-in-depth: base retract feature still ships too.
    assert!(feats.iter().any(|f| f == &Feature::message_retraction()));
}

#[test]
fn xep0424_muc_rooms_advertise_tombstone_feature_in_every_configuration() {
    // MUC rooms also stamp tombstones in their archives. The advert
    // travels with the room regardless of (persistent × members_only
    // × moderated × forum) configuration since the storage layer
    // doesn't gate tombstones on room config.
    let target = Feature::message_retraction_tombstone();
    for persistent in [false, true] {
        for members_only in [false, true] {
            for moderated in [false, true] {
                for forum in [false, true] {
                    let feats = muc_room_features(persistent, members_only, true, moderated, forum);
                    assert!(
                        feats.iter().any(|f| f == &target),
                        "muc_room_features(persistent={persistent}, members_only={members_only}, \
                         moderated={moderated}, forum={forum}) MUST advertise \
                         `urn:xmpp:message-retract:1#tombstone`"
                    );
                }
            }
        }
    }
}

#[test]
fn xep0424_tombstone_feature_constructor_pins_namespace_string() {
    // Defence-in-depth against a future rename that silently changes
    // the wire string.
    assert_eq!(
        Feature::message_retraction_tombstone().0,
        "urn:xmpp:message-retract:1#tombstone"
    );
}

// ── §3 wire shape: <retract/> ────────────────────────────────────────

#[test]
fn xep0424_retract_element_matches_spec_shape() {
    // XEP-0424 §3 example:
    //   <retract id='ORIGINAL_ID' xmlns='urn:xmpp:message-retract:1'/>
    let elem = build_retract_element("origin-id-1");
    assert_eq!(elem.name(), "retract");
    assert_eq!(elem.ns(), NS_MESSAGE_RETRACT);
    assert_eq!(elem.attr("id"), Some("origin-id-1"));
    assert_eq!(
        elem.children().count(),
        0,
        "<retract/> is a leaf element per §3"
    );
}

#[test]
fn xep0424_classifier_accepts_retract_shape_only() {
    let canonical = build_retract_element("x");
    assert!(is_retract_element(&canonical));

    let wrong_ns = minidom::Element::builder("retract", "wrong:ns")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .build();
    assert!(!is_retract_element(&wrong_ns));

    let wrong_name = minidom::Element::builder("retraction", NS_MESSAGE_RETRACT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .build();
    assert!(!is_retract_element(&wrong_name));
}

#[test]
fn xep0424_retraction_message_carries_retract_and_fallback_body() {
    // §3 RECOMMENDED: the retraction message ships with a `<body/>`
    // fallback so non-supporting clients still see something. The
    // helper's responsibility is to provide one; without it the
    // retraction would silently disappear on legacy clients.
    let msg = build_retraction_message(
        Some("lord@capulet.example".parse().expect("jid")),
        Some("juliet@example.com/web".parse().expect("jid")),
        "wrong-recipient-1",
    );
    assert!(
        is_retraction_message(&msg),
        "built retraction MUST classify as one"
    );
    assert!(
        !msg.bodies.is_empty(),
        "§3 RECOMMENDED <body/> fallback is present"
    );

    let kind = extract_retraction_from_message(&msg).expect("retraction extracted");
    match kind {
        RetractionKind::Request(r) => assert_eq!(r.retracts_id, "wrong-recipient-1"),
        RetractionKind::Tombstone(_) => panic!("expected Request, got Tombstone"),
    }
}

// ── §"Tombstones" wire shape: <retracted/> ──────────────────────────

#[test]
fn xep0424_retracted_element_uses_retraction_id_not_original_id() {
    // XEP-0424 §"Tombstones": "the <retracted/> element MUST
    // include an 'id' attribute that's set to the value of the
    // retraction's <message/> element's 'id' attribute, so that
    // clients can match the tombstone to the retraction." That is
    // emphatically NOT the original message id (which is preserved
    // as the stanza's own `id`, marking the position in the archive
    // the tombstone occupies).
    let elem = build_retracted_element("retract-msg-id-99", Some("2026-05-16T13:00:00Z"));
    assert_eq!(elem.name(), "retracted");
    assert_eq!(elem.ns(), NS_MESSAGE_RETRACT);
    assert_eq!(
        elem.attr("id"),
        Some("retract-msg-id-99"),
        "`id` on <retracted/> is the retraction message's id (§Tombstones MUST)"
    );
    assert_eq!(
        elem.attr("stamp"),
        Some("2026-05-16T13:00:00Z"),
        "`stamp` (SHOULD per spec) carries the retraction timestamp"
    );
}

#[test]
fn xep0424_retracted_element_omits_stamp_when_unknown() {
    // `stamp` is SHOULD, not MUST. The builder must accept absence
    // without falling back to a placeholder.
    let elem = build_retracted_element("retract-msg-id-99", None);
    assert!(
        elem.attr("stamp").is_none(),
        "absent stamp MUST NOT be invented"
    );
}

#[test]
fn xep0424_classifier_accepts_retracted_shape_only() {
    let canonical = build_retracted_element("x", Some("2026-05-16T13:00:00Z"));
    assert!(is_retracted_element(&canonical));

    let wrong_ns = minidom::Element::builder("retracted", "wrong:ns")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .build();
    assert!(!is_retracted_element(&wrong_ns));

    let wrong_name = minidom::Element::builder("retraction", NS_MESSAGE_RETRACT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "x")
        .build();
    assert!(!is_retracted_element(&wrong_name));
}

#[test]
fn xep0424_tombstone_message_carries_retracted_and_preserves_original_id() {
    // The tombstone REPLACES the original archive entry. The
    // stanza's `id` MUST be the original message id (so MAM keeps
    // the same archival position), while `<retracted id='…'/>`
    // points at the retraction's id. Two distinct identifiers; the
    // helper must keep them straight.
    let msg = build_tombstone_message(
        Some("room@muc.example/joiner".parse().expect("jid")),
        Some("room@muc.example/setter".parse().expect("jid")),
        "ORIGINAL_MSG_ID",
        "RETRACTION_MSG_ID",
        Some("2026-05-16T13:00:00Z"),
    );
    assert!(
        is_tombstone_message(&msg),
        "tombstone classifier accepts the built message"
    );
    assert_eq!(
        msg.id.as_ref().map(|id| id.0.as_str()),
        Some("ORIGINAL_MSG_ID"),
        "stanza id MUST equal the ORIGINAL message id (preserves archive position)"
    );

    let kind = extract_retraction_from_message(&msg).expect("retracted extracted");
    match kind {
        RetractionKind::Tombstone(t) => {
            assert_eq!(
                t.retraction_id, "RETRACTION_MSG_ID",
                "<retracted id='…'/> MUST cite the RETRACTION id, not the original"
            );
            assert_eq!(t.stamp.as_deref(), Some("2026-05-16T13:00:00Z"));
        }
        RetractionKind::Request(_) => panic!("expected Tombstone, got Request"),
    }
    // A tombstone is NOT a retraction request — the two classifiers
    // must be mutually exclusive.
    assert!(
        !is_retraction_message(&msg),
        "tombstone MUST NOT classify as a retraction request"
    );
}

// ── Extractor robustness ────────────────────────────────────────────

#[test]
fn xep0424_extract_returns_none_for_payloads_with_empty_id() {
    // XEP-0424 makes `id` a required attribute on both <retract/>
    // and <retracted/>. An attacker stuffing in `id=""` to confuse a
    // naive consumer must be ignored.
    let mut msg = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    msg.payloads.push(
        minidom::Element::builder("retract", NS_MESSAGE_RETRACT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
            .build(),
    );
    msg.payloads.push(
        minidom::Element::builder("retracted", NS_MESSAGE_RETRACT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
            .build(),
    );
    assert!(
        extract_retraction_from_message(&msg).is_none(),
        "empty-id retraction/retracted payloads MUST be ignored"
    );
}

#[tokio::test]
async fn xep0424_groupchat_retransmit_after_retraction_hits_tombstone() {
    use jid::{BareJid, Jid};
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
        InMemoryMamStorage, MamStorage, StoreOutcome,
    };
    use waddle_xmpp_core::mam::ArchivedMucSender;
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;
    use xmpp_parsers::message::MessageType;

    fn archived(id: &str, real_jid: &str, generation: u64) -> ArchivedMessage {
        ArchivedMessage {
            id: id.to_string(),
            body: Some("retract me".to_string()),
            origin_id: Some(OriginId::new("retracted-origin")),
            message_type: MessageType::Groupchat,
            nickname_generation: Some(generation),
            rich: Some(ArchivedRichMessage {
                muc_sender: Some(ArchivedMucSender {
                    jid: real_jid.parse::<Jid>().expect("real sender JID"),
                    affiliation: Affiliation::Member,
                    role: Role::Participant,
                }),
                ..ArchivedRichMessage::default()
            }),
            ..ArchivedMessage::for_test(
                "room@conference.example.com/alice"
                    .parse::<Jid>()
                    .expect("occupant JID"),
                "room@conference.example.com"
                    .parse::<Jid>()
                    .expect("room JID"),
            )
        }
    }

    // XEP-0424 §Business Rules says a MUC service SHOULD prevent further
    // distribution of a retracted message; the retained tombstone must win
    // over a later retry carrying the original stable origin-id.
    let storage = InMemoryMamStorage::new();
    let room = "room@conference.example.com"
        .parse::<BareJid>()
        .expect("room bare JID");
    let original_id = "retracted-archive-id";
    let original = archived(original_id, "alice@example.com/session-a", 7);
    assert_eq!(
        storage
            .store_message(&room, &original)
            .await
            .expect("store original"),
        StoreOutcome::Stored(original_id.to_string())
    );
    assert!(storage
        .replace_with_tombstone(
            original_id,
            ArchivedTombstone {
                retraction_id: None,
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: None,
            },
        )
        .await
        .expect("replace with tombstone"));

    let retry = archived("retry-archive-id", "alice@example.com/session-b", 8);
    assert_eq!(
        storage
            .store_message(&room, &retry)
            .await
            .expect("store retry"),
        StoreOutcome::TombstoneHit(original_id.to_string()),
        "XEP-0424 tombstone retry must not create a new live archive row"
    );
    assert_eq!(storage.count_messages(&room).await.expect("count"), 1);
    let retained = storage
        .get_message_by_archive_or_stanza_id(&room, original_id)
        .await
        .expect("lookup tombstone")
        .expect("tombstone retained");
    assert!(retained.body.is_none());
    assert!(matches!(
        retained.rich.and_then(|rich| rich.payload),
        Some(ArchivedRichPayload::Tombstone(_))
    ));
}

/// XEP-0424 tombstones remain terminal when the retry is archived through a
/// caller-owned Postgres transaction.
#[tokio::test]
async fn xep0424_tx_archive_retry_after_tombstone_returns_tombstone_hit() {
    use sqlx::postgres::PgPoolOptions;
    use waddle_xmpp::mam::{
        store_archived_message_on_connection, MamStorage, MamTxStoreOutcome, SqlxMamStorage,
    };
    use waddle_xmpp_core::mam::{
        ArchivedMessage, ArchivedMucSender, ArchivedRichMessage, ArchivedTombstone,
    };
    use waddle_xmpp_core::types::{Affiliation, Role};
    use waddle_xmpp_core::xep0359::OriginId;

    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping XEP-0424 Postgres tx-write test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let storage = SqlxMamStorage::open(&url)
        .await
        .expect("initialize MAM schema");
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await
        .expect("connect Postgres");
    let room: jid::BareJid = format!("retract-tx-{}@conference.example.com", uuid::Uuid::now_v7())
        .parse()
        .expect("room JID");
    let original_id = format!("retract-tx-{}", uuid::Uuid::now_v7());
    let message = |id: String| ArchivedMessage {
        id,
        body: Some("retracted transaction fixture".to_string()),
        origin_id: Some(OriginId::new("retract-tx-origin")),
        message_type: xmpp_parsers::message::MessageType::Groupchat,
        rich: Some(ArchivedRichMessage {
            muc_sender: Some(ArchivedMucSender {
                jid: "alice@example.com/session".parse().expect("sender JID"),
                affiliation: Affiliation::Member,
                role: Role::Participant,
            }),
            ..ArchivedRichMessage::default()
        }),
        ..ArchivedMessage::for_test(
            format!("{room}/alice").parse().expect("occupant JID"),
            jid::Jid::from(room.clone()),
        )
    };
    let mut insert_tx = pool.begin().await.expect("begin insert transaction");
    store_archived_message_on_connection(&mut insert_tx, &room, &message(original_id.clone()))
        .await
        .expect("store original");
    insert_tx.commit().await.expect("commit original");
    assert!(storage
        .replace_with_tombstone(
            &original_id,
            ArchivedTombstone {
                retraction_id: None,
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: None,
            },
        )
        .await
        .expect("replace original with tombstone"));
    let mut retry_tx = pool.begin().await.expect("begin retry transaction");
    assert!(matches!(
        store_archived_message_on_connection(
            &mut retry_tx,
            &room,
            &message(format!("retract-retry-{}", uuid::Uuid::now_v7())),
        )
        .await
        .expect("retry against tombstone"),
        MamTxStoreOutcome::TombstoneHit(ref stanza_id) if stanza_id.id == original_id
    ));
    retry_tx.commit().await.expect("commit retry transaction");
}
