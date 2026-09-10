use super::*;
use crate::pending_delivery::SmSessionId;
use crate::stream_management::persistence::{
    InMemorySmPersistence, KeyedSnapshotOutcome, PersistedIngressAppend,
};
use crate::stream_management::{SmIngressAppendKey, SmIngressReceiptKind, SmKeyedAppendOutcome};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn obligation(jid: &FullJid) -> SmIngressAppendKey {
    SmIngressAppendKey {
        message_key: crate::ingress::MessageKey::new(),
        kind: SmIngressReceiptKind::from_storage(3),
        semantic_identity_hash: [7; 32],
        resource: jid.clone(),
    }
}

fn stanza() -> Stanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.id = Some(xmpp_parsers::message::Id("keyed-entry".to_owned()));
    Stanza::Message(message)
}

fn snapshot(registry: &InMemorySmSessionRegistry, stream: &str) -> DetachedSession {
    registry
        .sessions
        .read()
        .unwrap()
        .get(stream)
        .unwrap()
        .clone()
}

fn assert_snapshot_unchanged(before: &DetachedSession, after: &DetachedSession) {
    assert_eq!(after.outbound_count, before.outbound_count);
    assert_eq!(after.inbound_count, before.inbound_count);
    assert_eq!(after.last_acked, before.last_acked);
    assert_eq!(after.replay_gap_through, before.replay_gap_through);
    assert_eq!(after.unacked_stanzas, before.unacked_stanzas);
    assert_eq!(after.detached_at, before.detached_at);
    assert_eq!(after.max_resume_time, before.max_resume_time);
}

#[tokio::test]
async fn keyed_append_older_proof_precedes_missing_session_lookup() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let jid = make_test_jid();
    let key = obligation(&jid);
    registry
        .store_session(realistic_test_session("older"))
        .await
        .unwrap();
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &stanza(),
                Utc::now(),
                key.clone(),
            )
            .await
            .unwrap(),
        SmKeyedAppendOutcome::Appended {
            accepting_stream: SmSessionId::new("older")
        }
    );
    assert!(registry.take_session("older").await.unwrap().is_some());
    assert!(storage
        .get_session(&SmSessionId::new("older"))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key,)
            .await
            .unwrap(),
        SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: SmSessionId::new("older")
        }
    );
    assert!(registry.sessions.read().unwrap().is_empty());
}

#[tokio::test]
async fn keyed_append_resume_ack_redetach_retry_does_not_allocate_again() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let jid = make_test_jid();
    let key = obligation(&jid);
    registry
        .store_session(realistic_test_session("resuming"))
        .await
        .unwrap();
    assert!(registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key.clone(),)
        .await
        .unwrap()
        .is_allocated());
    registry.claim_session("resuming").await.unwrap().unwrap();
    let Some(SmClaimCompletion::Resumed(mut resumed)) = registry
        .complete_claim_if_resumable("resuming", 8)
        .await
        .unwrap()
    else {
        panic!("expected successful resume");
    };
    assert!(storage
        .get_session(&SmSessionId::new("resuming"))
        .await
        .unwrap()
        .is_none());
    // The connection accepts the client's h and re-detaches after all entries are acknowledged.
    resumed.last_acked = resumed.outbound_count;
    resumed.unacked_stanzas.clear();
    resumed.detached_at = Instant::now();
    registry.store_session(resumed).await.unwrap();
    let before = snapshot(&registry, "resuming");
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key,)
            .await
            .unwrap(),
        SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: SmSessionId::new("resuming")
        }
    );
    assert_snapshot_unchanged(&before, &snapshot(&registry, "resuming"));
}

#[tokio::test]
async fn keyed_append_rebind_retry_returns_old_stream_without_changing_replacement() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage));
    let jid = make_test_jid();
    let key = obligation(&jid);
    registry
        .store_session(realistic_test_session("old-binding"))
        .await
        .unwrap();
    registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key.clone())
        .await
        .unwrap();
    let displaced = registry
        .store_session(realistic_test_session("new-binding"))
        .await
        .unwrap();
    assert_eq!(displaced.len(), 1);
    let before = snapshot(&registry, "new-binding");
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key,)
            .await
            .unwrap(),
        SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: SmSessionId::new("old-binding")
        }
    );
    assert_snapshot_unchanged(&before, &snapshot(&registry, "new-binding"));
}

