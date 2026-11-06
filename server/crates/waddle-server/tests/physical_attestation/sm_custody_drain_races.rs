//! Characterize already-counted legacy frames racing a source handoff.
//!
//! These tests document existing writers, not an interlock between them and the
//! unselected candidate. The session-row lock pauses the actual SQL writer;
//! the source handoff and completion commit before that writer can continue.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use waddle_server::clustering::claims::PostgresClaimStore;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::ingress_substrate::{EffectReceiptKind, TerminalizeOutcome};
use waddle_server::ingress_uow::{CanonicalMessageRepository, EffectReceiptRepository};
use waddle_server::sm_custody::CandidateSmMemory;
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_server::sm_persistence_fenced::PostgresFencedSmPersistence;
use waddle_xmpp::ownership::{ClaimStore, Entity, EntityType, SharedNodeIdentity};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, KeyedSnapshotOutcome, PersistedDetachedAppend,
    PersistedIngressAppend, PersistedSession, PersistedUnackedStanza, SmClaimFence,
    SmPersistenceStorage,
};
use waddle_xmpp::stream_management::{SmFrameProvenance, SmIngressFrameReceipt};
use waddle_xmpp::Stanza;

use super::{now, url_with_schema, wait_for_blocked_application, Candidate, CATALOG_GRANT_LOCK};

#[derive(Clone, Copy, Debug)]
enum Writer {
    Portable,
    Fenced,
}

#[derive(Clone, Copy, Debug)]
enum Drain {
    Snapshot,
    Incremental,
}

fn earlier_frame(stream: &SmSessionId, sequence: u32) -> PersistedUnackedStanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message
        .bodies
        .insert(Default::default(), format!("earlier frame {sequence} — λ"));
    PersistedUnackedStanza {
        stream_id: stream.clone(),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: now() - chrono::Duration::seconds(i64::from(13 - sequence)),
        ingress_receipts: Vec::new(),
        provenance: if sequence == 11 {
            SmFrameProvenance::Unknown
        } else {
            SmFrameProvenance::NonIngress
        },
    }
}

