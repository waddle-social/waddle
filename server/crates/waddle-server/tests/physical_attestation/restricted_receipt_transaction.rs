//! Real PostgreSQL schedules for the dark fixed receipt transaction.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig, RestrictedEffectReceipt,
    RestrictedIngressProfileObservation, RestrictedReceiptPilot, RestrictedReceiptReadback,
    RestrictedReceiptWrite,
};
use waddle_server::db::Database;
use waddle_server::ingress_substrate::EffectReceiptKind;
use waddle_server::test_support::database_from_attested_postgres_pool;
use waddle_xmpp::ingress::MessageKey;

use super::super::{proxy_runtime_options, Fixture};

fn database(fixture: &Fixture, config: PhysicalAttestationConfig, name: &str) -> Database {
    let pool = attested_pool(
        fixture.runtime_options.clone().application_name(name),
        config,
        2,
        Duration::from_secs(2),
    )
    .expect("fixture attested pool");
    database_from_attested_postgres_pool(name, pool)
}

fn pilot(
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) -> RestrictedReceiptPilot {
    RestrictedReceiptPilot::new(db.clone(), db, api.clone(), expected.clone()).expect("exact pool")
}

async fn seed(fixture: &Fixture, count: u8) -> Vec<RestrictedEffectReceipt> {
    let mut connection = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("seed runtime");
    let mut tx = connection.begin().await.expect("seed transaction");
    let schema = &fixture.target;
    sqlx::query(&format!(
        "LOCK TABLE ONLY {schema}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *tx)
    .await
    .expect("seed epoch lock");
    let message = MessageKey::new();
    let inserted: bool = sqlx::query_scalar(&format!(
        "SELECT {schema}.waddle_ingress_insert_message($1,1, \
         pg_catalog.decode(pg_catalog.repeat('5a',32),'hex'),NULL::smallint,NULL::bytea, \
         NULL::text,NULL::uuid,NULL::numeric,NULL::numeric,NULL::uuid,NULL::numeric)"
    ))
    .bind(message.to_storage())
    .fetch_one(&mut *tx)
    .await
    .expect("seed message");
    assert!(inserted);
    let mut receipts = Vec::new();
    for index in 0..count {
        let identity = RestrictedEffectReceipt {
            message,
            kind: EffectReceiptKind::from_storage(i32::from(index) + 1),
            semantic_identity_hash: [index; 32],
        };
        let inserted: bool = sqlx::query_scalar(&format!(
            "SELECT {schema}.waddle_ingress_insert_effect_intent($1, $2::integer::numeric, $3, $4, $5)"
        )).bind(message.to_storage()).bind(i32::from(index) + 1)
          .bind(identity.kind.to_storage()).bind(identity.semantic_identity_hash.as_slice())
          .bind([1u8].as_slice()).fetch_one(&mut *tx).await.expect("seed intent");
        assert!(inserted);
        receipts.push(identity);
    }
    tx.commit().await.expect("commit seed");
    receipts
}

pub(super) async fn exercise(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    let runtime_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(&fixture.runtime)
        .fetch_one(&fixture.admin)
        .await
        .expect("runtime OID");
    let api_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(api.as_str())
        .fetch_one(&fixture.admin)
        .await
        .expect("API OID");
    let expected = RestrictedIngressProfileObservation {
        manifest_version: 1,
        schema_oid: fixture
            .lineage
            .postgres_identity
            .as_ref()
            .expect("identity")
            .schema
            .oid,
        epoch_relation_oid: epoch_oid,
        runtime_role_oid: runtime_oid.0,
        api_owner_oid: api_oid.0,
    };
    let receipts = seed(fixture, 10).await;
    let db = database(fixture, config.clone(), "restricted-receipt");
    let writer = pilot(&db, api, &expected);
    let other = database(fixture, config.clone(), "restricted-receipt-other");
    assert!(RestrictedReceiptPilot::new(other, &db, api.clone(), expected.clone()).is_err());
    for field in ["schema", "epoch", "runtime", "api", "version"] {
        let mut wrong = expected.clone();
        match field {
            "schema" => wrong.schema_oid += 1,
            "epoch" => wrong.epoch_relation_oid += 1,
            "runtime" => wrong.runtime_role_oid += 1,
            "api" => wrong.api_owner_oid += 1,
            "version" => wrong.manifest_version += 1,
            _ => unreachable!(),
        }
        match RestrictedReceiptPilot::new(db.clone(), &db, api.clone(), wrong) {
            Err(_) => assert!(matches!(field, "schema" | "version")),
            Ok(wrong) => {
                assert!(wrong.record(&receipts[0]).await.is_err(), "wrong {field}");
                assert_eq!(
                    wrong.readback(&receipts[0]).await,
                    RestrictedReceiptReadback::Indeterminate
                );
            }
        }
    }
    assert!(pilot(
        &db,
        &PgRoleName::new(&fixture.runtime).expect("role"),
        &expected
    )
    .record(&receipts[0])
    .await
    .is_err());
    let mut lineage = fixture.lineage.clone();
    lineage
        .postgres_identity
        .as_mut()
        .expect("identity")
        .system_identifier
        .0 += 1;
    let wrong = database(
        fixture,
        fixture.with_lineage(lineage),
        "restricted-receipt-wrong-lineage",
    );
    assert!(pilot(&wrong, api, &expected)
        .record(&receipts[0])
        .await
        .is_err());
    let mut wrong_database = fixture.lineage.clone();
    wrong_database
        .postgres_identity
        .as_mut()
        .expect("identity")
        .database
        .oid += 1;
    let wrong = database(
        fixture,
        fixture.with_lineage(wrong_database),
        "restricted-receipt-wrong-database",
    );
    assert!(pilot(&wrong, api, &expected)
        .record(&receipts[0])
        .await
        .is_err());
    let mut wrong_schema = fixture.lineage.clone();
    wrong_schema
        .postgres_identity
        .as_mut()
        .expect("identity")
        .schema
        .name
        .0 = "public".to_owned();
    let wrong = database(
        fixture,
        fixture.with_lineage(wrong_schema),
        "restricted-receipt-wrong-schema",
    );
    assert!(pilot(&wrong, api, &expected)
        .record(&receipts[0])
        .await
        .is_err());
    assert!(attested_pool(
        fixture.runtime_options.clone().username(&fixture.outsider),
        config.clone(),
        1,
        Duration::from_secs(1)
    )
    .is_err());

    let mut raw = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime login");
    for sql in [
        format!("SELECT * FROM {}.ingress_effect_receipts", fixture.target),
        format!(
            "INSERT INTO {}.ingress_effect_receipts DEFAULT VALUES",
            fixture.target
        ),
        format!(
            "UPDATE {}.ingress_effect_receipts SET kind=1 WHERE false",
            fixture.target
        ),
        format!(
            "DELETE FROM {}.ingress_effect_receipts WHERE false",
            fixture.target
        ),
    ] {
        assert!(
            sqlx::query(&sql).execute(&mut raw).await.is_err(),
            "runtime direct access: {sql}"
        );
    }
    drop(raw);
    assert_eq!(
        writer.readback(&receipts[0]).await,
        RestrictedReceiptReadback::Indeterminate
    );
    assert_eq!(
        writer.record(&receipts[0]).await.expect("write"),
        RestrictedReceiptWrite::Committed { inserted: true }
    );
    assert_eq!(
        writer.record(&receipts[0]).await.expect("duplicate"),
        RestrictedReceiptWrite::Committed { inserted: false }
    );
    assert_eq!(
        writer.readback(&receipts[0]).await,
        RestrictedReceiptReadback::Exists
    );
    let wrong_hash = RestrictedEffectReceipt {
        semantic_identity_hash: [99; 32],
        ..receipts[0]
    };
    assert_eq!(
        writer.readback(&wrong_hash).await,
        RestrictedReceiptReadback::Indeterminate
    );
    assert!(writer
        .record_rolled_back(&receipts[1])
        .await
        .expect("rollback"));
    assert_eq!(
        writer.readback(&receipts[1]).await,
        RestrictedReceiptReadback::Indeterminate
    );

    concurrent(fixture, &db, api, &expected, [receipts[2], receipts[2]]).await;
    waited_drift(fixture, &db, api, &expected, receipts[4]).await;
    replacement_epoch_view(fixture, &db, api, &expected, receipts[9]).await;
    reconnect(fixture, config, api, &expected, receipts[5]).await;
    lost_commit(fixture, config, api, &expected, receipts[6], &writer).await;
    delayed_commit(fixture, config, api, &expected, receipts[7], &writer).await;
    cancelled_commit(fixture, config, api, &expected, receipts[8], &writer).await;
}

async fn concurrent(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipts: [RestrictedEffectReceipt; 2],
) {
    let one = pilot(db, api, expected);
    let two = pilot(db, api, expected);
    let (ready1, got1) = oneshot::channel();
    let (ready2, got2) = oneshot::channel();
    let (release1, wait1) = oneshot::channel();
    let (release2, wait2) = oneshot::channel();
    let [r1, r2] = receipts;
    let first = tokio::spawn(async move { one.record_paused(&r1, ready1, wait1).await });
    let second = tokio::spawn(async move { two.record_paused(&r2, ready2, wait2).await });
    let pids = [
        got1.await.expect("first ready"),
        got2.await.expect("second ready"),
    ];
    assert_ne!(pids[0], pids[1]);
    for pid in pids {
        let held: (i64, bool) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE locktype='advisory' AND mode='ShareLock' AND granted), \
             bool_or(relation=$2 AND mode='AccessShareLock' AND granted) FROM pg_catalog.pg_locks WHERE pid=$1"
        ).bind(pid).bind(Oid(expected.epoch_relation_oid)).fetch_one(&fixture.admin).await.expect("locks");
        assert_eq!(held, (2, true));
    }
    release1.send(()).expect("release first");
    release2.send(()).expect("release second");
    let results = [
        first.await.expect("first task"),
        second.await.expect("second task"),
    ];
    let mut inserted = results.map(|result| match result.expect("write") {
        RestrictedReceiptWrite::Committed { inserted } => inserted,
        RestrictedReceiptWrite::Indeterminate => panic!("concurrent COMMIT unacknowledged"),
    });
    inserted.sort();
    assert_eq!(inserted, [false, true]);
}

