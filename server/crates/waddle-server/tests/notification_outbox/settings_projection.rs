//! Tests for the XEP-0402/XEP-0492 notification settings projection:
//! bookmark-derived mutations, publish validation, effective settings,
//! and store round-trips.
//!
//! Extracted from the former inline `mod tests` in
//! `src/notification_settings_projection.rs`.

use jid::BareJid;
use minidom::Element;
use waddle_server::notification_settings_projection::*;
use waddle_xmpp::xep::NotificationLevel;

fn bare(value: &str) -> BareJid {
    value.parse().expect("valid bare JID")
}

async fn migrated_in_memory_store() -> NotificationSettingsProjectionStore {
    let storage = waddle_server::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
        .await
        .expect("pubsub storage");
    NotificationSettingsProjectionStore::new(storage.database())
}

#[tokio::test]
async fn effective_setting_uses_xep0492_default_by_conversation_kind() {
    let store = migrated_in_memory_store().await;
    let owner = bare("alice@example.com");
    let direct = bare("bob@example.com");
    let public = bare("town@muc.example.com");

    assert_eq!(
        store
            .effective_setting(&owner, &direct, ConversationKind::Direct)
            .await
            .expect("direct default"),
        NotificationLevel::Always
    );
    assert_eq!(
        store
            .effective_setting(&owner, &public, ConversationKind::PrivateGroup)
            .await
            .expect("private group default"),
        NotificationLevel::Always
    );
    assert_eq!(
        store
            .effective_setting(&owner, &public, ConversationKind::PublicGroup)
            .await
            .expect("public default"),
        NotificationLevel::OnMention
    );
}

#[tokio::test]
async fn projection_store_persists_file_backing() {
    let artifacts =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
    std::fs::create_dir_all(&artifacts).expect("artifacts dir");
    let path = artifacts.join(format!(
        "notification-settings-projection-{}.db",
        uuid::Uuid::new_v4()
    ));
    let url = format!("sqlite://{}", path.display());

    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");

    {
        let storage = waddle_server::pubsub::DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("pubsub storage");
        let store = NotificationSettingsProjectionStore::new(storage.database());
        store
            .upsert(&NotificationSettingsProjection {
                owner_bare_jid: owner.clone(),
                conversation_jid: conversation.clone(),
                conversation_kind: ConversationKind::PrivateGroup,
                mode: NotificationLevel::Never,
                rich_payload_opt_in: true,
                source_version: 7,
                updated_at_ms: 42,
                source: NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: conversation.clone(),
            })
            .await
            .expect("upsert");
    }

    {
        let storage = waddle_server::pubsub::DatabasePubSubStorage::open(Some(&url))
            .await
            .expect("reopen pubsub storage");
        let store = NotificationSettingsProjectionStore::new(storage.database());
        let loaded = store
            .get(&owner, &conversation)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(loaded.mode, NotificationLevel::Never);
        assert!(loaded.rich_payload_opt_in);
        assert_eq!(loaded.conversation_kind, ConversationKind::PrivateGroup);
        assert_eq!(loaded.source_version, 7);
        assert_eq!(loaded.updated_at_ms, 42);
    }

    for cleanup in [
        path.clone(),
        std::path::PathBuf::from(format!("{}-shm", path.display())),
        std::path::PathBuf::from(format!("{}-wal", path.display())),
    ] {
        let _ = std::fs::remove_file(cleanup);
    }
}

#[tokio::test]
async fn delete_all_for_source_removes_only_owner_bookmark_projection_rows() {
    let store = migrated_in_memory_store().await;
    let alice = bare("alice@example.com");
    let bob = bare("bob@example.com");
    let room_one = bare("one@muc.example.com");
    let room_two = bare("two@muc.example.com");

    for (owner, conversation) in [
        (alice.clone(), room_one.clone()),
        (alice.clone(), room_two.clone()),
        (bob.clone(), room_one.clone()),
    ] {
        store
            .upsert(&NotificationSettingsProjection {
                owner_bare_jid: owner,
                conversation_jid: conversation.clone(),
                conversation_kind: ConversationKind::PrivateGroup,
                mode: NotificationLevel::Never,
                rich_payload_opt_in: false,
                source_version: 7,
                updated_at_ms: 42,
                source: NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: conversation,
            })
            .await
            .expect("upsert");
    }

    let deleted = store
        .delete_all_for_source(&alice, NotificationSettingsSource::Xep0402Bookmarks)
        .await
        .expect("delete all");
    assert_eq!(deleted, 2);
    assert!(store
        .get(&alice, &room_one)
        .await
        .expect("alice room one")
        .is_none());
    assert!(store
        .get(&alice, &room_two)
        .await
        .expect("alice room two")
        .is_none());
    assert!(store
        .get(&bob, &room_one)
        .await
        .expect("bob room one")
        .is_some());
}

