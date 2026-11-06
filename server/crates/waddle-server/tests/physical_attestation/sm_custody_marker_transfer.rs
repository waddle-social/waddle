//! Source handoff is required for the dark candidate transfer journal.

use uuid::Uuid;
use waddle_server::sm_custody::{CandidateSmMemory, CustodyError, CustodyTerminal, HandoffStage};

use super::{multiframe, pg_seed_transaction, Candidate, CATALOG_GRANT_LOCK};

#[tokio::test]
async fn postgres_transfer_requires_its_own_marker_and_retains_unmarked_recovery() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    repo.preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("first operation has a marker");
    let second = multiframe::source(&c, "second").await;
    let second_admission = repo
        .admit(&second.0, &second.1)
        .await
        .expect("admit second");
    assert!(matches!(
        repo.transfer(&second_admission).await,
        Err(CustodyError::Rejected)
    ));
    repo.bind(&second_admission).await.expect("bind second");
    assert!(matches!(
        repo.transfer(&second_admission).await,
        Err(CustodyError::Rejected)
    ));
    let q = &c.fixture.target;
    assert!(sqlx::query(&format!("INSERT INTO {q}.sm_custody_transfers (operation_id, resource, payload, received_at) SELECT operation_id, resource, payload, received_at FROM {q}.sm_custody_operations WHERE operation_id = $1"))
        .bind(second.0.operation_id).execute(&c.fixture.admin).await.is_err(), "another operation's marker cannot authorize a transfer");
    assert!(
        sqlx::query(&format!(
            "INSERT INTO {q}.sm_custody_terminals (operation_id, disposition) VALUES ($1, 2)"
        ))
        .bind(second.0.operation_id)
        .execute(&c.fixture.admin)
        .await
        .is_err(),
        "a raw terminal cannot bypass the marked transfer"
    );
    let state: (bool, i64, i64) = sqlx::query_as(&format!("SELECT sealed, (SELECT count(*) FROM {q}.sm_custody_terminals), (SELECT count(*) FROM {q}.sm_custody_transfers) FROM {q}.sm_custody_gates WHERE gate_id=$1"))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("state after refusal");
    assert_eq!(state, (false, 0, 0));

    let third = multiframe::source(&c, "unmarked at recovery").await;
    repo.admit(&third.0, &third.1)
        .await
        .expect("third admission");
    repo.resume_preaccept(&repo.handoff_recovery(&second.0))
        .await
        .expect("exact second marker");
    repo.transfer(&second_admission)
        .await
        .expect("marked transfer");
    repo.transfer(&second_admission)
        .await
        .expect("exact terminal retry");
    let seal = repo
        .seal_for_recovery(&c.authority())
        .await
        .expect("sealed gate");
    let page = repo
        .inventory(&seal, &c.authority(), None)
        .await
        .expect("inventory");
    let third_item = page
        .operations
        .iter()
        .find(|item| item.frame().operation_id == third.0.operation_id)
        .expect("unmarked retained");
    assert_eq!(third_item.terminal(), None);
    assert!(matches!(
        repo.transfer_recovered(
            &seal,
            &c.authority(),
            &third_item.admission(&seal).expect("exact admission")
        )
        .await,
        Err(CustodyError::Rejected)
    ));
    let second_item = page
        .operations
        .iter()
        .find(|item| item.frame().operation_id == second.0.operation_id)
        .expect("marked retained");
    assert_eq!(second_item.terminal(), Some(CustodyTerminal::Transfer));
    repo.transfer_recovered(
        &seal,
        &c.authority(),
        &second_item.admission(&seal).expect("exact admission"),
    )
    .await
    .expect("sealed exact retry");
    let after = repo
        .inventory(&seal, &c.authority(), None)
        .await
        .expect("inventory after refusal");
    assert_eq!(
        after
            .operations
            .iter()
            .find(|item| item.frame().operation_id == third.0.operation_id)
            .expect("retained")
            .terminal(),
        None
    );
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_reclaimed_attempt_cannot_create_marker_or_transfer() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let admission = repo.admit(&c.frame, &c.lease).await.expect("admit");
    repo.bind(&admission).await.expect("bind");
    let q = &c.fixture.target;
    let mut replace = pg_seed_transaction(&c.fixture).await;
    sqlx::query(&format!(
        "UPDATE {q}.ingress_send_attempts SET lease_token=$1 WHERE message_key=$2"
    ))
    .bind(Uuid::new_v4().to_string())
    .bind(c.frame.key.message_key.to_storage())
    .execute(&mut *replace)
    .await
    .expect("replace exact attempt token");
    replace.commit().await.expect("commit replacement");
    let failed = repo
        .resume_preaccept(&repo.handoff_recovery(&c.frame))
        .await
        .expect_err("replacement attempt cannot complete the original handoff");
    assert_eq!(failed.stage(), HandoffStage::SourceCompletion);
    assert_eq!(failed.recovery().ordinal(), Some(1));
    assert_eq!(failed.recovery().source(), &c.frame.key);
    assert_eq!(failed.recovery().capture(), &c.capture);
    assert!(sqlx::query(&format!(
        "INSERT INTO {q}.sm_custody_handoffs (operation_id) VALUES ($1)"
    ))
    .bind(c.frame.operation_id)
    .execute(&c.fixture.admin)
    .await
    .is_err());
    assert!(matches!(
        repo.transfer(&admission).await,
        Err(CustodyError::Rejected)
    ));
    let state: (bool, i64, i64) = sqlx::query_as(&format!("SELECT sealed, (SELECT count(*) FROM {q}.sm_custody_handoffs), (SELECT count(*) FROM {q}.sm_custody_terminals) FROM {q}.sm_custody_gates WHERE gate_id=$1"))
        .bind(c.capture.gate_id).fetch_one(&c.fixture.admin).await.expect("refusal state");
    assert_eq!(state, (false, 0, 0));
    let sealed = repo.seal_for_recovery(&c.authority()).await.expect("seal");
    let page = repo
        .inventory(&sealed, &c.authority(), None)
        .await
        .expect("retained inventory");
    assert_eq!(page.operations[0].frame(), &c.frame);
    assert!(matches!(
        repo.transfer_recovered(&sealed, &c.authority(), &admission)
            .await,
        Err(CustodyError::Rejected)
    ));
    drop(repo);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_ack_and_marked_transfer_have_one_gate_winner() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let handoff = repo
        .preaccept_local_direct(&c.frame, &c.lease)
        .await
        .expect("marker");
    let admission = repo
        .committed_admission(&handoff.recovery())
        .await
        .expect("admission");
    let slot = repo.bind(&admission).await.expect("slot");
    let mut memory = CandidateSmMemory::new(c.capture.clone());
    repo.hydrate_preaccepted(&mut memory, &handoff)
        .await
        .expect("hydrate");
    repo.expose(&memory, &slot).await.expect("expose");
    let (ack, transfer) = tokio::join!(repo.acknowledge(slot.h()), repo.transfer(&admission));
    assert_ne!(ack.is_ok(), transfer.is_ok(), "exactly one terminal wins");
    let q = &c.fixture.target;
    let (disposition, copies): (i32, i64) = sqlx::query_as(&format!("SELECT disposition, (SELECT count(*) FROM {q}.sm_custody_transfers) FROM {q}.sm_custody_terminals WHERE operation_id=$1"))
        .bind(c.frame.operation_id).fetch_one(&c.fixture.admin).await.expect("terminal");
    assert_eq!(
        (disposition, copies),
        if ack.is_ok() { (1, 0) } else { (2, 1) }
    );
    drop(repo);
    c.fixture.cleanup().await;
}