async fn waited_drift(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
) {
    let s = &fixture.target;
    let function = format!("{s}.waddle_ingress_effect_receipt_exists(uuid,integer,bytea,text)");
    let mut owner = fixture.admin.begin().await.expect("AX owner");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *owner)
    .await
    .expect("hold AX");
    let candidate = pilot(db, api, expected);
    let task = tokio::spawn(async move { candidate.record(&receipt).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity a JOIN pg_catalog.pg_locks l ON l.pid=a.pid \
                 WHERE a.application_name='restricted-receipt' AND l.relation=$1 AND l.mode='AccessShareLock' AND NOT l.granted)"
            ).bind(Oid(expected.epoch_relation_oid)).fetch_one(&fixture.admin).await.expect("waiter");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("actual AS wait");
    sqlx::query(&format!(
        "REVOKE EXECUTE ON FUNCTION {function} FROM {}",
        fixture.runtime
    ))
    .execute(&mut *owner)
    .await
    .expect("drift after wait begins");
    owner.commit().await.expect("publish drift");
    assert!(
        task.await.expect("waiting task").is_err(),
        "fresh profile must reject drift"
    );
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION {function} TO {}",
        fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore grant");
    assert_eq!(
        pilot(db, api, expected).readback(&receipt).await,
        RestrictedReceiptReadback::Indeterminate
    );
}

