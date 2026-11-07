//! PostgreSQL proof for the unselected, snapshot-only revision candidate.

use super::{
    application_url, backend_pid, frame, test_url, url_with_schema, wait_for_blocked_application,
    Fixture,
};
use uuid::Uuid;
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver};
use waddle_server::sm_persistence::recipient_route_interlock_candidate::{
    self as route, CandidateRouteError, CandidateSnapshotLedger, CandidateSnapshotOutcome,
};
use waddle_server::sm_persistence::snapshot_revision_candidate::{
    self as revision, Capture, CaptureMode, Readback, Revision, SnapshotInput,
};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedIngressAppend, PersistedSession, PersistedUnackedStanza,
};

struct Lab {
    fixture: Fixture,
    db: Database,
    schema: String,
}

impl Lab {
    async fn new(url: String) -> Self {
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
        let db = Database::from_config(
            "revision owner",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                url_with_schema(&fixture.base_url, &schema),
            ),
        )
        .await
        .expect("owner database");
        waddle_server::sm_persistence::bootstrap_portable_sm_owner_schema(&db)
            .await
            .expect("SM tables");
        sqlx::query(&format!(
            "CREATE TABLE {schema}.ingress_messages (message_key UUID PRIMARY KEY)"
        ))
        .execute(&fixture.admin)
        .await
        .expect("canonical sources");
        sqlx::raw_sql(&route::CANDIDATE_DDL.replace("__SCHEMA__", &schema))
            .execute(&fixture.admin)
            .await
            .expect("route candidate tables");
        sqlx::raw_sql(&revision::CANDIDATE_DDL.replace("__SCHEMA__", &schema))
            .execute(&fixture.admin)
            .await
            .expect("revision candidate tables");
        Self {
            fixture,
            db,
            schema,
        }
    }

    async fn add_source(&self, source: Uuid) {
        sqlx::query(&format!(
            "INSERT INTO {}.ingress_messages (message_key) VALUES ($1)",
            self.schema
        ))
        .bind(source)
        .execute(&self.fixture.admin)
        .await
        .expect("canonical source");
    }

    async fn seed(&self, stream: &str, recipient: &str, initial_source: Uuid) {
        self.add_source(initial_source).await;
        let generation = waddle_xmpp_core::OccupancySessionGeneration::mint().to_string();
        let context = Uuid::new_v4().to_string();
        sqlx::query(&format!("INSERT INTO {}.sm_sessions (stream_id,user_id,full_jid,occupancy_session,inbound_count,outbound_count,last_acked,max_resume_secs,detached_at_ms,max_resume_duration_ms,carbons_enabled,roster_interested,presence_available,presence_priority,bare_jid,auth_context_id,auth_context_version,principal_auth_epoch) VALUES ($1,'fixture',$2,$3,0,1,0,60,0,60000,0,0,0,0,$4,$5,1,1)", self.schema))
            .bind(stream).bind(format!("{recipient}/phone")).bind(generation).bind(recipient).bind(context)
            .execute(&self.fixture.admin).await.expect("initial session");
        let mut tx = self.db.begin().await.expect("seed transaction");
        route::append_complete_sources_if_open(
            &mut tx,
            &self.schema,
            &recipient.parse().expect("bare JID"),
            &frame(
                initial_source,
                stream,
                recipient,
                1,
                "original counted payload",
            ),
        )
        .await
        .expect("initial actual portable append");
        tx.commit().await.expect("commit initial frame");
    }

    async fn capture(&self, stream: &str, mode: CaptureMode) -> Capture {
        let tx = self.db.begin().await.expect("capture transaction");
        let (tx, capture) = revision::capture(tx, &self.schema, &SmSessionId::new(stream), mode)
            .await
            .expect("capture snapshot and revision");
        tx.commit().await.expect("commit capture");
        capture
    }

    async fn close(self) {
        drop(self.db);
        self.fixture.cleanup().await;
    }
}

#[derive(Clone)]
struct Plan {
    op: Uuid,
    revision: Revision,
    principal: Option<AuthenticatedPrincipalRef>,
    session: PersistedSession,
    frames: Vec<PersistedUnackedStanza>,
    ledger: CandidateSnapshotLedger,
}