#[test]
fn derives_rich_payload_opt_in_from_xep0492_advanced_extension() {
    let owner = bare("alice@example.com");
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always>\
                        <advanced>\
                            <rich-payload xmlns='urn:waddle:push:rich:0' />\
                        </advanced>\
                    </always>\
                </notify>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid bookmark payload");

    let mutation = derive_bookmark_projection_mutation(
        &owner,
        "room@muc.example.com",
        Some(&payload),
        ConversationKind::PrivateGroup,
        7,
        11,
    )
    .expect("derive");

    let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
        panic!("expected upsert mutation");
    };
    assert_eq!(projection.mode, NotificationLevel::Always);
    assert!(
        projection.rich_payload_opt_in,
        "XEP-0492 <advanced/> rich-payload child must set the opt-in"
    );
}

#[tokio::test]
async fn effective_rich_payload_opt_in_defaults_off_and_round_trips() {
    let store = migrated_in_memory_store().await;
    let owner = bare("alice@example.com");
    let conversation = bare("room@muc.example.com");

    // Default — no projection row — is opt-out (minimal payload).
    assert!(!store
        .effective_rich_payload_opt_in(&owner, &conversation)
        .await
        .expect("default opt-in"));

    store
        .upsert(&NotificationSettingsProjection {
            owner_bare_jid: owner.clone(),
            conversation_jid: conversation.clone(),
            conversation_kind: ConversationKind::PrivateGroup,
            mode: NotificationLevel::Always,
            rich_payload_opt_in: true,
            source_version: 7,
            updated_at_ms: 42,
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: conversation.clone(),
        })
        .await
        .expect("upsert");

    assert!(store
        .effective_rich_payload_opt_in(&owner, &conversation)
        .await
        .expect("stored opt-in"));
}

#[test]
fn derives_projection_from_xep0402_bookmark_notify_extension() {
    let owner = bare("alice@example.com");
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid bookmark payload");

    let mutation = derive_bookmark_projection_mutation(
        &owner,
        "room@muc.example.com",
        Some(&payload),
        ConversationKind::PrivateGroup,
        7,
        11,
    )
    .expect("derive");

    let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
        panic!("expected upsert mutation");
    };
    assert_eq!(projection.owner_bare_jid, owner);
    assert_eq!(projection.conversation_jid, bare("room@muc.example.com"));
    assert_eq!(projection.mode, NotificationLevel::Never);
    assert_eq!(projection.source_version, 11);
}

#[test]
fn malformed_xep0492_notify_is_rejected() {
    let owner = bare("alice@example.com");
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <notify xmlns='urn:xmpp:notification-settings:1'>\
                    <always />\
                    <never />\
                </notify>\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid XML payload");

    let error = derive_bookmark_projection_mutation(
        &owner,
        "room@muc.example.com",
        Some(&payload),
        ConversationKind::PrivateGroup,
        7,
        11,
    )
    .expect_err("malformed official XEP-0492 payload must be rejected");

    assert!(
        matches!(
            error,
            NotificationSettingsProjectionError::InvalidNotify(
                waddle_xmpp::xep::NotificationSettingsError::MultipleFallbackSettings
            )
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn missing_xep0492_notify_deletes_existing_projection() {
    let owner = bare("alice@example.com");
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1' />"
        .parse()
        .expect("valid bookmark payload");

    let mutation = derive_bookmark_projection_mutation(
        &owner,
        "room@muc.example.com",
        Some(&payload),
        ConversationKind::PrivateGroup,
        7,
        11,
    )
    .expect("derive");

    assert_eq!(
        mutation,
        NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner,
            conversation_jid: bare("room@muc.example.com"),
            source: NotificationSettingsSource::Xep0402Bookmarks,
        }
    );
}

#[test]
fn xep0469_pinning_inside_extensions_is_valid_bookmark_payload() {
    let owner = bare("alice@example.com");
    let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
            <extensions>\
                <pinned xmlns='urn:xmpp:bookmarks-pinning:0' />\
            </extensions>\
        </conference>"
        .parse()
        .expect("valid XML payload");

    validate_xep0402_bookmark_publish("room@muc.example.com", &payload)
        .expect("XEP-0469 pinning belongs inside XEP-0402 extensions");
    let mutation = derive_bookmark_projection_mutation(
        &owner,
        "room@muc.example.com",
        Some(&payload),
        ConversationKind::PrivateGroup,
        7,
        11,
    )
    .expect("derive");
    assert_eq!(
        mutation,
        NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner,
            conversation_jid: bare("room@muc.example.com"),
            source: NotificationSettingsSource::Xep0402Bookmarks,
        }
    );
}

fn dm_bookmark(
    item_id: &str,
    inner: &str,
) -> waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmark {
    let payload: Element =
        format!("<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>{inner}</dm-bookmark>")
            .parse()
            .expect("valid dm-bookmark payload");
    validate_dm_bookmark_publish(item_id, &payload).expect("dm-bookmark validates")
}

#[test]
fn derives_direct_projection_from_dm_bookmark_never_override() {
    let owner = bare("alice@example.com");
    let bookmark = dm_bookmark(
        "bob@example.com",
        "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
    );

    let mutation =
        derive_dm_bookmark_projection_mutation(&owner, &bookmark, 42, 11).expect("derive");

    let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
        panic!("expected upsert mutation");
    };
    assert_eq!(projection.owner_bare_jid, owner);
    assert_eq!(projection.conversation_jid, bare("bob@example.com"));
    assert_eq!(projection.conversation_kind, ConversationKind::Direct);
    assert_eq!(projection.mode, NotificationLevel::Never);
    assert_eq!(
        projection.source,
        NotificationSettingsSource::WaddleDmBookmarks
    );
    assert_eq!(projection.source_item_jid, bare("bob@example.com"));
    assert_eq!(projection.source_version, 11);
    assert_eq!(projection.updated_at_ms, 42);
    assert!(!projection.rich_payload_opt_in);
}

