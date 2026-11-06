//! Direct PG17 schedules for the closed post-commit completion operation.

use std::time::Duration;

use sqlx::{Connection, PgConnection};
use tokio::sync::oneshot;
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig, RestrictedEffectReceipt,
    RestrictedIngressProfileError, RestrictedIngressProfileObservation, RestrictedReceiptPilot,
    RestrictedReceiptWrite, RestrictedTerminalization, RestrictedTerminalizationWrite,
};
use waddle_server::test_support::database_from_attested_postgres_pool;
use waddle_xmpp::ingress::MessageKey;

use super::super::{proxy_runtime_options, Fixture};
use super::receipt_transaction::{
    database, hold_commit_request, lose_receipt_commit_reply, pilot, seed, terminal,
};

fn committed(outcome: RestrictedTerminalization) -> RestrictedTerminalizationWrite {
    RestrictedTerminalizationWrite::Committed { outcome }
}

async fn record(pilot: &RestrictedReceiptPilot, receipt: &RestrictedEffectReceipt) {
    assert_eq!(
        pilot.record(receipt).await.expect("record receipt"),
        RestrictedReceiptWrite::Committed { inserted: true },
    );
}

async fn timestamps(fixture: &Fixture, message: MessageKey) -> (Option<String>, Option<String>) {
    sqlx::query_as(&format!(
        "SELECT terminal_at::text, retention_eligible_at::text FROM {}.ingress_messages WHERE message_key=$1",
        fixture.target
    ))
    .bind(message.to_storage())
    .fetch_one(&fixture.admin)
    .await
    .expect("read completion state as owner")
}

pub(super) async fn exercise(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let db = database(fixture, config.clone(), "restricted-completion");
    let writer = pilot(&db, api, expected);
    let receipts = seed(fixture, 2).await;
    let message = receipts[0].message;
    assert_eq!(
        writer
            .terminalize_if_complete(message)
            .await
            .expect("incomplete"),
        committed(RestrictedTerminalization::Incomplete),
    );
    record(&writer, &receipts[0]).await;
    assert_eq!(
        writer
            .terminalize_if_complete(message)
            .await
            .expect("still incomplete"),
        committed(RestrictedTerminalization::Incomplete),
    );
    assert_eq!(timestamps(fixture, message).await, (None, None));
    record(&writer, &receipts[1]).await;
    assert_eq!(
        writer
            .terminalize_rolled_back(message)
            .await
            .expect("rollback"),
        RestrictedTerminalization::Terminalized,
    );
    assert_eq!(timestamps(fixture, message).await, (None, None));
    assert_eq!(
        writer
            .terminalize_if_complete(message)
            .await
            .expect("complete"),
        committed(RestrictedTerminalization::Terminalized),
    );
    let first = timestamps(fixture, message).await;
    assert!(
        first.0.is_some() && first.1.is_some(),
        "terminal and retention set together"
    );
    assert_eq!(
        writer
            .terminalize_if_complete(message)
            .await
            .expect("duplicate"),
        committed(RestrictedTerminalization::AlreadyTerminal),
    );
    assert_eq!(
        timestamps(fixture, message).await,
        first,
        "first timestamps persist"
    );
    assert_eq!(
        writer
            .terminalize_if_complete(MessageKey::new())
            .await
            .expect("absent"),
        committed(RestrictedTerminalization::MessageVanished),
    );

    let mut runtime = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("restricted runtime");
    for statement in [
        format!("SELECT * FROM {}.ingress_messages", fixture.target),
        format!(
            "UPDATE {}.ingress_messages SET terminal_at=clock_timestamp() WHERE false",
            fixture.target
        ),
        format!(
            "DELETE FROM {}.ingress_messages WHERE false",
            fixture.target
        ),
    ] {
        assert!(sqlx::query(&statement).execute(&mut runtime).await.is_err());
    }
    drop(runtime);

    let ready = seed(fixture, 1).await;
    record(&writer, &ready[0]).await;
    let one = pilot(&db, api, expected);
    let two = pilot(&db, api, expected);
    let key = ready[0].message;
    let (one, two) = tokio::join!(
        one.terminalize_if_complete(key),
        two.terminalize_if_complete(key)
    );
    let results = [
        one.expect("first concurrent"),
        two.expect("second concurrent"),
    ];
    assert!(results.contains(&committed(RestrictedTerminalization::Terminalized)));
    assert!(results.contains(&committed(RestrictedTerminalization::AlreadyTerminal)));
    assert!(timestamps(fixture, key).await.1.is_some());

    canonical_wait_sees_new_intent(fixture).await;
    refresh_failure_rolls_back(fixture, &writer).await;
    epoch_wait_is_bounded(fixture, &db, api, expected).await;
    checkout_wait_is_bounded(fixture, config, api, expected).await;
    cancelled_begin_is_cleaned_up(fixture, config, api, expected, &writer).await;
    lost_completion_commit(fixture, config, api, expected, &writer).await;
    cancelled_completion_commit(fixture, config, api, expected, &writer).await;
}