impl Plan {
    fn input(&self) -> SnapshotInput<'_> {
        SnapshotInput {
            operation_id: self.op,
            revision: &self.revision,
            principal: self.principal.as_ref(),
            session: &self.session,
            unacked: &self.frames,
        }
    }
}

fn plan(capture: Capture, source: Uuid, body: &str) -> Plan {
    let mut session = capture.session;
    session.outbound_count = 2;
    let new_frame = frame(
        source,
        session.stream_id.as_str(),
        &session.jid.to_bare().to_string(),
        2,
        body,
    );
    let append = PersistedIngressAppend {
        key: new_frame.provenance.claims().expect("ingress claim")[0].clone(),
        accepting_stream: new_frame.stream_id.clone(),
        sequence: new_frame.sequence,
        appended_at: chrono::Utc::now(),
        payload: new_frame.stanza.as_ref().clone(),
        original_receipt_at: new_frame.original_receipt_at,
        disposition: IngressCustodyDisposition::Pending,
    };
    let mut frames = capture.unacked;
    frames.push(new_frame);
    Plan {
        op: Uuid::new_v4(),
        revision: capture.revision,
        principal: capture.principal,
        session,
        frames,
        ledger: CandidateSnapshotLedger::Exclusive(append),
    }
}

async fn apply<'a>(
    tx: waddle_server::db::Transaction<'a>,
    schema: &str,
    plan: &Plan,
) -> Result<(waddle_server::db::Transaction<'a>, CandidateSnapshotOutcome), CandidateRouteError> {
    revision::store(tx, schema, plan.input(), plan.ledger.clone()).await
}

async fn lookup(db: &Database, schema: &str, plan: &Plan) -> Result<Readback, CandidateRouteError> {
    let mut tx = db.begin().await.expect("readback transaction");
    let result = revision::readback(&mut tx, schema, plan.input(), &plan.ledger).await;
    tx.rollback().await.expect("readback rollback");
    result
}

async fn saved_body(lab: &Lab, stream: &str, sequence: i64) -> String {
    sqlx::query_scalar(&format!(
        "SELECT stanza_xml FROM {}.sm_unacked WHERE stream_id=$1 AND sequence=$2",
        lab.schema
    ))
    .bind(stream)
    .bind(sequence)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("durable payload")
}

