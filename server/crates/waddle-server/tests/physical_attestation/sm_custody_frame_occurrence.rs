//! PG17 checks for the unselected occurrence journal. No transport is selected.

use std::time::Duration;

use sqlx::{Connection, PgConnection};
use uuid::Uuid;
use waddle_server::sm_custody::{
    CustodyError, CustodyLimits, CustodyRecoveryAuthority, SessionGeneration, SmCustodyRepository,
};
use waddle_server::sm_frame_occurrence::{FrameCapture, FrameKnowledge, RetainedFrameOccurrence};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};
use waddle_xmpp::stream_management::persistence::SmReplayPayload;
use waddle_xmpp::stream_management::SmFrameProvenance;

use super::super::super::super::wait_for_blocked_application;
use super::{uow, Candidate, CATALOG_GRANT_LOCK};

fn frame(c: &Candidate, id: Uuid, wire: Option<String>) -> RetainedFrameOccurrence {
    let payload = wire.map_or_else(
        || {
            SmReplayPayload::from(Box::new(waddle_xmpp::Stanza::Message(
                c.frame.payload.clone(),
            )))
        },
        |wire| SmReplayPayload::from_wire(wire).expect("exact stanza frame"),
    );
    RetainedFrameOccurrence::retain(
        FrameCapture::from(&c.capture),
        c.frame.operation_id,
        id,
        payload,
        c.frame.received_at,
        SmFrameProvenance::ingress([c.frame.key.clone()]),
    )
}

fn position(frame: &RetainedFrameOccurrence) -> (u64, u32) {
    let FrameKnowledge::Committed(position) = frame.knowledge() else {
        panic!("frame has no committed position")
    };
    (position.ordinal(), position.h())
}

