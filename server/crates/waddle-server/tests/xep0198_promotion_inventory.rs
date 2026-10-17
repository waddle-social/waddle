//! XEP-0198 unavailable-resource custody survives a process restart.
pub mod ingress_support;

use ingress_support::IngressFixture;
use kameo::actor::Spawn;
use std::sync::Arc;
use std::time::Duration;
use waddle_server::{
    db::DatabaseDriver, pending_delivery::DatabasePendingDeliveryStorage,
    sm_persistence::DatabaseSmPersistence, sm_promotion::promote_session_unacked,
};
use waddle_xmpp::{
    ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    },
    pending_delivery::{
        storage::PendingDeliveryStorage, InsertOutcome, PendingPayload, PendingRow, PendingRowId,
        QuotaPolicy, SmSessionId,
    },
    protocol::session_state::Blocklist,
    registry::{ConnectionRegistry, UserRegistryActor},
    stream_management::persistence::SmPersistenceStorage,
    stream_management::{
        DetachedSessionSnapshot, InMemorySmSessionRegistry, SmClaimCompletion, SmSessionRegistry,
        StreamManagementState,
    },
    telemetry::attributes::SmEvictionPath,
    Stanza,
};

fn message(id: &str) -> Stanza {
    let mut message =
        xmpp_parsers::message::Message::new(Some("alice@example.com".parse().unwrap()));
    message.from = Some("bob@example.com/phone".parse().unwrap());
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id(id.to_owned()));
    message.bodies.insert(
        xmpp_parsers::message::Lang::new(),
        "durable work".to_owned(),
    );
    Stanza::Message(message)
}