async fn reconnect(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
) {
    let name = "restricted-receipt-reconnect";
    let pool = attested_pool(
        fixture.runtime_options.clone().application_name(name),
        config.clone(),
        1,
        Duration::from_secs(2),
    )
    .expect("one-slot reconnect pool");
    let db = database_from_attested_postgres_pool(name, pool);
    let candidate = pilot(&db, api, expected);
    let (ready, got) = oneshot::channel();
    let (release, wait) = oneshot::channel();
    let task = tokio::spawn(async move { candidate.record_paused(&receipt, ready, wait).await });
    let pid = got.await.expect("backend");
    release.send(()).expect("release");
    assert_eq!(
        task.await.expect("task").expect("write"),
        RestrictedReceiptWrite::Committed { inserted: true }
    );
    let terminated: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&fixture.admin)
        .await
        .expect("terminate idle backend");
    assert!(terminated);
    assert_eq!(
        pilot(&db, api, expected).readback(&receipt).await,
        RestrictedReceiptReadback::Exists
    );
    let replacement: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name=$1 AND pid<>$2",
    )
    .bind(name)
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("one-slot pool reconnected on a new backend");
    assert_ne!(replacement, pid);
    drop(db);
}

async fn terminal(fixture: &Fixture, pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let active: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND xact_start IS NOT NULL) \
                 OR EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory')"
            ).bind(pid).fetch_one(&fixture.admin).await.expect("backend terminal");
            if !active { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("original backend terminal and locks released");
}

