use super::*;

// XEP-0313 — ArchiveDirect persistence
// -----------------------------------------------------------------

#[tokio::test]
async fn xep_0313_archive_direct_persists_to_mam_storage() {
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let from: jid::BareJid = "alice@example.com".parse().expect("bare");
    let to: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hello");
    msg.id = Some(xmpp_parsers::message::Id("orig-1".to_string()));

    let events = vec![OutboundEvent::ArchiveDirect {
        archive_jid: archive_jid.clone(),
        from: from.into(),
        to: to.into(),
        message: Box::new(msg),
    }];
    let _outcome = interpret(events, &deps).await;

    let stored = mam
        .query_messages(
            &archive_jid,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query");
    assert_eq!(
        stored.messages.len(),
        1,
        "ArchiveDirect persists exactly one row"
    );
    let row = &stored.messages[0];
    assert_eq!(row.from.to_string(), "alice@example.com");
    assert_eq!(row.to.to_string(), "bob@example.com");
    assert_eq!(row.body.as_deref(), Some("hello"));
    assert_eq!(
        row.stanza_id.as_ref().map(|s| s.id.as_str()),
        Some("orig-1")
    );
}

#[tokio::test]
async fn direct_correction_preview_ref_target_resolves_to_wire_message_id() {
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let sender: jid::BareJid = "alice@example.com".parse().expect("bare");
    mam.store_message(
        &archive_jid,
        &ArchivedMessage {
            id: "canonical-archive-id".to_string(),
            body: Some("message with preview".to_string()),
            stanza_id: Some(StanzaId::new(
                "wire-msg-id",
                jid::Jid::from(archive_jid.clone()),
            )),
            message_type: xmpp_parsers::message::MessageType::Chat,
            ..ArchivedMessage::for_test(
                "alice@example.com/web".parse().expect("jid"),
                "bob@example.com".parse().expect("jid"),
            )
        },
    )
    .await
    .expect("seed direct archive row");

    let target = super::direct_archive::resolve_direct_correction_target_message_id(
        &deps,
        &archive_jid,
        &sender,
        "wire-msg-id",
    )
    .await;

    assert_eq!(
        target.as_deref(),
        Some("wire-msg-id"),
        "link preview refs are keyed by original direct message wire ids, not canonical MAM ids"
    );

    mam.store_message(
        &archive_jid,
        &ArchivedMessage {
            id: "origin-canonical-archive-id".to_string(),
            body: Some("message with origin id".to_string()),
            origin_id: Some(OriginId::new("client-origin-id")),
            stanza_id: Some(StanzaId::new(
                "origin-wire-msg-id",
                jid::Jid::from(archive_jid.clone()),
            )),
            message_type: xmpp_parsers::message::MessageType::Chat,
            ..ArchivedMessage::for_test(
                "alice@example.com/phone".parse().expect("jid"),
                "bob@example.com".parse().expect("jid"),
            )
        },
    )
    .await
    .expect("seed origin-id direct archive row");

    let target = super::direct_archive::resolve_direct_correction_target_message_id(
        &deps,
        &archive_jid,
        &sender,
        "client-origin-id",
    )
    .await;

    assert_eq!(
        target.as_deref(),
        Some("origin-wire-msg-id"),
        "origin-id corrections must still clear refs by the original direct wire message id"
    );
}

#[tokio::test]
async fn xep_0359_archive_ref_pivots_inbox_row_to_mam_row_via_archive_or_stanza_id() {
    // End-to-end of the bug Qodo + Codex flagged: inbox writes
    // `archive_ref` from the canonical XEP-0359 `<stanza-id>`
    // stamp, and `MamStorage::get_message_by_archive_or_stanza_id`
    // must resolve that same id against `archive_jid` by querying
    // both the archive's primary key (`id`) and the wire id
    // (`stanza_id`). If the projection ever stops using the
    // canonical stamp as `ArchivedMessage.id`, the inbox row
    // points at a dangling stanza-id and clients can't pivot to
    // the archive.
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::{build_stanza_id_element, StanzaId};
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "pivot test");
    msg.id = Some(xmpp_parsers::message::Id("wire-id".to_string()));
    // Simulate CanonicalizeHandler stamping the canonical id
    // under alice's archive — the same id InboxHandler will
    // emit as `archive_ref`.
    let canonical_id = "alice-canonical-1";
    msg.payloads.push(build_stanza_id_element(
        canonical_id,
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob.clone(),
            message: Box::new(msg),
            archive_ref: StanzaId::new(canonical_id, jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
    ];
    let _outcome = interpret(events, &deps).await;

    // Inbox row carries the canonical stamp.
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, canonical_id);

    // The same id resolves a MAM row in alice's archive — pivot
    // works. The XEP-0359 canonical stamp is stored as the row's
    // `id` (primary key) per the legacy projection shape, so the
    // pivot uses `get_message_by_archive_or_stanza_id` (queries
    // both `id` and `stanza_id`).
    let row = mam
        .get_message_by_archive_or_stanza_id(&alice, canonical_id)
        .await
        .expect("mam lookup")
        .expect("MAM row keyed by canonical stanza-id");
    assert_eq!(row.id, canonical_id);
    assert_eq!(row.body.as_deref(), Some("pivot test"));
}

#[tokio::test]
async fn origin_id_dedup_rewrites_downstream_archive_refs_to_existing_mam_id() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{
        build_origin_id_element, build_stanza_id_element, OriginId, StanzaId,
    };

    let registry = ConnectionRegistry::new();
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let origin_id = OriginId::new("retry-origin-1");

    mam_concrete
        .store_message(
            &alice,
            &ArchivedMessage {
                id: "existing-archive-id".to_string(),
                body: Some("original copy".to_string()),
                origin_id: Some(origin_id.clone()),
                message_type: xmpp_parsers::message::MessageType::Chat,
                ..ArchivedMessage::for_test(
                    "alice@example.com/web-old".parse().expect("jid"),
                    "bob@example.com".parse().expect("jid"),
                )
            },
        )
        .await
        .expect("seed original archive row");

    let mut retry = chat_msg(
        "alice@example.com/web-new",
        "bob@example.com",
        "original copy",
    );
    retry
        .payloads
        .push(build_origin_id_element(origin_id.as_str()));
    retry.payloads.push(build_stanza_id_element(
        "fresh-retry-archive-id",
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(retry.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob,
            message: Box::new(retry.clone()),
            archive_ref: StanzaId::new("fresh-retry-archive-id", jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Message(retry))),
    ];
    let outcome = interpret(events, &deps).await;

    assert_eq!(
        mam_concrete.count_messages(&alice).await.expect("count"),
        1,
        "origin-id retry must not insert a second MAM row"
    );
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].last_stanza_id, "existing-archive-id",
        "downstream inbox projection must point at the retained MAM row"
    );
    assert_eq!(outcome.frames.len(), 1);
    assert!(
        outcome.frames[0].contains("existing-archive-id"),
        "wire stanza-id must name the retained MAM row"
    );
    assert!(
        !outcome.frames[0].contains("fresh-retry-archive-id"),
        "wire stanza-id must not point at the skipped duplicate row"
    );
}

