use super::*;
use std::sync::Arc;
use tokio::sync::Notify;
use waddle_extensions::{
    observer_test_support::{ObserverTestBehavior, ObserverTestPlugin},
    ExtensionManager, PluginId,
};

fn plugin(name: &str, behavior: ObserverTestBehavior) -> Arc<ObserverTestPlugin> {
    ObserverTestPlugin::new(PluginId::new(name).expect("plugin"), behavior)
}

async fn state_with_manager(
    manager: ExtensionManager,
) -> Arc<crate::server::routes::websocket::WebSocketState> {
    let mut state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unique state")
        .deps
        .protocol
        .extension_manager = Arc::new(manager);
    state
}

async fn fair_observers(fixture: IngressFixture, slow_first: bool) {
    let release = Arc::new(Notify::new());
    let slow = plugin(
        "observer-slow",
        ObserverTestBehavior::Blocked(release.clone()),
    );
    let fast = plugin("observer-fast", ObserverTestBehavior::Success);
    let plugins = if slow_first {
        vec![slow.clone(), fast.clone()]
    } else {
        vec![fast.clone(), slow.clone()]
    };
    let manager = ExtensionManager::with_observer_test_plugins(plugins).await;
    let mut submission = fixture.submission(Some("fair-observers"), "canonical observer body");
    membership::select_observers(&mut submission, &manager);
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let state = state_with_manager(manager).await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let execution = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_millis(750),
    );
    let receipt_before_deadline = async {
        tokio::time::timeout(Duration::from_millis(500), async {
            while fixture.count("ingress_effect_receipts").await != 1 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(fast.invocations().len(), 1);
            assert_eq!(slow.invocations().len(), 1, "both plugins started");
        })
        .await
        .expect("fast receipt persists while slow plugin is still blocked");
    };
    let (report, ()) = tokio::join!(execution, receipt_before_deadline);
    let fast_index = usize::from(slow_first);
    let slow_index = usize::from(!slow_first);
    assert_eq!(report.outcomes[fast_index].1, ExternalOutcome::Done);
    assert_eq!(report.outcomes[slow_index].1, ExternalOutcome::Uncertain);
    assert!(report.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(
        !terminalize_if_complete(&fixture.uow, first.message_key.expect("key"))
            .await
            .expect("pending")
    );
    drop(deps);
    drop(state);

    // A newly loaded manager and fresh decision recover solely from durable
    // receipts. The completed plugin must not be invoked after restart.
    let restarted_fast = plugin("observer-fast", ObserverTestBehavior::Success);
    let restarted_slow = plugin("observer-slow", ObserverTestBehavior::Success);
    let manager = ExtensionManager::with_observer_test_plugins(vec![
        restarted_fast.clone(),
        restarted_slow.clone(),
    ])
    .await;
    membership::select_observers(&mut submission, &manager);
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("restart replay");
    let state = state_with_manager(manager).await;
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert!(report.receipt_failures.is_empty());
    assert!(restarted_fast.invocations().is_empty());
    assert_eq!(restarted_slow.invocations().len(), 1);
    assert_eq!(
        restarted_slow.invocations()[0].body.as_str(),
        "canonical observer body"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(
        terminalize_if_complete(&fixture.uow, first.message_key.expect("key"))
            .await
            .expect("terminal")
    );
    fixture.close().await;
}

async fn warning_observer(fixture: IngressFixture) {
    let success = plugin("observer-success", ObserverTestBehavior::Success);
    let warning = plugin("observer-warning", ObserverTestBehavior::Warning);
    let manager =
        ExtensionManager::with_observer_test_plugins(vec![warning.clone(), success.clone()]).await;
    let mut submission = fixture.submission(Some("observer-warning"), "canonical body");
    membership::select_observers(&mut submission, &manager);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let state = state_with_manager(manager).await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Failed);
    assert_eq!(report.outcomes[1].1, ExternalOutcome::Done);
    assert_eq!(warning.invocations().len(), 1);
    assert_eq!(success.invocations().len(), 1);
    assert_eq!(report.frame_obligations.len(), 1);
    assert!(report.frame_obligations[0].receipt_keys.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(
        !terminalize_if_complete(&fixture.uow, decision.message_key.expect("key"))
            .await
            .expect("pending warning")
    );
    fixture.close().await;
}

async fn receipt_failure(fixture: IngressFixture) {
    let failed = plugin("observer-failed-receipt", ObserverTestBehavior::Success);
    let success = plugin("observer-success", ObserverTestBehavior::Success);
    let manager =
        ExtensionManager::with_observer_test_plugins(vec![failed.clone(), success.clone()]).await;
    let mut submission = fixture.submission(Some("observer-receipt-failure"), "canonical body");
    membership::select_observers(&mut submission, &manager);
    let first = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = &first.external_receipts[0][0];
    let hash = hex::encode(key.semantic_identity_hash);
    // SQL encodes receipt storage bytes; no protocol payload is serialized here.
    match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => fixture.execute(&format!("CREATE TRIGGER fail_observer_receipt BEFORE INSERT ON ingress_effect_receipts WHEN NEW.semantic_identity_hash = X'{hash}' BEGIN SELECT RAISE(FAIL, 'injected observer receipt failure'); END"), ()).await,
        crate::db::DatabaseDriver::Postgres => {
            fixture.execute(&format!("CREATE FUNCTION fail_observer_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.semantic_identity_hash = decode('{hash}', 'hex') THEN RAISE EXCEPTION 'injected observer receipt failure'; END IF; RETURN NEW; END $$"), ()).await;
            fixture.execute("CREATE TRIGGER fail_observer_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW EXECUTE FUNCTION fail_observer_receipt()", ()).await;
        }
    }
    let state = state_with_manager(manager).await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(state.as_ref());
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &first,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(report.receipt_failures.len(), 1);
    assert_eq!(&report.receipt_failures[0].0, key);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(
        !terminalize_if_complete(&fixture.uow, first.message_key.expect("key"))
            .await
            .expect("pending receipt")
    );
    let sql = match fixture.db.driver() {
        crate::db::DatabaseDriver::Sqlite => "DROP TRIGGER fail_observer_receipt",
        crate::db::DatabaseDriver::Postgres => {
            "DROP TRIGGER fail_observer_receipt ON ingress_effect_receipts"
        }
    };
    fixture.execute(sql, ()).await;
    let retry = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("retry");
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &retry,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert_eq!(
        failed.invocations().len(),
        2,
        "unreceipted observer retries"
    );
    assert_eq!(success.invocations().len(), 1, "receipted sibling skips");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_observer_slow_first_receipts_fast_before_deadline() {
    fair_observers(IngressFixture::sqlite().await, true).await;
}
#[tokio::test]
async fn postgres_observer_slow_first_receipts_fast_before_deadline() {
    if let Some(fixture) = IngressFixture::postgres("observer_slow_first").await {
        fair_observers(fixture, true).await;
    }
}
#[tokio::test]
async fn sqlite_observer_fast_first_receipts_fast_before_deadline() {
    fair_observers(IngressFixture::sqlite().await, false).await;
}
#[tokio::test]
async fn postgres_observer_fast_first_receipts_fast_before_deadline() {
    if let Some(fixture) = IngressFixture::postgres("observer_fast_first").await {
        fair_observers(fixture, false).await;
    }
}
#[tokio::test]
async fn sqlite_observer_warning_only_success_receipted() {
    warning_observer(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_warning_only_success_receipted() {
    if let Some(fixture) = IngressFixture::postgres("observer_warning").await {
        warning_observer(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_observer_receipt_failure_retries_only_unresolved_plugin() {
    receipt_failure(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_observer_receipt_failure_retries_only_unresolved_plugin() {
    if let Some(fixture) = IngressFixture::postgres("observer_receipt_failure").await {
        receipt_failure(fixture).await;
    }
}
