use super::*;
use crate::ingress::{effects::Effect, test_support::IngressFixture, PlannedEffect};
use crate::ingress_uow::{ConfiguredPluginGrants, ExtensionGrantRepository};
use waddle_extensions::PluginId;
use waddle_xmpp::ingress::{EffectMessageIdentity, IngressEffectIntent, TransportGeneration};

async fn submission(f: &IngressFixture, origin: &str) -> IngressSubmission {
    let plugin = PluginId::new("nested-test").expect("plugin id");
    let mut tx = f.uow.begin().await.expect("grant transaction");
    ExtensionGrantRepository::sync_configured(
        &mut tx,
        &[ConfiguredPluginGrants {
            plugin: plugin.clone(),
            can_send: true,
            provider_rooms: vec![],
        }],
    )
    .await
    .expect("sync grants");
    let grant = ExtensionGrantRepository::active_send_grant(&mut tx, &plugin)
        .await
        .expect("lookup grant")
        .expect("active grant");
    tx.commit().await.expect("commit grant");
    let mut submission = f.submission(Some(origin), "nested body");
    let sender = f.principal.bare_jid().clone();
    submission.principal =
        crate::ingress::IngressPrincipal::Extension(crate::ingress::ExtensionPrincipal {
            grant,
            requester: Some(sender.clone()),
            sender: sender.clone(),
        });
    submission.identity = crate::ingress::IngressStreamIdentity::Extension {
        plugin,
        requester: Some(sender),
    };
    submission.connection_generation = TransportGeneration::Host;
    let mut reply = submission.plan.sanitized_message.clone();
    reply.to = Some(submission.sender.clone().into());
    submission
        .plan
        .intents
        .push(IngressEffectIntent::RouteDirect {
            recipient: submission.sender.to_bare(),
            fanout: vec![submission.sender.clone()],
            route_identity: EffectMessageIdentity::OriginId(
                waddle_xmpp_core::xep0359::extract_origin_id(&reply).expect("origin id"),
            ),
        });
    submission
        .plan
        .plan
        .push(PlannedEffect::new(Effect::External(
            crate::ingress::ExternalEffect::Frame(Box::new(waddle_xmpp::Stanza::Message(reply))),
        )));
    submission
}

async fn state(
    f: &IngressFixture,
    authority: Arc<IngressAuthority>,
) -> Arc<crate::server::routes::websocket::WebSocketState> {
    let pool = crate::db::DatabasePool::new(
        crate::db::DatabaseConfig::new(f.db.driver(), f.db.database_url()),
        crate::db::PoolConfig,
    )
    .await
    .expect("database pool");
    crate::server::routes::websocket::tests::create_test_websocket_state_with_db_pool_and_ingress(
        Arc::new(pool),
        authority,
    )
    .await
}

async fn exercise(f: IngressFixture) {
    let stopped = Arc::new(f.authority().await);
    assert!(stopped.drain_and_join(Duration::from_secs(10)).await);
    assert!(matches!(
        stopped.try_begin_nested(),
        Err(AuthorityUnavailable::Stopped)
    ));
    assert_eq!(f.count("ingress_messages").await, 0);
    drop(stopped);
    let authority = Arc::new(f.authority().await);
    // This is the outer observer's admission-lock condition, without invoking WASM.
    let outer = authority.admission.read().await;
    let writer_lock = Arc::clone(&authority.admission);
    let mut writer = Box::pin(writer_lock.write_owned());
    assert!(futures::poll!(writer.as_mut()).is_pending());
    assert!(matches!(
        authority.try_begin_nested(),
        Err(AuthorityUnavailable::Busy)
    ));
    drop(writer);
    drop(outer);
    assert_eq!(f.count("ingress_messages").await, 0);

    let offered = submission(&f, "nested-origin").await;
    let gate = Arc::new(TestGate::default());
    let mut continuation = NestedContinuation::new(state(&f, Arc::clone(&authority)).await, None);
    continuation.before_execute = Some(Arc::clone(&gate));
    let operation = authority.try_begin_nested().expect("admitted operation");
    let outcome = operation
        .commit_and_continue(offered.clone(), continuation)
        .await;
    let NestedOutcome::Committed { settlement, .. } = outcome else {
        panic!("committed nested row")
    };
    gate.reached.notified().await;
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("ingress_effect_receipts").await, 0);
    let draining = Arc::clone(&authority);
    let drain = tokio::spawn(async move { draining.drain_and_join(Duration::from_secs(10)).await });
    authority.cancellation.cancelled().await;
    assert!(!drain.is_finished());
    assert!(matches!(
        authority.try_begin_nested(),
        Err(AuthorityUnavailable::Stopped)
    ));
    gate.release.notify_one();
    assert!(settlement
        .await
        .expect("settlement task")
        .terminal
        .expect("persist settlement"));
    assert!(drain.await.expect("drain task"));
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(f.count("ingress_effect_receipts").await, 1);
    drop(authority);

    let offered = submission(&f, "cancelled-origin").await;
    // A caller timeout drops the settlement handle, but the admitted task remains alive.
    let authority = Arc::new(f.authority().await);
    let gate = Arc::new(TestGate::default());
    let mut continuation = NestedContinuation::new(state(&f, Arc::clone(&authority)).await, None);
    continuation.before_settlement = Some(Arc::clone(&gate));
    let operation = authority.try_begin_nested().expect("replay admission");
    let caller = tokio::spawn(async move {
        let NestedOutcome::Committed { settlement, .. } =
            operation.commit_and_continue(offered, continuation).await
        else {
            panic!("committed replay")
        };
        tokio::time::timeout_at(
            tokio::time::Instant::now() + Duration::from_millis(30),
            settlement,
        )
        .await
    });
    gate.reached.notified().await;
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    assert!(caller.await.expect("caller task").is_err());
    gate.release.notify_one();
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(f.count("ingress_messages").await, 2);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        2
    );
    assert_eq!(f.count("ingress_effect_receipts").await, 2);
    drop(authority);
    f.close().await;
}