#[tokio::test]
async fn origin_id_collision_with_distinct_content_keeps_fresh_archive_refs() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp_core::xep0359::{
        build_origin_id_element, build_stanza_id_element, OriginId, StanzaId,
    };

    let registry = ConnectionRegistry::new();
    let mam_concrete = Arc::new(InMemoryMamStorage::new());
    let mam: Arc<dyn MamStorage> = mam_concrete.clone();
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let origin_id = OriginId::new("retry-origin-collision");

    mam_concrete
        .store_message(
            &alice,
            &ArchivedMessage {
                id: "existing-archive-id".to_string(),
                body: Some("original copy".to_string()),
                origin_id: Some(origin_id.clone()),
                message_type: xmpp_parsers::message::MessageType::Chat,
                ..ArchivedMessage::for_test(
                    "alice@example.com/web-old".parse().expect("jid"),
                    "bob@example.com".parse().expect("jid"),
                )
            },
        )
        .await
        .expect("seed original archive row");

    let mut distinct = chat_msg(
        "alice@example.com/web-new",
        "bob@example.com",
        "new content",
    );
    distinct
        .payloads
        .push(build_origin_id_element(origin_id.as_str()));
    distinct.payloads.push(build_stanza_id_element(
        "fresh-distinct-archive-id",
        &jid::Jid::from(alice.clone()),
    ));

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(distinct.clone()),
        },
        OutboundEvent::ProjectInbox {
            owner: alice.clone(),
            peer: bob,
            message: Box::new(distinct.clone()),
            archive_ref: StanzaId::new("fresh-distinct-archive-id", jid::Jid::from(alice.clone())),
            increment_unread: false,
        },
        OutboundEvent::SendStanza(Box::new(Stanza::Message(distinct))),
    ];
    let outcome = interpret(events, &deps).await;

    assert_eq!(
        mam_concrete.count_messages(&alice).await.expect("count"),
        2,
        "origin-id reuse with distinct content must archive a new MAM row"
    );
    let entries = inbox_concrete.list(&alice).await.expect("inbox list");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].last_stanza_id, "fresh-distinct-archive-id");
    assert_eq!(outcome.frames.len(), 1);
    assert!(outcome.frames[0].contains("fresh-distinct-archive-id"));
    assert!(!outcome.frames[0].contains("existing-archive-id"));
}