#[tokio::test]
async fn keyed_append_distinct_obligations_preserve_sequence_and_original_timestamp() {
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage));
    let jid = make_test_jid();
    let first = obligation(&jid);
    let second = SmIngressAppendKey {
        semantic_identity_hash: [8; 32],
        ..first.clone()
    };
    registry
        .store_session(realistic_test_session("distinct"))
        .await
        .unwrap();
    let receipt = Utc::now() - chrono::Duration::hours(2);
    for key in [first, second] {
        assert_eq!(
            registry
                .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), receipt, key,)
                .await
                .unwrap(),
            SmKeyedAppendOutcome::Appended {
                accepting_stream: SmSessionId::new("distinct")
            }
        );
    }
    let session = snapshot(&registry, "distinct");
    assert_eq!(session.outbound_count, 9);
    assert_eq!(session.unacked_stanzas.len(), 4);
    assert_eq!(session.unacked_stanzas[2].sequence, 8);
    assert_eq!(session.unacked_stanzas[3].sequence, 9);
    assert_eq!(session.unacked_stanzas[2].original_receipt_at, receipt);
    assert_eq!(session.unacked_stanzas[3].original_receipt_at, receipt);
}

#[tokio::test]
async fn keyed_append_no_session_for_missing_or_expired_resource() {
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new())),
    );
    let jid = make_test_jid();
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &stanza(),
                Utc::now(),
                obligation(&jid),
            )
            .await
            .unwrap(),
        SmKeyedAppendOutcome::NoSession
    );
    let mut expired = realistic_test_session("expired-keyed");
    expired.max_resume_time = Some(0);
    registry.store_session(expired).await.unwrap();
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &stanza(),
                Utc::now(),
                obligation(&jid),
            )
            .await
            .unwrap(),
        SmKeyedAppendOutcome::NoSession
    );
}

#[tokio::test]
async fn keyed_append_losing_transaction_preserves_every_snapshot_field_and_expiry() {
    let stream = "keyed-loser";
    let storage = Arc::new(GatedSnapshotPersistence::new(stream));
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let jid = make_test_jid();
    let mut full = realistic_test_session(stream);
    let capacity = crate::stream_management::DEFAULT_MAX_UNACKED_QUEUE_SIZE;
    full.last_acked = 0;
    full.outbound_count = capacity.try_into().unwrap();
    full.replay_gap_through = Some(0);
    full.unacked_stanzas = (1..=full.outbound_count)
        .map(|sequence| DetachedUnackedStanza {
            sequence,
            ..full.unacked_stanzas[0].clone()
        })
        .collect();
    registry.store_session(full).await.unwrap();
    let before = snapshot(&registry, stream);
    let durable_before = storage
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .unwrap();
    let durable_queue = storage
        .list_unacked(&SmSessionId::new(stream))
        .await
        .unwrap();
    let key = obligation(&jid);
    storage.armed.store(true, Ordering::SeqCst);
    let appending_registry = registry.clone();
    let appending_key = key.clone();
    let append = tokio::spawn(async move {
        appending_registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &stanza(),
                Utc::now(),
                appending_key,
            )
            .await
    });
    storage.reached.notified().await;
    // A different transaction wins after this call's ledger pre-check, without
    // changing this stream's snapshot. The losing clone must never be published.
    assert_eq!(
        storage
            .inner
            .store_session_atomic_with_ingress_append(
                durable_before.clone(),
                durable_queue,
                PersistedIngressAppend {
                    key,
                    accepting_stream: SmSessionId::new("earlier-winner"),
                    sequence: 41,
                    appended_at: Utc::now(),
                    supersedes: None
                },
            )
            .await
            .unwrap(),
        KeyedSnapshotOutcome::Committed
    );
    storage.proceed.notify_one();
    assert_eq!(
        append.await.unwrap().unwrap(),
        SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: SmSessionId::new("earlier-winner")
        }
    );
    assert_snapshot_unchanged(&before, &snapshot(&registry, stream));
    let durable_after = storage
        .get_session(&SmSessionId::new(stream))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(durable_after.detached_at, durable_before.detached_at);
    assert_eq!(
        durable_after.max_resume_duration,
        durable_before.max_resume_duration
    );
    assert_eq!(durable_after.outbound_count, durable_before.outbound_count);
    assert_eq!(durable_after.last_acked, durable_before.last_acked);
    assert_eq!(
        durable_after.replay_gap_through,
        durable_before.replay_gap_through
    );
}

