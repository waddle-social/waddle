//! Dark portable writer and snapshot regression coverage.

use super::*;

fn snapshot_session(
    stream: &str,
    recipient: &str,
) -> waddle_xmpp::stream_management::persistence::PersistedSession {
    waddle_xmpp::stream_management::persistence::PersistedSession {
        stream_id: SmSessionId::new(stream),
        user_id: "fixture".to_string(),
        jid: format!("{recipient}/phone").parse().expect("full JID"),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 2,
        last_acked: 0,
        replay_gap_through: None,
        max_resume_time: Some(60),
        detached_at: chrono::Utc::now(),
        max_resume_duration: std::time::Duration::from_secs(60),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
    }
}

fn snapshot_principal(recipient: &str) -> waddle_xmpp::auth::AuthenticatedPrincipalRef {
    use waddle_xmpp::auth::{
        AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
    };
    AuthenticatedPrincipalRef::new(
        recipient.parse().expect("bare JID"),
        AuthContextId::new(uuid::Uuid::new_v4()),
        AuthContextVersion::INITIAL,
        PrincipalAuthEpoch::INITIAL,
    )
}

fn snapshot_ledger(
    frame: &PersistedUnackedStanza,
) -> waddle_xmpp::stream_management::persistence::PersistedIngressAppend {
    use waddle_xmpp::stream_management::persistence::{
        IngressCustodyDisposition, PersistedIngressAppend,
    };
    PersistedIngressAppend {
        key: frame.provenance.claims().expect("ingress claims")[0].clone(),
        accepting_stream: frame.stream_id.clone(),
        sequence: frame.sequence,
        appended_at: chrono::Utc::now(),
        payload: frame.stanza.as_ref().clone(),
        original_receipt_at: frame.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    }
}

#[tokio::test]
async fn portable_sqlite_snapshot_and_promotion_wrappers_preserve_conflicts_and_payloads() {
    use waddle_server::sm_persistence::DatabaseSmPersistence;
    use waddle_xmpp::stream_management::persistence::{
        KeyedSnapshotOutcome, PromotionAppendOutcome, SmPersistenceStorage,
    };

    let storage = DatabaseSmPersistence::open(None)
        .await
        .expect("SQLite SM storage");
    let session = snapshot_session("sqlite-snapshot", "alice@example.test");
    let initial = frame(
        uuid::Uuid::new_v4(),
        "sqlite-snapshot",
        "alice@example.test",
        1,
        "first counted payload",
    );
    let proof = snapshot_ledger(&initial);
    assert_eq!(
        storage
            .store_session_atomic_with_ingress_append(
                session.clone(),
                vec![initial.clone()],
                proof.clone()
            )
            .await
            .expect("initial snapshot"),
        KeyedSnapshotOutcome::Committed
    );
    let other = frame(
        uuid::Uuid::new_v4(),
        "sqlite-snapshot",
        "alice@example.test",
        2,
        "replacement",
    );
    assert!(
        matches!(storage.store_session_atomic_with_ingress_append(session.clone(), vec![other.clone()], proof.clone()).await.expect("conflicting snapshot"), KeyedSnapshotOutcome::ObligationAlreadyAllocated { accepting_stream } if accepting_stream == session.stream_id)
    );
    let saved = storage
        .list_unacked(&session.stream_id)
        .await
        .expect("queue after rollback");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].stanza.to_element(), initial.stanza.to_element());

    let principal = snapshot_principal("alice@example.test");
    let withheld = storage
        .store_session_atomic_with_principal_and_ingress_appends(
            &principal,
            session.clone(),
            vec![other.clone()],
            vec![proof.clone()],
        )
        .await
        .expect("drained snapshot");
    assert_eq!(withheld, vec![proof.key]);
    let saved = storage
        .list_unacked(&session.stream_id)
        .await
        .expect("counted replacement");
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].stanza.to_element(), other.stanza.to_element());

    let ordinary = frame(
        uuid::Uuid::new_v4(),
        "sqlite-snapshot",
        "alice@example.test",
        3,
        "ordinary append",
    );
    storage
        .append_unacked(ordinary.clone())
        .await
        .expect("ordinary SQLite append");
    let saved = storage
        .list_unacked(&session.stream_id)
        .await
        .expect("queue after ordinary append");
    assert!(saved
        .iter()
        .any(|row| row.sequence == 3 && row.stanza.to_element() == ordinary.stanza.to_element()));

    let mut promotion = snapshot_session("sqlite-promotion", "bob@example.test");
    promotion.max_resume_time = Some(0);
    promotion.max_resume_duration = std::time::Duration::ZERO;
    promotion.outbound_count = 0;
    storage
        .upsert_session(promotion.clone())
        .await
        .expect("promotion session");
    let suffix = frame(
        uuid::Uuid::new_v4(),
        "sqlite-promotion",
        "bob@example.test",
        1,
        "promotion payload",
    );
    assert_eq!(
        storage
            .append_promotion_stanza(suffix.clone())
            .await
            .expect("append promotion"),
        PromotionAppendOutcome::Appended
    );
    assert_eq!(
        storage
            .append_promotion_stanza(suffix)
            .await
            .expect("repeat promotion"),
        PromotionAppendOutcome::AlreadyRecorded
    );
}