#[tokio::test]
async fn xep_0313_archive_direct_writes_one_entry_per_event() {
    // Sender pass + recipient pass on the same dispatch (true
    // local-to-local) emit two events with distinct archive_jids
    // — the interpreter writes one entry per archive.
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");

    let events = vec![
        OutboundEvent::ArchiveDirect {
            archive_jid: alice.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg.clone()),
        },
        OutboundEvent::ArchiveDirect {
            archive_jid: bob.clone(),
            from: alice.clone().into(),
            to: bob.clone().into(),
            message: Box::new(msg),
        },
    ];
    let _outcome = interpret(events, &deps).await;

    let alice_archive = mam
        .query_messages(
            &alice,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query alice");
    let bob_archive = mam
        .query_messages(
            &bob,
            waddle_xmpp::mam::MamArchiveKind::Personal,
            &Default::default(),
        )
        .await
        .expect("query bob");
    assert_eq!(
        alice_archive.messages.len(),
        1,
        "alice archive has the sender-pass entry"
    );
    assert_eq!(
        bob_archive.messages.len(),
        1,
        "bob archive has the recipient-pass entry"
    );
}

#[tokio::test]
async fn xep_0313_archive_direct_drops_when_storage_errors() {
    // Storage errors must NOT fail dispatch. We use a fake that
    // always errors and assert interpret returns normally; the
    // archive write is logged-and-dropped.
    use async_trait::async_trait;
    use waddle_xmpp::mam::storage::{MamStorage, MamStorageError, StoreOutcome};
    use waddle_xmpp::mam::{ArchivedMessage, MamQuery, MamResult};

    struct FailingMam;
    #[async_trait]
    impl MamStorage for FailingMam {
        async fn store_message(
            &self,
            _: &jid::BareJid,
            _: &ArchivedMessage,
        ) -> Result<StoreOutcome, MamStorageError> {
            Err(MamStorageError::Database("simulated".into()))
        }
        async fn query_messages(
            &self,
            _: &jid::BareJid,
            _: waddle_xmpp::mam::MamArchiveKind,
            _: &MamQuery,
        ) -> Result<MamResult, MamStorageError> {
            Ok(MamResult {
                messages: Vec::new(),
                complete: true,
                first_id: None,
                last_id: None,
                count: Some(0),
            })
        }
        async fn get_message(&self, _: &str) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn replace_with_tombstone(
            &self,
            _: &str,
            _: waddle_xmpp::mam::ArchivedTombstone,
        ) -> Result<bool, MamStorageError> {
            Ok(false)
        }
        async fn replace_with_terminal_tombstone(
            &self,
            _: &str,
            _: waddle_xmpp::mam::ArchivedTombstone,
        ) -> Result<waddle_xmpp::mam::TerminalTombstoneOutcome, MamStorageError> {
            Ok(waddle_xmpp::mam::TerminalTombstoneOutcome::NotFound)
        }
        async fn get_message_by_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_message_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_sender_and_origin_id(
            &self,
            _: &jid::BareJid,
            _: waddle_xmpp::mam::MamArchiveKind,
            _: &jid::Jid,
            _: &waddle_xmpp_core::xep0359::OriginId,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn get_message_by_archive_or_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }
        async fn count_messages(&self, _: &jid::BareJid) -> Result<u32, MamStorageError> {
            Ok(0)
        }
        async fn delete_before(
            &self,
            _: &jid::BareJid,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, MamStorageError> {
            Ok(0)
        }
    }

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(FailingMam);
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let alice: jid::BareJid = "alice@example.com".parse().expect("bare");
    let bob: jid::BareJid = "bob@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "yo");
    let events = vec![OutboundEvent::ArchiveDirect {
        archive_jid: alice.clone(),
        from: alice.into(),
        to: bob.into(),
        message: Box::new(msg),
    }];
    let outcome = interpret(events, &deps).await;
    // No frames, no close — error swallowed.
    assert!(outcome.frames.is_empty());
    assert!(!outcome.close);
}

// -----------------------------------------------------------------
// Inbox projection
// -----------------------------------------------------------------

#[tokio::test]
async fn inbox_project_writes_owner_peer_keyed_row_with_typed_archive_ref() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let owner: jid::BareJid = "alice@example.com".parse().expect("bare");
    let peer: jid::BareJid = "bob@example.com".parse().expect("bare");
    let mut msg = chat_msg("alice@example.com/web", "bob@example.com", "hi there");
    msg.id = Some(xmpp_parsers::message::Id("origin-X".to_string()));

    let events = vec![OutboundEvent::ProjectInbox {
        owner: owner.clone(),
        peer: peer.clone(),
        message: Box::new(msg),
        archive_ref: StanzaId::new("alice-archive-1", jid::Jid::from(owner.clone())),
        increment_unread: false,
    }];
    let _outcome = interpret(events, &deps).await;

    let entries = inbox_concrete.list(&owner).await.expect("list");
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.partner, peer);
    assert_eq!(
        entry.last_stanza_id, "alice-archive-1",
        "last_stanza_id is sourced from the typed archive_ref, not the wire id"
    );
    assert_eq!(entry.unread, 0, "increment_unread=false leaves unread at 0");
}