#[tokio::test]
async fn nested_drain_and_caller_cancellation_sqlite() {
    tokio::time::timeout(
        Duration::from_secs(20),
        exercise(IngressFixture::sqlite().await),
    )
    .await
    .expect("nested lifecycle must not deadlock");
}

#[tokio::test]
async fn nested_drain_and_caller_cancellation_postgres() {
    if let Some(f) = IngressFixture::postgres("nested_lifecycle").await {
        tokio::time::timeout(Duration::from_secs(20), exercise(f))
            .await
            .expect("nested lifecycle must not deadlock");
    }
}

async fn persistence_failure(f: IngressFixture) {
    let authority = Arc::new(f.authority().await);
    let offered = submission(&f, "failed-frame").await;
    match f.db.driver() {
        crate::db::DatabaseDriver::Sqlite => f.execute("CREATE TRIGGER fail_nested_receipt BEFORE INSERT ON ingress_effect_receipts BEGIN SELECT RAISE(FAIL, 'injected frame receipt failure'); END", ()).await,
        crate::db::DatabaseDriver::Postgres => {
            f.execute("CREATE FUNCTION fail_nested_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected frame receipt failure'; END $$", ()).await;
            f.execute("CREATE TRIGGER fail_nested_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_nested_receipt()", ()).await;
        }
    }
    let context = state(&f, Arc::clone(&authority)).await;
    let operation = authority.try_begin_nested().expect("admit failure case");
    let started = tokio::time::Instant::now();
    let outcome = operation
        .commit_and_continue(
            offered.clone(),
            NestedContinuation::new(Arc::clone(&context), None),
        )
        .await;
    let NestedOutcome::Committed { settlement, .. } = outcome else {
        panic!("receipt failure must not revise commit")
    };
    assert!(settlement.await.expect("settlement task").terminal.is_err());
    assert!(
        started.elapsed() >= Duration::from_secs(4),
        "receipt failures must be retried within the bounded budget"
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    assert_eq!(f.count("ingress_effect_receipts").await, 0);
    let drop_trigger = match f.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER fail_nested_receipt",
        crate::db::DatabaseDriver::Postgres => {
            "DROP TRIGGER fail_nested_receipt ON ingress_effect_receipts"
        }
    };
    f.execute(drop_trigger, ()).await;
    let outcome = authority
        .try_begin_nested()
        .expect("retry admission")
        .commit_and_continue(offered, NestedContinuation::new(Arc::clone(&context), None))
        .await;
    let NestedOutcome::Committed {
        decision_class,
        settlement,
        ..
    } = outcome
    else {
        panic!("committed retry")
    };
    assert_ne!(decision_class, IngressDecisionClass::Accepted);
    assert!(settlement
        .await
        .expect("retry settlement")
        .terminal
        .expect("receipt retry"));
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(f.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(context);
    drop(authority);
    f.close().await;
}

#[tokio::test]
async fn nested_receipt_failure_and_replay_sqlite() {
    persistence_failure(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn nested_receipt_failure_and_replay_postgres() {
    if let Some(f) = IngressFixture::postgres("nested_receipts").await {
        persistence_failure(f).await;
    }
}
