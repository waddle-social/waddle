use super::*;

// XEP-0424 — groupchat retraction lookup (#281)
// -----------------------------------------------------------------

#[tokio::test]
async fn xep_0424_groupchat_retraction_target_resolved_by_room_stanza_id() {
    // XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // `<retract id='...'/>` cites the room-assigned XEP-0359 stanza-id
    // (the `<stanza-id by='room'/>` value), NOT the original wire `id`
    // attribute. That stanza-id is persisted as the archive primary key
    // (see `finish_archive_groupchat_message`). waddle's own chat client
    // is conformant — it sends `replyableId = stampedByRoom` (the room
    // stanza-id). The retraction target lookup MUST resolve that id.
    //
    // Regression for the channel-delete failure: the lookup keyed off the
    // `stanza_id` column (which stores the wire `id` attribute), so a
    // conformant retraction citing the room stanza-id returned
    // `<item-not-found/>` and channel deletes silently did nothing.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "retract-by-stanza-id@muc.example.com"
        .parse()
        .expect("room");
    let room_stanza_id = "room-stamp-uuid-CCC";
    let wire_id = "alice-orig-3";
    seed_groupchat_archive_row(&mam, &room, room_stanza_id, wire_id).await;

    let resolved = lookup_groupchat_retraction_target(&mam, &room, room_stanza_id)
        .await
        .expect("lookup must not error")
        .expect("retraction citing the room stanza-id must resolve the seeded archive row");
    assert_eq!(
        resolved.id, room_stanza_id,
        "lookup_groupchat_retraction_target resolves the row by its room XEP-0359 stanza-id (archive PK)"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_target_rejects_wire_id_and_origin_id() {
    // Strict XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // retraction resolves ONLY by the room-assigned XEP-0359 stanza-id
    // (the archive primary key). A citation of the original wire `id`
    // attribute (the `stanza_id` column) or the client origin-id MUST
    // NOT resolve — the server replies `<item-not-found/>`, matching the
    // XEP-0425 moderation target semantics.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "retract-strict@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-AAA";
    let wire_id = "alice-orig-1";
    let origin_id = "alice-origin-1";
    // One row carrying three distinct ids: PK (room stanza-id), the wire
    // `id` attribute (`stanza_id` column), and a client origin-id.
    let row = MamArchivedMessage {
        id: archive_pk.to_string(),
        timestamp: chrono::Utc::now(),
        from: format!("{room}/alice").parse().expect("room/nick jid"),
        to: jid::Jid::from(room.clone()),
        body: Some("remove me".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            wire_id,
            jid::Jid::from(room.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new(origin_id)),
        message_type: XmppMessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: Some(0),
    };
    mam.store_message(&room, &row).await.expect("seed mam row");

    // Conformant id (room stanza-id = archive PK) resolves.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, archive_pk)
            .await
            .expect("lookup must not error")
            .is_some(),
        "retraction citing the room stanza-id must resolve"
    );
    // Wire `id` attribute (the `stanza_id` column) must NOT resolve.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, wire_id)
            .await
            .expect("lookup must not error")
            .is_none(),
        "retraction citing the wire id must not resolve — XEP-0424 §3 uses the room stanza-id"
    );
    // Client origin-id must NOT resolve.
    assert!(
        lookup_groupchat_retraction_target(&mam, &room, origin_id)
            .await
            .expect("lookup must not error")
            .is_none(),
        "retraction citing the client origin-id must not resolve"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_lookup_is_room_scoped() {
    // Resolution is scoped to the room: the room stanza-id (archive PK)
    // of a message in room A must not satisfy a retraction addressed to
    // room B. `lookup_groupchat_retraction_target` resolves by PK via
    // `get_message` and rejects any row whose archived `to` is a
    // different room.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room_a: jid::BareJid = "room-a@muc.example.com".parse().expect("room a");
    let room_b: jid::BareJid = "room-b@muc.example.com".parse().expect("room b");
    let archive_pk = "pk-A";
    seed_groupchat_archive_row(&mam, &room_a, archive_pk, "alice-orig-1").await;

    // Room A's room stanza-id queried under room B — must not resolve.
    let cross = lookup_groupchat_retraction_target(&mam, &room_b, archive_pk)
        .await
        .expect("lookup must not error");
    assert!(
        cross.is_none(),
        "groupchat retraction lookup must not return a row from a different room's archive"
    );
}

#[tokio::test]
async fn xep_0424_apply_groupchat_retraction_tombstone_keys_off_room_stanza_id() {
    // The tombstone-application site must resolve the target by the
    // room-assigned XEP-0359 stanza-id (the archive primary key) — the
    // id a conformant XEP-0424 client cites. Drives the
    // `OutboundEvent::ApplyGroupchatRetractionTombstone` arm of
    // `interpret` and asserts the seeded row gets its body
    // scrubbed and a `<retracted/>` tombstone written in its
    // place.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let room: jid::BareJid = "retract-tombstone@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-BBB";
    let wire_id = "alice-orig-2";
    seed_groupchat_archive_row(&mam, &room, archive_pk, wire_id).await;

    // Build the retraction message the chain would emit.
    let mut retraction = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    retraction.id = Some(xmpp_parsers::message::Id("retract-stanza-1".to_string()));
    retraction.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(archive_pk));

    let events = vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
        room: room.clone(),
        target_message_id: archive_pk.to_string(),
        retraction_message: Box::new(retraction),
    }];
    let _outcome = interpret(events, &deps).await;

    // The seeded row's body must now be scrubbed and a
    // `<retracted/>` payload must replace it (XEP-0424 §"prevent
    // further distribution"). Read the row back by its room stanza-id
    // (archive primary key) — the same id the retraction cited.
    let row = mam
        .get_message(archive_pk)
        .await
        .expect("post-tombstone lookup")
        .expect("row still present after tombstone replace");
    assert!(row.body.is_none(), "tombstone must clear the original body");
    let rich = row
        .rich
        .as_ref()
        .expect("post-tombstone row must carry an ArchivedRichMessage");
    match rich.payload.as_ref() {
        Some(waddle_xmpp::mam::ArchivedRichPayload::Tombstone(ts)) => {
            assert_eq!(
                ts.retraction_id.as_ref().map(|id| id.as_str()),
                Some("retract-stanza-1"),
                "tombstone cites the retraction stanza id"
            );
        }
        other => {
            panic!("expected ArchivedRichPayload::Tombstone after retraction, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0424_retraction_retry_preserves_existing_moderation_tombstone() {
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::mam::{ArchivedModeration, ArchivedTombstone, RichMessageId, RichText};

    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let room: jid::BareJid = "moderated-retraction@muc.example.com"
        .parse()
        .expect("room");
    let archive_pk = "moderated-room-stamp";
    seed_groupchat_archive_row(&mam, &room, archive_pk, "original-wire-id").await;

    let moderation_tombstone = ArchivedTombstone {
        retraction_id: RichMessageId::new("moderation-archive-id"),
        stamp: chrono::Utc::now(),
        moderation: Some(ArchivedModeration {
            target_id: RichMessageId::new(archive_pk).expect("non-empty target id"),
            moderated_by: "room-owner@example.com".parse().expect("moderator JID"),
            stamp: Some(chrono::Utc::now()),
            reason: RichText::new("room policy violation"),
        }),
        sender_scope: None,
    };
    assert!(
        mam.replace_with_tombstone(archive_pk, moderation_tombstone.clone())
            .await
            .expect("moderation tombstone replacement"),
        "the real storage replacement path must tombstone the target"
    );

    let mut retry = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    retry.id = Some(xmpp_parsers::message::Id(
        "author-retraction-retry".to_string(),
    ));
    retry.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retry.type_ = XmppMessageType::Groupchat;

    assert!(
        apply_groupchat_retraction_tombstone(&mam, None, None, &room, archive_pk, &retry).await,
        "an already-applied retraction remains a successful heal-retry no-op"
    );

    let row = mam
        .get_message(archive_pk)
        .await
        .expect("post-retry lookup")
        .expect("moderation tombstone remains stored");
    let actual = row
        .rich
        .as_ref()
        .and_then(|rich| rich.payload.as_ref())
        .expect("tombstone payload remains present");
    assert_eq!(
        actual,
        &waddle_xmpp::mam::ArchivedRichPayload::Tombstone(moderation_tombstone),
        "XEP-0424 retry must not overwrite XEP-0425 attribution, reason, id, or stamp"
    );
}

#[tokio::test]
async fn xep_0424_groupchat_retraction_scrubs_pending_delivery_rows() {
    // F2: promotion (#1097/#1098) parks undelivered copies in
    // pending_delivery. The retraction arm must scrub that layer with
    // the same keys as the SM-queue scrub, or the retracted content
    // (Transient rows) / a tombstone stub (Archived pointers) delivers
    // at the recipient's next login.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp::pending_delivery::storage::{
        InMemoryPendingDeliveryStorage, PendingDeliveryStorage,
    };
    use waddle_xmpp::pending_delivery::{PendingPayload, PendingRow, PendingRowId};

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let pending: Arc<dyn PendingDeliveryStorage> =
        Arc::new(InMemoryPendingDeliveryStorage::unlimited());
    let mut deps = Deps::test_with_storage(&registry, &mam, &inbox);
    deps.pending_delivery_storage = Some(&pending);

    let room: jid::BareJid = "retract-pending@muc.example.com".parse().expect("room");
    let archive_pk = "room-stamp-uuid-PPP";
    let wire_id = "alice-orig-3";
    seed_groupchat_archive_row(&mam, &room, archive_pk, wire_id).await;

    // A promoted Archived pointer row for an offline member, keyed by
    // the room's XEP-0359 stamp (archive id == wire stanza-id
    // invariant), plus an unrelated row that must survive.
    let recipient: jid::BareJid = "offline@example.com".parse().expect("jid");
    pending
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                archive_pk,
                jid::Jid::from(room.clone()),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert");
    pending
        .insert(PendingRow {
            id: PendingRowId::fresh(),
            recipient: recipient.clone(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Archived(waddle_xmpp_core::xep0359::StanzaId::new(
                "unrelated-stamp",
                jid::Jid::from(room.clone()),
            )),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("insert");

    let mut retraction = xmpp_parsers::message::Message::new(Some(jid::Jid::from(room.clone())));
    retraction.id = Some(xmpp_parsers::message::Id("retract-stanza-2".to_string()));
    retraction.from = Some(format!("{room}/alice").parse().expect("room/nick"));
    retraction.type_ = XmppMessageType::Groupchat;
    retraction
        .payloads
        .push(waddle_xmpp::xep::xep0424::build_retract_element(archive_pk));

    let events = vec![OutboundEvent::ApplyGroupchatRetractionTombstone {
        room: room.clone(),
        target_message_id: archive_pk.to_string(),
        retraction_message: Box::new(retraction),
    }];
    let _outcome = interpret(events, &deps).await;

    let rows = pending.list(&recipient).await.expect("list");
    assert_eq!(
        rows.len(),
        1,
        "retraction must scrub the matching pending row; unrelated rows survive"
    );
    match &rows[0].payload {
        PendingPayload::Archived(r) => assert_eq!(r.id.as_str(), "unrelated-stamp"),
        other => panic!("expected surviving Archived row, got {other:?}"),
    }
}