#[tokio::test]
async fn keyed_append_wraps_outbound_sequence_without_reordering() {
    let registry = std::sync::Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new())),
    );
    let jid = make_test_jid();
    let mut session = realistic_test_session("keyed-wrap");
    session.outbound_count = u32::MAX;
    session.last_acked = u32::MAX - 2;
    session.unacked_stanzas[0].sequence = u32::MAX - 1;
    session.unacked_stanzas[1].sequence = u32::MAX;
    registry.store_session(session).await.unwrap();
    registry
        .record_keyed_stanza_for_detached_bound_resource(
            &jid,
            &stanza(),
            Utc::now(),
            obligation(&jid),
        )
        .await
        .unwrap();
    let after = snapshot(&registry, "keyed-wrap");
    assert_eq!(after.outbound_count, 0);
    assert_eq!(after.last_acked, u32::MAX - 2);
    assert_eq!(after.replay_gap_through, None);
    assert_eq!(
        after
            .unacked_stanzas
            .iter()
            .map(|entry| entry.sequence)
            .collect::<Vec<_>>(),
        vec![u32::MAX - 1, u32::MAX, 0]
    );
}

async fn cancel_after_keyed_commit(
    stream: &SmSessionId,
) -> (
    Arc<InMemorySmSessionRegistry>,
    Arc<GatedSnapshotPersistence>,
    SmIngressAppendKey,
    chrono::DateTime<Utc>,
) {
    let storage = Arc::new(GatedSnapshotPersistence::new(stream.as_str()));
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .unwrap();
    storage.commit_then_gate.store(true, Ordering::SeqCst);
    storage.armed.store(true, Ordering::SeqCst);
    let jid = make_test_jid();
    let key = obligation(&jid);
    let append_registry = registry.clone();
    let append_key = key.clone();
    let receipt = Utc::now() - chrono::Duration::hours(1);
    let append = tokio::spawn(async move {
        append_registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), receipt, append_key)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), storage.reached.notified())
        .await
        .expect("append commits before cancellation");
    let unpublished = snapshot(&registry, stream.as_str());
    assert_eq!(unpublished.outbound_count, 7);
    assert_eq!(storage.inner.list_unacked(stream).await.unwrap().len(), 3);
    append.abort();
    assert!(append.await.unwrap_err().is_cancelled());
    // The persist deliberately outlives caller cancellation and owns the stream
    // shard until it resolves, so release its gate and let it settle; otherwise
    // the consumers under test would simply block on that shard.
    storage.proceed.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while snapshot(&registry, stream.as_str()).outbound_count != 8 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the cancelled persist settles and publishes");
    // Now reproduce what a crash between commit and publication actually leaves:
    // storage ahead of memory. The consumers must reconcile from storage rather
    // than trust the in-memory queue, which is the property these tests exist for.
    {
        let mut sessions = registry.sessions.write().unwrap();
        *sessions
            .get_mut(stream.as_str())
            .expect("session still registered") = unpublished;
    }
    registry.mark_snapshot_stale(stream).unwrap();
    storage.armed.store(false, Ordering::SeqCst);
    (registry, storage, key, receipt)
}

fn assert_keyed_entry(session: &DetachedSession, receipt: chrono::DateTime<Utc>) {
    assert_eq!(session.outbound_count, 8);
    assert_eq!(session.unacked_stanzas.len(), 3);
    let entry = session
        .unacked_stanzas
        .iter()
        .find(|entry| entry.sequence == 8)
        .unwrap();
    let parsed: minidom::Element = entry.stanza_xml.parse().unwrap();
    assert_eq!(parsed.attr("id"), Some("keyed-entry"));
    assert_eq!(entry.original_receipt_at, receipt);
}