#[tokio::test]
async fn snapshot_revision_both_winners_are_serialized_and_exact_operation_is_reconciled() {
    let Some(url) = test_url() else { return };
    let lab = Lab::new(url).await;
    for index in 0..2 {
        let stream = format!("cas-race-{index}");
        let recipient = format!("race{index}@example.test");
        let original_source = Uuid::new_v4();
        let source_a = Uuid::new_v4();
        let source_b = Uuid::new_v4();
        lab.seed(&stream, &recipient, original_source).await;
        lab.add_source(source_a).await;
        lab.add_source(source_b).await;
        let a = plan(
            lab.capture(&stream, CaptureMode::Current).await,
            source_a,
            "snapshot A",
        );
        let b = plan(
            lab.capture(&stream, CaptureMode::Current).await,
            source_b,
            "snapshot B",
        );
        let (winner, loser, winning_source, losing_source, body) = if index == 0 {
            (a, b, source_a, source_b, "snapshot A")
        } else {
            (b, a, source_b, source_a, "snapshot B")
        };
        // The capture token cannot be paired with a different principal, even
        // when its bare JID matches. Drift in the stored reference also fails
        // under the gate before any portable snapshot SQL executes.
        let original = winner.principal.clone().expect("captured principal");
        let mut different = winner.clone();
        different.principal = Some(AuthenticatedPrincipalRef::new(
            original.bare_jid().clone(),
            waddle_xmpp::auth::AuthContextId::new(Uuid::new_v4()),
            original.auth_context_version(),
            original.auth_epoch(),
        ));
        let tx = lab
            .db
            .begin()
            .await
            .expect("different principal transaction");
        assert!(matches!(
            apply(tx, &lab.schema, &different).await,
            Err(CandidateRouteError::GenerationChanged)
        ));
        let mut nil = winner.clone();
        nil.op = Uuid::nil();
        let tx = lab.db.begin().await.expect("nil operation transaction");
        assert!(matches!(
            apply(tx, &lab.schema, &nil).await,
            Err(CandidateRouteError::OperationIdentity)
        ));
        sqlx::query(&format!(
            "UPDATE {}.sm_sessions SET auth_context_id=$1 WHERE stream_id=$2",
            lab.schema
        ))
        .bind(Uuid::new_v4().to_string())
        .bind(&stream)
        .execute(&lab.fixture.admin)
        .await
        .expect("change stored principal");
        let tx = lab.db.begin().await.expect("changed principal transaction");
        assert!(matches!(
            apply(tx, &lab.schema, &winner).await,
            Err(CandidateRouteError::GenerationChanged)
        ));
        sqlx::query(&format!(
            "UPDATE {}.sm_sessions SET auth_context_id=$1 WHERE stream_id=$2",
            lab.schema
        ))
        .bind(original.auth_context_id().as_uuid().to_string())
        .bind(&stream)
        .execute(&lab.fixture.admin)
        .await
        .expect("restore test principal");
        let application = format!("cas-snapshot-waiter-{index}");
        let waiter_db = Database::from_config(
            "CAS waiter",
            &DatabaseConfig::new(
                DatabaseDriver::Postgres,
                application_url(&lab.fixture.base_url, &lab.schema, &application),
            ),
        )
        .await
        .expect("waiter pool");
        let tx = lab.db.begin().await.expect("winner transaction");
        let (mut winner_tx, outcome) = apply(tx, &lab.schema, &winner)
            .await
            .expect("winner snapshot");
        assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
        let blocker_pid = backend_pid(&mut winner_tx).await;
        let losing_copy = loser.clone();
        let waiter_schema = lab.schema.clone();
        let waiter = tokio::spawn(async move {
            let tx = waiter_db.begin().await.expect("loser transaction");
            match apply(tx, &waiter_schema, &losing_copy).await {
                Ok((tx, _)) => {
                    tx.rollback().await.expect("unexpected loser rollback");
                    Ok(())
                }
                Err(error) => Err(error),
            }
        });
        wait_for_blocked_application(&lab.fixture.admin, &application, blocker_pid).await;
        winner_tx.commit().await.expect("commit winner");
        assert!(matches!(
            waiter.await.expect("loser task"),
            Err(CandidateRouteError::StaleRevision)
        ));
        assert!(saved_body(&lab, &stream, 2).await.contains(body));
        let rows: Vec<String> = sqlx::query_scalar(&format!("SELECT message_key FROM {}.sm_ingress_appends WHERE accepting_stream_id=$1 ORDER BY message_key", lab.schema)).bind(&stream).fetch_all(&lab.fixture.admin).await.expect("ledger rows");
        assert_eq!(rows, vec![winning_source.to_string()]);
        assert!(!rows.contains(&losing_source.to_string()));
        assert_eq!(
            lookup(&lab.db, &lab.schema, &winner)
                .await
                .expect("positive readback"),
            Readback::Applied
        );
        assert_eq!(
            lookup(&lab.db, &lab.schema, &loser)
                .await
                .expect("loser readback"),
            Readback::Unconfirmed
        );
        let tx = lab.db.begin().await.expect("exact retry transaction");
        let (tx, outcome) = apply(tx, &lab.schema, &winner).await.expect("exact retry");
        assert_eq!(outcome, CandidateSnapshotOutcome::AlreadyApplied);
        tx.commit().await.expect("commit no-op retry");
        let mut altered = winner.clone();
        altered.session.inbound_count += 1;
        assert!(matches!(
            lookup(&lab.db, &lab.schema, &altered).await,
            Err(CandidateRouteError::OperationIdentity)
        ));
        assert!(saved_body(&lab, &stream, 2).await.contains(body));
    }
    lab.close().await;
}

