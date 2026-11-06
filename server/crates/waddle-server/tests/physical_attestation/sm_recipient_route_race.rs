//! Dark PostgreSQL 17 proof that portable SM append and pending acceptance
//! contend on one canonical-source and recipient interlock.

use sqlx::Row;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
#[cfg(feature = "test-support")]
use waddle_server::sm_persistence::recipient_route_interlock_candidate as route;
use waddle_xmpp::pending_delivery::{
    InsertOutcome, PendingPayload, PendingRow, PendingRowId, QuotaPolicy, SmSessionId,
};
use waddle_xmpp::stream_management::persistence::PersistedUnackedStanza;
use waddle_xmpp::stream_management::SmFrameProvenance;
use waddle_xmpp::Stanza;

use super::{test_url, url_with_schema, wait_for_blocked_application, Fixture};

fn application_url(base: &str, schema: &str, application: &str) -> String {
    let mut url = url::Url::parse(&url_with_schema(base, schema)).expect("database URL");
    url.query_pairs_mut()
        .append_pair("application_name", application);
    url.to_string()
}

async fn temp_shadows(tx: &mut waddle_server::db::Transaction<'_>, schema: &str) {
    for relation in [
        "ingress_messages",
        "sm_recipient_route_source_guards",
        "sm_recipient_route_guards",
        "sm_recipient_route_frame_owners",
        "sm_sessions",
        "sm_unacked",
        "pending_delivery",
    ] {
        tx.execute(
            &format!("CREATE TEMP TABLE {relation} (LIKE {schema}.{relation} INCLUDING ALL)"),
            (),
        )
        .await
        .expect("hostile temporary shadow");
    }
}

async fn backend_pid(tx: &mut waddle_server::db::Transaction<'_>) -> i32 {
    tx.query("SELECT pg_catalog.pg_backend_pid()", ())
        .await
        .expect("backend pid query")
        .next()
        .await
        .expect("backend pid row")
        .expect("backend pid value")
        .get(0)
        .expect("backend pid decode")
}

async fn tx_count(tx: &mut waddle_server::db::Transaction<'_>, sql: &str) -> i64 {
    tx.query(sql, ())
        .await
        .expect("count query")
        .next()
        .await
        .expect("count row")
        .expect("count value")
        .get(0)
        .expect("count decode")
}

fn frame(
    source: uuid::Uuid,
    stream: &str,
    recipient: &str,
    sequence: u32,
    body: &str,
) -> PersistedUnackedStanza {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.to = Some(format!("{recipient}/phone").parse().expect("recipient JID"));
    message.bodies.insert(Default::default(), body.to_string());
    let resource = format!("{recipient}/phone").parse().expect("full JID");
    let claim = waddle_xmpp::stream_management::SmIngressAppendKey {
        message_key: waddle_xmpp::ingress::MessageKey::from_storage(source),
        kind: waddle_xmpp::stream_management::SmIngressReceiptKind::from_storage(1),
        semantic_identity_hash: [sequence as u8; 32],
        resource,
    };
    PersistedUnackedStanza {
        provenance: SmFrameProvenance::ingress([claim]),
        ingress_receipts: Vec::new(),
        stream_id: SmSessionId::new(stream),
        sequence,
        stanza: Box::new(Stanza::Message(message)),
        original_receipt_at: chrono::Utc::now(),
    }
}