async fn promote_captured_queue(
    session: &DetachedSession,
    pending: &crate::pending_delivery::storage::InMemoryPendingDeliveryStorage,
) {
    use crate::pending_delivery::storage::PendingDeliveryStorage;
    use crate::pending_delivery::{PendingPayload, PendingRow, PendingRowId};
    // Exercise the registry's real promotion handoff with actual pending storage:
    // each captured replay message becomes a pending row before confirm_drained.
    for entry in &session.unacked_stanzas {
        let element: minidom::Element = entry.stanza_xml.parse().unwrap();
        let message = xmpp_parsers::message::Message::try_from(element).unwrap();
        pending
            .insert(PendingRow {
                id: PendingRowId::fresh(),
                recipient: session.jid.to_bare(),
                original_receipt_at: entry.original_receipt_at,
                payload: PendingPayload::Transient(Box::new(message)),
                flushed_in_session: None,
                outbound_sequence: None,
            })
            .await
            .unwrap();
    }
}

async fn assert_pending_keyed_entry(
    pending: &crate::pending_delivery::storage::InMemoryPendingDeliveryStorage,
    receipt: chrono::DateTime<Utc>,
) {
    use crate::pending_delivery::storage::PendingDeliveryStorage;
    use crate::pending_delivery::PendingPayload;
    let rows = pending.list(&make_test_jid().to_bare()).await.unwrap();
    assert_eq!(rows.len(), 3);
    let keyed = rows
        .iter()
        .find(|row| {
            matches!(
                &row.payload,
                PendingPayload::Transient(message)
                    if message.id.as_ref().is_some_and(|id| id.0 == "keyed-entry")
            )
        })
        .expect("committed keyed stanza survives SM deletion in pending delivery");
    assert_eq!(keyed.original_receipt_at, receipt);
}