async fn lost_commit(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
    reader: &RestrictedReceiptPilot,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let port = listener.local_addr().expect("port").port();
    let (held, waiting) = oneshot::channel();
    let (disconnect, disconnected) = oneshot::channel();
    let proxy = tokio::spawn(lose_receipt_commit_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        disconnected,
    ));
    let options = proxy_runtime_options(fixture, port).application_name("receipt-lost-commit");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("receipt-lost-commit", pool);
    let candidate = pilot(&db, api, expected);
    let task = tokio::spawn(async move { candidate.record(&receipt).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT response held")
        .expect("server emitted CommandComplete COMMIT");
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='receipt-lost-commit'",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("original backend");
    assert_eq!(
        reader.readback(&receipt).await,
        RestrictedReceiptReadback::Exists
    );
    disconnect.send(()).expect("drop COMMIT response");
    assert_eq!(
        task.await.expect("task").expect("write outcome"),
        RestrictedReceiptWrite::Indeterminate
    );
    proxy.await.expect("proxy task").expect("proxy");
    terminal(fixture, pid).await;
    drop(db);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("backend closure");
            if !exists {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lost socket backend closed");
    assert_eq!(
        reader.readback(&receipt).await,
        RestrictedReceiptReadback::Exists
    );
}

// Confirm the server emitted the exact COMMIT CommandComplete frame, then
// discard it and close both sides before the client can observe it.
async fn lose_receipt_commit_reply(
    listener: tokio::net::TcpListener,
    upstream: sqlx::postgres::PgConnectOptions,
    held: oneshot::Sender<()>,
    disconnect: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let forward = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
    });
    loop {
        let tag = server_read.read_u8().await?;
        let length = server_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::other("invalid backend frame"));
        }
        let mut body = vec![0; (length - 4) as usize];
        server_read.read_exact(&mut body).await?;
        if tag == b'C' && body == b"COMMIT\0" {
            held.send(())
                .map_err(|_| std::io::Error::other("holder dropped"))?;
            disconnect
                .await
                .map_err(|_| std::io::Error::other("disconnect dropped"))?;
            break;
        }
        client_write.write_u8(tag).await?;
        client_write.write_u32(length).await?;
        client_write.write_all(&body).await?;
    }
    forward.abort();
    let _ = forward.await;
    client_write.shutdown().await
}

