use super::{apply_pin_mutation, ensure_schema, PinDurabilityError, PinOperationId};
use crate::db::{Database, DatabaseConfig, DatabaseDriver};
use crate::muc_durable::tests::projection_test_room;
use crate::room_effect_outbox::RoomEffectOutboxStore;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use jid::BareJid;
use tokio_util::sync::CancellationToken;
use waddle_xmpp::muc::durable::{
    ChannelId, MucDurableStore, RoomDurableMutation, RoomMutationEffects, WaddleId,
};
use waddle_xmpp::muc::pin::{PinPreview, PinStateChange, PinnedEntry, MAX_PINNED_ENTRIES};
use waddle_xmpp::muc::{RoomClaimFenceContext, RoomConfig};
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, SharedNodeIdentity};
use waddle_xmpp_core::xep0359::StanzaId;

fn bare_jid(value: &str) -> BareJid {
    value.parse().expect("valid bare JID")
}

fn timestamp() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-01T12:34:56Z")
        .expect("valid timestamp")
        .with_timezone(&Utc)
}

fn entry(room: &BareJid, target: &str, body: &str) -> PinnedEntry {
    let author = bare_jid("alice@example.test");
    PinnedEntry {
        target_stanza_id: StanzaId::new(target, jid::Jid::from(room.clone())),
        pinner_jid: bare_jid("moderator@example.test"),
        pinned_at: timestamp(),
        preview: PinPreview::new(author, Some("Alice".into()), body, timestamp()),
    }
}

async fn active_room(prefix: &str) -> Option<crate::muc_durable::tests::ProjectionTestRoom> {
    let room = projection_test_room(prefix).await?;
    ensure_schema(&room.db)
        .await
        .expect("install explicit pin lab schema");
    room.store
        .commit_room_mutation(
            &room.room_jid,
            &room.fence,
            RoomDurableMutation::Activate,
            RoomMutationEffects::none(),
        )
        .await
        .expect("activate durable room");
    Some(room)
}

#[test]
fn pin_mutations_preserve_replacement_order_unpin_and_cap() {
    let room = bare_jid("room@conference.example.test");
    let mut entries = vec![
        entry(&room, "second", "second preview"),
        entry(&room, "first", "first preview"),
    ];

    apply_pin_mutation(
        &mut entries,
        &PinStateChange::Pin(entry(&room, "first", "replacement preview")),
    );
    assert_eq!(entries[0].target_stanza_id.id, "first");
    assert_eq!(entries[0].preview.text, "replacement preview");
    assert_eq!(entries[1].target_stanza_id.id, "second");

    apply_pin_mutation(
        &mut entries,
        &PinStateChange::Unpin {
            target_stanza_id: StanzaId::new("second", jid::Jid::from(room.clone())),
        },
    );
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].target_stanza_id.id, "first");

    let mut at_cap = (0..MAX_PINNED_ENTRIES)
        .map(|index| entry(&room, &format!("target-{index}"), "preview"))
        .collect::<Vec<_>>();
    apply_pin_mutation(
        &mut at_cap,
        &PinStateChange::Pin(entry(&room, "newest", "new preview")),
    );
    assert_eq!(at_cap.len(), MAX_PINNED_ENTRIES);
    assert_eq!(at_cap[0].target_stanza_id.id, "newest");
    assert!(!at_cap
        .iter()
        .any(|pinned| pinned.target_stanza_id.id == format!("target-{}", MAX_PINNED_ENTRIES - 1)));
}