#[tokio::test]
async fn keyed_append_cancelled_after_commit_immediate_resume_replays_durable_entry() {
    let stream = SmSessionId::new("keyed-cancel-resume");
    let (registry, storage, _, receipt) = cancel_after_keyed_commit(&stream).await;
    // No append or retry occurs between cancellation and claim/replay.
    let claimed = registry
        .claim_session(stream.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_keyed_entry(&claimed, receipt);
    let Some(SmClaimCompletion::Resumed(resumed)) = registry
        .complete_claim_if_resumable(stream.as_str(), 5)
        .await
        .unwrap()
    else {
        panic!("expected reconciled session to resume");
    };
    assert_keyed_entry(&resumed, receipt);
    assert!(storage.inner.get_session(&stream).await.unwrap().is_none());
}

#[tokio::test]
async fn keyed_append_cancelled_after_commit_expiry_promotes_durable_entry() {
    let stream = SmSessionId::new("keyed-cancel-expiry");
    let (registry, storage, _, receipt) = cancel_after_keyed_commit(&stream).await;
    // Advance only the in-memory monotonic age, without an intervening append.
    registry
        .sessions
        .write()
        .unwrap()
        .get_mut(stream.as_str())
        .unwrap()
        .detached_at = Instant::now() - Duration::from_secs(121);
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    assert_keyed_entry(&drained[0], receipt);
    let pending = crate::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited();
    promote_captured_queue(&drained[0], &pending).await;
    assert!(registry.confirm_drained(stream.as_str()).await);
    assert!(storage.inner.get_session(&stream).await.unwrap().is_none());
    assert_pending_keyed_entry(&pending, receipt).await;
}

#[tokio::test]
async fn keyed_append_committed_while_displaced_is_promoted_before_confirm_drained() {
    let stream = SmSessionId::new("keyed-displaced");
    let storage = Arc::new(GatedSnapshotPersistence::new(stream.as_str()));
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .unwrap();
    let old_lock = registry.stream_lock(stream.as_str()).unwrap();
    let replacement_stream = (0..10_000)
        .map(|n| format!("keyed-replacement-{n}"))
        .find(|candidate| !Arc::ptr_eq(&old_lock, &registry.stream_lock(candidate).unwrap()))
        .expect("replacement uses a different shard");
    let key = obligation(&make_test_jid());
    let append_key = key.clone();
    let append_registry = registry.clone();
    let receipt = Utc::now() - chrono::Duration::hours(3);
    storage.armed.store(true, Ordering::SeqCst);
    let append = tokio::spawn(async move {
        append_registry
            .record_keyed_stanza_for_detached_bound_resource(
                &make_test_jid(),
                &stanza(),
                receipt,
                append_key,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), storage.reached.notified())
        .await
        .expect("keyed transaction pauses before commit");
    let replacement_registry = registry.clone();
    let replacement = tokio::spawn(async move {
        replacement_registry
            .store_session(realistic_test_session(&replacement_stream))
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while registry
            .sessions
            .read()
            .unwrap()
            .contains_key(stream.as_str())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement captures the old queue before the append commits");
    storage.proceed.notify_one();
    assert_eq!(
        append.await.unwrap().unwrap(),
        SmKeyedAppendOutcome::Appended {
            accepting_stream: stream.clone(),
        }
    );
    let displaced = replacement.await.unwrap().unwrap();
    assert_eq!(displaced.len(), 1);
    assert_keyed_entry(&displaced[0], receipt);
    let pending = crate::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited();
    promote_captured_queue(&displaced[0], &pending).await;
    assert!(registry.confirm_drained(stream.as_str()).await);
    assert!(storage.inner.get_session(&stream).await.unwrap().is_none());
    assert_pending_keyed_entry(&pending, receipt).await;
    assert_eq!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &make_test_jid(),
                &stanza(),
                Utc::now(),
                key,
            )
            .await
            .unwrap(),
        SmKeyedAppendOutcome::AlreadyAppended {
            accepting_stream: stream
        }
    );
    assert_pending_keyed_entry(&pending, receipt).await;
}

#[tokio::test]
async fn keyed_append_promotion_confirmation_preserves_rows_on_reconciliation_failure() {
    let stream = SmSessionId::new("keyed-confirm-read-failure");
    let storage = Arc::new(GatedGetSessionPersistence::new(stream.as_str()));
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let mut expired = realistic_test_session(stream.as_str());
    expired.max_resume_time = Some(0);
    registry.store_session(expired).await.unwrap();
    let drained = registry.drain_expired().await.unwrap();
    assert_eq!(drained.len(), 1);
    storage.fail_get_once.store(true, Ordering::SeqCst);
    assert!(!registry.confirm_drained(stream.as_str()).await);
    assert!(storage.inner.get_session(&stream).await.unwrap().is_some());
    assert_eq!(storage.inner.list_unacked(&stream).await.unwrap().len(), 2);
    assert!(registry.confirm_drained(stream.as_str()).await);
}

#[tokio::test]
async fn keyed_append_confirmation_retries_unseen_durable_entry_before_deletion() {
    use crate::stream_management::persistence::PersistedUnackedStanza;
    let stream = SmSessionId::new("keyed-confirm-new-entry");
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry =
        std::sync::Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    let mut expired = realistic_test_session(stream.as_str());
    expired.max_resume_time = Some(0);
    registry.store_session(expired).await.unwrap();
    let captured = registry.drain_expired().await.unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].unacked_stanzas.len(), 2);
    // Model the durable completion of an uncertain write after the caller's
    // promotion baseline was captured. Confirmation must reject that baseline.
    let mut durable = storage.get_session(&stream).await.unwrap().unwrap();
    let mut queue = storage.list_unacked(&stream).await.unwrap();
    durable.outbound_count = durable.outbound_count.wrapping_add(1);
    let receipt = Utc::now() - chrono::Duration::hours(4);
    queue.push(PersistedUnackedStanza {
        stream_id: stream.clone(),
        ingress_receipts: Vec::new(),
        sequence: durable.outbound_count,
        stanza: Box::new(stanza()),
        original_receipt_at: receipt,
    });
    storage
        .store_session_atomic_with_ingress_append(
            durable,
            queue,
            PersistedIngressAppend {
                key: obligation(&make_test_jid()),
                accepting_stream: stream.clone(),
                sequence: 9,
                appended_at: Utc::now(),
                supersedes: None,
            },
        )
        .await
        .unwrap();
    assert!(!registry.confirm_drained(stream.as_str()).await);
    assert!(storage.get_session(&stream).await.unwrap().is_some());
    let retry = registry.drain_expired().await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_keyed_entry(&retry[0], receipt);
    let pending = crate::pending_delivery::storage::InMemoryPendingDeliveryStorage::unlimited();
    promote_captured_queue(&retry[0], &pending).await;
    assert!(registry.confirm_drained(stream.as_str()).await);
    assert!(storage.get_session(&stream).await.unwrap().is_none());
    assert_pending_keyed_entry(&pending, receipt).await;
}

