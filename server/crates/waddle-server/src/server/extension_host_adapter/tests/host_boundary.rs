//! Real plugin host boundary: committed work survives both response deadlines.
use super::super::ExtensionHostAdapter;
use super::direct_ingress::{adapter, invocation, plugin, request};
use crate::{db::DatabaseDriver, ingress::test_support::IngressFixture};
use std::{sync::Arc, time::Duration};
use waddle_extensions::{host_tools as host, DisplayText, WaddleId};
use waddle_xmpp::xep::xep0191::InMemoryBlockingStorage;

fn context() -> host::InvocationContext {
    host::InvocationContext {
        waddle_id: WaddleId::new("host-boundary").expect("waddle"),
        plugin_id: plugin(),
        requester: Some("romeo@example.com".parse().expect("requester")),
        source_room: None,
        kind: host::InvocationKind::MessageHook,
        provider_room_grants: vec![],
    }
}

fn host_request() -> host::SendMessageRequest {
    host::SendMessageRequest {
        target: host::MessageTarget::Direct("juliet@example.com".parse().expect("recipient")),
        body: DisplayText::new("extension direct body").expect("body"),
        thread_id: None,
        reply_to: None,
        markup: vec![],
        extensions: None,
    }
}

async fn blocked_adapter(f: &IngressFixture) -> ExtensionHostAdapter {
    let mut adapter = adapter(f).await;
    let blocking = Arc::new(InMemoryBlockingStorage::new());
    blocking.set_blocklist(
        "romeo@example.com".parse().expect("requester"),
        vec!["juliet@example.com".parse().expect("recipient")],
    );
    Arc::get_mut(&mut adapter.state)
        .expect("unique host state")
        .deps
        .protocol
        .blocking_storage = blocking;
    adapter
}

pub(super) async fn fail_receipts(f: &IngressFixture) {
    match f.db.driver() {
        DatabaseDriver::Sqlite => f.execute("CREATE TRIGGER fail_host_receipt BEFORE INSERT ON ingress_effect_receipts WHEN NEW.kind = 11 BEGIN SELECT RAISE(FAIL, 'injected host receipt failure'); END", ()).await,
        DatabaseDriver::Postgres => {
            f.execute("CREATE FUNCTION fail_host_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected host receipt failure'; END $$", ()).await;
            f.execute("CREATE TRIGGER fail_host_receipt BEFORE INSERT ON ingress_effect_receipts FOR EACH ROW WHEN (NEW.kind = 11) EXECUTE FUNCTION fail_host_receipt()", ()).await;
        }
    }
}

pub(super) async fn restore_receipts(f: &IngressFixture) {
    let sql = match f.db.driver() {
        DatabaseDriver::Sqlite => "DROP TRIGGER fail_host_receipt",
        DatabaseDriver::Postgres => "DROP TRIGGER fail_host_receipt ON ingress_effect_receipts",
    };
    f.execute(sql, ()).await;
}

async fn wait_for_commit(f: &IngressFixture) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if f.count("ingress_messages").await == 1 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real host call commits before observer cancellation");
}

async fn internal_deadline_accepts_committed_send(f: IngressFixture) {
    let adapter = blocked_adapter(&f).await;
    fail_receipts(&f).await;
    let started = tokio::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        host::ExtensionHostTools::send_message(&adapter, &context(), host_request()),
    )
    .await
    .expect("internal response deadline is shorter than settlement retry budget")
    .expect("committed send remains accepted when receipt persistence fails");
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 11").await, 0);
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    restore_receipts(&f).await;
    // The plugin API generates a fresh origin per invocation. Replay the same
    // returned origin at the adapter boundary to check its internal retry identity.
    let replay = adapter
        .send_message(&invocation(), request(result.stanza_id.as_str()))
        .await;
    assert!(
        replay.is_err(),
        "settled replay exposes the recorded rejection"
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(f.count("ingress_effect_receipts WHERE kind = 11").await > 0);
    drop(adapter);
    f.close().await;
}