#[tokio::test]
async fn sqlite_pin_schema_installs_immutable_outcome_guards() {
    let db = Database::from_config(
        "room-pin-schema-sqlite-test",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
    )
    .await
    .expect("open in-memory SQLite database");
    ensure_schema(&db)
        .await
        .expect("install SQLite pin lab schema");

    let connection = db.guard().await.expect("database guard");
    connection
        .execute(
            "INSERT INTO clustering_muc_room_pin_outcomes \
             (operation_id, room_jid, lifecycle_id, owner_node_id, owner_node_epoch, claim_epoch, \
              snapshot_revision, mutation_json, created_at_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![
                "op-1",
                "room@conference.example.test",
                "lifecycle-1",
                "node-1",
                "node-epoch-1",
                1_i64,
                1_i64,
                r#"{"type":"unpin","target_stanza_id":{"id":"target","by":"room@conference.example.test"}}"#,
                1_i64,
            ],
        )
        .await
        .expect("insert outcome");
    assert!(connection
        .execute(
            "UPDATE clustering_muc_room_pin_outcomes SET mutation_json = ? WHERE operation_id = ?",
            crate::db_params!["changed", "op-1"],
        )
        .await
        .is_err());
    assert!(connection
        .execute(
            "DELETE FROM clustering_muc_room_pin_outcomes WHERE operation_id = ?",
            crate::db_params!["op-1"],
        )
        .await
        .is_err());
    let mut rows = connection
        .query(
            "SELECT COUNT(*) FROM clustering_muc_room_pin_outcomes WHERE operation_id = ?",
            crate::db_params!["op-1"],
        )
        .await
        .expect("read retained outcome");
    let row = rows.next().await.expect("read count").expect("count row");
    let count: i64 = row.get(0).expect("count value");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn exact_operation_id_is_idempotent_and_rejects_payload_mismatch() {
    let _guard = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(room) = active_room("pin-op-payload").await else {
        return;
    };
    let operation_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    let mutation = PinStateChange::Pin(entry(&room.room_jid, "target-1", "original preview"));

    let first = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation.clone())
        .await
        .expect("commit exact pin operation");
    let replay = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation.clone())
        .await
        .expect("replay exact operation");
    assert!(replay.was_replay);
    assert_eq!(replay.outcome, first.outcome);
    assert_eq!(replay.snapshot, first.snapshot);

    let mismatch = room
        .store
        .commit_pin_operation(
            &room.room_jid,
            &room.fence,
            operation_id,
            PinStateChange::Pin(entry(
                &room.room_jid,
                "target-1",
                "different preview payload",
            )),
        )
        .await;
    assert!(matches!(
        mismatch,
        Err(PinDurabilityError::OperationIdConflict)
    ));

    let unpin_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    room.store
        .commit_pin_operation(
            &room.room_jid,
            &room.fence,
            unpin_id,
            PinStateChange::Unpin {
                target_stanza_id: StanzaId::new("target-1", jid::Jid::from(room.room_jid.clone())),
            },
        )
        .await
        .expect("unpin the first target");
    let replacement_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    let replacement = room
        .store
        .commit_pin_operation(
            &room.room_jid,
            &room.fence,
            replacement_id,
            PinStateChange::Pin(entry(&room.room_jid, "target-2", "newer preview")),
        )
        .await
        .expect("commit a newer replacement pin");
    assert_eq!(replacement.snapshot.revision, first.snapshot.revision + 2);
    assert_eq!(replacement.snapshot.entries.len(), 1);
    assert_eq!(
        replacement.snapshot.entries[0].target_stanza_id.id,
        "target-2"
    );

    let replay_after_newer_state = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation)
        .await
        .expect("replay the original operation after newer mutations");
    assert!(replay_after_newer_state.was_replay);
    assert_eq!(replay_after_newer_state.outcome, first.outcome);
    assert_eq!(replay_after_newer_state.snapshot, replacement.snapshot);
    assert_eq!(
        replay_after_newer_state.snapshot.revision,
        replacement.snapshot.revision
    );
    assert_eq!(
        replay_after_newer_state.snapshot.entries[0]
            .target_stanza_id
            .id,
        "target-2"
    );

    let snapshot = room
        .store
        .load_pin_snapshot_fenced(&room.room_jid, &room.fence)
        .await
        .expect("load pin snapshot")
        .expect("snapshot exists");
    assert_eq!(snapshot, replacement.snapshot);
}