/// #1756: cancelling the caller while its keyed write is still in flight must not
/// release the stream shard.
///
/// The SQLite driver runs an already-submitted COMMIT on its own worker thread and
/// deliberately ignores the drop-triggered rollback once it succeeds
/// (`sqlx-sqlite` worker, `Command::Commit`). If the shard were released at
/// cancellation, the next writer could read pre-commit state and then overwrite the
/// committed entry with a full snapshot replacement — while the ledger proof for it
/// survived. That is unrecoverable loss, because the proof suppresses every retry.
///
/// The persist therefore owns the shard guard, so a second obligation for the same
/// resource cannot even begin until the first write has resolved.
#[tokio::test]
async fn cancelled_inflight_keyed_write_keeps_the_shard_until_it_resolves() {
    let stream = SmSessionId::new("keyed-inflight-cancel");
    let storage = Arc::new(GatedSnapshotPersistence::new(stream.as_str()));
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .unwrap();
    let jid = make_test_jid();
    let first_key = obligation(&jid);
    let mut second_key = obligation(&jid);
    second_key.semantic_identity_hash = [99; 32];
    let seeded = storage.inner.list_unacked(&stream).await.unwrap().len();

    // Gate the first write BEFORE it commits, so cancellation lands mid-flight.
    storage.armed.store(true, Ordering::SeqCst);
    let first = tokio::spawn({
        let registry = registry.clone();
        let jid = jid.clone();
        let key = first_key.clone();
        async move {
            registry
                .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), storage.reached.notified())
        .await
        .expect("first write reaches the pre-commit gate");
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    // A second, DISTINCT obligation for the same resource must block: the
    // cancelled write still owns the shard because its commit is unresolved.
    storage.armed.store(false, Ordering::SeqCst);
    let second = tokio::spawn({
        let registry = registry.clone();
        let jid = jid.clone();
        let key = second_key.clone();
        async move {
            registry
                .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
                .await
        }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(300), async {
            while !second.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_err(),
        "the second obligation must not proceed while the cancelled write is unresolved"
    );

    // Releasing the gate resolves the first write; the second then sees it.
    storage.proceed.notify_one();
    let second = tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("second obligation completes once the shard is free")
        .expect("second append task")
        .expect("second append succeeds");
    assert!(matches!(
        second,
        crate::stream_management::SmKeyedAppendOutcome::Appended { .. }
    ));

    // Neither allocation was lost: both proofs exist and both stanzas are queued.
    for key in [&first_key, &second_key] {
        assert!(
            storage.get_ingress_append(key).await.unwrap().is_some(),
            "ledger proof survives for every allocated obligation"
        );
    }
    let durable = storage.inner.list_unacked(&stream).await.unwrap();
    assert_eq!(
        durable.len(),
        seeded + 2,
        "both keyed appends remain queued alongside the seeded entries"
    );
    // A retry of the first obligation must find its proof AND its queue entry.
    assert!(matches!(
        registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), first_key)
            .await
            .unwrap(),
        crate::stream_management::SmKeyedAppendOutcome::AlreadyAppended { .. }
    ));
    assert_eq!(
        storage.inner.list_unacked(&stream).await.unwrap().len(),
        seeded + 2,
        "the suppressed retry neither appends nor loses the committed entry"
    );
}

/// #1756: proof for a payload the bounded queue has since evicted must not
/// suppress the retry.
///
/// The hazard: a keyed append commits, its route-progress transaction fails, and
/// later appends push the stanza out of the queue. Eviction records a replay gap
/// through that sequence, so the payload is provably gone — yet the ledger row
/// still says allocated. Returning `AlreadyAppended` there would let the caller
/// commit progress and a receipt, terminalizing a message neither resume nor
/// promotion can ever deliver.
///
/// An acknowledged entry is deliberately treated differently: it also leaves the
/// queue, but it left because it was delivered, so its proof still stands.
#[tokio::test]
async fn proof_for_an_evicted_payload_allows_a_replacement_allocation() {
    let stream = SmSessionId::new("keyed-evicted");
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .unwrap();
    let jid = make_test_jid();
    let key = obligation(&jid);

    let first = registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key.clone())
        .await
        .unwrap();
    let crate::stream_management::SmKeyedAppendOutcome::Appended { .. } = first else {
        panic!("first append allocates: {first:?}");
    };
    let allocated = storage
        .get_ingress_append(&key)
        .await
        .unwrap()
        .expect("proof recorded");

    // While the obligation is still unsettled, evict its payload the way queue
    // overflow does: drop the entry and record the replay gap through it. The
    // eviction is applied to DURABLE state, which is what the void decision
    // reads — memory can lie about deliverability in both directions.
    let mut durable = storage.get_session(&stream).await.unwrap().unwrap();
    durable.replay_gap_through = Some(allocated.sequence);
    let retained: Vec<_> = storage
        .list_unacked(&stream)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.sequence != allocated.sequence)
        .collect();
    storage
        .store_session_atomic(durable, retained)
        .await
        .unwrap();
    {
        let mut sessions = registry.sessions.write().unwrap();
        let session = sessions.get_mut(stream.as_str()).unwrap();
        session
            .unacked_stanzas
            .retain(|entry| entry.sequence != allocated.sequence);
        session.replay_gap_through = Some(allocated.sequence);
    }

    // The retry must allocate again rather than report the lost payload as queued.
    let replacement = registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key.clone())
        .await
        .unwrap();
    let crate::stream_management::SmKeyedAppendOutcome::Appended { .. } = replacement else {
        panic!("an evicted allocation is replaced, not suppressed: {replacement:?}");
    };
    let proof = storage
        .get_ingress_append(&key)
        .await
        .unwrap()
        .expect("replacement proof");
    assert_ne!(
        proof.sequence, allocated.sequence,
        "the ledger now points at the entry that actually exists"
    );
    assert!(
        snapshot(&registry, stream.as_str())
            .unacked_stanzas
            .iter()
            .any(|entry| entry.sequence == proof.sequence),
        "the replacement payload is queued"
    );

    // A retained allocation is still suppressed: only eviction voids proof.
    let again = registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
        .await
        .unwrap();
    assert!(matches!(
        again,
        crate::stream_management::SmKeyedAppendOutcome::AlreadyAppended { .. }
    ));
}