// Parse frontend frames and hold the actual simple-query COMMIT before it is
// forwarded. The server still owns the open transaction while readback runs.
async fn hold_commit_request(
    listener: tokio::net::TcpListener,
    upstream: sqlx::postgres::PgConnectOptions,
    held: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (client, _) = listener.accept().await?;
    let server = crate::postgres_proxy::connect_local_postgres(&upstream).await?;
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = tokio::io::split(server);
    let upstream = async move {
        // SSL is disabled for this local-only proxy. Startup has no tag.
        let length = client_read.read_u32().await?;
        if !(4..=1_048_576).contains(&length) {
            return Err(std::io::Error::other("invalid startup frame"));
        }
        let mut body = vec![0; (length - 4) as usize];
        client_read.read_exact(&mut body).await?;
        server_write.write_u32(length).await?;
        server_write.write_all(&body).await?;
        let mut held = Some(held);
        let mut release = Some(release);
        loop {
            let tag = client_read.read_u8().await?;
            let length = client_read.read_u32().await?;
            if !(4..=1_048_576).contains(&length) {
                return Err(std::io::Error::other("invalid frontend frame"));
            }
            let mut body = vec![0; (length - 4) as usize];
            client_read.read_exact(&mut body).await?;
            if tag == b'Q' && body == b"COMMIT\0" {
                held.take()
                    .ok_or_else(|| std::io::Error::other("second COMMIT"))?
                    .send(())
                    .map_err(|_| std::io::Error::other("holder dropped"))?;
                release
                    .take()
                    .ok_or_else(|| std::io::Error::other("second COMMIT"))?
                    .await
                    .map_err(|_| std::io::Error::other("release dropped"))?;
            }
            server_write.write_u8(tag).await?;
            server_write.write_u32(length).await?;
            server_write.write_all(&body).await?;
        }
    };
    let downstream = async move {
        tokio::io::copy(&mut server_read, &mut client_write)
            .await
            .map(|_| ())
    };
    tokio::select! { result = upstream => result, result = downstream => result }
}