#[tokio::test]
async fn inbox_project_increment_unread_bumps_recipient_count() {
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::mam::storage::InMemoryMamStorage;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox_concrete = Arc::new(InMemoryInboxStorage::new());
    let inbox: Arc<dyn InboxStorage> = inbox_concrete.clone();
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let owner: jid::BareJid = "bob@example.com".parse().expect("bare");
    let peer: jid::BareJid = "alice@example.com".parse().expect("bare");
    let msg = chat_msg("alice@example.com/web", "bob@example.com", "hi bob");

    let events = vec![OutboundEvent::ProjectInbox {
        owner: owner.clone(),
        peer: peer.clone(),
        message: Box::new(msg),
        archive_ref: StanzaId::new("bob-archive-1", jid::Jid::from(owner.clone())),
        increment_unread: true,
    }];
    let _outcome = interpret(events, &deps).await;

    let total = inbox_concrete.total_unread(&owner).await.expect("unread");
    assert_eq!(
        total, 1,
        "increment_unread=true bumps the owner's unread count"
    );
}

// -----------------------------------------------------------------
// XEP-0308/0424/0461 — LookupArchivedMessage callback round-trip
// -----------------------------------------------------------------

#[tokio::test]
async fn xep_0424_lookup_archived_message_by_stanza_id_feeds_archived_loaded_back() {
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    // Seed the archive with a row keyed under alice's bare,
    // canonical stamp = "canon-A1".
    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "canon-A1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("hello".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "canon-A1",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Chat,
        stanza_xml: Some(
            r#"<message xmlns='jabber:client' type='chat' from='alice@example.com/web' to='bob@example.com'><body>hello</body></message>"#.to_string(),
        ),
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(7),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("canon-A1", jid::Jid::from(archive_jid.clone())),
        },
    }];
    let outcome = interpret(events, &deps).await;

    assert_eq!(outcome.feedback.len(), 1);
    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded { id, result } => {
            assert_eq!(id, CallbackId(7));
            let archived = result.expect("row resolved");
            assert_eq!(archived.stanza_id.as_str(), "canon-A1");
            assert_eq!(archived.stanza_id.by, jid::Jid::from(archive_jid.clone()));
            assert!(!archived.tombstoned);
            assert_eq!(
                archived.message.bodies.get("").cloned(),
                Some("hello".to_string()),
                "stanza_xml is parsed back into a typed Message"
            );
        }
        other => panic!("expected ArchivedMessageLoaded, got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0424_lookup_archived_message_not_found_feeds_none_back() {
    use waddle_xmpp::mam::InMemoryMamStorage;
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(11),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("never-stamped", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(11),
            result: None,
        } => {}
        other => panic!("expected ArchivedMessageLoaded(None), got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_by_origin_id_feeds_archived_loaded_back() {
    // OriginId lookup MUST be sender-scoped per the typed
    // `MessageRef::OriginId { sender, origin_id }` contract.
    // Seed two rows in alice's archive that share the same
    // `origin_id` value but come from different senders:
    // post-filter on `sender` must pick the alice-authored row,
    // not the bob-authored one.
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let alice_bare: jid::BareJid = "alice@example.com".parse().expect("bare");

    // Bob-authored row in alice's archive (cross-resource self
    // chat / received DM) sharing the colliding origin-id.
    let bob_row = ArchivedMessage {
        id: "row-from-bob".to_string(),
        timestamp: chrono::Utc::now(),
        from: "bob@example.com".parse().expect("jid"),
        to: "alice@example.com".parse().expect("jid"),
        body: Some("from bob".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp-bob",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    // Alice-authored row in alice's archive (sender-side) with
    // the same origin-id.
    let alice_row = ArchivedMessage {
        id: "row-from-alice".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("from alice".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp-alice",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("collision")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    // Insert bob's row FIRST so a naive OR-matcher would return it.
    mam.store_message(&archive_jid, &bob_row)
        .await
        .expect("seed bob");
    mam.store_message(&archive_jid, &alice_row)
        .await
        .expect("seed alice");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(21),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::OriginId {
            sender: alice_bare.clone().into(),
            origin_id: OriginId::new("collision"),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(21),
            result: Some(archived),
        } => {
            let body = archived.message.bodies.get("").cloned().unwrap_or_default();
            assert_eq!(
                body, "from alice",
                "OriginId lookup must scope to sender; bob's row was a collision decoy"
            );
        }
        other => panic!("expected alice-authored row, got {other:?}"),
    }
}

async fn assert_origin_lookup_finds_targets_beyond_default_page(mam: Arc<dyn MamStorage>) {
    use waddle_xmpp::mam::{ArchivedMessage, MamArchiveKind};
    use waddle_xmpp_core::xep0359::OriginId;

    struct SeedRow<'a> {
        id: String,
        offset: i64,
        from: &'a str,
        to: &'a str,
        origin_id: String,
        body: String,
    }

    async fn store_row(mam: &dyn MamStorage, archive: &jid::BareJid, seed: SeedRow<'_>) {
        let row = ArchivedMessage {
            id: seed.id,
            timestamp: chrono::DateTime::from_timestamp(1_700_000_000 + seed.offset, 0)
                .expect("fixture timestamp"),
            from: seed.from.parse().expect("from JID"),
            to: seed.to.parse().expect("to JID"),
            body: Some(seed.body),
            origin_id: Some(OriginId::new(seed.origin_id)),
            ..ArchivedMessage::for_test(
                seed.from.parse().expect("from JID"),
                seed.to.parse().expect("to JID"),
            )
        };
        mam.store_message(archive, &row)
            .await
            .expect("seed MAM row");
    }

    let registry = ConnectionRegistry::new();
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    // Place a cross-sender collision and enough ordinary rows ahead of the
    // target to prove lookup is sender-indexed rather than bounded by the
    // default MAM page.
    let owner_archive: jid::BareJid = "owner@example.com".parse().expect("owner archive");
    store_row(
        mam.as_ref(),
        &owner_archive,
        SeedRow {
            id: "owner-000".to_string(),
            offset: 0,
            from: "mallory@example.com",
            to: "owner@example.com",
            origin_id: "owner-target".to_string(),
            body: "collision decoy".to_string(),
        },
    )
    .await;
    for index in 1..=100 {
        store_row(
            mam.as_ref(),
            &owner_archive,
            SeedRow {
                id: format!("owner-{index:03}"),
                offset: index,
                from: "owner@example.com",
                to: "peer@example.com",
                origin_id: format!("owner-decoy-{index}"),
                body: format!("owner decoy {index}"),
            },
        )
        .await;
    }
    store_row(
        mam.as_ref(),
        &owner_archive,
        SeedRow {
            id: "owner-101".to_string(),
            offset: 101,
            from: "owner@example.com",
            to: "peer@example.com",
            origin_id: "owner-target".to_string(),
            body: "owner page two target".to_string(),
        },
    )
    .await;

    let owner_result = super::archive_lookup::lookup_archived_message(
        &deps,
        &owner_archive,
        MamArchiveKind::Personal,
        &MessageRef::OriginId {
            sender: owner_archive.clone().into(),
            origin_id: OriginId::new("owner-target"),
        },
    )
    .await
    .expect("owner target beyond default page boundary");
    assert_eq!(
        owner_result.message.bodies.get("").map(String::as_str),
        Some("owner page two target")
    );

    // Recipient-side decoys with the same origin id must not hide a later row
    // actually authored by the requested sender.
    let peer_archive: jid::BareJid = "archive@example.com".parse().expect("peer archive");
    for index in 0..=100 {
        store_row(
            mam.as_ref(),
            &peer_archive,
            SeedRow {
                id: format!("peer-{index:03}"),
                offset: index,
                from: "archive@example.com",
                to: "sender@example.com",
                origin_id: if index == 0 {
                    "peer-target".to_string()
                } else {
                    format!("peer-decoy-{index}")
                },
                body: format!("peer decoy {index}"),
            },
        )
        .await;
    }
    store_row(
        mam.as_ref(),
        &peer_archive,
        SeedRow {
            id: "peer-101".to_string(),
            offset: 101,
            from: "sender@example.com/mobile",
            to: "archive@example.com",
            origin_id: "peer-target".to_string(),
            body: "sender page two target".to_string(),
        },
    )
    .await;
    let sender: jid::BareJid = "sender@example.com".parse().expect("sender");
    let peer_result = super::archive_lookup::lookup_archived_message(
        &deps,
        &peer_archive,
        MamArchiveKind::Personal,
        &MessageRef::OriginId {
            sender: sender.into(),
            origin_id: OriginId::new("peer-target"),
        },
    )
    .await
    .expect("sender target beyond default page boundary");
    assert_eq!(
        peer_result.message.bodies.get("").map(String::as_str),
        Some("sender page two target")
    );
}

#[tokio::test]
async fn origin_lookup_finds_targets_beyond_default_page_in_memory() {
    assert_origin_lookup_finds_targets_beyond_default_page(Arc::new(
        waddle_xmpp::mam::InMemoryMamStorage::new(),
    ))
    .await;
}

#[tokio::test]
async fn origin_lookup_finds_targets_beyond_default_page_in_sqlite() {
    let storage = waddle_xmpp::mam::SqlxMamStorage::open_in_memory()
        .await
        .expect("SQLite MAM storage");
    assert_origin_lookup_finds_targets_beyond_default_page(Arc::new(storage)).await;
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_propagates_room_archive_kind() {
    use async_trait::async_trait;
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedTombstone, MamArchiveKind, MamQuery, MamResult, MamStorageError,
        StoreOutcome,
    };
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    struct KindProbeMam {
        row: ArchivedMessage,
    }

    #[async_trait]
    impl MamStorage for KindProbeMam {
        async fn store_message(
            &self,
            _: &jid::BareJid,
            _: &ArchivedMessage,
        ) -> Result<StoreOutcome, MamStorageError> {
            unreachable!("lookup regression never stores")
        }

        async fn query_messages(
            &self,
            _: &jid::BareJid,
            archive_kind: MamArchiveKind,
            _: &MamQuery,
        ) -> Result<MamResult, MamStorageError> {
            assert_eq!(
                archive_kind,
                MamArchiveKind::Room,
                "event archive kind must reach MamStorage"
            );
            Ok(MamResult {
                messages: vec![self.row.clone()],
                complete: true,
                first_id: Some(self.row.id.clone()),
                last_id: Some(self.row.id.clone()),
                count: Some(1),
            })
        }

        async fn get_message(&self, _: &str) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn replace_with_tombstone(
            &self,
            _: &str,
            _: ArchivedTombstone,
        ) -> Result<bool, MamStorageError> {
            Ok(false)
        }

        async fn replace_with_terminal_tombstone(
            &self,
            _: &str,
            _: ArchivedTombstone,
        ) -> Result<waddle_xmpp::mam::TerminalTombstoneOutcome, MamStorageError> {
            Ok(waddle_xmpp::mam::TerminalTombstoneOutcome::NotFound)
        }

        async fn get_message_by_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn get_message_by_message_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn get_message_by_sender_and_origin_id(
            &self,
            _: &jid::BareJid,
            archive_kind: MamArchiveKind,
            _: &jid::Jid,
            _: &OriginId,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            assert_eq!(
                archive_kind,
                MamArchiveKind::Room,
                "event archive kind must reach MamStorage"
            );
            Ok(Some(self.row.clone()))
        }

        async fn get_message_by_archive_or_stanza_id(
            &self,
            _: &jid::BareJid,
            _: &str,
        ) -> Result<Option<ArchivedMessage>, MamStorageError> {
            Ok(None)
        }

        async fn count_messages(&self, _: &jid::BareJid) -> Result<u32, MamStorageError> {
            Ok(0)
        }

        async fn delete_before(
            &self,
            _: &jid::BareJid,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<u64, MamStorageError> {
            Ok(0)
        }
    }

    let registry = ConnectionRegistry::new();
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());

    let room: jid::BareJid = "room@conference.example".parse().expect("room bare");
    let row = ArchivedMessage {
        id: "room-row-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "room@conference.example/alice"
            .parse()
            .expect("occupant jid"),
        to: "bob@example.com".parse().expect("recipient jid"),
        body: Some("room message".to_string()),
        stanza_id: None,
        thread: None,
        reply: None,
        origin_id: Some(OriginId::new("room-origin-1")),
        message_type: XmppMessageType::Groupchat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    let mam: Arc<dyn MamStorage> = Arc::new(KindProbeMam { row });
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let outcome = interpret(
        vec![OutboundEvent::LookupArchivedMessage {
            id: CallbackId(22),
            archive: room.clone(),
            archive_kind: MamArchiveKind::Room,
            reference: MessageRef::OriginId {
                sender: "room@conference.example/alice"
                    .parse()
                    .expect("occupant JID"),
                origin_id: OriginId::new("room-origin-1"),
            },
        }],
        &deps,
    )
    .await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(22),
            result: Some(archived),
        } => assert_eq!(
            archived.message.bodies.get("").map(String::as_str),
            Some("room message"),
        ),
        other => panic!("room archive kind must reach storage lookup, got {other:?}"),
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_by_origin_id_rejects_cross_sender_collision() {
    // Same archive, same origin_id, different sender than
    // requested -> result MUST be None (handler will treat as
    // <item-not-found>).
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::OriginId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "row-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "bob@example.com".parse().expect("jid"),
        to: "alice@example.com".parse().expect("jid"),
        body: Some("bob's".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "alice-stamp",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("oid-1")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    // Look up for a DIFFERENT sender (charlie) with the colliding
    // origin-id. Must surface as not-found.
    let charlie_bare: jid::BareJid = "charlie@example.com".parse().expect("bare");
    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(31),
        archive: archive_jid,
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::OriginId {
            sender: charlie_bare.into(),
            origin_id: OriginId::new("oid-1"),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(31),
            result: None,
        } => {}
        other => {
            panic!("OriginId lookup must reject cross-sender collisions, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0359_lookup_archived_message_strict_stanza_id_ignores_origin_id_collision() {
    // StanzaId path uses `get_message_by_message_id` (stanza_id
    // ONLY), so a row whose `origin_id` happens to equal the
    // requested stanza-id MUST NOT be returned.
    use waddle_xmpp::mam::{ArchivedMessage, InMemoryMamStorage};
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    // Row whose origin_id equals the value the caller is looking
    // up via StanzaId. Stanza_id is something else.
    let collision_row = ArchivedMessage {
        id: "row-collide".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: Some("collide".to_string()),
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "real-stamp",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: Some(waddle_xmpp_core::xep0359::OriginId::new("queried-id")),
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: None,
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &collision_row)
        .await
        .expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(41),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("queried-id", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            id: CallbackId(41),
            result: None,
        } => {}
        other => {
            panic!("strict stanza-id lookup must ignore origin-id collisions, got {other:?}")
        }
    }
}

#[tokio::test]
async fn xep_0424_lookup_archived_message_propagates_tombstone_state() {
    use waddle_xmpp::mam::{
        ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone,
        InMemoryMamStorage, RichMessageId,
    };
    use waddle_xmpp::protocol::event::CallbackId;
    use waddle_xmpp_core::xep0359::StanzaId;

    let registry = ConnectionRegistry::new();
    let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
    let inbox: Arc<dyn InboxStorage> =
        Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
    let deps = Deps::test_with_storage(&registry, &mam, &inbox);

    let archive_jid: jid::BareJid = "alice@example.com".parse().expect("bare");
    let row = ArchivedMessage {
        id: "tomb-1".to_string(),
        timestamp: chrono::Utc::now(),
        from: "alice@example.com".parse().expect("jid"),
        to: "bob@example.com".parse().expect("jid"),
        body: None,
        stanza_id: Some(waddle_xmpp_core::xep0359::StanzaId::new(
            "tomb-1",
            jid::Jid::from(archive_jid.clone()),
        )),
        thread: None,
        reply: None,
        origin_id: None,
        message_type: XmppMessageType::Chat,
        stanza_xml: None,
        rich: Some(ArchivedRichMessage {
            payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
                retraction_id: Some(RichMessageId::new("retract-1").expect("rich id")),
                stamp: chrono::Utc::now(),
                moderation: None,
                sender_scope: None,
            })),
            reply: None,
            references: Vec::new(),
            mentions: Vec::new(),
            subjects: Default::default(),
            occupant_id: None,
            muc_sender: None,
        }),
        nickname_generation: None,
    };
    mam.store_message(&archive_jid, &row).await.expect("seed");

    let events = vec![OutboundEvent::LookupArchivedMessage {
        id: CallbackId(13),
        archive: archive_jid.clone(),
        archive_kind: waddle_xmpp::mam::MamArchiveKind::Personal,
        reference: MessageRef::StanzaId {
            stanza_id: StanzaId::new("tomb-1", jid::Jid::from(archive_jid)),
        },
    }];
    let outcome = interpret(events, &deps).await;

    match outcome.feedback.into_iter().next().expect("feedback") {
        InboundEvent::ArchivedMessageLoaded {
            result: Some(archived),
            ..
        } => {
            assert!(
                archived.tombstoned,
                "ArchivedRichPayload::Tombstone surfaces as `tombstoned: true`"
            );
        }
        other => panic!("expected Some archived row, got {other:?}"),
    }
}

// -----------------------------------------------------------------