async fn durable_inventory(fixture: IngressFixture) {
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .unwrap(),
    );
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    let mut sm = StreamManagementState::new();
    sm.enable("promotion-inventory".to_owned(), true, Some(300));
    let _ = sm.record_outbound(
        waddle_xmpp::parser::stanza_to_string(match message("prefix") {
            Stanza::Message(message) => message,
            _ => unreachable!(),
        })
        .unwrap(),
        SmEvictionPath::DirectOutbound,
    );
    let mut session = sm
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_owned(),
            jid: "alice@example.com/web".parse().unwrap(),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .unwrap();
    session.promotion_custody =
        waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed();
    let stream = SmSessionId::new(&session.stream_id);
    let inventory = registry.register_promotion_session(&session).await.unwrap();
    registry
        .append_promotion_stanza(&session, &message("suffix"), 2, chrono::Utc::now())
        .await
        .unwrap();
    let persisted = storage.get_session(&stream).await.unwrap().unwrap();
    assert_eq!(persisted.outbound_count, 2);
    assert_eq!(persisted.max_resume_time, Some(0));
    assert_eq!(persisted.max_resume_duration, std::time::Duration::ZERO);
    assert_eq!(storage.list_unacked(&stream).await.unwrap().len(), 2);

    // Force a failure after payload INSERT but before watermark UPDATE.
    let rejection = match fixture.db.driver() {
        DatabaseDriver::Sqlite => "CREATE TRIGGER reject_inventory_watermark BEFORE UPDATE OF outbound_count ON sm_sessions WHEN NEW.outbound_count = 3 BEGIN SELECT RAISE(ABORT, 'injected watermark failure'); END",
        DatabaseDriver::Postgres => "ALTER TABLE sm_sessions ADD CONSTRAINT reject_inventory_watermark CHECK (outbound_count <> 3)",
    };
    fixture
        .db
        .guard()
        .await
        .unwrap()
        .execute(rejection, ())
        .await
        .unwrap();
    assert!(registry
        .append_promotion_stanza(&session, &message("rolled-back"), 3, chrono::Utc::now())
        .await
        .is_err());
    assert_eq!(
        storage
            .get_session(&stream)
            .await
            .unwrap()
            .unwrap()
            .outbound_count,
        2
    );
    assert_eq!(
        storage.list_unacked(&stream).await.unwrap().len(),
        2,
        "failed watermark commit must roll back the payload too"
    );

    // A successful sink may already have retired a row before a crash.
    registry
        .delete_unacked_sequences(&session.stream_id, &[2])
        .await
        .unwrap();
    registry
        .append_promotion_stanza(&session, &message("suffix"), 2, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        storage.list_unacked(&stream).await.unwrap().len(),
        1,
        "retrying an accepted sequence must not recreate a retired row"
    );
    let authoritative = registry
        .register_promotion_session(&inventory)
        .await
        .unwrap();
    assert_eq!(authoritative.unacked_stanzas.len(), 1);
    drop(registry);

    let restarted = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(restarted.restore_from_persistence().await.unwrap(), 1);
    assert!(restarted
        .peek_session(&session.stream_id)
        .await
        .unwrap()
        .is_none());
    let retry = restarted.drain_expired().await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].unacked_stanzas.len(), 1);
    assert!(retry[0].unacked_stanzas[0].stanza_xml.contains("prefix"));
    assert!(restarted.confirm_drained(&session.stream_id).await);
    assert!(storage.get_session(&stream).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream).await.unwrap().is_empty());

    // Best-effort bulk metadata scans must not hide readable message rows
    // from the strict durable retraction sweep.
    let mut poisoned = session.clone();
    poisoned.stream_id = "poisoned-promotion".to_owned();
    poisoned.promotion_custody =
        waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed();
    let poison_registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    poison_registry
        .register_promotion_session(&poisoned)
        .await
        .unwrap();
    fixture
        .db
        .guard()
        .await
        .unwrap()
        .execute(
            "UPDATE sm_sessions SET occupancy_session = ? WHERE stream_id = ?",
            waddle_server::db_params!["invalid", poisoned.stream_id.as_str()],
        )
        .await
        .unwrap();
    assert!(storage.list_all_sessions().await.unwrap().is_empty());
    let target = waddle_xmpp::tombstone::TombstoneTarget::Direct {
        wire_id: "prefix".to_owned(),
        author: "bob@example.com".parse().unwrap(),
        archive: "alice@example.com".parse().unwrap(),
    };
    let scrubber = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
    assert_eq!(
        scrubber
            .snapshot_unacked_for_tombstone(&target)
            .await
            .unwrap()
            .len(),
        1
    );
    scrubber.scrub_unacked_for_tombstone(&target).await.unwrap();
    assert!(
        storage
            .list_unacked(&SmSessionId::new(&poisoned.stream_id))
            .await
            .unwrap()
            .is_empty(),
        "metadata poison must not retain retracted message content"
    );
    drop(storage);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_nonresumable_inventory_and_atomic_suffix_survive_restart() {
    durable_inventory(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_nonresumable_inventory_and_atomic_suffix_survive_restart() {
    let Some(fixture) = IngressFixture::postgres("sminv").await else {
        return;
    };
    durable_inventory(fixture).await;
}

fn recorded_resume_session(
    stream: &str,
    resume_seconds: u32,
) -> waddle_xmpp::stream_management::DetachedSession {
    let mut sm = StreamManagementState::new();
    sm.enable(stream.to_owned(), true, Some(resume_seconds));
    for id in ["first", "second"] {
        let Stanza::Message(message) = message(id) else {
            unreachable!();
        };
        let _ = sm.record_outbound(
            waddle_xmpp::parser::stanza_to_string(message).expect("typed message XML"),
            SmEvictionPath::DirectOutbound,
        );
    }
    let mut session = sm
        .to_detached_session(DetachedSessionSnapshot {
            user_id: "alice".to_owned(),
            jid: "alice@example.com/web".parse().expect("recipient"),
            occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
            carbons_enabled: false,
            roster_interested: false,
            blocklist_interested: false,
            presence_available: false,
            presence_show: None,
            presence_status: None,
            presence_priority: 0,
            presence_payloads: Vec::new(),
            pending_subscribes_flushed: false,
        })
        .expect("detached resumable queue");
    session.promotion_custody =
        waddle_xmpp::stream_management::PromotionCaptureCustody::never_claimed();
    session
}

async fn expired_resume_completion_survives_restart(fixture: IngressFixture) {
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("canonical SM storage"),
    );
    let pending = Arc::new(
        DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("portable pending storage"),
    );
    let claims = Arc::new(InProcessClaimStore::new());
    let owner = NodeIdentity::new("resume-node", "expiry-incarnation");
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), SharedNodeIdentity::new(owner.clone()));
    let session = recorded_resume_session("expired-resume-completion", 1);
    let stream = SmSessionId::new(&session.stream_id);
    let entity = Entity::new(EntityType::SmSession, session.stream_id.clone());
    // This first stanza originated in pending storage and was flushed before
    // detaching. Leave its ACK settlement unfinished across the restart.
    let Stanza::Message(first_message) = message("first") else {
        unreachable!();
    };
    let first_pending_id = PendingRowId::fresh();
    pending
        .insert(PendingRow {
            id: first_pending_id.clone(),
            recipient: session.jid.to_bare(),
            original_receipt_at: chrono::Utc::now(),
            payload: PendingPayload::Transient(Box::new(first_message)),
            flushed_in_session: None,
            outbound_sequence: None,
        })
        .await
        .expect("original offline delivery");
    assert_eq!(
        pending
            .claim_for_session(&session.jid.to_bare(), &stream)
            .await
            .expect("flush original pending row")
            .len(),
        1
    );
    pending
        .record_pushed_at(&first_pending_id, 1)
        .await
        .expect("record original outbound sequence");
    assert!(registry
        .store_session(session)
        .await
        .expect("store resumable queue")
        .is_empty());
    registry
        .claim_session(stream.as_str())
        .await
        .expect("reserve in-flight resume")
        .expect("session live at claim time");
    let fence = registry
        .current_sm_claim_fence(stream.as_str())
        .expect("resume's exact claim");
    let recorded = storage.list_unacked(&stream).await.expect("recorded queue");
    assert_eq!(recorded.len(), 2);

    // Pause after reservation. An expired completion still consumes valid
    // h=1, retaining only the second stanza as an offline delivery obligation.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let Some(SmClaimCompletion::Expired(expired)) = registry
        .complete_claim_if_resumable(stream.as_str(), 1)
        .await
        .expect("expired completion retains custody")
    else {
        panic!("resume completion must reject the expired session");
    };
    assert_eq!(expired.promotion_custody.captured_fence(), Some(&fence));
    let durable = storage
        .get_session(&stream)
        .await
        .expect("canonical source readable")
        .expect("expiry cannot delete canonical source");
    assert_eq!(durable.max_resume_time, Some(0));
    assert_eq!(durable.max_resume_duration, Duration::ZERO);
    assert_eq!(durable.last_acked, 1);
    assert_eq!(
        storage
            .pending_acknowledgements(&stream)
            .await
            .expect("durable pending ACK window"),
        vec![waddle_xmpp::stream_management::persistence::SmAckWindow {
            from_exclusive: 0,
            to_inclusive: 1,
        }]
    );
    let retained = storage.list_unacked(&stream).await.expect("retained queue");
    assert_eq!(retained.len(), 1);
    for (retained, original) in retained.iter().zip(&recorded[1..]) {
        assert_eq!(retained.sequence, original.sequence);
        assert_eq!(retained.original_receipt_at, original.original_receipt_at);
        assert_eq!(retained.stanza.to_element(), original.stanza.to_element());
    }
    let claim = claims
        .current_claim(&entity)
        .await
        .expect("retained claim")
        .expect("claim remains until canonical retirement");
    assert_eq!(claim.owner, owner);
    assert_eq!(claim.claim_epoch, fence.epoch());

    // Simulate interruption after one independent pending handoff commits but
    // before any canonical source pruning. Its receipt must survive the crash.
    let first = &expired.unacked_stanzas[0];
    let message = xmpp_parsers::message::Message::try_from(
        first
            .stanza_xml
            .parse::<minidom::Element>()
            .expect("recorded XML"),
    )
    .expect("recorded message");
    assert_eq!(
        pending
            .insert_promotion(
                PendingRow {
                    id: PendingRowId::fresh(),
                    recipient: expired.jid.to_bare(),
                    original_receipt_at: first.original_receipt_at,
                    payload: PendingPayload::Transient(Box::new(message)),
                    flushed_in_session: None,
                    outbound_sequence: None,
                },
                stream.as_str(),
                first.sequence,
                Some(&fence),
            )
            .await
            .expect("first durable handoff"),
        InsertOutcome::Inserted
    );
    // Lose every process-local cache and pool, including in-process claims.
    // Reopening the durable backends must recover without the old incarnation.
    drop(registry);
    drop(storage);
    drop(pending);
    drop(claims);
    let storage = Arc::new(
        DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("reopened canonical storage"),
    );
    let pending = Arc::new(
        DatabasePendingDeliveryStorage::open(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("reopened portable pending storage"),
    );
    let claims = Arc::new(InProcessClaimStore::new());
    let restarted_owner = NodeIdentity::new("resume-node", "restarted-incarnation");
    let restarted = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(
            claims.clone(),
            SharedNodeIdentity::new(restarted_owner.clone()),
        );
    assert!(!restarted
        .retire_orphaned_promotion_receipts(&stream, pending.as_ref())
        .await
        .expect("source existence checked before restore"));
    assert_eq!(
        restarted.restore_from_persistence().await.expect("restart"),
        1
    );
    assert!(restarted
        .claim_session(stream.as_str())
        .await
        .expect("inventory cannot resume")
        .is_none());
    let mut recovered = restarted
        .drain_expired()
        .await
        .expect("recovered inventory");
    assert_eq!(recovered.len(), 1);
    let recovered = recovered.pop().expect("one source");
    let recovered_fence = recovered
        .promotion_custody
        .captured_fence()
        .expect("fresh recovery authority");
    assert_eq!(recovered_fence.owner(), &restarted_owner);
    assert_ne!(recovered_fence, &fence);
    assert_eq!(recovered.unacked_stanzas.len(), 1);
    assert_eq!(recovered.unacked_stanzas[0].sequence, 2);
    let pending_storage: Arc<dyn PendingDeliveryStorage> = pending.clone();
    let summary = promote_session_unacked(
        &recovered,
        &ConnectionRegistry::new(),
        &UserRegistryActor::spawn(UserRegistryActor::new()),
        &pending_storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert!(!summary.has_storage_failure(), "{summary:?}");
    assert_eq!(summary.queued, 1);
    assert!(
        !restarted.confirm_promotion_session(&recovered).await,
        "canonical source must retain its unsettled pending ACK journal"
    );
    restarted
        .settle_session_acknowledgements(&recovered, pending.as_ref())
        .await
        .expect("restart settles the original pending ACK window");
    assert!(storage
        .pending_acknowledgements(&stream)
        .await
        .expect("settled durable ACK journal")
        .is_empty());
    let rows = pending
        .list(&recovered.jid.to_bare())
        .await
        .expect("pending queue");
    let mut message_ids = rows
        .iter()
        .map(|row| {
            let PendingPayload::Transient(message) = &row.payload else {
                panic!("peer message remains transient");
            };
            message.id.as_ref().expect("original message ID").0.as_str()
        })
        .collect::<Vec<_>>();
    message_ids.sort_unstable();
    assert_eq!(
        message_ids,
        ["second"],
        "acknowledged first message must not return and handoff retry must not duplicate"
    );
    assert!(!restarted
        .retire_orphaned_promotion_receipts(&stream, pending.as_ref())
        .await
        .expect("source still exists after sink commit"));
    assert_eq!(
        pending
            .promotion_receipt_sources_before(i64::MAX, None, 8)
            .await
            .expect("retained receipts"),
        vec![stream.clone()]
    );
    assert!(restarted.confirm_promotion_session(&recovered).await);
    assert!(storage
        .get_session(&stream)
        .await
        .expect("canonical retirement")
        .is_none());
    assert!(storage
        .list_unacked(&stream)
        .await
        .expect("replay retirement")
        .is_empty());
    assert!(restarted
        .retire_orphaned_promotion_receipts(&stream, pending.as_ref())
        .await
        .expect("durably absent source authorizes receipt reclamation"));
    assert!(pending
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("settled receipts")
        .is_empty());
    assert!(claims
        .current_claim(&entity)
        .await
        .expect("claim released")
        .is_none());
    assert!(restarted
        .drain_expired()
        .await
        .expect("no duplicate recovery")
        .is_empty());
    drop(restarted);
    drop(storage);
    drop(pending_storage);
    drop(pending);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_expired_resume_completion_retains_exact_queue_until_promotion() {
    expired_resume_completion_survives_restart(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_expired_resume_completion_retains_exact_queue_until_promotion() {
    let Some(fixture) = IngressFixture::postgres("smcompleteexpiry").await else {
        return;
    };
    expired_resume_completion_survives_restart(fixture).await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn postgres_fenced_expired_completion_uses_supplied_authority_and_exact_epoch() {
    use waddle_server::{
        clustering::claims::{NodeLeaseStore, PostgresClaimStore},
        db::{Database, DatabaseConfig, DEFAULT_CONTROL_PLANE_POOL_SIZE},
        sm_persistence_fenced::PostgresFencedSmPersistence,
    };
    use waddle_xmpp::{
        ownership::StalePredicate,
        pending_delivery::storage::PendingStorageError,
        stream_management::persistence::{SmClaimFence, SmPersistenceError},
    };

    let Some(fixture) = IngressFixture::postgres("smfencedexpiry").await else {
        return;
    };
    let db = Database::from_config(
        "fenced-expired-completion",
        &DatabaseConfig::new(DatabaseDriver::Postgres, fixture.db.database_url())
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("isolated main and control-plane pools");
    let claims = Arc::new(PostgresClaimStore::new(db.clone()));
    claims.ensure_schema().await.expect("claim schema");
    let owner = NodeIdentity::new("fenced-resume-node", "expiry-incarnation");
    claims
        .register(&owner, None)
        .await
        .expect("original node lease");
    let identity = SharedNodeIdentity::new(owner.clone());
    let pending = Arc::new(
        waddle_server::pending_delivery::open_for_cluster_mode(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
            true,
            Some((claims.clone(), identity.clone())),
            &db,
        )
        .await
        .expect("fenced pending storage"),
    );
    let storage = Arc::new(
        PostgresFencedSmPersistence::open(db.clone(), claims.clone(), identity.clone())
            .await
            .expect("fenced persistence"),
    );
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), identity.clone());
    let stream = SmSessionId::new("fenced-expired-resume-completion");
    registry
        .store_session(recorded_resume_session(stream.as_str(), 1))
        .await
        .expect("store queue");
    registry
        .claim_session(stream.as_str())
        .await
        .expect("reserve resume")
        .expect("live session");
    let fence = registry
        .current_sm_claim_fence(stream.as_str())
        .expect("original fence");
    let recorded = storage.list_unacked(&stream).await.expect("recorded queue");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let operation = registry
        .lock_session_operation(stream.as_str())
        .await
        .expect("stream operation");
    let authority = identity
        .guard_if_current(&owner)
        .await
        .expect("current identity authority");
    let Some(SmClaimCompletion::Expired(expired)) = registry
        .complete_claim_with_authority(operation, &authority)
        .await
        .expect("guarded expired completion")
    else {
        panic!("fenced resume must expire");
    };
    drop(authority);
    assert_eq!(expired.promotion_custody.captured_fence(), Some(&fence));
    let durable = storage
        .get_session(&stream)
        .await
        .expect("canonical inventory")
        .expect("source preserved");
    assert_eq!(durable.max_resume_time, Some(0));
    assert_eq!(durable.max_resume_duration, Duration::ZERO);
    let retained = storage
        .list_unacked(&stream)
        .await
        .expect("queue preserved");
    assert_eq!(retained.len(), recorded.len());
    for (retained, original) in retained.iter().zip(&recorded) {
        assert_eq!(retained.sequence, original.sequence);
        assert_eq!(retained.original_receipt_at, original.original_receipt_at);
        assert_eq!(retained.stanza.to_element(), original.stanza.to_element());
    }
    assert_eq!(
        claims
            .current_claim(&Entity::new(EntityType::SmSession, stream.as_str()))
            .await
            .expect("claim retained")
            .expect("exact claim")
            .claim_epoch,
        fence.epoch()
    );

    // Same-owner epoch takeover defeats any implementation that substitutes
    // a fresh claim/cache entry for the immutable capture supplied by callers.
    let stale_stream = SmSessionId::new("fenced-stale-expiry-source");
    registry
        .store_session(recorded_resume_session(stale_stream.as_str(), 300))
        .await
        .expect("second source");
    let stale_fence = registry
        .current_sm_claim_fence(stale_stream.as_str())
        .expect("old fence");
    let entity = Entity::new(EntityType::SmSession, stale_stream.as_str());
    claims
        .release_exact(&entity, &owner, stale_fence.epoch())
        .await
        .expect("retire old claim");
    let successor_epoch = claims
        .ensure_claimed(&entity, &owner)
        .await
        .expect("successor epoch");
    assert!(successor_epoch > stale_fence.epoch());
    let authority = identity
        .guard_if_current(&owner)
        .await
        .expect("current authority");
    assert!(matches!(
        storage
            .mark_session_nonresumable(&stale_stream, 0, &stale_fence, Some(&authority))
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    let foreign = SmClaimFence::new(
        NodeIdentity::new("other-node", "other-incarnation"),
        successor_epoch,
    );
    assert!(matches!(
        storage
            .mark_session_nonresumable(&stale_stream, 0, &foreign, Some(&authority))
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    let untouched = storage
        .get_session(&stale_stream)
        .await
        .expect("rejected source readable")
        .expect("source retained");
    assert_eq!(untouched.max_resume_time, Some(300));
    assert_eq!(untouched.max_resume_duration, Duration::from_secs(300));
    let queue = storage
        .list_unacked(&stale_stream)
        .await
        .expect("rejected queue retained");
    assert_eq!(queue.len(), 2);
    for (retained, original) in queue.iter().zip(&recorded) {
        assert_eq!(retained.sequence, original.sequence);
        assert_eq!(retained.stanza.to_element(), original.stanza.to_element());
    }
    let successor = SmClaimFence::new(owner.clone(), successor_epoch);
    storage
        .mark_session_nonresumable(&stale_stream, 0, &successor, Some(&authority))
        .await
        .expect("exact successor may retire resume eligibility");
    assert_eq!(
        storage
            .get_session(&stale_stream)
            .await
            .expect("converted source")
            .expect("source retained")
            .max_resume_time,
        Some(0)
    );
    let missing = SmSessionId::new("fenced-missing-expiry-source");
    let missing_epoch = claims
        .ensure_claimed(
            &Entity::new(EntityType::SmSession, missing.as_str()),
            &owner,
        )
        .await
        .expect("missing source's exact claim");
    assert!(storage
        .mark_session_nonresumable(
            &missing,
            0,
            &SmClaimFence::new(owner.clone(), missing_epoch),
            Some(&authority)
        )
        .await
        .is_err());
    assert!(storage
        .get_session(&missing)
        .await
        .expect("missing stays absent")
        .is_none());
    drop(authority);

    // Commit one atomic pending handoff, then lose all process-local state.
    // Fenced handoffs prune their canonical row in the same transaction.
    let first = &expired.unacked_stanzas[0];
    let first_message = xmpp_parsers::message::Message::try_from(
        first
            .stanza_xml
            .parse::<minidom::Element>()
            .expect("recorded XML"),
    )
    .expect("recorded message");
    let first_pending = PendingRow {
        id: PendingRowId::fresh(),
        recipient: expired.jid.to_bare(),
        original_receipt_at: first.original_receipt_at,
        payload: PendingPayload::Transient(Box::new(first_message)),
        flushed_in_session: None,
        outbound_sequence: None,
    };
    assert_eq!(
        pending
            .insert_promotion(first_pending, stream.as_str(), first.sequence, Some(&fence))
            .await
            .expect("first atomic fenced handoff"),
        InsertOutcome::Inserted,
    );
    assert_eq!(
        storage
            .list_unacked(&stream)
            .await
            .expect("atomic prune")
            .len(),
        1
    );
    drop(registry);
    drop(storage);
    drop(pending);
    drop(claims);
    drop(identity);
    drop(db);

    let db = Database::from_config(
        "restarted-fenced-expired-completion",
        &DatabaseConfig::new(DatabaseDriver::Postgres, fixture.db.database_url())
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("reopened process pools");
    let claims = Arc::new(PostgresClaimStore::new(db.clone()));
    let restarted_owner = NodeIdentity::new("fenced-resume-node", "restarted-incarnation");
    let identity = SharedNodeIdentity::new(restarted_owner.clone());
    let storage = Arc::new(
        PostgresFencedSmPersistence::open(db.clone(), claims.clone(), identity.clone())
            .await
            .expect("reopened fenced canonical storage"),
    );
    let pending = Arc::new(
        waddle_server::pending_delivery::open_for_cluster_mode(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
            true,
            Some((claims.clone(), identity.clone())),
            &db,
        )
        .await
        .expect("reopened fenced pending storage"),
    );
    let restarted = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), identity);
    assert_eq!(
        restarted
            .restore_from_persistence()
            .await
            .expect("startup before reclaim"),
        0,
        "a new incarnation cannot reuse the crashed owner's claim"
    );
    claims
        .register(&restarted_owner, None)
        .await
        .expect("fresh node incarnation");
    let entity = Entity::new(EntityType::SmSession, stream.as_str());
    let recovered_epoch = claims
        .steal_stale(
            &entity,
            fence.epoch(),
            StalePredicate::OwnerStale,
            &restarted_owner,
        )
        .await
        .expect("recover crashed owner's claim");
    assert!(recovered_epoch > fence.epoch());
    assert_eq!(
        restarted
            .restore_from_persistence()
            .await
            .expect("startup after claim recovery"),
        1
    );
    let mut recovered = restarted
        .drain_expired()
        .await
        .expect("recovered inventory");
    assert_eq!(recovered.len(), 1);
    let recovered = recovered.pop().expect("recovered source");
    let recovered_fence = SmClaimFence::new(restarted_owner, recovered_epoch);
    assert_eq!(
        recovered.promotion_custody.captured_fence(),
        Some(&recovered_fence)
    );
    assert_eq!(recovered.unacked_stanzas.len(), 1);
    assert_eq!(recovered.unacked_stanzas[0].sequence, 2);
    // A pre-crash capture cannot write under the successor's new epoch.
    let Stanza::Message(stale_message) = message("stale-capture") else {
        unreachable!()
    };
    assert!(matches!(
        pending
            .insert_promotion(
                PendingRow {
                    id: PendingRowId::fresh(),
                    recipient: recovered.jid.to_bare(),
                    original_receipt_at: first.original_receipt_at,
                    payload: PendingPayload::Transient(Box::new(stale_message)),
                    flushed_in_session: None,
                    outbound_sequence: None,
                },
                stream.as_str(),
                2,
                Some(&fence)
            )
            .await,
        Err(PendingStorageError::NotOwner { .. })
    ));
    let pending_storage: Arc<dyn PendingDeliveryStorage> = pending.clone();
    let summary = promote_session_unacked(
        &recovered,
        &ConnectionRegistry::new(),
        &UserRegistryActor::spawn(UserRegistryActor::new()),
        &pending_storage,
        &Blocklist::empty(),
        "example.com",
        &[],
    )
    .await;
    assert!(!summary.has_storage_failure(), "{summary:?}");
    assert_eq!(summary.queued, 1);
    let rows = pending
        .list(&recovered.jid.to_bare())
        .await
        .expect("both durable handoffs");
    let mut ids = rows
        .iter()
        .map(|row| {
            let PendingPayload::Transient(message) = &row.payload else {
                panic!("transient message")
            };
            message.id.as_ref().expect("message ID").0.as_str()
        })
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, ["first", "second"]);
    assert!(storage
        .list_unacked(&stream)
        .await
        .expect("recovered atomic handoff")
        .is_empty());
    assert!(storage
        .get_session(&stream)
        .await
        .expect("source before confirmation")
        .is_some());
    assert!(pending
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("no portable fenced receipts")
        .is_empty());
    assert!(restarted.confirm_promotion_session(&recovered).await);
    assert!(storage
        .get_session(&stream)
        .await
        .expect("canonical source retired")
        .is_none());
    assert!(claims
        .current_claim(&entity)
        .await
        .expect("claim retired")
        .is_none());
    assert!(pending
        .promotion_receipt_sources_before(i64::MAX, None, 8)
        .await
        .expect("no leaked receipts")
        .is_empty());
    assert!(restarted
        .drain_expired()
        .await
        .expect("no duplicate recovery")
        .is_empty());
    drop(restarted);
    drop(storage);
    drop(pending_storage);
    drop(pending);
    drop(claims);
    drop(db);
    fixture.close().await;
}

async fn postgres_fenced_poison_settles_ack_before_quarantine(corrupt_header: bool) {
    use waddle_server::{
        clustering::claims::{NodeLeaseStore, PostgresClaimStore},
        db::{Database, DatabaseConfig, DEFAULT_CONTROL_PLANE_POOL_SIZE},
        sm_persistence_fenced::PostgresFencedSmPersistence,
    };
    use waddle_xmpp::{
        ownership::{ExactReleaseOutcome, StalePredicate},
        stream_management::{
            persistence::{SmAckWindow, SmClaimFence, SmPersistenceError},
            ReclaimedHydrationOutcome,
        },
    };

    let Some(fixture) = IngressFixture::postgres("smackpoison").await else {
        return;
    };
    let db = Database::from_config(
        "fenced-poison-ack-settlement",
        &DatabaseConfig::new(DatabaseDriver::Postgres, fixture.db.database_url())
            .with_control_plane_pool(DEFAULT_CONTROL_PLANE_POOL_SIZE),
    )
    .await
    .expect("isolated main and control-plane pools");
    let claims = Arc::new(PostgresClaimStore::new(db.clone()));
    claims.ensure_schema().await.expect("claim schema");
    let owner = NodeIdentity::new("poison-resume-node", "original-incarnation");
    claims.register(&owner, None).await.expect("original lease");
    let identity = SharedNodeIdentity::new(owner.clone());
    let storage = Arc::new(
        PostgresFencedSmPersistence::open(db.clone(), claims.clone(), identity.clone())
            .await
            .expect("original canonical storage"),
    );
    let pending = Arc::new(
        waddle_server::pending_delivery::open_for_cluster_mode(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
            true,
            Some((claims.clone(), identity.clone())),
            &db,
        )
        .await
        .expect("fenced pending storage"),
    );
    let registry = InMemorySmSessionRegistry::new()
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), identity);
    let session = recorded_resume_session("fenced-poison-ack", 300);
    let stream = SmSessionId::new(&session.stream_id);
    let recipient = session.jid.to_bare();
    registry
        .store_session(session)
        .await
        .expect("canonical queue");
    let original_fence = registry
        .current_sm_claim_fence(stream.as_str())
        .expect("original exact fence");
    let mut pending_ids = Vec::new();
    for id in ["first", "second"] {
        let Stanza::Message(message) = message(id) else {
            unreachable!();
        };
        let row_id = PendingRowId::fresh();
        pending
            .insert(PendingRow {
                id: row_id.clone(),
                recipient: recipient.clone(),
                original_receipt_at: chrono::Utc::now(),
                payload: PendingPayload::Transient(Box::new(message)),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .expect("original pending row");
        pending_ids.push(row_id);
    }
    assert_eq!(
        pending
            .claim_for_session(&recipient, &stream)
            .await
            .unwrap()
            .len(),
        2
    );
    for (id, sequence) in pending_ids.iter().zip([1, 2]) {
        pending.record_pushed_at(id, sequence).await.unwrap();
    }
    storage
        .acknowledge_claimed_session(&stream, 1, &original_fence, None)
        .await
        .expect("commit ACK before pending deletion");
    let ack_window = SmAckWindow {
        from_exclusive: 0,
        to_inclusive: 1,
    };
    assert_eq!(
        storage.pending_acknowledgements(&stream).await.unwrap(),
        vec![ack_window]
    );
    let corruption = if corrupt_header {
        "UPDATE sm_sessions SET occupancy_session = 'invalid' WHERE stream_id = ?"
    } else {
        "UPDATE sm_unacked SET stanza_xml = '<broken' WHERE stream_id = ? AND sequence = 2"
    };
    db.guard()
        .await
        .unwrap()
        .execute(corruption, waddle_server::db_params![stream.as_str()])
        .await
        .expect("inject durable poison after ACK commit");
    drop(registry);
    drop(storage);
    drop(pending);

    // The node process restarts before it can settle pending ACKs. The
    // reclaimed source cannot be decoded into a DetachedSession at all.
    let successor = NodeIdentity::new("poison-resume-node", "successor-incarnation");
    claims
        .register(&successor, None)
        .await
        .expect("successor lease");
    let entity = Entity::new(EntityType::SmSession, stream.as_str());
    let epoch = claims
        .steal_stale(
            &entity,
            original_fence.epoch(),
            StalePredicate::OwnerStale,
            &successor,
        )
        .await
        .expect("reclaim crashed source");
    assert!(epoch > original_fence.epoch());
    let successor_fence = SmClaimFence::new(successor.clone(), epoch);
    let identity = SharedNodeIdentity::new(successor);
    let storage = Arc::new(
        PostgresFencedSmPersistence::open(db.clone(), claims.clone(), identity.clone())
            .await
            .expect("reopened canonical storage"),
    );
    let pending = Arc::new(
        waddle_server::pending_delivery::open_for_cluster_mode(
            Some(fixture.db.database_url()),
            QuotaPolicy::Unlimited,
            true,
            Some((claims.clone(), identity.clone())),
            &db,
        )
        .await
        .expect("reopened pending storage"),
    );
    assert!(matches!(
        storage
            .confirm_acknowledgement(&stream, ack_window, &original_fence, None)
            .await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert!(matches!(
        storage.quarantine_session(&stream, &original_fence).await,
        Err(SmPersistenceError::NotOwner { .. })
    ));
    assert_eq!(pending.list(&recipient).await.unwrap().len(), 2);
    assert_eq!(
        storage.pending_acknowledgements(&stream).await.unwrap(),
        vec![ack_window],
        "stale cleanup cannot discard the successor's ACK evidence"
    );
    let registry = InMemorySmSessionRegistry::with_capacity(1)
        .with_persistence(storage.clone())
        .with_claim_store(claims.clone(), identity)
        .with_pending_delivery_storage(pending.clone());
    let reservation = registry
        .reserve_reclaimed_claim_capacity(&entity)
        .expect("reclaimed source consumes capacity");
    assert_eq!(registry.claim_fence_capacity_used(), 1);
    assert_eq!(
        registry
            .hydrate_reclaimed_typed(&entity, &successor_fence, reservation)
            .await
            .expect("poison hydration settles durable ACKs"),
        ReclaimedHydrationOutcome::PoisonReleased
    );
    assert!(storage.get_session(&stream).await.unwrap().is_none());
    assert!(storage.list_unacked(&stream).await.unwrap().is_empty());
    assert!(storage
        .pending_acknowledgements(&stream)
        .await
        .unwrap()
        .is_empty());
    let rows = pending
        .list(&recipient)
        .await
        .expect("unacknowledged custody");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, pending_ids[1]);
    assert_eq!(rows[0].outbound_sequence, Some(2));
    assert_eq!(rows[0].flushed_in_session.as_ref(), Some(&stream));

    assert_eq!(
        registry
            .release_reclaimed_claim(&entity, &successor_fence, reservation)
            .await
            .expect("terminal poison cleanup"),
        ExactReleaseOutcome::Released
    );
    assert_eq!(registry.claim_fence_capacity_used(), 0);
    assert!(claims.current_claim(&entity).await.unwrap().is_none());
    let protected = registry.pending_claim_protected_streams().await.unwrap();
    assert!(!protected.contains(&stream));
    let orphaned = pending
        .list_orphaned_claims(&protected, i64::MAX)
        .await
        .expect("remaining delivery becomes releasable");
    assert_eq!(orphaned, vec![(pending_ids[1].clone(), stream.clone())]);
    assert_eq!(
        pending
            .release_row_if_session(&pending_ids[1], &stream)
            .await
            .unwrap(),
        1
    );
    let replay = pending
        .claim_for_session(&recipient, &SmSessionId::new("reconnected-resource"))
        .await
        .expect("reconnect recovers only unacknowledged delivery");
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].id, pending_ids[1]);
    drop(registry);
    drop(storage);
    drop(pending);
    drop(claims);
    drop(db);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_fenced_poison_header_settles_ack_before_quarantine() {
    postgres_fenced_poison_settles_ack_before_quarantine(true).await;
}

#[tokio::test]
async fn postgres_fenced_poison_replay_settles_ack_before_quarantine() {
    postgres_fenced_poison_settles_ack_before_quarantine(false).await;
}
