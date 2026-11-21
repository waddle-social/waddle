//! Deliver durable enrolled Space deletion notices to subscribers.

use std::sync::Arc;
use std::time::Duration;

use futures::{stream, StreamExt};
use jid::Jid;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use waddle_xmpp::stream_management::space_deletion_notice_id;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::pubsub::build_pubsub_delete_event;
use xmpp_parsers::message::Id;

use crate::clustering::NodeLifecycle;
use crate::permissions::host_grant::{
    HostGrantGate, HostGrantStore, SpaceDeletionAttempt, SpaceDeletionAttemptOutcome,
    SpaceDeletionDeliveryKind,
};
use crate::s2s::route::{S2sRouter, SpaceDeletionCompletion};
use crate::server::routes::websocket::{local_space_deletion, WebSocketState};

const BATCH_LIMIT: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(30);

enum DeliveryRoute {
    Local(Arc<WebSocketState>),
    Remote(S2sRouter),
}

impl DeliveryRoute {
    fn kind(&self) -> SpaceDeletionDeliveryKind {
        match self {
            Self::Local(_) => SpaceDeletionDeliveryKind::Local,
            Self::Remote(_) => SpaceDeletionDeliveryKind::Remote,
        }
    }
}

pub(super) fn spawn(
    state: Arc<WebSocketState>,
    router: Option<S2sRouter>,
    stop: CancellationToken,
) {
    let grants = &state.deps.app_state.host_grants;
    let lifecycle = state.deps.app_state.node_lifecycle.clone();
    if let Some(router) = router {
        spawn_route(
            grants.clone(),
            lifecycle.clone(),
            stop.clone(),
            DeliveryRoute::Remote(router),
        );
    }
    spawn_route(grants.clone(), lifecycle, stop, DeliveryRoute::Local(state));
}

/// Start the same remote worker on an in-process S2S wire fixture.
#[cfg(feature = "test-support")]
pub(crate) fn spawn_remote_for_test(
    state: &Arc<WebSocketState>,
    router: S2sRouter,
    stop: CancellationToken,
) {
    spawn_route(
        state.deps.app_state.host_grants.clone(),
        state.deps.app_state.node_lifecycle.clone(),
        stop,
        DeliveryRoute::Remote(router),
    );
}

fn spawn_route(
    grants: HostGrantGate,
    lifecycle: NodeLifecycle,
    stop: CancellationToken,
    route: DeliveryRoute,
) {
    let Some(store) = grants.store().cloned() else {
        return;
    };
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = stop.cancelled() => {}
            () = deliver(grants, store, lifecycle, route) => {}
        }
    });
}

async fn deliver(
    grants: HostGrantGate,
    store: HostGrantStore,
    lifecycle: NodeLifecycle,
    route: DeliveryRoute,
) {
    let kind = route.kind();
    loop {
        if lifecycle.is_ready() && grants.attachment().is_some() {
            match store.claim_space_deletion_jobs(kind, BATCH_LIMIT).await {
                Ok(attempts) => {
                    stream::iter(attempts)
                        .for_each_concurrent(BATCH_LIMIT, |attempt| {
                            settle_attempt(&grants, &store, &route, attempt)
                        })
                        .await;
                }
                Err(error) => warn!(?kind, %error, "could not claim Space deletion notices"),
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn settle_attempt(
    grants: &HostGrantGate,
    store: &HostGrantStore,
    route: &DeliveryRoute,
    attempt: SpaceDeletionAttempt,
) {
    let router = match route {
        DeliveryRoute::Local(state) => {
            let result = tokio::time::timeout(
                COMPLETION_DEADLINE,
                local_space_deletion::deliver(state, store, &attempt),
            )
            .await;
            if let Ok(Err(error)) = result {
                warn!(%error, "could not deliver Space deletion to local resources");
            }
            return;
        }
        DeliveryRoute::Remote(router) => router,
    };
    let Some(outcome) = offer_remote_attempt(grants, router, &attempt).await else {
        // The persisted phase settles when an unobserved attempt expires.
        return;
    };
    match store.finish_space_deletion_attempt(&attempt, outcome).await {
        Ok(true) => {}
        Ok(false) => {
            debug!(kind = ?attempt.kind, "Space deletion attempt no longer owns its lease")
        }
        Err(error) => warn!(kind = ?attempt.kind, %error, "could not settle Space deletion notice"),
    }
}

async fn offer_remote_attempt(
    grants: &HostGrantGate,
    router: &S2sRouter,
    attempt: &SpaceDeletionAttempt,
) -> Option<SpaceDeletionAttemptOutcome> {
    let Some(grant) = grants.remote_space_deletion_event(attempt).await else {
        // No transfer was attempted, and the gate may be reattaching.
        return Some(SpaceDeletionAttemptOutcome::Retry);
    };
    let service = attempt.key.service();
    let recipient = Jid::from(attempt.subject.jid().clone());
    let mut notice =
        build_pubsub_delete_event(&Jid::from(service.clone()), &recipient, attempt.key.node());
    notice.id = Some(Id(space_deletion_notice_id(
        attempt.record.operation.as_uuid(),
        attempt.subject.jid(),
    )));
    let completion = match router.offer_space_deletion(
        service,
        &recipient,
        Stanza::Message(notice),
        Arc::new(grant),
    ) {
        Ok(receiver) => tokio::time::timeout(COMPLETION_DEADLINE, receiver)
            .await
            .ok()?
            .ok()?,
        Err(outcome) => outcome,
    };
    Some(match completion {
        SpaceDeletionCompletion::WrittenToTls => SpaceDeletionAttemptOutcome::Custody,
        SpaceDeletionCompletion::RetryBeforeStage => SpaceDeletionAttemptOutcome::Retry,
        SpaceDeletionCompletion::PermanentRefusal => SpaceDeletionAttemptOutcome::Refused,
        SpaceDeletionCompletion::Ambiguous => SpaceDeletionAttemptOutcome::Ambiguous,
    })
}