#[tokio::test]
async fn outcome_and_full_snapshot_restore_under_successor_claim() {
    let _guard = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(room) = active_room("pin-op-restore").await else {
        return;
    };
    let operation_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    let mutation = PinStateChange::Pin(entry(
        &room.room_jid,
        "restore-target",
        "full frozen preview",
    ));
    let committed = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation.clone())
        .await
        .expect("commit pin operation");

    let successor = crate::muc_durable::tests::node_identity();
    let next_epoch = room.fence.epoch.0 + 1;
    let entity_key = format!("{}:{}", EntityType::RoomActor.as_db_str(), room.room_jid);
    let connection = room.db.guard().await.expect("database guard");
    assert_eq!(
        connection
            .execute(
                "UPDATE clustering_claims SET node_id = ?, node_epoch = ?, claim_epoch = ? \
                 WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
                crate::db_params![
                    successor.node_id.clone(),
                    successor.node_epoch.clone(),
                    next_epoch,
                    entity_key,
                    room.fence.owner.node_id.clone(),
                    room.fence.owner.node_epoch.clone(),
                    room.fence.epoch.0,
                ],
            )
            .await
            .expect("transfer room claim"),
        1
    );
    drop(connection);

    let successor_fence = RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room.room_jid.to_string()),
        successor.clone(),
        ClaimEpoch(next_epoch),
    );
    let successor_store = crate::muc_durable::PostgresMucRoomStore::open(
        room.db.clone(),
        CancellationToken::new(),
        SharedNodeIdentity::new(successor),
    )
    .await
    .expect("open successor room store");
    successor_store.record_claim_fence(&room.room_jid, successor_fence.clone());

    let restored = successor_store
        .load_pin_snapshot_fenced(&room.room_jid, &successor_fence)
        .await
        .expect("restore under successor claim")
        .expect("snapshot exists");
    assert_eq!(restored, committed.snapshot);
    let retained = successor_store
        .load_pin_operation_outcome_fenced(&room.room_jid, &successor_fence, operation_id)
        .await
        .expect("load retained outcome")
        .expect("outcome exists");
    assert_eq!(retained, committed.outcome);
    let replay = successor_store
        .commit_pin_operation(&room.room_jid, &successor_fence, operation_id, mutation)
        .await
        .expect("replay after claim transfer");
    assert!(replay.was_replay);
    assert_eq!(replay.outcome, committed.outcome);
    assert_eq!(replay.snapshot, committed.snapshot);
}

#[tokio::test]
async fn exact_outcome_remains_queryable_after_room_recreation() {
    let _guard = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(room) = active_room("pin-op-retired-lifecycle").await else {
        return;
    };
    let operation_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    let mutation = PinStateChange::Pin(entry(
        &room.room_jid,
        "retired-target",
        "frozen preview from old lifecycle",
    ));
    let committed = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation.clone())
        .await
        .expect("commit pin outcome in original lifecycle");

    room.store
        .commit_room_mutation(
            &room.room_jid,
            &room.fence,
            RoomDurableMutation::Destroy {
                completion_attempt: None,
            },
            RoomMutationEffects::none(),
        )
        .await
        .expect("retire original lifecycle");
    let recreated = room
        .store
        .commit_room_mutation(
            &room.room_jid,
            &room.fence,
            RoomDurableMutation::Create {
                waddle_id: WaddleId::new("waddle-pin-retired-lifecycle".into()),
                channel_id: ChannelId::new("channel-pin-retired-lifecycle".into()),
                config: RoomConfig::default(),
                initial_affiliations: Vec::new(),
            },
            RoomMutationEffects::none(),
        )
        .await
        .expect("create successor lifecycle");
    assert_ne!(recreated.coordinates.lifecycle, committed.outcome.lifecycle);
    room.store
        .commit_room_mutation(
            &room.room_jid,
            &room.fence,
            RoomDurableMutation::Activate,
            RoomMutationEffects::none(),
        )
        .await
        .expect("activate successor lifecycle");

    let restored = room
        .store
        .load_pin_snapshot_fenced(&room.room_jid, &room.fence)
        .await
        .expect("restore successor lifecycle snapshot");
    assert_eq!(restored, None);
    let historical = room
        .store
        .load_pin_operation_outcome_fenced(&room.room_jid, &room.fence, operation_id)
        .await
        .expect("load exact outcome under current claim")
        .expect("retained outcome survives lifecycle retirement");
    assert_eq!(historical, committed.outcome);
    assert_eq!(historical.lifecycle, committed.outcome.lifecycle);

    let reused_id = room
        .store
        .commit_pin_operation(&room.room_jid, &room.fence, operation_id, mutation)
        .await;
    assert!(matches!(
        reused_id,
        Err(PinDurabilityError::OperationIdConflict)
    ));
    assert_eq!(
        room.store
            .load_pin_snapshot_fenced(&room.room_jid, &room.fence)
            .await
            .expect("read successor snapshot after rejected ID reuse"),
        None
    );
}

#[tokio::test]
async fn stale_claim_cannot_commit_a_pin_outcome() {
    let _guard = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(room) = active_room("pin-op-stale").await else {
        return;
    };
    let connection = room.db.guard().await.expect("database guard");
    assert_eq!(
        connection
            .execute(
                "UPDATE clustering_claims SET claim_epoch = ? \
                 WHERE entity = ? AND node_id = ? AND node_epoch = ? AND claim_epoch = ?",
                crate::db_params![
                    room.fence.epoch.0 + 1,
                    format!("{}:{}", EntityType::RoomActor.as_db_str(), room.room_jid),
                    room.fence.owner.node_id.clone(),
                    room.fence.owner.node_epoch.clone(),
                    room.fence.epoch.0,
                ],
            )
            .await
            .expect("advance claim epoch"),
        1
    );
    drop(connection);

    let result = room
        .store
        .commit_pin_operation(
            &room.room_jid,
            &room.fence,
            PinOperationId::from_uuid(uuid::Uuid::now_v7()),
            PinStateChange::Pin(entry(&room.room_jid, "stale-target", "preview")),
        )
        .await;
    assert!(matches!(
        result,
        Err(PinDurabilityError::Claim(
            waddle_xmpp::muc::durable::RoomCommitError::NotOwner
        ))
    ));
    assert!(room
        .store
        .load_pin_snapshot_fenced(&room.room_jid, &room.fence)
        .await
        .is_err());
}