/// #1756 review round 2: the void decision must come from durable state.
///
/// Two ways in-memory state lies about whether an allocation is still
/// deliverable, both of which would have let a lost stanza be reported as queued:
///
/// 1. another append evicted the sequence, committed, and was cancelled before
///    publishing — memory still shows the entry while storage records the gap;
/// 2. a same-JID replacement moved the old stream off both maps into promotion
///    ownership while its durable row still exists — a missing map entry is not
///    evidence that delivery completed.
#[tokio::test]
async fn void_allocation_reads_durable_state_not_memory() {
    for off_map in [false, true] {
        let stream = SmSessionId::new(if off_map {
            "keyed-void-offmap"
        } else {
            "keyed-void-stale"
        });
        let storage = Arc::new(InMemorySmPersistence::new());
        let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
        registry
            .store_session(realistic_test_session(stream.as_str()))
            .await
            .unwrap();
        let jid = make_test_jid();
        let key = obligation(&jid);
        registry
            .record_keyed_stanza_for_detached_bound_resource(
                &jid,
                &stanza(),
                Utc::now(),
                key.clone(),
            )
            .await
            .unwrap();
        let allocated = storage.get_ingress_append(&key).await.unwrap().unwrap();

        // Evict the payload in DURABLE state only, leaving memory untouched.
        let mut durable = storage.get_session(&stream).await.unwrap().unwrap();
        durable.replay_gap_through = Some(allocated.sequence);
        let retained: Vec<_> = storage
            .list_unacked(&stream)
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.sequence != allocated.sequence)
            .collect();
        storage
            .store_session_atomic(durable, retained)
            .await
            .unwrap();
        // Memory still shows the entry the durable gap has lost.
        assert!(snapshot(&registry, stream.as_str())
            .unacked_stanzas
            .iter()
            .any(|entry| entry.sequence == allocated.sequence));
        if off_map {
            // Model promotion ownership: off both maps, durable row still present.
            registry.sessions.write().unwrap().remove(stream.as_str());
        }

        let outcome = registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
            .await
            .unwrap();
        if off_map {
            // Nothing local can accept the replacement, so the obligation stays
            // unresolved for its recorded route rather than being reported queued.
            assert!(
                !outcome.is_allocated(),
                "an evicted payload on a promotion-owned stream is not discharged: {outcome:?}"
            );
        } else {
            assert!(
                matches!(
                    outcome,
                    crate::stream_management::SmKeyedAppendOutcome::Appended { .. }
                ),
                "durable eviction voids the proof despite stale memory: {outcome:?}"
            );
        }
    }
}