async fn checkout_wait_is_bounded(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let receipt = seed(fixture, 1).await[0];
    let options = fixture
        .runtime_options
        .clone()
        .application_name("completion-checkout-bound");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("one-slot pool");
    let db = database_from_attested_postgres_pool("completion-checkout-bound", pool);
    let holder = pilot(&db, api, expected);
    let (ready, waiting) = oneshot::channel();
    let (release, proceed) = oneshot::channel();
    let task = tokio::spawn(async move { holder.record_paused(&receipt, ready, proceed).await });
    let pid = waiting.await.expect("holder owns pool slot");
    let candidate = pilot(&db, api, expected);
    let start = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        candidate.terminalize_if_complete(receipt.message),
    )
    .await
    .expect("bounded checkout");
    assert!(matches!(
        result,
        Err(RestrictedIngressProfileError::Timeout)
    ));
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "100ms acquisition bound"
    );
    assert_eq!(timestamps(fixture, receipt.message).await, (None, None));
    release.send(()).expect("release holder");
    assert_eq!(
        task.await.expect("holder task").expect("receipt"),
        RestrictedReceiptWrite::Committed { inserted: true }
    );
    terminal(fixture, pid).await;
    assert_eq!(
        candidate
            .terminalize_if_complete(receipt.message)
            .await
            .expect("pool remains usable"),
        committed(RestrictedTerminalization::Terminalized)
    );
}

async fn cancelled_begin_is_cleaned_up(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    reader: &RestrictedReceiptPilot,
) {
    let receipt = seed(fixture, 1).await[0];
    record(reader, &receipt).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("port").port();
    let (held, waiting) = oneshot::channel();
    let (release, proceed) = oneshot::channel();
    let proxy = tokio::spawn(hold_begin_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        proceed,
    ));
    let options =
        proxy_runtime_options(fixture, port).application_name("completion-cancelled-begin");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("completion-cancelled-begin", pool);
    let candidate = pilot(&db, api, expected);
    let key = receipt.message;
    let task = tokio::spawn(async move { candidate.terminalize_if_complete(key).await });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("BEGIN response held")
        .expect("proxy");
    let (pid, open): (i32, bool) = sqlx::query_as("SELECT pid, xact_start IS NOT NULL FROM pg_catalog.pg_stat_activity WHERE application_name='completion-cancelled-begin'")
        .fetch_one(&fixture.admin).await.expect("backend");
    assert!(open, "PostgreSQL entered BEGIN before its reply was held");
    assert!(matches!(
        task.await.expect("task"),
        Err(RestrictedIngressProfileError::Timeout)
    ));
    assert_eq!(timestamps(fixture, key).await, (None, None));
    release.send(()).expect("release BEGIN reply");
    terminal(fixture, pid).await;
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match pilot(&db, api, expected).terminalize_if_complete(key).await {
                Err(RestrictedIngressProfileError::Timeout) => tokio::task::yield_now().await,
                result => break result,
            }
        }
    })
    .await
    .expect("pool cleanup after cancelled BEGIN")
    .expect("completion");
    assert_eq!(next, committed(RestrictedTerminalization::Terminalized));
    terminal(fixture, pid).await;
    drop(db);
    proxy.abort();
    let _ = proxy.await;
}

async fn hold_begin_reply(
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
    let forward = async move {
        tokio::io::copy(&mut client_read, &mut server_write)
            .await
            .map(|_| ())
    };
    let backward = async move {
        let mut held = Some(held);
        let mut release = Some(release);
        loop {
            let tag = server_read.read_u8().await?;
            let length = server_read.read_u32().await?;
            if !(4..=1_048_576).contains(&length) {
                return Err(std::io::Error::other("invalid backend frame"));
            }
            let mut body = vec![0; (length - 4) as usize];
            server_read.read_exact(&mut body).await?;
            if tag == b'C' && body == b"BEGIN\0" && held.is_some() {
                held.take()
                    .expect("first BEGIN")
                    .send(())
                    .map_err(|_| std::io::Error::other("holder dropped"))?;
                release
                    .take()
                    .expect("first BEGIN")
                    .await
                    .map_err(|_| std::io::Error::other("release dropped"))?;
            }
            client_write.write_u8(tag).await?;
            client_write.write_u32(length).await?;
            client_write.write_all(&body).await?;
        }
    };
    tokio::select! { result = forward => result, result = backward => result }
}