#[test]
fn missing_dm_bookmark_notify_deletes_existing_projection() {
    let owner = bare("alice@example.com");
    // An empty <dm-bookmark/> carries no override.
    let bookmark = dm_bookmark("bob@example.com", "");

    let mutation =
        derive_dm_bookmark_projection_mutation(&owner, &bookmark, 7, 11).expect("derive");

    assert_eq!(
        mutation,
        NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner,
            conversation_jid: bare("bob@example.com"),
            source: NotificationSettingsSource::WaddleDmBookmarks,
        }
    );
}

#[test]
fn malformed_dm_bookmark_notify_is_rejected_at_publish_validation() {
    // Two account-wide fallback settings violate XEP-0492 §3. The
    // strict DM parser rejects this at publish-validation time and
    // surfaces it as InvalidDmBookmark(InvalidNotify(..)).
    let payload: Element = "<dm-bookmark xmlns='urn:waddle:dm-bookmarks:0'>\
            <notify xmlns='urn:xmpp:notification-settings:1'>\
                <always />\
                <never />\
            </notify>\
        </dm-bookmark>"
        .parse()
        .expect("valid XML payload");

    let error = validate_dm_bookmark_publish("bob@example.com", &payload)
        .expect_err("malformed hosted XEP-0492 notify must be rejected");

    assert!(
        matches!(
            error,
            NotificationSettingsProjectionError::InvalidDmBookmark(
                waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmarkError::InvalidNotify(
                    waddle_xmpp::xep::NotificationSettingsError::MultipleFallbackSettings
                )
            )
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn derives_rich_payload_opt_in_from_dm_bookmark_advanced_extension() {
    let owner = bare("alice@example.com");
    let bookmark = dm_bookmark(
        "bob@example.com",
        "<notify xmlns='urn:xmpp:notification-settings:1'>\
            <always>\
                <advanced>\
                    <rich-payload xmlns='urn:waddle:push:rich:0' />\
                </advanced>\
            </always>\
        </notify>",
    );

    let mutation =
        derive_dm_bookmark_projection_mutation(&owner, &bookmark, 7, 11).expect("derive");

    let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
        panic!("expected upsert mutation");
    };
    assert_eq!(projection.mode, NotificationLevel::Always);
    assert_eq!(projection.conversation_kind, ConversationKind::Direct);
    assert!(
        projection.rich_payload_opt_in,
        "XEP-0492 <advanced/> rich-payload child must set the opt-in"
    );
}

#[tokio::test]
async fn dm_bookmark_projection_round_trips_through_store_as_direct_row() {
    let store = migrated_in_memory_store().await;
    let owner = bare("alice@example.com");
    let contact = bare("bob@example.com");

    let bookmark = dm_bookmark(
        "bob@example.com",
        "<notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>",
    );
    let mutation =
        derive_dm_bookmark_projection_mutation(&owner, &bookmark, 42, 7).expect("derive");
    let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
        panic!("expected upsert mutation");
    };
    store.upsert(&projection).await.expect("upsert");

    let loaded = store
        .get(&owner, &contact)
        .await
        .expect("get")
        .expect("row present");
    assert_eq!(loaded.conversation_kind, ConversationKind::Direct);
    assert_eq!(loaded.mode, NotificationLevel::Never);
    assert_eq!(loaded.source, NotificationSettingsSource::WaddleDmBookmarks);
    assert_eq!(loaded.source_item_jid, contact);

    // A Direct row default for a contact with no projection is Always.
    assert_eq!(
        store
            .effective_setting(&owner, &bare("carol@example.com"), ConversationKind::Direct)
            .await
            .expect("default"),
        NotificationLevel::Always
    );
}
