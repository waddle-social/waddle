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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage);
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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage);
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
    let registry =
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new()));
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
                    appended_at: Utc::now()
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
    let registry =
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new()));
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
    assert_eq!(snapshot(&registry, stream.as_str()).outbound_count, 7);
    assert_eq!(storage.inner.list_unacked(stream).await.unwrap().len(), 3);
    append.abort();
    assert!(append.await.unwrap_err().is_cancelled());
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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
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
    let registry = InMemorySmSessionRegistry::new().with_persistence(storage.clone());
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
                appended_at: Utc::now(),
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