// A parent lock acquired before an added intent causes the waiting completion
// statement to see that intent after the lock holder commits.
async fn canonical_wait_sees_new_intent(fixture: &Fixture) {
    let receipt = seed(fixture, 1).await[0];
    let s = &fixture.target;
    let mut connection = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("holder runtime");
    let mut owner = connection.begin().await.expect("parent holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *owner)
    .await
    .expect("holder epoch lock");
    let locked: bool = sqlx::query_scalar(&format!("SELECT {s}.waddle_ingress_lock_message($1)"))
        .bind(receipt.message.to_storage())
        .fetch_one(&mut *owner)
        .await
        .expect("hold canonical lock");
    assert!(locked);
    // The first intent has a receipt; the holder adds a second obligation.
    let recorded: bool = sqlx::query_scalar(&format!(
        "SELECT {s}.waddle_ingress_record_effect_receipt($1,$2,$3)"
    ))
    .bind(receipt.message.to_storage())
    .bind(receipt.kind.to_storage())
    .bind(receipt.semantic_identity_hash.as_slice())
    .fetch_one(&mut *owner)
    .await
    .expect("seed receipt");
    assert!(recorded);
    let inserted: bool = sqlx::query_scalar(&format!(
        "SELECT {s}.waddle_ingress_insert_effect_intent($1,2::numeric,2,$2,$3)"
    ))
    .bind(receipt.message.to_storage())
    .bind([42u8; 32].as_slice())
    .bind([1u8].as_slice())
    .fetch_one(&mut *owner)
    .await
    .expect("add incomplete obligation");
    assert!(inserted);
    let options = fixture
        .runtime_options
        .clone()
        .application_name("completion-parent-wait");
    let schema = s.clone();
    let key = receipt.message;
    let task = tokio::spawn(async move {
        let mut connection = PgConnection::connect_with(&options).await.expect("runtime");
        let mut tx = connection.begin().await.expect("transaction");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {schema}.ingress_protocol_epoch IN ACCESS SHARE MODE"
        ))
        .execute(&mut *tx)
        .await
        .expect("epoch lock");
        let outcome: i32 = sqlx::query_scalar(&format!(
            "SELECT {schema}.waddle_ingress_terminalize_if_complete($1)"
        ))
        .bind(key.to_storage())
        .fetch_one(&mut *tx)
        .await
        .expect("completion after wait");
        tx.commit().await.expect("commit");
        outcome
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE application_name='completion-parent-wait' AND wait_event_type='Lock')")
                .fetch_one(&fixture.admin).await.expect("canonical waiter");
            if waiting { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("actual canonical wait");
    owner.commit().await.expect("publish additional intent");
    assert_eq!(task.await.expect("waiting task"), 0);
    assert_eq!(timestamps(fixture, key).await, (None, None));
}

async fn refresh_failure_rolls_back(fixture: &Fixture, writer: &RestrictedReceiptPilot) {
    let receipt = seed(fixture, 1).await[0];
    record(writer, &receipt).await;
    let s = &fixture.target;
    sqlx::raw_sql(&format!(
        "CREATE FUNCTION {s}.reject_completion_refresh() RETURNS trigger LANGUAGE plpgsql \
         AS $$ BEGIN RAISE EXCEPTION 'injected retention failure'; END $$; \
         CREATE TRIGGER reject_completion_refresh BEFORE UPDATE OF retention_eligible_at \
         ON {s}.ingress_messages FOR EACH ROW EXECUTE FUNCTION {s}.reject_completion_refresh()"
    ))
    .execute(&fixture.admin)
    .await
    .expect("install retention failure");
    let mut connection = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime");
    let mut tx = connection.begin().await.expect("transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS SHARE MODE"
    ))
    .execute(&mut *tx)
    .await
    .expect("epoch lock");
    let result = sqlx::query(&format!(
        "SELECT {s}.waddle_ingress_terminalize_if_complete($1)"
    ))
    .bind(receipt.message.to_storage())
    .execute(&mut *tx)
    .await;
    assert!(result.is_err(), "retention failure aborts the operation");
    tx.rollback().await.expect("rollback failed transaction");
    assert_eq!(timestamps(fixture, receipt.message).await, (None, None));
    sqlx::raw_sql(&format!("DROP TRIGGER reject_completion_refresh ON {s}.ingress_messages; DROP FUNCTION {s}.reject_completion_refresh()"))
        .execute(&fixture.admin).await.expect("restore catalog");
}