#[tokio::test]
async fn snapshot_revision_rollback_cancel_terminal_and_generation_are_fail_closed() {
    let Some(url) = test_url() else { return };
    let lab = Lab::new(url).await;
    let stream = "cas-cancel";
    let recipient = "cancel@example.test";
    lab.seed(stream, recipient, Uuid::new_v4()).await;
    let source = Uuid::new_v4();
    lab.add_source(source).await;
    let captured = lab.capture(stream, CaptureMode::Current).await;
    let pending = plan(captured, source, "must roll back");
    let tx = lab.db.begin().await.expect("explicit rollback transaction");
    let (tx, _) = apply(tx, &lab.schema, &pending)
        .await
        .expect("snapshot before rollback");
    tx.rollback().await.expect("explicit rollback");
    assert_eq!(
        lookup(&lab.db, &lab.schema, &pending)
            .await
            .expect("rollback readback"),
        Readback::Unconfirmed
    );
    assert!(saved_body(&lab, stream, 1)
        .await
        .contains("original counted payload"));
    let rows: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_ingress_appends WHERE accepting_stream_id=$1",
        lab.schema
    ))
    .bind(stream)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("rollback ledger");
    assert_eq!(rows, 0);

    sqlx::raw_sql(&format!(
        r#"
        CREATE FUNCTION {0}.pause_revision_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
          IF NEW.stream_id = 'cas-cancel' AND NEW.sequence = 2 THEN
            PERFORM pg_catalog.pg_advisory_xact_lock(82212001);
          END IF;
          RETURN NEW;
        END $$;
        CREATE TRIGGER pause_revision_snapshot BEFORE INSERT ON {0}.sm_unacked
          FOR EACH ROW EXECUTE FUNCTION {0}.pause_revision_snapshot();
    "#,
        lab.schema
    ))
    .execute(&lab.fixture.admin)
    .await
    .expect("blocking trigger");
    let mut blocker = lab.fixture.admin.begin().await.expect("trigger blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock(82212001)")
        .execute(&mut *blocker)
        .await
        .expect("hold trigger gate");
    let pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *blocker)
        .await
        .expect("blocker pid");
    let app = "cas-cancel-writer";
    let writer_db = Database::from_config(
        "cancel writer",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            application_url(&lab.fixture.base_url, &lab.schema, app),
        ),
    )
    .await
    .expect("cancel pool");
    let writer_schema = lab.schema.clone();
    let in_flight = pending.clone();
    let writer = tokio::spawn(async move {
        let tx = writer_db.begin().await.expect("cancel transaction");
        if let Ok((tx, _)) = apply(tx, &writer_schema, &in_flight).await {
            tx.rollback().await.expect("unexpected complete rollback");
        }
    });
    wait_for_blocked_application(&lab.fixture.admin, app, pid).await;
    writer.abort();
    assert!(writer.await.expect_err("writer canceled").is_cancelled());
    blocker.rollback().await.expect("release blocker");
    let payload: String = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut tx = lab.fixture.admin.begin().await.expect("locking read");
        let payload = sqlx::query_scalar(&format!(
            "SELECT stanza_xml FROM {}.sm_unacked WHERE stream_id=$1 AND sequence=1 FOR UPDATE",
            lab.schema
        ))
        .bind(stream)
        .fetch_one(&mut *tx)
        .await
        .expect("old frame preserved");
        tx.rollback().await.expect("read rollback");
        payload
    })
    .await
    .expect("canceled transaction releases locks");
    assert!(payload.contains("original counted payload"));
    assert_eq!(
        lookup(&lab.db, &lab.schema, &pending)
            .await
            .expect("cancel readback"),
        Readback::Unconfirmed
    );
    let tx = lab.db.begin().await.expect("checked retry transaction");
    let (tx, outcome) = apply(tx, &lab.schema, &pending)
        .await
        .expect("retry performs CAS");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    tx.commit().await.expect("commit checked retry");
    assert!(saved_body(&lab, stream, 2).await.contains("must roll back"));

    let stale_capture = lab.capture(stream, CaptureMode::Current).await;
    let stale_source = Uuid::new_v4();
    lab.add_source(stale_source).await;
    let stale = plan(
        lab.capture(stream, CaptureMode::Current).await,
        stale_source,
        "stale after delete",
    );
    let tx = lab.db.begin().await.expect("retire transaction");
    let tx = revision::retire(tx, &lab.schema, &stale_capture)
        .await
        .expect("retire generation");
    tx.commit().await.expect("commit terminal authority");
    let terminal: bool = sqlx::query_scalar(&format!(
        "SELECT terminal FROM {}.sm_snapshot_candidate_authority WHERE stream_id=$1",
        lab.schema
    ))
    .bind(stream)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("terminal survives session deletion");
    assert!(terminal);
    assert_eq!(
        lookup(&lab.db, &lab.schema, &pending)
            .await
            .expect("durable historical receipt"),
        Readback::Applied
    );
    lab.seed(stream, recipient, Uuid::new_v4()).await;
    let tx = lab.db.begin().await.expect("terminal capture transaction");
    assert!(matches!(
        revision::capture(
            tx,
            &lab.schema,
            &SmSessionId::new(stream),
            CaptureMode::Current
        )
        .await,
        Err(CandidateRouteError::Terminal)
    ));
    let tx = lab.db.begin().await.expect("terminal stale transaction");
    assert!(matches!(
        apply(tx, &lab.schema, &stale).await,
        Err(CandidateRouteError::Terminal)
    ));
    let reopened = lab.capture(stream, CaptureMode::ReopenTerminal).await;
    let tx = lab.db.begin().await.expect("old generation transaction");
    assert!(matches!(
        apply(tx, &lab.schema, &stale).await,
        Err(CandidateRouteError::GenerationChanged)
    ));
    let generations: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_snapshot_candidate_generations WHERE stream_id=$1",
        lab.schema
    ))
    .bind(stream)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("generation history");
    assert_eq!(generations, 2);
    let fresh_source = Uuid::new_v4();
    lab.add_source(fresh_source).await;
    let fresh = plan(reopened, fresh_source, "new generation");
    let tx = lab.db.begin().await.expect("new generation transaction");
    let (tx, _) = apply(tx, &lab.schema, &fresh)
        .await
        .expect("new generation snapshot");
    tx.commit().await.expect("commit new generation");
    assert!(saved_body(&lab, stream, 2).await.contains("new generation"));
    let exhausted_stream = "cas-exhausted";
    lab.seed(exhausted_stream, "exhausted@example.test", Uuid::new_v4())
        .await;
    let _ = lab.capture(exhausted_stream, CaptureMode::Current).await;
    sqlx::query(&format!(
        "UPDATE {}.sm_snapshot_candidate_authority SET revision=$1 WHERE stream_id=$2",
        lab.schema
    ))
    .bind(i64::MAX)
    .bind(exhausted_stream)
    .execute(&lab.fixture.admin)
    .await
    .expect("exhaust revision");
    let source = Uuid::new_v4();
    lab.add_source(source).await;
    let exhausted = plan(
        lab.capture(exhausted_stream, CaptureMode::Current).await,
        source,
        "must not wrap",
    );
    let tx = lab.db.begin().await.expect("exhausted transaction");
    assert!(matches!(
        apply(tx, &lab.schema, &exhausted).await,
        Err(CandidateRouteError::RevisionExhausted)
    ));
    assert!(saved_body(&lab, exhausted_stream, 1)
        .await
        .contains("original counted payload"));
    lab.close().await;
}