#[tokio::test]
async fn failed_outcome_append_rolls_back_the_snapshot_update() {
    let _guard = crate::clustering::claims::clustering_control_plane_table_lock()
        .lock()
        .await;
    let Some(room) = active_room("pin-op-rollback").await else {
        return;
    };
    let operation_id = PinOperationId::from_uuid(uuid::Uuid::now_v7());
    let operation_text = operation_id.to_string();
    let suffix = operation_id.as_uuid().simple().to_string();
    let function = format!("waddle_test_pin_reject_{suffix}");
    let trigger = format!("waddle_test_pin_reject_trigger_{suffix}");
    let connection = room.db.guard().await.expect("database guard");
    connection
        .execute(
            &format!(
                "CREATE FUNCTION {function}() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN IF NEW.operation_id = '{operation_text}' THEN \
                   RAISE EXCEPTION 'injected pin outcome failure'; \
                 END IF; RETURN NEW; END; $$"
            ),
            (),
        )
        .await
        .expect("create rejecting trigger function");
    connection
        .execute(
            &format!(
                "CREATE TRIGGER {trigger} BEFORE INSERT ON clustering_muc_room_pin_outcomes \
                 FOR EACH ROW EXECUTE FUNCTION {function}()"
            ),
            (),
        )
        .await
        .expect("create rejecting trigger");
    drop(connection);

    let result = room
        .store
        .commit_pin_operation(
            &room.room_jid,
            &room.fence,
            operation_id,
            PinStateChange::Pin(entry(&room.room_jid, "rollback-target", "preview")),
        )
        .await;
    let connection = room.db.guard().await.expect("database guard");
    connection
        .execute(
            &format!("DROP TRIGGER IF EXISTS {trigger} ON clustering_muc_room_pin_outcomes"),
            (),
        )
        .await
        .expect("drop rejecting trigger");
    connection
        .execute(&format!("DROP FUNCTION IF EXISTS {function}()"), ())
        .await
        .expect("drop rejecting trigger function");

    let snapshot = room
        .store
        .load_pin_snapshot_fenced(&room.room_jid, &room.fence)
        .await
        .expect("load rolled-back snapshot");
    let outcome = room
        .store
        .load_pin_operation_outcome_fenced(&room.room_jid, &room.fence, operation_id)
        .await
        .expect("load absent outcome");
    assert!(matches!(result, Err(PinDurabilityError::Database(_))));
    assert_eq!(snapshot, None);
    assert_eq!(outcome, None);
}

#[tokio::test]
async fn sqlite_schema_does_not_claim_room_fencing_support() {
    let db = Database::from_config(
        "room-pin-api-sqlite-test",
        &DatabaseConfig::new(DatabaseDriver::Sqlite, "sqlite::memory:"),
    )
    .await
    .expect("open in-memory SQLite database");
    ensure_schema(&db)
        .await
        .expect("install SQLite pin lab schema");
    let identity = crate::muc_durable::tests::node_identity();
    let room_effect_outbox = RoomEffectOutboxStore::try_new(db.clone())
        .await
        .expect("open room effect outbox");
    let store = crate::muc_durable::PostgresMucRoomStore {
        db,
        room_effect_outbox,
        node_identity: SharedNodeIdentity::new(identity.clone()),
        stop_token: CancellationToken::new(),
        exact_claim_fences: DashMap::new(),
        published_claim_fences: DashMap::new(),
    };
    let room = bare_jid("pin-api@conference.example.test");
    let fence = RoomClaimFenceContext::new(
        Entity::new(EntityType::RoomActor, room.to_string()),
        identity,
        ClaimEpoch(1),
    );
    store.record_claim_fence(&room, fence.clone());
    let result = store.load_pin_snapshot_fenced(&room, &fence).await;
    assert!(matches!(
        result,
        Err(PinDurabilityError::UnsupportedBackend)
    ));
}