fn pending(recipient: jid::BareJid, body: &str) -> PendingRow {
    let mut message = xmpp_parsers::message::Message::new(None::<jid::Jid>);
    message.bodies.insert(Default::default(), body.to_string());
    PendingRow {
        id: PendingRowId::new(uuid::Uuid::new_v4().to_string()),
        recipient,
        original_receipt_at: chrono::Utc::now(),
        payload: PendingPayload::Transient(Box::new(message)),
        flushed_in_session: None,
        outbound_sequence: None,
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn portable_append_and_pending_acceptance_share_route_lock_and_preserve_refused_frame() {
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
        "route race owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &schema),
        ),
    )
    .await
    .expect("owner database");
    waddle_server::sm_persistence::bootstrap_portable_sm_owner_schema(&owner)
        .await
        .expect("prepare SM tables");
    waddle_server::pending_delivery::bootstrap_pending_delivery_owner_schema(&owner)
        .await
        .expect("prepare pending tables");
    sqlx::query(&format!(
        "CREATE TABLE {schema}.ingress_messages (message_key UUID PRIMARY KEY)"
    ))
    .execute(&fixture.admin)
    .await
    .expect("minimal canonical-source fixture");
    let ddl = route::CANDIDATE_DDL.replace("__SCHEMA__", &schema);
    sqlx::raw_sql(&ddl)
        .execute(&fixture.admin)
        .await
        .expect("candidate-only route schema");

    let source_a = uuid::Uuid::new_v4();
    let source_b = uuid::Uuid::new_v4();
    let source_c = uuid::Uuid::new_v4();
    let source_d = uuid::Uuid::new_v4();
    let source_e = uuid::Uuid::new_v4();
    let source_f = uuid::Uuid::new_v4();
    for source in [source_a, source_b, source_c, source_d, source_e, source_f] {
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_messages (message_key) VALUES ($1)"
        ))
        .bind(source)
        .execute(&fixture.admin)
        .await
        .expect("canonical source");
    }
    for (stream, bare, outbound_count) in [
        ("route-race-a", "alice@example.test", 1),
        ("route-race-b", "carol@example.test", 0),
        ("route-race-c", "bob@example.test", 0),
        ("route-race-e", "erin@example.test", 0),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {schema}.sm_sessions (stream_id,user_id,full_jid,inbound_count,outbound_count,last_acked,detached_at_ms,max_resume_duration_ms,carbons_enabled,roster_interested,presence_available,presence_priority,bare_jid) VALUES ($1,'fixture',$2,0,$4,0,0,60000,0,0,0,0,$3)"
        ))
        .bind(stream)
        .bind(format!("{bare}/phone"))
        .bind(bare)
        .bind(outbound_count)
        .execute(&fixture.admin)
        .await
        .expect("session inventory row");
    }

    let writer_db = Database::from_config(
        "route append winner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-route-append-winner"),
        ),
    )
    .await
    .expect("append writer database");
    let accept_db = Database::from_config(
        "route acceptance waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-route-accept-waiter"),
        ),
    )
    .await
    .expect("acceptance waiter database");

    // Writer wins first. Acceptance waits on the canonical source row, sees
    // the committed production replay row, refuses, and leaves it intact.
    let alice: jid::BareJid = "alice@example.test".parse().expect("bare JID");
    let mut writer_tx = writer_db.begin().await.expect("writer transaction");
    temp_shadows(&mut writer_tx, &schema).await;
    route::append_unacked_if_open(
        &mut writer_tx,
        &schema,
        source_a,
        &alice,
        &frame(
            source_a,
            "route-race-a",
            "alice@example.test",
            1,
            "counted before pending acceptance",
        ),
    )
    .await
    .expect("portable append under shared route lock");
    let blocker_pid = backend_pid(&mut writer_tx).await;
    let accept_owner = accept_db.clone();
    let accept_schema = schema.clone();
    let accept_row = pending(alice.clone(), "must remain unaccepted");
    let accept = tokio::spawn(async move {
        let mut tx = accept_owner.begin().await.expect("accept transaction");
        temp_shadows(&mut tx, &accept_schema).await;
        let result = route::accept_pending_if_quiescent(
            &mut tx,
            &accept_schema,
            &[source_a],
            &alice,
            &accept_row,
            QuotaPolicy::Unlimited,
        )
        .await;
        let _ = tx.rollback().await;
        result
    });
    wait_for_blocked_application(&fixture.admin, "sm-route-accept-waiter", blocker_pid).await;
    let temp_unacked = tx_count(&mut writer_tx, "SELECT count(*) FROM pg_temp.sm_unacked").await;
    assert_eq!(temp_unacked, 0);
    writer_tx.commit().await.expect("commit append winner");
    assert!(matches!(
        accept.await.expect("accept task"),
        Err(route::CandidateRouteError::OutstandingSm)
    ));
    let saved_xml: String = sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {schema}.sm_unacked WHERE stream_id='route-race-a' AND sequence=1"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("refused source remains durable");
    assert!(saved_xml.contains("counted before pending acceptance"));

    // The stream itself must belong to the claimed recipient. A matching
    // stanza and source claim cannot relabel another recipient's session.
    let mut wrong_stream = writer_db.begin().await.expect("wrong-stream transaction");
    assert!(matches!(
        route::append_unacked_if_open(
            &mut wrong_stream,
            &schema,
            source_b,
            &"alice@example.test".parse().expect("Alice JID"),
            &frame(
                source_b,
                "route-race-b",
                "alice@example.test",
                2,
                "wrong stream"
            ),
        )
        .await,
        Err(route::CandidateRouteError::Identity)
    ));
    wrong_stream
        .rollback()
        .await
        .expect("wrong-stream rollback");

    // The single-source candidate must not accept a second, unlocked source
    // hidden in a multi-claim replay frame.
    let carol: jid::BareJid = "carol@example.test".parse().expect("Carol JID");
    let mut mixed = frame(
        source_b,
        "route-race-b",
        "carol@example.test",
        2,
        "mixed claims",
    );
    let first_claim = mixed.provenance.claims().expect("ingress claims")[0].clone();
    let mut missing_claim = first_claim.clone();
    missing_claim.message_key =
        waddle_xmpp::ingress::MessageKey::from_storage(uuid::Uuid::new_v4());
    mixed.provenance = SmFrameProvenance::ingress([first_claim, missing_claim]);
    let mut multi_claim = writer_db.begin().await.expect("multi-claim transaction");
    assert!(matches!(
        route::append_unacked_if_open(&mut multi_claim, &schema, source_b, &carol, &mixed).await,
        Err(route::CandidateRouteError::Identity)
    ));
    multi_claim.rollback().await.expect("multi-claim rollback");

    // Later session identity drift cannot hide a frame. The candidate ledger
    // records the checked recipient at append time and is tied to the frame.
    sqlx::query(&format!(
        "UPDATE {schema}.sm_sessions SET bare_jid='bob@example.test', full_jid='bob@example.test/phone' WHERE stream_id='route-race-a'"
    ))
    .execute(&fixture.admin)
    .await
    .expect("mutate session identity after append");
    let alice: jid::BareJid = "alice@example.test".parse().expect("Alice JID");
    let mut drifted = accept_db.begin().await.expect("identity-drift transaction");
    assert!(matches!(
        route::accept_pending_if_quiescent(
            &mut drifted,
            &schema,
            &[source_a],
            &alice,
            &pending(alice.clone(), "must still refuse"),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(route::CandidateRouteError::OutstandingSm)
    ));
    drifted.rollback().await.expect("identity-drift rollback");

    // A caller-owned stale snapshot is rejected before taking the route lock.
    let mut stale = accept_db.begin().await.expect("stale-snapshot transaction");
    stale
        .execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ", ())
        .await
        .expect("set stale isolation");
    tx_count(&mut stale, "SELECT count(*) FROM sm_sessions").await;
    assert!(matches!(
        route::accept_pending_if_quiescent(
            &mut stale,
            &schema,
            &[source_b],
            &carol,
            &pending(carol.clone(), "stale snapshot"),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(route::CandidateRouteError::ReadCommittedRequired)
    ));
    stale.rollback().await.expect("stale-snapshot rollback");

    // Acceptance wins on a second recipient. The writer waits, then observes
    // the closed route and refuses; the actual pending insert remains durable.
    let accept_winner_db = Database::from_config(
        "route acceptance winner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-route-accept-winner"),
        ),
    )
    .await
    .expect("acceptance winner database");
    let writer_waiter_db = Database::from_config(
        "route writer waiter",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&fixture.base_url, &schema, "sm-route-writer-waiter"),
        ),
    )
    .await
    .expect("writer waiter database");
    let mut accept_tx = accept_winner_db
        .begin()
        .await
        .expect("accept winner transaction");
    temp_shadows(&mut accept_tx, &schema).await;
    route::accept_pending_if_quiescent(
        &mut accept_tx,
        &schema,
        &[source_b],
        &carol,
        &pending(carol.clone(), "accepted while route was quiescent"),
        QuotaPolicy::Unlimited,
    )
    .await
    .expect("real pending insert under route lock");
    assert_eq!(
        tx_count(
            &mut accept_tx,
            "SELECT count(*) FROM pg_temp.pending_delivery"
        )
        .await,
        0,
        "TEMP pending relation must not shadow the trusted target"
    );
    let blocker_pid = backend_pid(&mut accept_tx).await;
    let writer_owner = writer_waiter_db.clone();
    let writer_schema = schema.clone();
    let writer = tokio::spawn(async move {
        let mut tx = writer_owner.begin().await.expect("late writer transaction");
        temp_shadows(&mut tx, &writer_schema).await;
        let result = route::append_unacked_if_open(
            &mut tx,
            &writer_schema,
            source_b,
            &carol,
            &frame(
                source_b,
                "route-race-b",
                "carol@example.test",
                1,
                "late counted frame",
            ),
        )
        .await;
        if result.is_ok() {
            tx.commit().await.expect("late writer commit");
        } else {
            let _ = tx.rollback().await;
        }
        result
    });
    wait_for_blocked_application(&fixture.admin, "sm-route-writer-waiter", blocker_pid).await;
    accept_tx.commit().await.expect("commit acceptance winner");
    assert!(matches!(
        writer.await.expect("writer task"),
        Err(route::CandidateRouteError::Closed)
    ));
    let row = sqlx::query(&format!(
        "SELECT transient_xml FROM {schema}.pending_delivery WHERE recipient_jid='carol@example.test'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("accepted pending row remains durable");
    let pending_xml: Option<String> = row.try_get(0).expect("pending payload");
    assert!(pending_xml
        .unwrap_or_default()
        .contains("accepted while route was quiescent"));
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.sm_unacked WHERE stream_id='route-race-b'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("late append count");
    assert_eq!(count, 0);
    sqlx::query(&format!(
        "DELETE FROM {schema}.sm_unacked WHERE stream_id='route-race-a' AND sequence=1"
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove acknowledged fixture frame");
    let owner_count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {schema}.sm_recipient_route_frame_owners WHERE stream_id='route-race-a'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("owner cascade count");
    assert_eq!(
        owner_count, 0,
        "frame delete must cascade its candidate owner"
    );

    // Different canonical sources for Erin do not contend on source rows.
    // The recipient guard itself must serialize both winner orderings.
    let erin: jid::BareJid = "erin@example.test".parse().expect("Erin JID");
    let mut erin_writer = writer_db.begin().await.expect("Erin append winner");
    route::append_unacked_if_open(
        &mut erin_writer,
        &schema,
        source_e,
        &erin,
        &frame(
            source_e,
            "route-race-e",
            "erin@example.test",
            1,
            "Erin replay",
        ),
    )
    .await
    .expect("Erin counted append");
    let blocker_pid = backend_pid(&mut erin_writer).await;
    let accept_owner = accept_db.clone();
    let accept_schema = schema.clone();
    let erin_waiter = erin.clone();
    let wait_accept = tokio::spawn(async move {
        let mut tx = accept_owner.begin().await.expect("Erin accept waiter");
        let result = route::accept_pending_if_quiescent(
            &mut tx,
            &accept_schema,
            &[source_f],
            &erin_waiter,
            &pending(erin_waiter.clone(), "must wait on recipient"),
            QuotaPolicy::Unlimited,
        )
        .await;
        let _ = tx.rollback().await;
        result
    });
    let waiter =
        wait_for_blocked_application(&fixture.admin, "sm-route-accept-waiter", blocker_pid).await;
    let blocked_sql: String =
        sqlx::query_scalar("SELECT query FROM pg_catalog.pg_stat_activity WHERE pid=$1")
            .bind(waiter)
            .fetch_one(&fixture.admin)
            .await
            .expect("Erin blocked SQL");
    assert!(blocked_sql.contains("sm_recipient_route_guards"));
    erin_writer.commit().await.expect("commit Erin append");
    assert!(matches!(
        wait_accept.await.expect("Erin accept waiter task"),
        Err(route::CandidateRouteError::OutstandingSm)
    ));
    sqlx::query(&format!(
        "DELETE FROM {schema}.sm_unacked WHERE stream_id='route-race-e' AND sequence=1"
    ))
    .execute(&fixture.admin)
    .await
    .expect("clear Erin counted fixture");

    let mut erin_accept = accept_winner_db.begin().await.expect("Erin accept winner");
    assert_eq!(
        route::accept_pending_if_quiescent(
            &mut erin_accept,
            &schema,
            &[source_f],
            &erin,
            &pending(erin.clone(), "Erin accepted"),
            QuotaPolicy::Unlimited,
        )
        .await
        .expect("Erin accepted pending"),
        InsertOutcome::Inserted
    );
    let blocker_pid = backend_pid(&mut erin_accept).await;
    let writer_owner = writer_waiter_db.clone();
    let writer_schema = schema.clone();
    let erin_waiter = erin.clone();
    let wait_writer = tokio::spawn(async move {
        let mut tx = writer_owner.begin().await.expect("Erin writer waiter");
        let result = route::append_unacked_if_open(
            &mut tx,
            &writer_schema,
            source_e,
            &erin_waiter,
            &frame(
                source_e,
                "route-race-e",
                "erin@example.test",
                2,
                "late Erin replay",
            ),
        )
        .await;
        let _ = tx.rollback().await;
        result
    });
    let waiter =
        wait_for_blocked_application(&fixture.admin, "sm-route-writer-waiter", blocker_pid).await;
    let blocked_sql: String =
        sqlx::query_scalar("SELECT query FROM pg_catalog.pg_stat_activity WHERE pid=$1")
            .bind(waiter)
            .fetch_one(&fixture.admin)
            .await
            .expect("Erin writer blocked SQL");
    assert!(blocked_sql.contains("sm_recipient_route_guards"));
    erin_accept.commit().await.expect("commit Erin acceptance");
    assert!(matches!(
        wait_writer.await.expect("Erin writer waiter task"),
        Err(route::CandidateRouteError::Closed)
    ));

    // A quota refusal is a successful, committable outcome. It must leave
    // the route open so a later counted frame can still be saved.
    let bob: jid::BareJid = "bob@example.test".parse().expect("Bob JID");
    let mut capped = accept_db.begin().await.expect("quota transaction");
    let outcome = route::accept_pending_if_quiescent(
        &mut capped,
        &schema,
        &[source_c],
        &bob,
        &pending(bob.clone(), "quota refused"),
        QuotaPolicy::CountCap { max_rows: 0 },
    )
    .await
    .expect("quota outcome");
    assert_eq!(outcome, InsertOutcome::QuotaExceeded);
    capped.commit().await.expect("commit quota refusal");
    let closed: bool = sqlx::query_scalar(&format!(
        "SELECT closed FROM {schema}.sm_recipient_route_guards WHERE recipient='bob@example.test'"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("quota route status");
    assert!(!closed);
    let mut after_quota = writer_db
        .begin()
        .await
        .expect("post-quota append transaction");
    route::append_unacked_if_open(
        &mut after_quota,
        &schema,
        source_c,
        &bob,
        &frame(
            source_c,
            "route-race-c",
            "bob@example.test",
            1,
            "saved after quota refusal",
        ),
    )
    .await
    .expect("append after quota refusal");
    after_quota
        .commit()
        .await
        .expect("commit post-quota append");

    // An untracked frame could belong to any recipient. The candidate must
    // refuse even an unrelated route until that frame is accounted for.
    sqlx::query(&format!(
        "DELETE FROM {schema}.sm_recipient_route_frame_owners WHERE stream_id='route-race-c' AND sequence=1"
    ))
    .execute(&fixture.admin)
    .await
    .expect("simulate legacy untracked frame");
    let dave: jid::BareJid = "dave@example.test".parse().expect("Dave JID");
    let mut unknown = accept_db.begin().await.expect("unknown-owner transaction");
    assert!(matches!(
        route::accept_pending_if_quiescent(
            &mut unknown,
            &schema,
            &[source_d],
            &dave,
            &pending(dave.clone(), "must refuse unknown frame"),
            QuotaPolicy::Unlimited,
        )
        .await,
        Err(route::CandidateRouteError::OutstandingSm)
    ));
    unknown.rollback().await.expect("unknown-owner rollback");

    drop(writer_db);
    drop(accept_db);
    drop(accept_winner_db);
    drop(writer_waiter_db);
    drop(owner);
    fixture.cleanup().await;
}