#[tokio::test]
async fn snapshot_revision_does_not_fence_a_nonparticipating_append() {
    let Some(url) = test_url() else { return };
    let lab = Lab::new(url).await;
    let stream = "cas-limit";
    let recipient = "limit@example.test";
    lab.seed(stream, recipient, Uuid::new_v4()).await;
    let captured = lab.capture(stream, CaptureMode::Current).await;
    let source = Uuid::new_v4();
    lab.add_source(source).await;
    let mut tx = lab
        .db
        .begin()
        .await
        .expect("unversioned append transaction");
    route::append_complete_sources_if_open(
        &mut tx,
        &lab.schema,
        &recipient.parse().expect("bare JID"),
        &frame(source, stream, recipient, 2, "unversioned append"),
    )
    .await
    .expect("append shares gate but does not advance revision");
    tx.commit().await.expect("commit unversioned append");
    let tx = lab.db.begin().await.expect("snapshot after append");
    let (tx, outcome) = revision::store(
        tx,
        &lab.schema,
        SnapshotInput {
            operation_id: Uuid::new_v4(),
            revision: &captured.revision,
            principal: captured.principal.as_ref(),
            session: &captured.session,
            unacked: &captured.unacked,
        },
        CandidateSnapshotLedger::Untouched,
    )
    .await
    .expect("snapshot-only CAS cannot detect this writer");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    tx.commit().await.expect("commit experimental replacement");
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_unacked WHERE stream_id=$1 AND sequence=2",
        lab.schema
    ))
    .bind(stream)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("unparticipating append result");
    assert_eq!(
        count, 0,
        "the unselected snapshot-only seam is insufficient as a serving fence"
    );
    lab.close().await;
}

