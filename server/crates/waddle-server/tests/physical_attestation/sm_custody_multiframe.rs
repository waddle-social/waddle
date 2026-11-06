//! Required PG17 coverage for the unselected multi-frame candidate.

use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_server::ingress_uow::SendLease;
use waddle_server::sm_custody::{
    CandidateHydration, CandidateSmMemory, CustodyCapture, CustodyError, CustodyLimits,
    CustodyRecoveryAuthority, CustodyResolution, CustodyTerminal, PreparedDirectFrame,
    SmCustodyRepository,
};
use waddle_xmpp::ingress::{IngressEffectIntent, StoredMessagePayload};
use waddle_xmpp::ownership::{ClaimEpoch, NodeIdentity};

use super::{direct_pin, pg_attempt, request, seed_pg_direct, uow, Candidate, CATALOG_GRANT_LOCK};

pub(super) async fn source(c: &Candidate, body: &str) -> (PreparedDirectFrame, SendLease) {
    let (mut pin, _, encoded) = direct_pin(Uuid::new_v4(), "bob@example.test/laptop");
    pin.payload
        .bodies
        .insert(Default::default(), body.to_owned());
    let mut intent =
        IngressEffectIntent::decode_v1(pin.key.kind.to_storage(), &encoded).expect("direct intent");
    let IngressEffectIntent::RouteDirect { prepared, .. } = &mut intent else {
        panic!("direct fixture");
    };
    *prepared = Some(StoredMessagePayload::try_new(pin.payload.clone()).expect("prepared"));
    pin.key.semantic_identity_hash =
        Sha256::digest(intent.semantic_key().storage_identity().as_bytes()).into();
    let encoded = intent
        .with_encoded_v1(|_, bytes| bytes.to_vec())
        .expect("encode intent");
    let envelope = serde_json::to_vec(&serde_json::json!({
        "message": waddle_xmpp::parser::message_to_string(&pin.payload).expect("serialize message"),
        "observer_request": null
    }))
    .expect("envelope");
    seed_pg_direct(&c.fixture, &pin, envelope, encoded).await;
    let mut request = request(pin);
    let lease = pg_attempt(&c.fixture, &c.config, &mut request).await;
    let frame = PreparedDirectFrame::for_lease(
        request.pin.admission_id,
        request.pin.key,
        request.pin.payload,
        request.pin.received_at,
        &lease,
    )
    .expect("frame");
    (frame, lease)
}