#[tokio::test]
async fn portable_snapshot_interlock_revalidates_inventory_and_preserves_conflict_winners() {
    use route::{CandidateRouteError, CandidateSnapshotLedger, CandidateSnapshotOutcome};
    use waddle_xmpp::stream_management::persistence::PromotionAppendOutcome;

    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let version: i32 = sqlx::query_scalar("SELECT current_setting('server_version_num')::int")
        .fetch_one(&fixture.admin)
        .await
        .expect("server version");
    assert!(
        (170000..180000).contains(&version),
        "PostgreSQL 17 required"
    );
    let schema = fixture.target.clone();
    let owner = Database::from_config(
        "SM snapshot owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &schema),
        ),
    )
    .await
    .expect("owner");
    waddle_server::sm_persistence::bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("SM schema");
    waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("pending schema");
    sqlx::query(&format!(
        "CREATE TABLE {schema}.ingress_messages (message_key UUID PRIMARY KEY)"
    ))
    .execute(&fixture.admin)
    .await
    .expect("sources");
    sqlx::raw_sql(&route::CANDIDATE_DDL.replace("__SCHEMA__", &schema))
        .execute(&fixture.admin)
        .await
        .expect("route schema");
    let sources = [
        uuid::Uuid::from_u128(1),
        uuid::Uuid::from_u128(2),
        uuid::Uuid::from_u128(3),
        uuid::Uuid::from_u128(4),
    ];
    for source in sources {
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_messages (message_key) VALUES ($1)"
        ))
        .bind(source)
        .execute(&fixture.admin)
        .await
        .expect("canonical source");
    }
    for (stream, recipient, resume, duration) in [
        ("snapshot", "alice@example.test", 60_i64, 60000_i64),
        ("promotion", "bob@example.test", 0_i64, 0_i64),
        ("multi-writer", "grace@example.test", 60_i64, 60000_i64),
        ("multi-pending", "gwen@example.test", 60_i64, 60000_i64),
        ("snapshot-winner", "hugo@example.test", 60_i64, 60000_i64),
        ("snapshot-loser", "ivy@example.test", 60_i64, 60000_i64),
        ("snapshot-cancel", "jane@example.test", 60_i64, 60000_i64),
    ] {
        sqlx::query(&format!("INSERT INTO {schema}.sm_sessions (stream_id,user_id,full_jid,inbound_count,outbound_count,last_acked,max_resume_secs,detached_at_ms,max_resume_duration_ms,carbons_enabled,roster_interested,presence_available,presence_priority,bare_jid) VALUES ($1,'fixture',$2,0,0,0,$4,0,$5,0,0,0,0,$3)"))
            .bind(stream).bind(format!("{recipient}/phone")).bind(recipient).bind(resume).bind(duration).execute(&fixture.admin).await.expect("session");
    }
    let alice: jid::BareJid = "alice@example.test".parse().expect("Alice JID");
    let bob: jid::BareJid = "bob@example.test".parse().expect("Bob JID");
    let session = snapshot_session("snapshot", "alice@example.test");
    let old = frame(
        sources[0],
        "snapshot",
        "alice@example.test",
        1,
        "original counted",
    );
    let replacement = frame(
        sources[1],
        "snapshot",
        "alice@example.test",
        2,
        "replacement counted",
    );

    let mut tx = owner.begin().await.expect("old frame transaction");
    route::append_complete_sources_if_open(&mut tx, &schema, &alice, &old)
        .await
        .expect("old frame");
    tx.commit().await.expect("commit old frame");

    // Rollback after the production replacement body restores both the old
    // payload and its candidate owner, including cascading deletes.
    let tx = owner.begin().await.expect("snapshot rollback transaction");
    let (tx, outcome) = route::store_snapshot_if_open(
        tx,
        &schema,
        None,
        &session,
        &[replacement.clone()],
        CandidateSnapshotLedger::Untouched,
    )
    .await
    .expect("snapshot body");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    tx.rollback().await.expect("rollback snapshot");
    let old_payload: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='snapshot' AND sequence=1"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("old row survived");
    assert!(old_payload.contains("original counted"));
    let owners: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {schema}.sm_recipient_route_frame_owners WHERE stream_id='snapshot' AND sequence=1")).fetch_one(&fixture.admin).await.expect("owner survived");
    assert_eq!(owners, 1);

    let proof = snapshot_ledger(&replacement);
    let tx = owner.begin().await.expect("snapshot commit transaction");
    let (tx, outcome) = route::store_snapshot_if_open(
        tx,
        &schema,
        None,
        &session,
        &[replacement.clone()],
        CandidateSnapshotLedger::Exclusive(proof.clone()),
    )
    .await
    .expect("snapshot with ledger");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    tx.commit().await.expect("commit snapshot");
    let replacement_payload: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='snapshot' AND sequence=2"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("replacement");
    assert!(replacement_payload.contains("replacement counted"));

    let later = frame(
        sources[2],
        "snapshot",
        "alice@example.test",
        3,
        "later counted",
    );
    let conflict = owner
        .begin()
        .await
        .expect("conflicting snapshot transaction");
    assert!(matches!(
        route::store_snapshot_if_open(
            conflict,
            &schema,
            None,
            &session,
            &[later.clone()],
            CandidateSnapshotLedger::Exclusive(proof.clone())
        )
        .await,
        Err(CandidateRouteError::LedgerConflict)
    ));
    let winning_stream: String = sqlx::query_scalar(&format!(
        "SELECT accepting_stream_id FROM {schema}.sm_ingress_appends WHERE message_key=$1"
    ))
    .bind(proof.key.message_key.to_storage().to_string())
    .fetch_one(&fixture.admin)
    .await
    .expect("winner read after rollback");
    assert_eq!(winning_stream, "snapshot");
    let kept: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='snapshot' AND sequence=2"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("conflict preserved payload");
    assert!(kept.contains("replacement counted"));

    let tx = owner.begin().await.expect("drained snapshot transaction");
    let (tx, outcome) = route::store_snapshot_if_open(
        tx,
        &schema,
        None,
        &session,
        &[later.clone()],
        CandidateSnapshotLedger::Drained(vec![proof.clone()]),
    )
    .await
    .expect("drained snapshot");
    assert_eq!(
        outcome,
        CandidateSnapshotOutcome::Applied {
            withheld: vec![proof.key.clone()]
        }
    );
    tx.commit().await.expect("commit counted drained frame");
    let kept: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='snapshot' AND sequence=3"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("counted payload after withheld proof");
    assert!(kept.contains("later counted"));

    // Unknown provenance and untracked owners both fail before replacement.
    let mut unknown = frame(sources[3], "snapshot", "alice@example.test", 4, "unknown");
    unknown.provenance = SmFrameProvenance::Unknown;
    let tx = owner.begin().await.expect("unknown transaction");
    assert!(matches!(
        route::store_snapshot_if_open(
            tx,
            &schema,
            None,
            &session,
            &[unknown],
            CandidateSnapshotLedger::Untouched
        )
        .await,
        Err(CandidateRouteError::UnsupportedProvenance)
    ));
    sqlx::query(&format!("DELETE FROM {schema}.sm_recipient_route_frame_owners WHERE stream_id='snapshot' AND sequence=3")).execute(&fixture.admin).await.expect("untrack old frame");
    let tx = owner.begin().await.expect("untracked transaction");
    assert!(matches!(
        route::store_snapshot_if_open(
            tx,
            &schema,
            None,
            &session,
            &[],
            CandidateSnapshotLedger::Untouched
        )
        .await,
        Err(CandidateRouteError::UnsupportedProvenance)
    ));
    sqlx::query(&format!("INSERT INTO {schema}.sm_recipient_route_frame_owners VALUES ('snapshot',3,'alice@example.test')")).execute(&fixture.admin).await.expect("restore owner");

    // One frame can claim several distinct canonical sources; all of them
    // must be locked before the recipient and stream gates.
    let mut mixed = frame(
        sources[0],
        "snapshot",
        "alice@example.test",
        4,
        "two sources",
    );
    let other_claim = frame(sources[1], "snapshot", "alice@example.test", 4, "other");
    mixed.provenance = SmFrameProvenance::ingress([
        mixed.provenance.claims().expect("first claim")[0].clone(),
        other_claim.provenance.claims().expect("second claim")[0].clone(),
    ]);
    let mut tx = owner.begin().await.expect("multi-source transaction");
    route::append_complete_sources_if_open(&mut tx, &schema, &alice, &mixed)
        .await
        .expect("complete multi-source append");
    tx.commit().await.expect("commit multi-source frame");

    let promotion = frame(
        sources[3],
        "promotion",
        "bob@example.test",
        1,
        "promotion counted",
    );
    let mut tx = owner.begin().await.expect("promotion transaction");
    assert_eq!(
        route::append_promotion_if_open(&mut tx, &schema, &bob, promotion.clone())
            .await
            .expect("promotion append"),
        PromotionAppendOutcome::Appended
    );
    tx.commit().await.expect("commit promotion");
    let mut tx = owner.begin().await.expect("repeat promotion");
    assert_eq!(
        route::append_promotion_if_open(&mut tx, &schema, &bob, promotion.clone())
            .await
            .expect("same promotion"),
        PromotionAppendOutcome::AlreadyRecorded
    );
    tx.commit().await.expect("commit retry");
    let mut changed = promotion.clone();
    changed.stanza = frame(sources[3], "promotion", "bob@example.test", 1, "changed").stanza;
    let mut tx = owner.begin().await.expect("promotion conflict");
    assert!(
        route::append_promotion_if_open(&mut tx, &schema, &bob, changed)
            .await
            .is_err()
    );
    tx.rollback().await.expect("rollback promotion conflict");
    sqlx::query(&format!(
        "DELETE FROM {schema}.sm_unacked WHERE stream_id='promotion' AND sequence=1"
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate a retired promotion suffix");
    let mut tx = owner.begin().await.expect("retired promotion transaction");
    assert_eq!(
        route::append_promotion_if_open(&mut tx, &schema, &bob, promotion)
            .await
            .expect("retired promotion"),
        PromotionAppendOutcome::Retired
    );
    tx.commit().await.expect("commit retired promotion outcome");
    let watermark: i64 = sqlx::query_scalar(&format!(
        "SELECT outbound_count FROM {schema}.sm_sessions WHERE stream_id='promotion'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("retired watermark");
    assert_eq!(watermark, 1);

    // Block the snapshot during source locking, then append a new source before
    // its gate. Its second inventory read must force a new transaction.
    let blocker_db = Database::from_config(
        "snapshot blocker",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-snapshot-blocker"),
        ),
    )
    .await
    .expect("blocker");
    let waiter_db = Database::from_config(
        "snapshot waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-snapshot-waiter"),
        ),
    )
    .await
    .expect("waiter");
    let mut blocker = blocker_db.begin().await.expect("source lock blocker");
    blocker
        .query(
            &format!(
                "SELECT 1 FROM {schema}.ingress_messages WHERE message_key=?::uuid FOR UPDATE"
            ),
            waddle_server::db_params![sources[2].to_string()],
        )
        .await
        .expect("hold source");
    let blocker_pid = backend_pid(&mut blocker).await;
    let wait_schema = schema.clone();
    let wait_session = session.clone();
    let wait_frame = frame(
        sources[1],
        "snapshot",
        "alice@example.test",
        4,
        "stale replacement",
    );
    let waiter = tokio::spawn(async move {
        let tx = waiter_db
            .begin()
            .await
            .expect("snapshot waiter transaction");
        let result = route::store_snapshot_if_open(
            tx,
            &wait_schema,
            None,
            &wait_session,
            &[wait_frame],
            CandidateSnapshotLedger::Untouched,
        )
        .await;
        match result {
            Ok((tx, outcome)) => {
                tx.rollback().await.expect("waiter rollback");
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    });
    wait_for_blocked_application(&fixture.admin, "sm-snapshot-waiter", blocker_pid).await;
    let mut writer = owner.begin().await.expect("concurrent writer");
    let fresh = frame(
        sources[3],
        "snapshot",
        "alice@example.test",
        5,
        "raced counted",
    );
    route::append_complete_sources_if_open(&mut writer, &schema, &alice, &fresh)
        .await
        .expect("concurrent append");
    writer.commit().await.expect("commit concurrent append");
    blocker.rollback().await.expect("release source");
    assert!(matches!(
        waiter.await.expect("waiter join"),
        Err(CandidateRouteError::Retry)
    ));
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.sm_unacked WHERE stream_id='snapshot' AND sequence IN (3,5)"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("retained counted frames");
    assert_eq!(count, 2);

    // Distinct-source pending and a multi-source writer contend on the same
    // recipient guard; check both winner orders with real production inserts.
    let grace: jid::BareJid = "grace@example.test".parse().expect("Grace JID");
    let mut grace_frame = frame(
        sources[0],
        "multi-writer",
        "grace@example.test",
        1,
        "multi counted",
    );
    let second = frame(
        sources[1],
        "multi-writer",
        "grace@example.test",
        1,
        "second source",
    );
    grace_frame.provenance = SmFrameProvenance::ingress([
        grace_frame.provenance.claims().expect("first source")[0].clone(),
        second.provenance.claims().expect("second source")[0].clone(),
    ]);
    let pending_waiter_db = Database::from_config(
        "multi-source pending waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-multi-pending-waiter"),
        ),
    )
    .await
    .expect("pending waiter");
    let mut multi_writer = owner.begin().await.expect("multi writer transaction");
    route::append_complete_sources_if_open(&mut multi_writer, &schema, &grace, &grace_frame)
        .await
        .expect("multi writer");
    let blocker_pid = backend_pid(&mut multi_writer).await;
    let pending_schema = schema.clone();
    let pending_recipient = grace.clone();
    let pending_waiter = tokio::spawn(async move {
        let mut tx = pending_waiter_db
            .begin()
            .await
            .expect("pending waiter transaction");
        let result = route::accept_pending_if_quiescent(
            &mut tx,
            &pending_schema,
            &[sources[3]],
            &pending_recipient,
            &pending(pending_recipient.clone(), "must not win"),
            QuotaPolicy::Unlimited,
        )
        .await;
        tx.rollback().await.expect("pending waiter rollback");
        result
    });
    wait_for_blocked_application(&fixture.admin, "sm-multi-pending-waiter", blocker_pid).await;
    multi_writer.commit().await.expect("commit multi writer");
    assert!(matches!(
        pending_waiter.await.expect("pending waiter join"),
        Err(CandidateRouteError::OutstandingSm)
    ));
    let counted: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='multi-writer' AND sequence=1"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("counted multi-source frame");
    assert!(counted.contains("multi counted"));

    let gwen: jid::BareJid = "gwen@example.test".parse().expect("Gwen JID");
    let mut late = frame(
        sources[0],
        "multi-pending",
        "gwen@example.test",
        1,
        "late counted",
    );
    let second = frame(
        sources[1],
        "multi-pending",
        "gwen@example.test",
        1,
        "other source",
    );
    late.provenance = SmFrameProvenance::ingress([
        late.provenance.claims().expect("first source")[0].clone(),
        second.provenance.claims().expect("second source")[0].clone(),
    ]);
    let late_db = Database::from_config(
        "multi-source writer waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-multi-writer-waiter"),
        ),
    )
    .await
    .expect("writer waiter");
    let mut accept = owner.begin().await.expect("pending winner transaction");
    assert_eq!(
        route::accept_pending_if_quiescent(
            &mut accept,
            &schema,
            &[sources[3]],
            &gwen,
            &pending(gwen.clone(), "accepted"),
            QuotaPolicy::Unlimited
        )
        .await
        .expect("pending wins"),
        InsertOutcome::Inserted
    );
    let blocker_pid = backend_pid(&mut accept).await;
    let late_schema = schema.clone();
    let late_recipient = gwen.clone();
    let retained_late = late.clone();
    let late_writer = tokio::spawn(async move {
        let mut tx = late_db.begin().await.expect("late writer transaction");
        let result =
            route::append_complete_sources_if_open(&mut tx, &late_schema, &late_recipient, &late)
                .await;
        tx.rollback().await.expect("late writer rollback");
        result
    });
    wait_for_blocked_application(&fixture.admin, "sm-multi-writer-waiter", blocker_pid).await;
    accept.commit().await.expect("commit pending winner");
    assert!(matches!(
        late_writer.await.expect("late writer join"),
        Err(CandidateRouteError::Closed)
    ));
    assert_eq!(
        retained_late.stanza.to_element(),
        frame(
            sources[0],
            "multi-pending",
            "gwen@example.test",
            1,
            "late counted"
        )
        .stanza
        .to_element()
    );
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.sm_unacked WHERE stream_id='multi-pending'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("refused write count");
    assert_eq!(count, 0);

    // The snapshot uses the same recipient fence as real pending acceptance.
    // First hold a complete snapshot while a different source tries pending.
    let hugo: jid::BareJid = "hugo@example.test".parse().expect("Hugo JID");
    let snapshot_session_winner = snapshot_session("snapshot-winner", "hugo@example.test");
    let snapshot_frame = frame(
        sources[0],
        "snapshot-winner",
        "hugo@example.test",
        1,
        "snapshot wins",
    );
    let tx = owner.begin().await.expect("snapshot winner transaction");
    let (mut snapshot_winner, outcome) = route::store_snapshot_if_open(
        tx,
        &schema,
        None,
        &snapshot_session_winner,
        &[snapshot_frame],
        CandidateSnapshotLedger::Untouched,
    )
    .await
    .expect("snapshot winner");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    let blocker_pid = backend_pid(&mut snapshot_winner).await;
    let pending_db = Database::from_config(
        "snapshot pending waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-snapshot-pending-waiter"),
        ),
    )
    .await
    .expect("snapshot pending waiter");
    let pending_schema = schema.clone();
    let pending_recipient = hugo.clone();
    let pending_waiter = tokio::spawn(async move {
        let mut tx = pending_db
            .begin()
            .await
            .expect("snapshot pending waiter transaction");
        let result = route::accept_pending_if_quiescent(
            &mut tx,
            &pending_schema,
            &[sources[3]],
            &pending_recipient,
            &pending(pending_recipient.clone(), "blocked by snapshot"),
            QuotaPolicy::Unlimited,
        )
        .await;
        tx.rollback()
            .await
            .expect("snapshot pending waiter rollback");
        result
    });
    wait_for_blocked_application(&fixture.admin, "sm-snapshot-pending-waiter", blocker_pid).await;
    snapshot_winner
        .commit()
        .await
        .expect("commit snapshot winner");
    assert!(matches!(
        pending_waiter.await.expect("snapshot pending waiter join"),
        Err(CandidateRouteError::OutstandingSm)
    ));
    let snapshot_saved: String = sqlx::query_scalar(&format!("SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='snapshot-winner' AND sequence=1"))
        .fetch_one(&fixture.admin).await.expect("snapshot winner retained");
    assert!(snapshot_saved.contains("snapshot wins"));

    // A pending winner closes its recipient while the snapshot still owns its
    // immutable input; the snapshot returns its error only after rollback.
    let ivy: jid::BareJid = "ivy@example.test".parse().expect("Ivy JID");
    let mut pending_winner = owner.begin().await.expect("snapshot pending winner");
    assert_eq!(
        route::accept_pending_if_quiescent(
            &mut pending_winner,
            &schema,
            &[sources[3]],
            &ivy,
            &pending(ivy.clone(), "pending wins"),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("accept pending before snapshot"),
        InsertOutcome::Inserted
    );
    let blocker_pid = backend_pid(&mut pending_winner).await;
    let snapshot_waiter_db = Database::from_config(
        "pending snapshot waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-pending-snapshot-waiter"),
        ),
    )
    .await
    .expect("pending snapshot waiter");
    let wait_schema = schema.clone();
    let late_snapshot = frame(
        sources[0],
        "snapshot-loser",
        "ivy@example.test",
        1,
        "late snapshot input",
    );
    let retained_snapshot = late_snapshot.clone();
    let snapshot_waiter = tokio::spawn(async move {
        let session = snapshot_session("snapshot-loser", "ivy@example.test");
        let tx = snapshot_waiter_db
            .begin()
            .await
            .expect("pending snapshot waiter transaction");
        match route::store_snapshot_if_open(
            tx,
            &wait_schema,
            None,
            &session,
            &[late_snapshot],
            CandidateSnapshotLedger::Untouched,
        )
        .await
        {
            Ok((tx, outcome)) => {
                tx.rollback().await.expect("late snapshot rollback");
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    });
    wait_for_blocked_application(&fixture.admin, "sm-pending-snapshot-waiter", blocker_pid).await;
    pending_winner
        .commit()
        .await
        .expect("commit pending before snapshot");
    assert!(matches!(
        snapshot_waiter.await.expect("pending snapshot waiter join"),
        Err(CandidateRouteError::Closed)
    ));
    assert_eq!(
        retained_snapshot.stanza.to_element(),
        frame(
            sources[0],
            "snapshot-loser",
            "ivy@example.test",
            1,
            "late snapshot input"
        )
        .stanza
        .to_element()
    );
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.sm_unacked WHERE stream_id='snapshot-loser'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("late snapshot wrote no frame");
    assert_eq!(count, 0);

    // Pause inside the production replacement INSERT, after DELETE has run.
    // Aborting the task must drop its owned transaction; a locking read then
    // waits for rollback and proves the original payload and owner survived.
    let jane: jid::BareJid = "jane@example.test".parse().expect("Jane JID");
    let cancel_old = frame(
        sources[0],
        "snapshot-cancel",
        "jane@example.test",
        1,
        "survives cancellation",
    );
    let mut tx = owner.begin().await.expect("cancellation setup transaction");
    route::append_complete_sources_if_open(&mut tx, &schema, &jane, &cancel_old)
        .await
        .expect("cancellation setup frame");
    tx.commit().await.expect("commit cancellation setup");
    sqlx::raw_sql(&format!(
        r#"
        CREATE FUNCTION {schema}.pause_cancelled_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
          IF NEW.stream_id = 'snapshot-cancel' THEN
            PERFORM pg_catalog.pg_advisory_xact_lock(20116, 2245);
          END IF;
          RETURN NEW;
        END; $$;
        CREATE TRIGGER pause_cancelled_snapshot BEFORE INSERT ON {schema}.sm_unacked
          FOR EACH ROW EXECUTE FUNCTION {schema}.pause_cancelled_snapshot();
    "#
    ))
    .execute(&fixture.admin)
    .await
    .expect("install cancellation test trigger");
    let cancel_blocker_db = Database::from_config(
        "snapshot cancellation blocker",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-cancel-blocker"),
        ),
    )
    .await
    .expect("cancellation blocker");
    let cancel_writer_db = Database::from_config(
        "snapshot cancellation writer",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-cancel-writer"),
        ),
    )
    .await
    .expect("cancellation writer");
    let mut cancel_blocker = cancel_blocker_db
        .begin()
        .await
        .expect("cancellation blocker transaction");
    cancel_blocker
        .execute("SELECT pg_catalog.pg_advisory_xact_lock(20116, 2245)", ())
        .await
        .expect("hold trigger lock");
    let blocker_pid = backend_pid(&mut cancel_blocker).await;
    let cancel_schema = schema.clone();
    let cancelled = tokio::spawn(async move {
        let session = snapshot_session("snapshot-cancel", "jane@example.test");
        let replacement = frame(
            sources[1],
            "snapshot-cancel",
            "jane@example.test",
            2,
            "cancelled replacement",
        );
        let tx = cancel_writer_db
            .begin()
            .await
            .expect("cancellation writer transaction");
        if let Ok((tx, _)) = route::store_snapshot_if_open(
            tx,
            &cancel_schema,
            None,
            &session,
            &[replacement],
            CandidateSnapshotLedger::Untouched,
        )
        .await
        {
            tx.rollback()
                .await
                .expect("unexpected completed snapshot rollback");
        }
    });
    let waiter_pid =
        wait_for_blocked_application(&fixture.admin, "sm-cancel-writer", blocker_pid).await;
    let blocked_sql: String =
        sqlx::query_scalar("SELECT query FROM pg_catalog.pg_stat_activity WHERE pid=$1")
            .bind(waiter_pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("blocked snapshot SQL");
    assert!(
        blocked_sql.contains("INSERT INTO sm_unacked"),
        "snapshot must have passed its DELETE: {blocked_sql}"
    );
    cancelled.abort();
    assert!(cancelled
        .await
        .expect_err("snapshot task was aborted")
        .is_cancelled());
    cancel_blocker
        .rollback()
        .await
        .expect("release cancellation trigger");
    let survivor: String = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        sqlx::query_scalar(&format!("SELECT u.stanza_xml FROM {schema}.sm_unacked u JOIN {schema}.sm_recipient_route_frame_owners f USING (stream_id, sequence) WHERE u.stream_id='snapshot-cancel' AND u.sequence=1 AND f.recipient='jane@example.test' FOR UPDATE OF u"))
            .fetch_one(&fixture.admin),
    ).await.expect("rollback finished before locking read").expect("cancelled snapshot preserved owner and payload");
    assert!(survivor.contains("survives cancellation"));
    let mut tx = owner
        .begin()
        .await
        .expect("pending after cancellation transaction");
    assert!(matches!(
        route::accept_pending_if_quiescent(
            &mut tx,
            &schema,
            &[sources[3]],
            &jane,
            &pending(jane.clone(), "must refuse retained frame"),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(CandidateRouteError::OutstandingSm)
    ));
    tx.rollback()
        .await
        .expect("pending after cancellation rollback");
    drop(cancel_blocker_db);

    // Reversed input order must wait on the same first source, not deadlock.
    let sorted_waiter_db = Database::from_config(
        "ordered route waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-sorted-waiter"),
        ),
    )
    .await
    .expect("ordered waiter");
    let mut ordered = owner.begin().await.expect("ordered owner transaction");
    assert!(route::lock_source_recipient_set(
        &mut ordered,
        &schema,
        &[(sources[1], alice.clone()), (sources[0], alice.clone())]
    )
    .await
    .expect("first ordered lock set"));
    let blocker_pid = backend_pid(&mut ordered).await;
    let sorted_schema = schema.clone();
    let sorted_recipient = alice.clone();
    let sorted = tokio::spawn(async move {
        let mut tx = sorted_waiter_db
            .begin()
            .await
            .expect("ordered waiter transaction");
        let open = route::lock_source_recipient_set(
            &mut tx,
            &sorted_schema,
            &[
                (sources[0], sorted_recipient.clone()),
                (sources[1], sorted_recipient),
            ],
        )
        .await;
        tx.rollback().await.expect("ordered waiter rollback");
        open
    });
    wait_for_blocked_application(&fixture.admin, "sm-sorted-waiter", blocker_pid).await;
    ordered.commit().await.expect("release first ordered set");
    assert!(sorted
        .await
        .expect("ordered waiter join")
        .expect("second ordered set"));

    // Competing creators with different recipients have no common route lock.
    // The candidate stream gate protects the previously absent session, and
    // the loser must rediscover the winner in a fresh transaction.
    let creator_db = Database::from_config(
        "new stream creator",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-creator"),
        ),
    )
    .await
    .expect("creator");
    let creator_waiter_db = Database::from_config(
        "new stream waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-creator-waiter"),
        ),
    )
    .await
    .expect("creator waiter");
    let creator_session = snapshot_session("new-stream", "dave@example.test");
    let creator_principal = snapshot_principal("dave@example.test");
    let creator_frame = frame(
        sources[0],
        "new-stream",
        "dave@example.test",
        1,
        "creator payload",
    );
    let creator = creator_db.begin().await.expect("creator transaction");
    let (mut creator, outcome) = route::store_snapshot_if_open(
        creator,
        &schema,
        Some(&creator_principal),
        &creator_session,
        &[creator_frame],
        CandidateSnapshotLedger::Untouched,
    )
    .await
    .expect("create stream snapshot");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    let creator_pid = backend_pid(&mut creator).await;
    let contender_schema = schema.clone();
    let contender = tokio::spawn(async move {
        let session = snapshot_session("new-stream", "erin@example.test");
        let principal = snapshot_principal("erin@example.test");
        let frame = frame(
            sources[1],
            "new-stream",
            "erin@example.test",
            1,
            "contender payload",
        );
        let tx = creator_waiter_db
            .begin()
            .await
            .expect("creator waiter transaction");
        let result = route::store_snapshot_if_open(
            tx,
            &contender_schema,
            Some(&principal),
            &session,
            &[frame],
            CandidateSnapshotLedger::Untouched,
        )
        .await;
        match result {
            Ok((tx, outcome)) => {
                tx.rollback().await.expect("creator waiter rollback");
                Ok(outcome)
            }
            Err(error) => Err(error),
        }
    });
    wait_for_blocked_application(&fixture.admin, "sm-creator-waiter", creator_pid).await;
    creator.commit().await.expect("commit creator");
    assert!(matches!(
        contender.await.expect("creator contender join"),
        Err(CandidateRouteError::Retry)
    ));
    let winner: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='new-stream' AND sequence=1"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("creator payload");
    assert!(winner.contains("creator payload"));

    drop(creator_db);
    drop(owner);
    drop(blocker_db);
    fixture.cleanup().await;
}