async fn delayed_commit(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
    reader: &RestrictedReceiptPilot,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let port = listener.local_addr().expect("port").port();
    let (held, waiting) = oneshot::channel();
    let (release, proceed) = oneshot::channel();
    let proxy = tokio::spawn(hold_commit_request(
        listener,
        fixture.admin_options.clone(),
        held,
        proceed,
    ));
    let options = proxy_runtime_options(fixture, port).application_name("receipt-delayed-commit");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("receipt-delayed-commit", pool);
    let candidate = pilot(&db, api, expected);
    let task = tokio::spawn(async move { candidate.record(&receipt).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT intercepted")
        .expect("holder");
    let (pid, open): (i32, bool) = sqlx::query_as(
        "SELECT pid, xact_start IS NOT NULL FROM pg_catalog.pg_stat_activity WHERE application_name='receipt-delayed-commit'"
    ).fetch_one(&fixture.admin).await.expect("original backend");
    assert!(
        open,
        "original transaction remains open before forwarded COMMIT"
    );
    assert_eq!(
        reader.readback(&receipt).await,
        RestrictedReceiptReadback::Indeterminate
    );
    release.send(()).expect("forward COMMIT");
    assert_eq!(
        task.await.expect("task").expect("write"),
        RestrictedReceiptWrite::Committed { inserted: true }
    );
    terminal(fixture, pid).await;
    // A new checkout from the same single-connection pool must be safe after
    // the original transaction ended; the proxy keeps that socket open.
    assert_eq!(
        pilot(&db, api, expected).readback(&receipt).await,
        RestrictedReceiptReadback::Exists
    );
    let reused: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='receipt-delayed-commit'"
    ).fetch_one(&fixture.admin).await.expect("reused backend");
    assert_eq!(reused, pid);
    drop(db);
    proxy.abort();
    let _ = proxy.await;
}

async fn replacement_epoch_view(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
) {
    let s = &fixture.target;
    let mut owner = fixture.admin.begin().await.expect("AX owner");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *owner)
    .await
    .expect("hold AX");
    let candidate = pilot(db, api, expected);
    let task = tokio::spawn(async move { candidate.record(&receipt).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity a JOIN pg_catalog.pg_locks l ON l.pid=a.pid \
                 WHERE a.application_name='restricted-receipt' AND l.relation=$1 AND l.mode='AccessShareLock' AND NOT l.granted)"
            ).bind(Oid(expected.epoch_relation_oid)).fetch_one(&fixture.admin).await.expect("waiter");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("actual AS wait");
    sqlx::raw_sql(&format!(
        "ALTER TABLE {s}.ingress_protocol_epoch RENAME TO ingress_protocol_epoch_original; \
         CREATE VIEW {s}.ingress_protocol_epoch AS \
           SELECT id, epoch + CASE WHEN pg_catalog.pg_advisory_lock(987654321) IS NULL \
             THEN 0 ELSE 1 END AS epoch FROM {s}.ingress_protocol_epoch_original; \
         GRANT SELECT ON {s}.ingress_protocol_epoch TO {}",
        fixture.runtime,
    ))
    .execute(&mut *owner)
    .await
    .expect("replace epoch with volatile view");
    owner.commit().await.expect("publish replacement");
    assert!(task.await.expect("task").is_err());
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks l JOIN pg_catalog.pg_stat_activity a ON a.pid=l.pid \
         WHERE a.datname=$1 AND a.application_name='restricted-receipt' \
           AND l.locktype='advisory' AND l.mode='ExclusiveLock' AND l.granted"
    ).bind(&fixture.database_name).fetch_one(&fixture.admin).await.expect("session lock check");
    assert_eq!(leaked, 0, "replacement view must not be executed");
    sqlx::raw_sql(&format!(
        "DROP VIEW {s}.ingress_protocol_epoch; \
         ALTER TABLE {s}.ingress_protocol_epoch_original RENAME TO ingress_protocol_epoch"
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore epoch");
    assert_eq!(
        pilot(db, api, expected).readback(&receipt).await,
        RestrictedReceiptReadback::Indeterminate
    );
}

async fn cancelled_commit(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    receipt: RestrictedEffectReceipt,
    reader: &RestrictedReceiptPilot,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("port").port();
    let (held, waiting) = oneshot::channel();
    let (release, proceed) = oneshot::channel();
    let proxy = tokio::spawn(hold_commit_request(
        listener,
        fixture.admin_options.clone(),
        held,
        proceed,
    ));
    let options = proxy_runtime_options(fixture, port).application_name("receipt-cancelled-commit");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("receipt-cancelled-commit", pool);
    let candidate = pilot(&db, api, expected);
    let task = tokio::spawn(async move { candidate.record(&receipt).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT intercepted")
        .expect("holder");
    let pid: i32 = sqlx::query_scalar("SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='receipt-cancelled-commit'")
        .fetch_one(&fixture.admin).await.expect("original backend");
    assert_eq!(
        reader.readback(&receipt).await,
        RestrictedReceiptReadback::Indeterminate
    );
    task.abort();
    assert!(task.await.expect_err("cancelled future").is_cancelled());
    let same_pool = pilot(&db, api, expected);
    let mut checkout = tokio::spawn(async move { same_pool.readback(&receipt).await });
    assert!(
        tokio::time::timeout(Duration::from_millis(500), &mut checkout)
            .await
            .is_err(),
        "unresolved single-connection pool must not complete a new checkout"
    );
    let still_open: bool = sqlx::query_scalar(
        "SELECT xact_start IS NOT NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_one(&fixture.admin)
    .await
    .expect("original backend remains present");
    assert!(still_open, "original transaction must still be unresolved");
    release
        .send(())
        .expect("forward previously dispatched COMMIT");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reader.readback(&receipt).await == RestrictedReceiptReadback::Exists {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("COMMIT can succeed after caller cancellation");
    terminal(fixture, pid).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), checkout)
            .await
            .expect("same-pool checkout completes after cleanup")
            .expect("checkout task"),
        RestrictedReceiptReadback::Exists
    );
    let reused: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='receipt-cancelled-commit'",
    )
    .fetch_one(&fixture.admin)
    .await
    .expect("pool backend after cleanup");
    assert_eq!(reused, pid, "same backend was safely reused after cleanup");
    drop(db);
    proxy.abort();
    let _ = proxy.await;
}