#[tokio::test]
async fn postgres_multiframe_concurrent_bind_hydration_exposure_and_ack_prefix() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let second = source(&c, "two").await;
    let third = source(&c, "three").await;
    let frames = [(c.frame.clone(), c.lease.clone()), second, third];
    let mut admissions = Vec::new();
    for (frame, lease) in &frames {
        admissions.push(
            repo.admit(frame, lease)
                .await
                .expect("admit distinct source"),
        );
    }
    let mut wrong_id = frames[1].0.clone();
    wrong_id.operation_id = frames[0].0.operation_id;
    assert!(matches!(
        repo.admit(&wrong_id, &frames[1].1).await,
        Err(CustodyError::Conflict)
    ));
    let mut same_source = frames[0].0.clone();
    same_source.operation_id = Uuid::new_v4();
    assert!(matches!(
        repo.admit(&same_source, &frames[0].1).await,
        Err(CustodyError::Conflict)
    ));
    assert_eq!(
        repo.admit(&frames[0].0, &frames[0].1)
            .await
            .expect("exact retry"),
        admissions[0]
    );
    let (one, two, three) = tokio::join!(
        repo.bind(&admissions[0]),
        repo.bind(&admissions[1]),
        repo.bind(&admissions[2])
    );
    let mut ordered = vec![
        (one.expect("bind"), &frames[0].0),
        (two.expect("bind"), &frames[1].0),
        (three.expect("bind"), &frames[2].0),
    ];
    ordered.sort_by_key(|(slot, _)| slot.ordinal());
    assert_eq!(
        ordered
            .iter()
            .map(|(slot, _)| slot.ordinal())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    let mut handoffs = Vec::new();
    for (_, frame) in &ordered {
        handoffs.push(
            repo.resume_preaccept(frame.operation_id)
                .await
                .expect("source handoff"),
        );
    }
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    assert_eq!(
        repo.hydrate_preaccepted(&mut memory, &handoffs[2])
            .await
            .expect("hydrate third"),
        CandidateHydration::Inserted
    );
    assert_eq!(memory.accepted_ordinal(), None);
    assert!(repo.expose(&memory, &ordered[2].0).await.is_err());
    repo.hydrate_preaccepted(&mut memory, &handoffs[0])
        .await
        .expect("hydrate first");
    assert_eq!(memory.accepted_ordinal(), Some(1));
    repo.expose(&memory, &ordered[0].0)
        .await
        .expect("expose first");
    assert!(repo.expose(&memory, &ordered[2].0).await.is_err());
    repo.hydrate_preaccepted(&mut memory, &handoffs[1])
        .await
        .expect("hydrate middle");
    assert_eq!(memory.accepted_ordinal(), Some(3));
    repo.expose(&memory, &ordered[1].0)
        .await
        .expect("expose second");
    repo.expose(&memory, &ordered[2].0)
        .await
        .expect("expose third");
    assert!(repo.acknowledge(4).await.is_err());
    repo.acknowledge(2).await.expect("partial cumulative ACK");
    repo.acknowledge(2).await.expect("repeated frontier");
    let q = &c.fixture.target;
    let frontier: (i64, bool, i64) = sqlx::query_as(&format!("SELECT peer_acked,sealed,(SELECT count(*) FROM {q}.sm_custody_terminals) FROM {q}.sm_custody_gates WHERE gate_id=$1"))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("prefix");
    assert_eq!(frontier, (2, false, 2));
    repo.acknowledge(3).await.expect("remaining ACK");
    let fourth = source(&c, "after ack").await;
    repo.admit(&fourth.0, &fourth.1)
        .await
        .expect("ACK does not seal");
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_multiframe_wrap_gap_and_transfer_conflict() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    sqlx::query(&format!("UPDATE {}.sm_custody_gates SET appended=4294967294,exposed=4294967294,peer_acked=4294967294", c.fixture.target))
        .execute(&c.fixture.admin).await.expect("pre-wrap frontier");
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    let mut slots = Vec::new();
    for (frame, lease) in [
        (c.frame.clone(), c.lease.clone()),
        source(&c, "wrap zero").await,
        source(&c, "wrap one").await,
    ] {
        let handoff = repo
            .preaccept_local_direct(&frame, &lease)
            .await
            .expect("handoff");
        let admission = repo
            .committed_admission(frame.operation_id)
            .await
            .expect("admission");
        let slot = repo.bind(&admission).await.expect("slot");
        repo.hydrate_preaccepted(&mut memory, &handoff)
            .await
            .expect("hydrate");
        repo.expose(&memory, &slot).await.expect("expose");
        slots.push((slot, admission));
    }
    assert_eq!(
        slots.iter().map(|(s, _)| s.h()).collect::<Vec<_>>(),
        vec![u32::MAX, 0, 1]
    );
    repo.acknowledge(0).await.expect("wrap partial ACK");
    repo.acknowledge(0).await.expect("repeat wrap frontier");
    assert!(repo.acknowledge(2).await.is_err());
    repo.transfer(&slots[2].1)
        .await
        .expect("seal and transfer remaining");
    assert!(
        repo.acknowledge(1).await.is_err(),
        "sealed transfer cannot be ACKed"
    );
    let values: (i64, i64, i64) = sqlx::query_as(&format!("SELECT peer_acked,(SELECT count(*) FROM {}.sm_custody_terminals WHERE disposition=1),(SELECT count(*) FROM {}.sm_custody_transfers) FROM {}.sm_custody_gates WHERE gate_id=$1",c.fixture.target,c.fixture.target,c.fixture.target))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("atomic evidence");
    assert_eq!(values, (4294967296, 2, 1));
    drop(repo);
    c.fixture.cleanup().await;

    let c = Candidate::new().await;
    let repo = c.repository();
    let first = repo.admit(&c.frame, &c.lease).await.expect("first");
    repo.bind(&first).await.expect("slot one");
    // Simulate damaged historical evidence without deleting an immutable slot.
    sqlx::query(&format!(
        "UPDATE {}.sm_custody_gates SET appended=2 WHERE gate_id=$1",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .execute(&c.fixture.admin)
    .await
    .expect("create gap");
    let (frame, lease) = source(&c, "third").await;
    let admission = repo.admit(&frame, &lease).await.expect("third");
    assert_eq!(
        repo.bind(&admission).await.expect("slot three").ordinal(),
        3
    );
    sqlx::query(&format!(
        "UPDATE {}.sm_custody_gates SET exposed=3 WHERE gate_id=$1",
        c.fixture.target
    ))
    .bind(c.capture.gate_id)
    .execute(&c.fixture.admin)
    .await
    .expect("simulate damaged frontier");
    assert!(matches!(
        repo.acknowledge(3).await,
        Err(CustodyError::Conflict)
    ));
    let values: (i64, i64) = sqlx::query_as(&format!("SELECT peer_acked,(SELECT count(*) FROM {}.sm_custody_terminals) FROM {}.sm_custody_gates WHERE gate_id=$1", c.fixture.target,c.fixture.target))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("no partial update");
    assert_eq!(values, (0, 0));
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_multiframe_retained_quota_counts_utf8_bytes_and_identity() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let first = source(&c, "ééééé").await;
    let second = source(&c, "ééééé").await;
    let third = source(&c, "ééééé").await;
    let admission = repo.admit(&first.0, &first.1).await.expect("first");
    let (bytes, chars): (i64, i64) = sqlx::query_as(&format!("SELECT octet_length(payload)::bigint,char_length(payload)::bigint FROM {}.sm_custody_operations WHERE operation_id=$1",c.fixture.target))
        .bind(first.0.operation_id).fetch_one(&c.fixture.admin).await.expect("serialized bytes");
    assert!(bytes > chars);
    let exact = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(2, (2 * bytes) as u64).expect("quota"),
    )
    .expect("repo");
    exact
        .admit(&second.0, &second.1)
        .await
        .expect("exact byte boundary");
    assert!(matches!(
        exact.admit(&third.0, &third.1).await,
        Err(CustodyError::Capacity)
    ));
    repo.resume_preaccept(first.0.operation_id)
        .await
        .expect("source marker");
    repo.transfer(&admission)
        .await
        .expect("terminal still retained");
    let retained: (i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*),sum(octet_length(payload))::bigint FROM {}.sm_custody_operations",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("retained quota");
    assert_eq!(retained, (2, 2 * bytes));
    let copy: (i64, i64) = sqlx::query_as(&format!(
        "SELECT count(*),sum(octet_length(payload))::bigint FROM {}.sm_custody_transfers",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("bounded copy");
    assert_eq!(copy, (1, bytes));
    let mut other_capture = c.capture.clone();
    other_capture.gate_id = Uuid::new_v4();
    other_capture.token = Uuid::new_v4();
    other_capture.session = waddle_xmpp::pending_delivery::SmSessionId::new("quota-other-session");
    sqlx::query(&format!("INSERT INTO {}.clustering_claims (entity,entity_type,node_id,node_epoch,claim_epoch) VALUES ($1,$2,$3,$4,$5)",c.fixture.target))
        .bind(format!("{}:{}",waddle_xmpp::ownership::EntityType::SmSession.as_db_str(),other_capture.session.as_str()))
        .bind(waddle_xmpp::ownership::EntityType::SmSession.as_db_str()).bind(&other_capture.owner.node_id)
        .bind(&other_capture.owner.node_epoch).bind(other_capture.claim.0)
        .execute(&c.fixture.admin).await.expect("other claim");
    let other = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        other_capture.clone(),
        CustodyLimits::new(2, (2 * bytes) as u64).expect("limits"),
    )
    .expect("other repo");
    other.install_captured_session().await.expect("other gate");
    assert!(
        matches!(
            other.admit(&third.0, &third.1).await,
            Err(CustodyError::Capacity)
        ),
        "terminal evidence remains charged across gates"
    );
    assert!(
        sqlx::query(&format!(
            "INSERT INTO {}.sm_custody_slots (operation_id,gate_id,ordinal) VALUES ($1,$2,1)",
            c.fixture.target
        ))
        .bind(second.0.operation_id)
        .bind(other_capture.gate_id)
        .execute(&c.fixture.admin)
        .await
        .is_err(),
        "a slot cannot pair an operation with another gate"
    );
    assert!(sqlx::query(&format!("INSERT INTO {}.sm_custody_transfers (operation_id,resource,payload,received_at) VALUES ($1,$2,'forged',$3)",c.fixture.target))
        .bind(second.0.operation_id).bind(second.0.key.resource.to_string()).bind(second.0.received_at)
        .execute(&c.fixture.admin).await.is_err(), "a copied transfer must exactly match admission");
    drop(other);
    drop(exact);
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_multiframe_sealed_multipage_successor_recovery_and_replay() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = SmCustodyRepository::new(
        c.uow.clone(),
        &c.config,
        c.capture.clone(),
        CustodyLimits::new(32, 1024 * 1024).expect("limits"),
    )
    .expect("repo");
    let mut frames = vec![(c.frame.clone(), c.lease.clone())];
    for n in 1..19 {
        frames.push(source(&c, &format!("frame {n}")).await);
    }
    let mut admissions = Vec::new();
    for (frame, lease) in &frames {
        let admission = repo.admit(frame, lease).await.expect("admit");
        repo.bind(&admission).await.expect("bind");
        repo.resume_preaccept(frame.operation_id)
            .await
            .expect("source handoff marker");
        admissions.push(admission);
    }
    let incomplete = source(&c, "bound but source not handed off").await;
    let incomplete_admission = repo
        .admit(&incomplete.0, &incomplete.1)
        .await
        .expect("incomplete admission");
    assert_eq!(
        repo.bind(&incomplete_admission)
            .await
            .expect("incomplete slot")
            .ordinal(),
        20
    );
    let unbound = source(&c, "unbound").await;
    repo.admit(&unbound.0, &unbound.1)
        .await
        .expect("unbound admission");
    repo.transfer(&admissions[1])
        .await
        .expect("partial transfer seals");
    let next_owner = NodeIdentity::new("multi-successor", "incarnation");
    sqlx::query(&format!(
        "UPDATE {}.clustering_claims SET node_id=$1,node_epoch=$2,claim_epoch=42",
        c.fixture.target
    ))
    .bind(&next_owner.node_id)
    .bind(&next_owner.node_epoch)
    .execute(&c.fixture.admin)
    .await
    .expect("claim turnover");
    sqlx::query(&format!("DELETE FROM {}.sessions", c.fixture.target))
        .execute(&c.fixture.admin)
        .await
        .expect("retire principal");
    let authority = CustodyRecoveryAuthority {
        owner: next_owner.clone(),
        claim: ClaimEpoch(42),
    };
    let successor = SmCustodyRepository::recover_session(
        uow(
            &c.fixture.runtime_options,
            &c.config,
            c.fixture.lineage.deployment_uuid.0,
            &next_owner,
        )
        .await,
        &c.config,
        &c.capture.session,
        &c.capture.resource,
        &authority,
        CustodyLimits::new(32, 1024 * 1024).expect("limits"),
    )
    .await
    .expect("successor discovery");
    let seal = successor.seal_for_recovery(&authority).await.expect("seal");
    let page1 = successor
        .inventory(&seal, &authority, None)
        .await
        .expect("page one");
    assert_eq!(page1.operations.len(), 16);
    let page2 = successor
        .inventory(&seal, &authority, page1.next)
        .await
        .expect("page two");
    assert_eq!(page2.operations.len(), 5);
    assert!(page2.next.is_none());
    let inventory: Vec<_> = page1
        .operations
        .iter()
        .chain(page2.operations.iter())
        .collect();
    for (index, (frame, _)) in frames.iter().enumerate() {
        assert_eq!(inventory[index].frame(), frame);
        assert_eq!(
            inventory[index].slot().expect("slot").ordinal(),
            index as u64 + 1
        );
    }
    assert_eq!(inventory[1].terminal(), Some(CustodyTerminal::Transfer));
    assert_eq!(inventory[19].frame(), &incomplete.0);
    assert_eq!(inventory[19].slot().expect("incomplete slot").ordinal(), 20);
    assert_eq!(inventory.last().expect("unbound").frame(), &unbound.0);
    let replay1 = successor
        .replay_inventory(&seal, &authority, None)
        .await
        .expect("replay page one");
    assert_eq!(replay1.operations.len(), 16);
    assert_eq!(replay1.operations[0].slot().expect("slot").ordinal(), 1);
    assert_eq!(replay1.operations[1].slot().expect("gap").ordinal(), 3);
    let replay2 = successor
        .replay_inventory(&seal, &authority, replay1.next)
        .await
        .expect("replay page two");
    assert_eq!(replay2.operations.len(), 2);
    assert!(replay2.next.is_none());
    assert!(
        replay1
            .operations
            .iter()
            .chain(replay2.operations.iter())
            .all(|operation| operation.frame().operation_id != incomplete.0.operation_id),
        "a bound slot without a committed source handoff is not replayable"
    );
    let saved = successor
        .reconcile(&seal, &authority, frames[0].0.operation_id)
        .await
        .expect("positive reconciliation");
    assert!(matches!(saved, CustodyResolution::Committed(_)));
    successor
        .transfer_recovered(
            &seal,
            &authority,
            &inventory[0].admission(&seal).expect("admission"),
        )
        .await
        .expect("partial recovery continues");
    drop(successor);
    drop(repo);
    c.fixture.cleanup().await;
}

async fn shared_parent(c: &Candidate) -> [(PreparedDirectFrame, SendLease); 2] {
    let (pin, envelope, encoded) = direct_pin(Uuid::new_v4(), "bob@example.test/laptop");
    let mut intent =
        IngressEffectIntent::decode_v1(pin.key.kind.to_storage(), &encoded).expect("intent");
    let IngressEffectIntent::RouteDirect { fanout, .. } = &mut intent else {
        panic!("route")
    };
    fanout.push("bob@example.test/phone".parse().expect("phone"));
    let encoded = intent
        .with_encoded_v1(|_, bytes| bytes.to_vec())
        .expect("encode");
    seed_pg_direct(&c.fixture, &pin, envelope, encoded).await;
    let mut phone = pin.clone();
    phone.admission_id = Uuid::new_v4();
    phone.key.resource = "bob@example.test/phone".parse().expect("phone");
    let mut a = request(pin);
    let mut b = request(phone);
    let la = pg_attempt(&c.fixture, &c.config, &mut a).await;
    let lb = pg_attempt(&c.fixture, &c.config, &mut b).await;
    let fa = PreparedDirectFrame::for_lease(
        a.pin.admission_id,
        a.pin.key,
        a.pin.payload,
        a.pin.received_at,
        &la,
    )
    .expect("frame A");
    let fb = PreparedDirectFrame::for_lease(
        b.pin.admission_id,
        b.pin.key,
        b.pin.payload,
        b.pin.received_at,
        &lb,
    )
    .expect("frame B");
    [(fa, la), (fb, lb)]
}

async fn named_repo(c: &Candidate, capture: CustodyCapture, name: &str) -> SmCustodyRepository {
    SmCustodyRepository::new(
        uow(
            &c.fixture.runtime_options.clone().application_name(name),
            &c.config,
            c.fixture.lineage.deployment_uuid.0,
            &c.capture.owner,
        )
        .await,
        &c.config,
        capture,
        CustodyLimits::new(16, 65536).expect("limits"),
    )
    .expect("repository")
}

#[tokio::test]
async fn postgres_multiframe_opposite_orders_lock_canonical_parents_in_same_order() {
    use sqlx::Connection;
    use std::sync::Arc;
    use waddle_xmpp::ownership::EntityType;
    use waddle_xmpp::pending_delivery::SmSessionId;

    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let pair_a = shared_parent(&c).await;
    let pair_b = shared_parent(&c).await;
    let mut capture_b = c.capture.clone();
    capture_b.gate_id = Uuid::new_v4();
    capture_b.token = Uuid::new_v4();
    capture_b.resource = "bob@example.test/phone".parse().expect("phone");
    capture_b.session = SmSessionId::new("opposite-order-session");
    sqlx::query(&format!("INSERT INTO {}.clustering_claims (entity,entity_type,node_id,node_epoch,claim_epoch) VALUES ($1,$2,$3,$4,$5)",c.fixture.target))
        .bind(format!("{}:{}",EntityType::SmSession.as_db_str(),capture_b.session.as_str()))
        .bind(EntityType::SmSession.as_db_str()).bind(&capture_b.owner.node_id).bind(&capture_b.owner.node_epoch)
        .bind(capture_b.claim.0).execute(&c.fixture.admin).await.expect("second claim");
    let repo_a = Arc::new(named_repo(&c, c.capture.clone(), "multiframe-lock-a").await);
    let repo_b = Arc::new(named_repo(&c, capture_b.clone(), "multiframe-lock-b").await);
    repo_b
        .install_captured_session()
        .await
        .expect("second gate");
    let mut mem_a = CandidateSmMemory::new(c.capture.clone());
    let mut mem_b = CandidateSmMemory::new(capture_b);
    for (repo, memory, frames) in [
        (&repo_a, &mut mem_a, [&pair_a[0], &pair_b[0]]),
        (&repo_b, &mut mem_b, [&pair_b[1], &pair_a[1]]),
    ] {
        for (frame, lease) in frames {
            let handoff = repo
                .preaccept_local_direct(frame, lease)
                .await
                .expect("handoff");
            let admission = repo
                .committed_admission(frame.operation_id)
                .await
                .expect("admission");
            let slot = repo.bind(&admission).await.expect("slot");
            repo.hydrate_preaccepted(memory, &handoff)
                .await
                .expect("hydrate");
            repo.expose(memory, &slot).await.expect("expose");
        }
    }
    let mut parents = [
        pair_a[0].0.key.message_key.to_storage(),
        pair_b[0].0.key.message_key.to_storage(),
    ];
    parents.sort_unstable();
    let mut blocker = sqlx::PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("blocker");
    let mut block_tx = blocker.begin().await.expect("block transaction");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key=$1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(parents[0])
    .fetch_one(&mut *block_tx)
    .await
    .expect("hold first canonical");
    let a = Arc::clone(&repo_a);
    let b = Arc::clone(&repo_b);
    let ta = tokio::spawn(async move { a.acknowledge(2).await });
    let tb = tokio::spawn(async move { b.acknowledge(2).await });
    // PostgreSQL may queue the second waiter behind the first waiter's tuple
    // lock, so it need not report the original blocker as its immediate parent.
    for application in ["multiframe-lock-a", "multiframe-lock-b"] {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock')")
                    .bind(application).fetch_one(&c.fixture.admin).await.expect("observe lock waiter");
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("ACK is waiting for the first parent");
    }
    let mut probe = sqlx::PgConnection::connect_with(&c.fixture.admin_options)
        .await
        .expect("probe");
    let mut probe_tx = probe.begin().await.expect("probe tx");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.ingress_messages WHERE message_key=$1 FOR UPDATE NOWAIT",
        c.fixture.target
    ))
    .bind(parents[1])
    .fetch_one(&mut *probe_tx)
    .await
    .expect("neither ACK acquired the later parent first");
    probe_tx.rollback().await.expect("release probe");
    block_tx.commit().await.expect("release first parent");
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        ta.await.expect("task A").expect("ACK A");
        tb.await.expect("task B").expect("ACK B");
    })
    .await
    .expect("no opposite-order deadlock");
    drop(repo_a);
    drop(repo_b);
    c.fixture.cleanup().await;
}