async fn characterize(writer: Writer, drain: Drain) {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    // The shared retention fixture seeds an old terminal source. Reopen it
    // before the race so this test observes a new completion transaction.
    let mut setup = c.uow.begin().await.expect("source setup transaction");
    assert!(
        CanonicalMessageRepository::is_terminal(&mut setup, c.frame.key.message_key)
            .await
            .expect("fixture terminal state")
    );
    CanonicalMessageRepository::clear_terminal(&mut setup, c.frame.key.message_key)
        .await
        .expect("reopen source");
    setup.commit().await.expect("commit reopened source");
    let application = format!("sm_drain_{}", uuid::Uuid::new_v4().simple());
    let mut url = url::Url::parse(&url_with_schema(&c.fixture.base_url, &c.fixture.target))
        .expect("fixture URL");
    url.query_pairs_mut()
        .append_pair("application_name", &application);
    let url = url.to_string();

    let mut claims = None;
    let storage: Arc<dyn SmPersistenceStorage> = match writer {
        Writer::Portable => Arc::new(
            DatabaseSmPersistence::open(Some(&url))
                .await
                .expect("portable SM writer"),
        ),
        Writer::Fenced => {
            let db = Database::from_config(
                "drain race fenced writer",
                &DatabaseConfig::new(DatabaseDriver::Postgres, url).with_control_plane_pool(2),
            )
            .await
            .expect("fenced DB");
            let store = Arc::new(PostgresClaimStore::new(db.clone()));
            let storage = PostgresFencedSmPersistence::open(
                db,
                store.clone(),
                SharedNodeIdentity::new(c.capture.owner.clone()),
            )
            .await
            .expect("fenced SM writer");
            claims = Some(store);
            Arc::new(storage)
        }
    };

    let stream = SmSessionId::new("already-counted-drain");
    let mut session = PersistedSession {
        stream_id: stream.clone(),
        user_id: "bob".into(),
        jid: c.capture.resource.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::from_uuid(
            uuid::Uuid::new_v4(),
        ),
        inbound_count: 4,
        outbound_count: 12,
        last_acked: 10,
        replay_gap_through: Some(9),
        max_resume_time: Some(60),
        detached_at: now(),
        max_resume_duration: Duration::from_secs(60),
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: true,
        presence_available: true,
        presence_show: None,
        presence_status: Some("still here".into()),
        presence_priority: 3,
        presence_payloads: Vec::new(),
    };
    let earlier = vec![earlier_frame(&stream, 11), earlier_frame(&stream, 12)];
    storage
        .store_session_atomic_with_principal(&c.capture.principal, session.clone(), earlier.clone())
        .await
        .expect("initial detached session");
    let baseline = storage
        .get_session(&stream)
        .await
        .expect("baseline read")
        .expect("baseline exists");
    let fence = if let Some(claims) = claims.as_ref() {
        let entity = Entity::new(EntityType::SmSession, stream.as_str());
        let claim = claims
            .current_claim(&entity)
            .await
            .expect("claim read")
            .expect("initial write acquired claim");
        assert_eq!(claim.owner, c.capture.owner);
        Some(SmClaimFence::new(claim.owner, claim.claim_epoch))
    } else {
        None
    };

    // The old writer's third frame is already counted in memory before the
    // candidate begins the competing handoff for this same exact route.
    session.outbound_count = 13;
    let late = PersistedUnackedStanza {
        stream_id: stream.clone(),
        sequence: 13,
        stanza: Box::new(Stanza::Message(c.frame.payload.clone())),
        original_receipt_at: c.frame.received_at,
        ingress_receipts: vec![SmIngressFrameReceipt {
            message_key: c.frame.key.message_key,
            kind: c.frame.key.kind,
            semantic_identity_hash: c.frame.key.semantic_identity_hash,
        }],
        provenance: SmFrameProvenance::ingress([c.frame.key.clone()]),
    };
    let proof = PersistedIngressAppend {
        key: c.frame.key.clone(),
        accepting_stream: stream.clone(),
        sequence: late.sequence,
        appended_at: now(),
        payload: *late.stanza.clone(),
        original_receipt_at: late.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    };
    let repo = c.repository();
    let admission = repo
        .admit(&c.frame, &c.lease)
        .await
        .expect("candidate admission");
    let slot = repo.bind(&admission).await.expect("candidate slot");

    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("row-lock connection");
    let mut lock = blocker.begin().await.expect("row-lock transaction");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *lock)
        .await
        .expect("blocker PID");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.sm_sessions WHERE stream_id = $1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(stream.as_str())
    .fetch_one(&mut *lock)
    .await
    .expect("lock exact legacy session");

    let writer_storage = Arc::clone(&storage);
    let writer_session = session.clone();
    let writer_late = late.clone();
    let writer_proof = proof.clone();
    let principal = c.capture.principal.clone();
    let writer_fence = fence.clone();
    let writer_task = tokio::spawn(async move {
        match drain {
            Drain::Snapshot => writer_storage
                .store_session_atomic_with_principal_and_ingress_appends(
                    &principal,
                    writer_session,
                    vec![earlier[0].clone(), earlier[1].clone(), writer_late],
                    vec![writer_proof],
                )
                .await
                .map(|withheld| {
                    assert!(withheld.is_empty(), "no allocation conflict in this case");
                }),
            Drain::Incremental => writer_storage
                .append_detached_stanza_atomic(
                    PersistedDetachedAppend {
                        session: writer_session,
                        stanza: writer_late,
                        evicted_sequences: Vec::new(),
                        ingress_append: Some(writer_proof),
                        new_delivery: false,
                    },
                    writer_fence.as_ref(),
                )
                .await
                .map(|outcome| assert_eq!(outcome, KeyedSnapshotOutcome::Committed)),
        }
    });
    wait_for_blocked_application(&c.fixture.admin, &application, blocker_pid).await;
    assert!(
        !writer_task.is_finished(),
        "writer is still waiting at the row lock"
    );

    let handoff = repo
        .resume_preaccept(c.frame.operation_id)
        .await
        .expect("source handoff commits while old writer is paused");
    let mut source = c.uow.begin().await.expect("source completion transaction");
    assert!(
        CanonicalMessageRepository::lock(&mut source, c.frame.key.message_key)
            .await
            .expect("canonical lock")
    );
    EffectReceiptRepository::record_receipt(
        &mut source,
        c.frame.key.message_key,
        EffectReceiptKind::from_storage(c.frame.key.kind.to_storage()),
        &c.frame.key.semantic_identity_hash,
    )
    .await
    .expect("record source effect completion");
    assert_eq!(
        CanonicalMessageRepository::terminalize(&mut source, c.frame.key.message_key, now())
            .await
            .expect("terminalize source"),
        TerminalizeOutcome::Terminalized
    );
    source.commit().await.expect("commit source completion");
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    repo.hydrate_preaccepted(&mut memory, &handoff)
        .await
        .expect("hydrate candidate");
    repo.expose(&memory, &slot).await.expect("expose candidate");
    repo.acknowledge(slot.h()).await.expect("ack candidate");
    let state: (i32, bool, bool, i64, i64) = sqlx::query_as(&format!(
        "SELECT a.state, m.terminal_at IS NOT NULL, m.retention_eligible_at IS NOT NULL, \
         (SELECT count(*) FROM {q}.sm_custody_handoffs WHERE operation_id = $2), \
         (SELECT count(*) FROM {q}.sm_custody_terminals WHERE operation_id = $2) \
         FROM {q}.ingress_messages m JOIN {q}.ingress_send_attempts a USING (message_key) \
         WHERE m.message_key = $1",
        q = c.fixture.target
    ))
    .bind(c.frame.key.message_key.to_storage())
    .bind(c.frame.operation_id)
    .fetch_one(&c.fixture.admin)
    .await
    .expect("completion before legacy writer");
    assert_eq!(state, (2, true, true, 1, 1));
    assert!(!writer_task.is_finished());

    // A new-delivery write checks the source completion. The delayed frame
    // follows the separate already-counted path and must retain its payload.
    let mut control = session.clone();
    control.stream_id = SmSessionId::new("new-delivery-control");
    let mut control_proof = proof.clone();
    control_proof.accepting_stream = control.stream_id.clone();
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_delivery(control.clone(), Vec::new(), control_proof)
            .await
            .expect("new delivery observes completion"),
        KeyedSnapshotOutcome::ObligationAlreadyResolved
    );
    assert!(storage
        .get_session(&control.stream_id)
        .await
        .expect("control session read")
        .is_none());

    lock.commit().await.expect("release legacy session row");
    tokio::time::timeout(Duration::from_secs(10), writer_task)
        .await
        .expect("legacy writer resolves")
        .expect("writer task joins")
        .expect("already-counted writer commits");

    let saved = storage
        .get_session(&stream)
        .await
        .expect("session read")
        .expect("session retained");
    assert_eq!(saved.outbound_count, 13);
    assert_eq!(saved.inbound_count, session.inbound_count);
    assert_eq!(saved.last_acked, session.last_acked);
    assert_eq!(saved.replay_gap_through, session.replay_gap_through);
    assert_eq!(saved.occupancy_session, session.occupancy_session);
    assert_eq!(saved.detached_at, baseline.detached_at);
    assert_eq!(saved.max_resume_duration, baseline.max_resume_duration);
    assert_eq!(saved.presence_status, session.presence_status);
    assert_eq!(
        storage
            .get_session_principal(&stream)
            .await
            .expect("principal read"),
        Some(c.capture.principal.clone())
    );
    let queue = storage.list_unacked(&stream).await.expect("replay queue");
    let expected = [earlier_frame(&stream, 11), earlier_frame(&stream, 12), late];
    assert_eq!(queue.len(), expected.len());
    for (actual, expected) in queue.iter().zip(expected.iter()) {
        assert_eq!(actual.sequence, expected.sequence);
        assert_eq!(actual.stanza.to_element(), expected.stanza.to_element());
        assert_eq!(actual.original_receipt_at, expected.original_receipt_at);
        assert_eq!(actual.ingress_receipts, expected.ingress_receipts);
        assert_eq!(actual.provenance, expected.provenance);
    }
    assert_eq!(
        storage
            .get_ingress_append(&proof.key)
            .await
            .expect("custody read"),
        Some(proof.clone())
    );
    assert_eq!(
        storage
            .list_pending_ingress_appends(16)
            .await
            .expect("recovery inventory"),
        vec![proof]
    );
    let eligible: bool = sqlx::query_scalar(&format!(
        "SELECT retention_eligible_at IS NOT NULL FROM {}.ingress_messages WHERE message_key = $1",
        c.fixture.target
    ))
    .bind(c.frame.key.message_key.to_storage())
    .fetch_one(&c.fixture.admin)
    .await
    .expect("late-writer retention state");
    assert!(
        eligible,
        "the existing writer does not clear the earlier eligibility stamp"
    );

    drop(repo);
    drop(storage);
    drop(claims);
    drop(blocker);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_portable_snapshot_preserves_delayed_counted_frames() {
    characterize(Writer::Portable, Drain::Snapshot).await;
}

#[tokio::test]
async fn postgres_portable_incremental_preserves_delayed_counted_frames() {
    characterize(Writer::Portable, Drain::Incremental).await;
}

#[tokio::test]
async fn postgres_fenced_snapshot_preserves_delayed_counted_frames() {
    characterize(Writer::Fenced, Drain::Snapshot).await;
}

#[tokio::test]
async fn postgres_fenced_incremental_preserves_delayed_counted_frames() {
    characterize(Writer::Fenced, Drain::Incremental).await;
}