async fn observer_timeout_preserves_committed_settlement(f: IngressFixture) {
    let adapter = blocked_adapter(&f).await;
    fail_receipts(&f).await;
    let context = context();
    let response = {
        let call = host::ExtensionHostTools::send_message(&adapter, &context, host_request());
        tokio::pin!(call);
        tokio::select! {
            response = &mut call => panic!("host completed before the committed receipt gate: {response:?}"),
            () = wait_for_commit(&f) => {}
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        tokio::time::timeout_at(deadline, call).await
    };
    assert!(
        response.is_err(),
        "observer drops the real host future after commit"
    );
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NULL").await,
        1
    );
    assert_eq!(f.count("ingress_effect_receipts WHERE kind = 11").await, 0);
    restore_receipts(&f).await;
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    assert_eq!(f.count("ingress_messages").await, 1);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(f.count("ingress_effect_receipts WHERE kind = 11").await > 0);
    assert_eq!(f.count("pending_delivery").await, 0);
    assert_eq!(f.count("notification_candidates").await, 0);
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_host_boundary_internal_deadline_sqlite() {
    internal_deadline_accepts_committed_send(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_host_boundary_internal_deadline_postgres() {
    if let Some(f) = IngressFixture::postgres("host_response_deadline").await {
        internal_deadline_accepts_committed_send(f).await;
    }
}
#[tokio::test]
async fn extension_host_boundary_observer_timeout_sqlite() {
    observer_timeout_preserves_committed_settlement(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_host_boundary_observer_timeout_postgres() {
    if let Some(f) = IngressFixture::postgres("host_observer_timeout").await {
        observer_timeout_preserves_committed_settlement(f).await;
    }
}

async fn cancel_rejection_is_denied(f: IngressFixture) {
    let adapter = blocked_adapter(&f).await;
    let error = host::ExtensionHostTools::send_message(&adapter, &context(), host_request())
        .await
        .expect_err("cancel bounce must reach the plugin");
    assert_eq!(error.code, host::HostToolErrorCode::Denied);
    assert_eq!(
        f.count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        1
    );
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_host_boundary_cancel_is_denied_sqlite() {
    cancel_rejection_is_denied(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_host_boundary_cancel_is_denied_postgres() {
    if let Some(f) = IngressFixture::postgres("host_cancel_denied").await {
        cancel_rejection_is_denied(f).await;
    }
}

#[tokio::test]
async fn extension_host_boundary_known_rejection_survives_receipt_failure() {
    use crate::ingress::{
        nested::{NestedOutcome, SettlementOutcome},
        IngressDecisionClass,
    };
    let outcome = NestedOutcome::Committed {
        decision_class: IngressDecisionClass::Accepted,
        archive_ids: vec![],
        settlement: tokio::spawn(async {
            SettlementOutcome {
                rejection: Some(
                    crate::server::routes::interpret::offline_delivery::offline_quota_error(),
                ),
                terminal: Err(crate::ingress_uow::IngressUowError::AuthorityStopped.into()),
            }
        }),
    };
    assert!(
        matches!(super::super::settlement::finish_nested(outcome).await,
        Err(super::super::ExtensionHostAdapterError::Rejected(error))
            if error.type_ == xmpp_parsers::stanza_error::ErrorType::Cancel)
    );
}

#[test]
fn extension_host_boundary_plan_storage_failure_is_retryable() {
    use crate::server::routes::interpret::effects::PlanFailure;
    let error = super::super::conversions::host_tool_error(
        super::super::ExtensionHostAdapterError::Plan(PlanFailure::RecipientBlocklistRead),
    );
    assert_eq!(error.code, host::HostToolErrorCode::TemporaryFailure);
}

struct UnavailableRecipientBlocklist;

#[derive(Debug, thiserror::Error)]
#[error("injected recipient blocklist failure")]
struct RecipientBlocklistFailure;

#[async_trait::async_trait]
impl waddle_xmpp::xep::xep0191::BlockingStorage for UnavailableRecipientBlocklist {
    async fn list_blocked_jids(
        &self,
        user: &jid::BareJid,
    ) -> Result<Vec<jid::BareJid>, waddle_xmpp::xep::xep0191::BlockingStorageError> {
        if user.node().is_some_and(|node| node.as_str() == "juliet") {
            Err(waddle_xmpp::xep::xep0191::BlockingStorageError::new(
                RecipientBlocklistFailure,
            ))
        } else {
            Ok(vec![])
        }
    }
}

async fn direct_plan_failure_stays_typed(f: IngressFixture) {
    let mut adapter = adapter(&f).await;
    Arc::get_mut(&mut adapter.state)
        .expect("unique state")
        .deps
        .protocol
        .blocking_storage = Arc::new(UnavailableRecipientBlocklist);
    let result = adapter
        .send_message(&invocation(), request("typed-plan-error"))
        .await;
    assert!(
        matches!(
            result,
            Err(super::super::ExtensionHostAdapterError::Plan(
                crate::server::routes::interpret::effects::PlanFailure::RecipientBlocklistRead
            ))
        ),
        "typed plan failure: {result:?}"
    );
    assert_eq!(f.count("ingress_messages").await, 0);
    assert!(
        adapter
            .state
            .deps
            .protocol
            .ingress
            .drain_and_join(Duration::from_secs(10))
            .await
    );
    drop(adapter);
    f.close().await;
}

#[tokio::test]
async fn extension_host_boundary_direct_plan_failure_sqlite() {
    direct_plan_failure_stays_typed(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn extension_host_boundary_direct_plan_failure_postgres() {
    if let Some(f) = IngressFixture::postgres("host_direct_plan_failure").await {
        direct_plan_failure_stays_typed(f).await;
    }
}

#[tokio::test]
async fn extension_host_boundary_nested_refusal_stays_typed() {
    use crate::ingress::{
        nested::{NestedOutcome, NestedRefusal},
        IngressDecisionClass,
    };
    let error = super::super::settlement::finish_nested(NestedOutcome::Refused(
        NestedRefusal::Decision(IngressDecisionClass::Storage),
    ))
    .await
    .expect_err("storage decision is refused");
    assert!(matches!(
        error,
        super::super::ExtensionHostAdapterError::Refused(NestedRefusal::Decision(
            IngressDecisionClass::Storage
        ))
    ));
    assert_eq!(
        super::super::conversions::host_tool_error(error).code,
        host::HostToolErrorCode::TemporaryFailure
    );
}