/// #1756 review round 3: deleting a session must retire the proofs its replay
/// gap covers.
///
/// The gap is the only evidence separating an evicted allocation from a
/// delivered one. If the session is deleted — by expiry promotion, displacement
/// or resume — while an evicted allocation's proof survives, a later retry reads
/// "no durable session" as a discharged obligation and commits resource progress
/// for a stanza that was never promoted or replayed.
///
/// A *retained* allocation's proof must survive the same deletion: that one was
/// handed to promotion or replayed on resume, so it really was discharged.
#[tokio::test]
async fn deleting_a_session_retires_only_gap_covered_proofs() {
    let stream = SmSessionId::new("keyed-delete-voids");
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .unwrap();
    let jid = make_test_jid();
    let evicted_key = obligation(&jid);
    let mut retained_key = obligation(&jid);
    retained_key.semantic_identity_hash = [77; 32];

    for key in [evicted_key.clone(), retained_key.clone()] {
        registry
            .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
            .await
            .unwrap();
    }
    let evicted = storage
        .get_ingress_append(&evicted_key)
        .await
        .unwrap()
        .unwrap();
    let retained = storage
        .get_ingress_append(&retained_key)
        .await
        .unwrap()
        .unwrap();
    assert!(retained.sequence != evicted.sequence);

    // Evict the first allocation durably, then delete the session the way
    // expiry promotion or resume does.
    let mut durable = storage.get_session(&stream).await.unwrap().unwrap();
    durable.replay_gap_through = Some(evicted.sequence);
    let queue: Vec<_> = storage
        .list_unacked(&stream)
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.sequence != evicted.sequence)
        .collect();
    storage.store_session_atomic(durable, queue).await.unwrap();
    storage.delete_session(&stream).await.unwrap();

    assert!(
        storage
            .get_ingress_append(&evicted_key)
            .await
            .unwrap()
            .is_none(),
        "the evicted allocation's proof is retired so a retry can allocate again"
    );
    assert!(
        storage
            .get_ingress_append(&retained_key)
            .await
            .unwrap()
            .is_some(),
        "a delivered allocation's proof must still suppress its retry"
    );
}

/// #1756: an acknowledged allocation must never be treated as evicted.
///
/// Progress can fail after an append; the client then resumes and acknowledges
/// the stanza, which removes its queue entry while the ledger row stands. If
/// that stream detaches again and a later overflow advances the replay gap past
/// the old sequence, gap coverage alone would misread the acknowledged
/// allocation as lost and append a duplicate of a delivered stanza.
#[tokio::test]
async fn acknowledged_allocation_is_never_voided_by_a_later_gap() {
    let stream = SmSessionId::new("keyed-acked-gap");
    let storage = Arc::new(InMemorySmPersistence::new());
    let registry = Arc::new(InMemorySmSessionRegistry::new().with_persistence(storage.clone()));
    registry
        .store_session(realistic_test_session(stream.as_str()))
        .await
        .expect("seed session");
    let jid = make_test_jid();
    let key = obligation(&jid);
    registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key.clone())
        .await
        .expect("first allocation");
    let allocated = storage
        .get_ingress_append(&key)
        .await
        .expect("ledger read")
        .expect("proof recorded");

    // The client acknowledged through this sequence, so its entry left the queue
    // because it was delivered. A later overflow then advances the gap past it.
    let mut durable = storage
        .get_session(&stream)
        .await
        .expect("durable read")
        .expect("durable session");
    durable.last_acked = allocated.sequence;
    durable.replay_gap_through = Some(allocated.sequence.wrapping_add(3));
    let queue: Vec<_> = storage
        .list_unacked(&stream)
        .await
        .expect("queue read")
        .into_iter()
        .filter(|entry| entry.sequence != allocated.sequence)
        .collect();
    storage
        .store_session_atomic(durable, queue)
        .await
        .expect("apply ack and later gap");

    let outcome = registry
        .record_keyed_stanza_for_detached_bound_resource(&jid, &stanza(), Utc::now(), key)
        .await
        .expect("retry");
    assert!(
        matches!(
            outcome,
            crate::stream_management::SmKeyedAppendOutcome::AlreadyAppended { .. }
        ),
        "an acknowledged allocation stays discharged: {outcome:?}"
    );
}