#[tokio::test]
async fn postgres_frame_occurrences_keep_distinct_ids_exact_bytes_and_refusals() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo
        .admit(&c.frame, &c.lease)
        .await
        .expect("source admission");
    let id = Uuid::new_v4();
    let mut first = frame(&c, id, None);
    let original = first.payload().wire().to_owned();
    let variant = original.replacen("<message", "<message ", 1);
    assert_ne!(original, variant);
    let mut second = frame(&c, Uuid::new_v4(), Some(variant.clone()));
    let mut third = frame(&c, Uuid::new_v4(), Some(original.clone()));
    for (index, item) in [&mut first, &mut second, &mut third]
        .into_iter()
        .enumerate()
    {
        repo.persist_frame_occurrence(&admission, item)
            .await
            .expect("frame commit");
        assert_eq!(position(item), ((index + 1) as u64, (index + 1) as u32));
    }
    let q = &c.fixture.target;
    let rows: Vec<(Uuid, Uuid, i64, Vec<u8>, Vec<u8>)> = sqlx::query_as(&format!(
        "SELECT occurrence_id,operation_id,ordinal,payload,provenance FROM {q}.sm_custody_frame_occurrences ORDER BY ordinal"
    ))
    .fetch_all(&c.fixture.admin).await.expect("independent frame readback");
    assert_eq!(rows.len(), 3);
    for (index, (item, expected_wire)) in [
        (&first, &original),
        (&second, &variant),
        (&third, &original),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(rows[index].0, item.occurrence_id());
        assert_eq!(rows[index].1, c.frame.operation_id);
        assert_eq!(rows[index].2, (index + 1) as i64);
        assert_eq!(rows[index].3, expected_wire.as_bytes());
        assert_eq!(rows[index].4, item.provenance().encode());
    }
    let mut exact_retry = frame(&c, id, Some(original.clone()));
    repo.persist_frame_occurrence(&admission, &mut exact_retry)
        .await
        .expect("same UUID and bytes retry");
    assert_eq!(position(&exact_retry), (1, 1));
    let mut conflict = frame(&c, id, Some(variant.clone()));
    assert!(matches!(
        repo.persist_frame_occurrence(&admission, &mut conflict)
            .await,
        Err(CustodyError::Conflict)
    ));
    assert_eq!(conflict.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(conflict.payload().wire(), variant);

    let mut missing = frame(&c, Uuid::new_v4(), None);
    assert!(matches!(
        repo.reconcile_frame_occurrence(&admission, &mut missing)
            .await,
        Err(CustodyError::CommitUnknown)
    ));
    assert_eq!(missing.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(missing.payload().wire(), original);
    let mut unproven = RetainedFrameOccurrence::retain(
        FrameCapture::from(&c.capture),
        c.frame.operation_id,
        Uuid::new_v4(),
        first.payload().clone(),
        c.frame.received_at,
        SmFrameProvenance::Unknown,
    );
    assert!(matches!(
        repo.persist_frame_occurrence(&admission, &mut unproven)
            .await,
        Err(CustodyError::Rejected)
    ));
    assert_eq!(unproven.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(unproven.payload().wire(), original);
    let mut wrong_source = c.frame.key.clone();
    wrong_source.semantic_identity_hash[0] ^= 0x80;
    let mut forged = RetainedFrameOccurrence::retain(
        FrameCapture::from(&c.capture),
        c.frame.operation_id,
        Uuid::new_v4(),
        first.payload().clone(),
        c.frame.received_at,
        SmFrameProvenance::ingress([wrong_source]),
    );
    assert!(matches!(
        repo.persist_frame_occurrence(&admission, &mut forged).await,
        Err(CustodyError::Rejected)
    ));
    assert_eq!(forged.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(forged.payload().wire(), original);

    let limited = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(3, 64 * 1024).expect("quota"),
    )
    .expect("repository");
    let mut over_quota = frame(&c, Uuid::new_v4(), None);
    assert!(matches!(
        limited
            .persist_frame_occurrence(&admission, &mut over_quota)
            .await,
        Err(CustodyError::Capacity)
    ));
    assert_eq!(over_quota.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(over_quota.payload().wire(), original);

    let mut stale = c.capture.clone();
    stale.generation = SessionGeneration::new(stale.generation.get() + 1).expect("generation");
    let stale_repo = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        stale.clone(),
        CustodyLimits::new(16, 64 * 1024).expect("limits"),
    )
    .expect("repository");
    let mut stale_frame = RetainedFrameOccurrence::retain(
        FrameCapture::from(&stale),
        c.frame.operation_id,
        Uuid::new_v4(),
        first.payload().clone(),
        c.frame.received_at,
        first.provenance().clone(),
    );
    assert!(stale_repo
        .persist_frame_occurrence(&admission, &mut stale_frame)
        .await
        .is_err());
    assert_eq!(stale_frame.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(stale_frame.payload().wire(), original);

    assert!(
        sqlx::query(&format!(
            "INSERT INTO {q}.sm_custody_terminals (operation_id,disposition) VALUES ($1,1)"
        ))
        .bind(c.frame.operation_id)
        .execute(&c.fixture.admin)
        .await
        .is_err(),
        "old obligation-only settlement cannot discard the frame"
    );
    assert!(
        repo.bind(&admission).await.is_err(),
        "an old obligation slot cannot be bound after a frame occurrence"
    );
    let counters: (i64, i64, i64) = sqlx::query_as(&format!(
        "SELECT appended,exposed,peer_acked FROM {q}.sm_custody_gates WHERE gate_id=$1"
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&c.fixture.admin)
    .await
    .expect("existing counter");
    assert_eq!(
        counters,
        (0, 0, 0),
        "candidate does not select or count live SM"
    );
    let mut historical = frame(&c, id, Some(original));
    historical.mark_unknown();
    c.settle_executor().await;
    repo.reconcile_frame_occurrence(&admission, &mut historical)
        .await
        .expect("a later source receipt does not erase historical frame evidence");
    assert_eq!(position(&historical), (1, 1));
    repo.persist_frame_occurrence(&admission, &mut historical)
        .await
        .expect("an exact retry remains idempotent after the source receipt");
    let mut after_receipt = frame(&c, Uuid::new_v4(), None);
    assert!(matches!(
        repo.persist_frame_occurrence(&admission, &mut after_receipt)
            .await,
        Err(CustodyError::Rejected)
    ));
    assert_eq!(after_receipt.knowledge(), FrameKnowledge::Unknown);
    drop(stale_repo);
    drop(limited);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrences_serialize_concurrent_ids_and_exact_retries() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admission");
    let mut writers = Vec::new();
    for name in ["sm-frame-overlap-a", "sm-frame-overlap-b"] {
        let named = uow(
            &c.fixture.runtime_options.clone().application_name(name),
            &c.config,
            c.fixture.lineage.deployment_uuid.0,
            &c.capture.owner,
        )
        .await;
        let writer = SmCustodyRepository::new(
            named,
            &c.config,
            c.capture.clone(),
            CustodyLimits::new(16, 64 * 1024).expect("limits"),
        )
        .expect("writer");
        writers.push((writer, frame(&c, Uuid::new_v4(), None)));
    }
    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("gate blocker");
    sqlx::query("BEGIN")
        .execute(&mut blocker)
        .await
        .expect("block transaction");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.sm_custody_gates WHERE gate_id=$1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .fetch_one(&mut blocker)
    .await
    .expect("hold gate");
    let mut tasks = Vec::new();
    for (writer, mut item) in writers {
        let admission = admission.clone();
        tasks.push(tokio::spawn(async move {
            let result = writer.persist_frame_occurrence(&admission, &mut item).await;
            (result, item)
        }));
    }
    for name in ["sm-frame-overlap-a", "sm-frame-overlap-b"] {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock')")
                    .bind(name).fetch_one(&c.fixture.admin).await.expect("observe writer");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("both frame writers overlap behind locks");
    }
    sqlx::query("COMMIT")
        .execute(&mut blocker)
        .await
        .expect("release gate");
    let mut committed = Vec::new();
    for task in tasks {
        let (result, item) = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("bounded frame write")
            .expect("task");
        result.expect("concurrent occurrence");
        committed.push(item);
    }
    let (first, second) = (&committed[0], &committed[1]);
    let mut positions = [position(first).0, position(second).0];
    positions.sort_unstable();
    assert_eq!(positions, [1, 2]);
    let id = Uuid::new_v4();
    let mut retry_a = frame(&c, id, None);
    let mut retry_b = frame(&c, id, None);
    let (a, b) = tokio::join!(
        repo.persist_frame_occurrence(&admission, &mut retry_a),
        repo.persist_frame_occurrence(&admission, &mut retry_b),
    );
    a.expect("first identical retry");
    b.expect("second identical retry");
    assert_eq!(position(&retry_a), (3, 3));
    assert_eq!(position(&retry_b), (3, 3));
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_custody_frame_occurrences",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("unique occurrence count");
    assert_eq!(count, 3);
    drop(blocker);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrence_quota_counts_exact_utf8_bytes() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let (source, lease) = super::multiframe::source(&c, "héllø").await;
    let admission = repo.admit(&source, &lease).await.expect("source admission");
    let payload = SmReplayPayload::from(Box::new(waddle_xmpp::Stanza::Message(
        source.payload.clone(),
    )));
    let original = payload.wire().to_owned();
    assert!(
        original.len() > original.chars().count(),
        "UTF-8 bytes exceed character count"
    );
    let variant = original.replacen("<message", "<message ", 1);
    let make = |id, wire| {
        RetainedFrameOccurrence::retain(
            FrameCapture::from(&c.capture),
            source.operation_id,
            id,
            SmReplayPayload::from_wire(wire).expect("message"),
            source.received_at,
            SmFrameProvenance::ingress([source.key.clone()]),
        )
    };
    let mut first = make(Uuid::new_v4(), original.clone());
    let mut second = make(Uuid::new_v4(), variant.clone());
    let total = (original.len() + variant.len()) as u64;
    let short = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(4, total - 1).expect("one byte short"),
    )
    .expect("limited repository");
    short
        .persist_frame_occurrence(&admission, &mut first)
        .await
        .expect("first frame");
    assert!(matches!(
        short
            .persist_frame_occurrence(&admission, &mut second)
            .await,
        Err(CustodyError::Capacity)
    ));
    assert_eq!(second.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(second.payload().wire(), variant);
    let exact = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(4, total).expect("exact byte limit"),
    )
    .expect("exact repository");
    exact
        .persist_frame_occurrence(&admission, &mut second)
        .await
        .expect("exact byte boundary");
    assert_eq!(position(&second), (2, 2));
    let bytes: i64 = sqlx::query_scalar(&format!(
        "SELECT sum(octet_length(payload))::bigint FROM {}.sm_custody_frame_occurrences",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("retained bytes");
    assert_eq!(bytes, total as i64);
    drop(exact);
    drop(short);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrence_terminal_races_fence_stale_snapshots_and_gc() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for isolation in ["READ COMMITTED", "REPEATABLE READ"] {
        for occurrence_wins in [false, true] {
            let c = Candidate::new().await;
            let repo = c.repository();
            let admission = repo.admit(&c.frame, &c.lease).await.expect("admission");
            let mut loser = PgConnection::connect_with(&c.fixture.runtime_options)
                .await
                .expect("independent candidate writer");
            sqlx::query(&format!("BEGIN ISOLATION LEVEL {isolation}"))
                .execute(&mut loser)
                .await
                .expect("transaction");
            sqlx::query("SELECT 1 FROM pg_catalog.pg_class LIMIT 1")
                .fetch_one(&mut loser)
                .await
                .expect("pin snapshot");
            let q = &c.fixture.target;
            let mut retained = frame(&c, Uuid::new_v4(), None);
            let error = if occurrence_wins {
                repo.persist_frame_occurrence(&admission, &mut retained)
                    .await
                    .expect("frame winner");
                sqlx::query(&format!(
                    "INSERT INTO {q}.sm_custody_terminals (operation_id,disposition) VALUES ($1,1)"
                ))
                .bind(c.frame.operation_id)
                .execute(&mut loser)
                .await
                .expect_err("terminal must lose")
            } else {
                sqlx::query(&format!(
                    "INSERT INTO {q}.sm_custody_terminals (operation_id,disposition) VALUES ($1,1)"
                ))
                .bind(c.frame.operation_id)
                .execute(&c.fixture.admin)
                .await
                .expect("terminal winner");
                let initial = sqlx::query(&format!(
                    "INSERT INTO {q}.sm_custody_frame_frontiers (gate_id,appended) VALUES ($1,1)"
                ))
                .bind(c.capture.gate_id)
                .execute(&mut loser)
                .await;
                let result = match initial {
                    Ok(_) => sqlx::query(&format!("INSERT INTO {q}.sm_custody_frame_occurrences (occurrence_id,operation_id,gate_id,ordinal,payload,provenance,received_at) VALUES ($1,$2,$3,1,$4,$5,$6)"))
                        .bind(retained.occurrence_id()).bind(c.frame.operation_id).bind(c.capture.gate_id)
                        .bind(retained.payload().wire().as_bytes()).bind(retained.provenance().encode()).bind(retained.received_at())
                        .execute(&mut loser).await,
                    Err(error) => Err(error),
                };
                result.expect_err("frame must lose")
            };
            let error = error.as_database_error().expect("database rejection");
            if isolation == "REPEATABLE READ" {
                assert_eq!(error.code().as_deref(), Some("40001"));
            } else {
                assert_eq!(error.code().as_deref(), Some("P0001"));
                let expected = if occurrence_wins {
                    "SM frame occurrence remains unsettled"
                } else {
                    "SM frame gate, obligation or ordinal is invalid"
                };
                assert!(error.message().contains(expected), "{}", error.message());
            }
            sqlx::query("ROLLBACK")
                .execute(&mut loser)
                .await
                .expect("rollback losing transaction");
            let counts: (i64, i64) = sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {q}.sm_custody_terminals),(SELECT count(*) FROM {q}.sm_custody_frame_occurrences)"))
                .fetch_one(&c.fixture.admin).await.expect("winner evidence");
            assert_eq!(counts, if occurrence_wins { (0, 1) } else { (1, 0) });
            if occurrence_wins {
                let error = sqlx::query(&format!(
                    "DELETE FROM {q}.ingress_messages WHERE message_key=$1"
                ))
                .bind(c.frame.key.message_key.to_storage())
                .execute(&c.fixture.admin)
                .await
                .expect_err("unsettled occurrence must prevent parent deletion");
                let error = error.as_database_error().expect("database rejection");
                assert_eq!(error.code().as_deref(), Some("P0001"));
                assert!(error
                    .message()
                    .contains("pending SM custody prevents canonical deletion"));
                let parent: i64 = sqlx::query_scalar(&format!(
                    "SELECT count(*) FROM {q}.ingress_messages WHERE message_key=$1"
                ))
                .bind(c.frame.key.message_key.to_storage())
                .fetch_one(&c.fixture.admin)
                .await
                .expect("parent retained");
                assert_eq!(parent, 1);
            }
            drop(loser);
            drop(repo);
            c.fixture.cleanup().await;
        }
    }
}

#[tokio::test]
async fn postgres_frame_occurrence_refuses_an_existing_obligation_slot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admission");
    repo.bind(&admission).await.expect("old obligation slot");
    let mut retained = frame(&c, Uuid::new_v4(), None);
    let expected_wire = retained.payload().wire().to_owned();
    assert!(repo
        .persist_frame_occurrence(&admission, &mut retained)
        .await
        .is_err());
    assert_eq!(retained.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(retained.payload().wire(), expected_wire);
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_custody_frame_occurrences",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("no occurrence committed");
    assert_eq!(count, 0);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrence_successor_can_reconcile_after_principal_and_claim_loss() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admission");
    let id = Uuid::new_v4();
    let mut committed = frame(&c, id, None);
    repo.persist_frame_occurrence(&admission, &mut committed)
        .await
        .expect("committed occurrence");
    let mut uncertain = frame(&c, id, Some(committed.payload().wire().to_owned()));
    uncertain.mark_unknown();
    let q = &c.fixture.target;
    let owner = NodeIdentity::new("frame-successor", "incarnation");
    sqlx::query(&format!(
        "UPDATE {q}.clustering_claims SET node_id=$1,node_epoch=$2,claim_epoch=42"
    ))
    .bind(&owner.node_id)
    .bind(&owner.node_epoch)
    .execute(&c.fixture.admin)
    .await
    .expect("claim turnover");
    sqlx::query(&format!("DELETE FROM {q}.sessions"))
        .execute(&c.fixture.admin)
        .await
        .expect("principal retired");
    assert!(repo
        .reconcile_frame_occurrence(&admission, &mut uncertain)
        .await
        .is_err());
    assert_eq!(uncertain.knowledge(), FrameKnowledge::Unknown);
    let authority = CustodyRecoveryAuthority {
        owner: owner.clone(),
        claim: ClaimEpoch(42),
    };
    let successor = SmCustodyRepository::recover_session(
        uow(
            &c.fixture.runtime_options,
            &c.config,
            c.fixture.lineage.deployment_uuid.0,
            &owner,
        )
        .await,
        &c.config,
        &c.capture.session,
        &c.capture.resource,
        &authority,
        CustodyLimits::new(16, 64 * 1024).expect("limits"),
    )
    .await
    .expect("successor discovers captured gate");
    let sealed = successor
        .seal_for_recovery(&authority)
        .await
        .expect("successor seals old gate");
    let inventory = successor
        .inventory(&sealed, &authority, None)
        .await
        .expect("obligation inventory");
    let recovered_admission = inventory.operations[0]
        .admission(&sealed)
        .expect("original admission");
    successor
        .reconcile_frame_occurrence_recovered(
            &sealed,
            &authority,
            &recovered_admission,
            &mut uncertain,
        )
        .await
        .expect("exact historical frame readback");
    assert_eq!(position(&uncertain), (1, 1));
    assert_eq!(uncertain.payload().wire(), committed.payload().wire());
    let mut absent = frame(&c, Uuid::new_v4(), None);
    assert!(matches!(
        successor
            .reconcile_frame_occurrence_recovered(
                &sealed,
                &authority,
                &recovered_admission,
                &mut absent
            )
            .await,
        Err(CustodyError::CommitUnknown)
    ));
    assert_eq!(absent.knowledge(), FrameKnowledge::Unknown);
    drop(successor);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrence_ordinal_wraps_h_without_wrapping_absolute_position() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let q = &c.fixture.target;
    sqlx::query(&format!("UPDATE {q}.sm_custody_gates SET appended=4294967294,exposed=4294967294,peer_acked=4294967294 WHERE gate_id=$1"))
        .bind(c.capture.gate_id).execute(&c.fixture.admin).await.expect("wrap baseline");
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admission");
    for expected in [(4294967295, u32::MAX), (4294967296, 0), (4294967297, 1)] {
        let mut occurrence = frame(&c, Uuid::new_v4(), None);
        repo.persist_frame_occurrence(&admission, &mut occurrence)
            .await
            .expect("ordered commit");
        assert_eq!(position(&occurrence), expected);
    }
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_frame_occurrence_cancelled_commit_retains_and_reconciles_exact_bytes() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo
        .admit(&c.frame, &c.lease)
        .await
        .expect("source admission");
    let named = uow(
        &c.fixture
            .runtime_options
            .clone()
            .application_name("sm-frame-commit"),
        &c.config,
        c.fixture.lineage.deployment_uuid.0,
        &c.capture.owner,
    )
    .await;
    let writer = SmCustodyRepository::new(
        named,
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(16, 64 * 1024).expect("limits"),
    )
    .expect("writer");
    let mut blocker = PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("blocker");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut blocker)
        .await
        .expect("blocker PID");
    sqlx::query("SELECT pg_advisory_lock(2246001)")
        .execute(&mut blocker)
        .await
        .expect("commit barrier");
    let q = &c.fixture.target;
    sqlx::raw_sql(&format!("CREATE FUNCTION {q}.pause_frame_commit() RETURNS trigger LANGUAGE plpgsql SET lock_timeout='3s' AS $$ BEGIN PERFORM pg_catalog.pg_advisory_xact_lock(2246001); RETURN NEW; END $$; CREATE CONSTRAINT TRIGGER pause_frame_commit AFTER INSERT ON {q}.sm_custody_frame_occurrences DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION {q}.pause_frame_commit();"))
        .execute(&c.fixture.admin).await.expect("deferred commit barrier");
    let mut retained = frame(&c, Uuid::new_v4(), None);
    let expected_id = retained.occurrence_id();
    let expected_wire = retained.payload().wire().to_owned();
    let mut attempt = Box::pin(writer.persist_frame_occurrence(&admission, &mut retained));
    let pid = tokio::select! {
        result = &mut attempt => panic!("write ended before barrier: {result:?}"),
        pid = wait_for_blocked_application(&c.fixture.admin, "sm-frame-commit", blocker_pid) => pid,
    };
    let in_commit: bool =
        sqlx::query_scalar("SELECT query='COMMIT' FROM pg_stat_activity WHERE pid=$1")
            .bind(pid)
            .fetch_one(&c.fixture.admin)
            .await
            .expect("inside COMMIT");
    assert!(in_commit);
    drop(attempt);
    assert_eq!(retained.knowledge(), FrameKnowledge::Unknown);
    let visible: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {q}.sm_custody_frame_occurrences WHERE occurrence_id=$1"
    ))
    .bind(expected_id)
    .fetch_one(&c.fixture.admin)
    .await
    .expect("uncommitted observation");
    assert_eq!(
        visible, 0,
        "absence while COMMIT is pending proves no rollback"
    );
    assert_eq!(retained.occurrence_id(), expected_id);
    assert_eq!(retained.payload().wire(), expected_wire);
    sqlx::query("SELECT pg_advisory_unlock(2246001)")
        .execute(&mut blocker)
        .await
        .expect("release COMMIT");
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let visible: i64 = sqlx::query_scalar(&format!(
                "SELECT count(*) FROM {q}.sm_custody_frame_occurrences WHERE occurrence_id=$1"
            ))
            .bind(expected_id)
            .fetch_one(&c.fixture.admin)
            .await
            .expect("commit visibility");
            if visible == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled COMMIT completes on the server");
    sqlx::query(&format!(
        "REVOKE SELECT ON {q}.sm_custody_frame_occurrences FROM {}",
        c.fixture.runtime
    ))
    .execute(&c.fixture.admin)
    .await
    .expect("simulate unavailable readback");
    assert!(repo
        .reconcile_frame_occurrence(&admission, &mut retained)
        .await
        .is_err());
    assert_eq!(retained.knowledge(), FrameKnowledge::Unknown);
    assert_eq!(retained.payload().wire(), expected_wire);
    sqlx::query(&format!(
        "GRANT SELECT ON {q}.sm_custody_frame_occurrences TO {}",
        c.fixture.runtime
    ))
    .execute(&c.fixture.admin)
    .await
    .expect("restore readback");
    repo.reconcile_frame_occurrence(&admission, &mut retained)
        .await
        .expect("exact positive readback");
    assert_eq!(position(&retained), (1, 1));
    assert_eq!(retained.occurrence_id(), expected_id);
    assert_eq!(retained.payload().wire(), expected_wire);
    drop(writer);
    drop(repo);
    drop(blocker);
    c.fixture.cleanup().await;
}