async fn epoch_wait_is_bounded(
    fixture: &Fixture,
    db: &waddle_server::db::Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let key = seed(fixture, 1).await[0].message;
    let mut owner = fixture.admin.begin().await.expect("epoch holder");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *owner)
    .await
    .expect("hold epoch AX");
    let candidate = pilot(db, api, expected);
    let task = tokio::spawn(async move { candidate.terminalize_if_complete(key).await });
    let failed = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("100ms lock limit terminates wait")
        .expect("task");
    assert!(failed.is_err());
    owner.rollback().await.expect("release epoch");
    assert_eq!(timestamps(fixture, key).await, (None, None));
}

async fn lost_completion_commit(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    reader: &RestrictedReceiptPilot,
) {
    let receipt = seed(fixture, 1).await[0];
    record(reader, &receipt).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let port = listener.local_addr().expect("port").port();
    let (held, waiting) = oneshot::channel();
    let (disconnect, close) = oneshot::channel();
    let proxy = tokio::spawn(lose_receipt_commit_reply(
        listener,
        fixture.admin_options.clone(),
        held,
        close,
    ));
    let options = proxy_runtime_options(fixture, port).application_name("completion-lost-commit");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("completion-lost-commit", pool);
    let candidate = pilot(&db, api, expected);
    let key = receipt.message;
    let task = tokio::spawn(async move { candidate.terminalize_if_complete(key).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT response held")
        .expect("proxy");
    let pid: i32 = sqlx::query_scalar("SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='completion-lost-commit'")
        .fetch_one(&fixture.admin).await.expect("backend");
    assert!(
        timestamps(fixture, key).await.0.is_some(),
        "server committed before reply lost"
    );
    disconnect.send(()).expect("lose response");
    assert_eq!(
        task.await.expect("task").expect("outcome"),
        RestrictedTerminalizationWrite::Indeterminate
    );
    proxy.await.expect("proxy task").expect("proxy");
    terminal(fixture, pid).await;
    assert_eq!(
        reader
            .terminalize_if_complete(key)
            .await
            .expect("current state"),
        committed(RestrictedTerminalization::AlreadyTerminal)
    );
    drop(db);
}

async fn cancelled_completion_commit(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
    reader: &RestrictedReceiptPilot,
) {
    let receipt = seed(fixture, 1).await[0];
    record(reader, &receipt).await;
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
    let options =
        proxy_runtime_options(fixture, port).application_name("completion-cancelled-commit");
    let pool =
        attested_pool(options, config.clone(), 1, Duration::from_secs(2)).expect("proxied pool");
    let db = database_from_attested_postgres_pool("completion-cancelled-commit", pool);
    let candidate = pilot(&db, api, expected);
    let key = receipt.message;
    let task = tokio::spawn(async move { candidate.terminalize_if_complete(key).await });
    tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("COMMIT intercepted")
        .expect("proxy");
    let pid: i32 = sqlx::query_scalar("SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name='completion-cancelled-commit'")
        .fetch_one(&fixture.admin).await.expect("backend");
    assert_eq!(timestamps(fixture, key).await, (None, None));
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    let same = pilot(&db, api, expected);
    let checkout = tokio::time::timeout(Duration::from_secs(2), same.terminalize_if_complete(key))
        .await
        .expect("checkout is bounded");
    assert!(matches!(
        checkout,
        Err(RestrictedIngressProfileError::Timeout)
    ));
    assert_eq!(timestamps(fixture, key).await, (None, None));
    release.send(()).expect("forward original COMMIT");
    terminal(fixture, pid).await;
    let current = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match same.terminalize_if_complete(key).await {
                Err(RestrictedIngressProfileError::Timeout) => tokio::task::yield_now().await,
                result => break result,
            }
        }
    })
    .await
    .expect("checkout cleanup")
    .expect("current state");
    assert_eq!(
        current,
        committed(RestrictedTerminalization::AlreadyTerminal)
    );
    assert!(timestamps(fixture, key).await.0.is_some());
    drop(db);
    proxy.abort();
    let _ = proxy.await;
}