#[tokio::test]
async fn snapshot_revision_reconciles_a_lost_commit_reply() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Some(url) = test_url() else { return };
    let lab = Lab::new(url).await;
    let stream = "cas-lost-reply";
    lab.seed(stream, "reply@example.test", Uuid::new_v4()).await;
    let source = Uuid::new_v4();
    lab.add_source(source).await;
    let pending = plan(
        lab.capture(stream, CaptureMode::Current).await,
        source,
        "committed despite missing reply",
    );

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("loopback proxy");
    let port = listener.local_addr().expect("proxy address").port();
    let upstream = lab.fixture.admin_options.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.expect("proxy client");
        let server = crate::postgres_proxy::connect_local_postgres(&upstream)
            .await
            .expect("local PostgreSQL upstream");
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let forward = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
        });
        loop {
            let tag = server_read.read_u8().await.expect("server message tag");
            let length = server_read.read_u32().await.expect("server message length");
            assert!((4..=1_048_576).contains(&length));
            let mut body = vec![0; usize::try_from(length - 4).expect("message length")];
            server_read
                .read_exact(&mut body)
                .await
                .expect("message body");
            if tag == b'C' && body == b"COMMIT\0" {
                break;
            }
            client_write.write_u8(tag).await.expect("forward tag");
            client_write
                .write_u32(length)
                .await
                .expect("forward length");
            client_write.write_all(&body).await.expect("forward body");
        }
        forward.abort();
        let _ = forward.await;
        client_write.shutdown().await.expect("drop COMMIT reply");
    });
    let direct_url = url_with_schema(&lab.fixture.base_url, &lab.schema);
    let proxy_url = crate::postgres_proxy::local_proxy_url(&direct_url, port)
        .expect("local-only proxy URL")
        .to_string();
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, proxy_url);
    config.pool_size = 1;
    let proxied = Database::from_config("lost reply snapshot", &config)
        .await
        .expect("proxied database");
    let tx = proxied.begin().await.expect("proxied transaction");
    let (tx, outcome) = apply(tx, &lab.schema, &pending)
        .await
        .expect("snapshot before uncertain commit");
    assert!(matches!(outcome, CandidateSnapshotOutcome::Applied { .. }));
    assert!(
        tx.commit().await.is_err(),
        "client must not receive COMMIT reply"
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), proxy)
        .await
        .expect("proxy completes")
        .expect("proxy observed backend COMMIT");
    drop(proxied);

    assert_eq!(
        lookup(&lab.db, &lab.schema, &pending)
            .await
            .expect("positive readback after lost reply"),
        Readback::Applied
    );
    assert!(saved_body(&lab, stream, 2)
        .await
        .contains("committed despite missing reply"));
    let tx = lab.db.begin().await.expect("exact retry transaction");
    let (tx, outcome) = apply(tx, &lab.schema, &pending)
        .await
        .expect("exact retry after missing reply");
    assert_eq!(outcome, CandidateSnapshotOutcome::AlreadyApplied);
    tx.commit().await.expect("finish no-op retry");
    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_snapshot_candidate_operations WHERE operation_id=$1",
        lab.schema
    ))
    .bind(pending.op)
    .fetch_one(&lab.fixture.admin)
    .await
    .expect("single durable receipt");
    assert_eq!(count, 1);
    lab.close().await;
}
