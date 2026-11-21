//! Deliver durable enrolled Space deletion notices to remote subscribers.

use std::sync::Arc;
use std::time::Duration;

use futures::{stream, StreamExt};
use jid::Jid;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::pubsub::build_pubsub_delete_event;
use xmpp_parsers::message::Id;

use crate::clustering::NodeLifecycle;
use crate::permissions::host_grant::{
    HostGrantGate, HostGrantStore, SpaceDeletionAttempt, SpaceDeletionAttemptOutcome,
    SpaceDeletionDeliveryKind,
};
use crate::s2s::route::{S2sRouter, SpaceDeletionCompletion};

const BATCH_LIMIT: usize = 8;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const COMPLETION_DEADLINE: Duration = Duration::from_secs(30);

pub(super) fn spawn_remote(
    grants: HostGrantGate,
    router: S2sRouter,
    lifecycle: NodeLifecycle,
    stop: CancellationToken,
) {
    let Some(store) = grants.store().cloned() else {
        return;
    };
    tokio::spawn(async move {
        tokio::select! {
            biased;
            () = stop.cancelled() => {}
            () = deliver_remote(grants, store, router, lifecycle) => {}
        }
    });
}

async fn deliver_remote(
    grants: HostGrantGate,
    store: HostGrantStore,
    router: S2sRouter,
    lifecycle: NodeLifecycle,
) {
    loop {
        if lifecycle.is_ready() && grants.attachment().is_some() {
            match store
                .claim_space_deletion_jobs(SpaceDeletionDeliveryKind::Remote, BATCH_LIMIT)
                .await
            {
                Ok(attempts) => {
                    stream::iter(attempts)
                        .for_each_concurrent(BATCH_LIMIT, |attempt| {
                            settle_remote_attempt(&grants, &store, &router, attempt)
                        })
                        .await
                }
                Err(error) => warn!(%error, "could not claim remote Space deletion notices"),
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn settle_remote_attempt(
    grants: &HostGrantGate,
    store: &HostGrantStore,
    router: &S2sRouter,
    attempt: SpaceDeletionAttempt,
) {
    let Some(outcome) = offer_remote_attempt(grants, router, &attempt).await else {
        // A vanished worker or an elapsed observation deadline provides no
        // transfer evidence. The persisted phase settles when the lease expires.
        return;
    };
    match store.finish_space_deletion_attempt(&attempt, outcome).await {
        Ok(true) => {}
        Ok(false) => debug!("remote Space deletion attempt no longer owns its lease"),
        Err(error) => warn!(%error, "could not settle remote Space deletion notice"),
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
    notice.id = Some(Id(attempt.record.operation.as_uuid().simple().to_string()));
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
