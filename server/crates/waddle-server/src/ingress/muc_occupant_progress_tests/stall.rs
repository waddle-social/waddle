//! Cancel the selected stalled copy only after it reaches the delivery seam.
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::ingress::{
    execute::ExecutionReport,
    execute_uow::{CANCEL_STALLED_DELIVERY, STALL_DELIVERY_RESOURCE},
};

pub(super) async fn execute(
    target: jid::FullJid,
    execution: impl Future<Output = ExecutionReport>,
) -> ExecutionReport {
    let entered = Arc::new(AtomicBool::new(false));
    let cancel = tokio_util::sync::CancellationToken::new();
    let scoped = STALL_DELIVERY_RESOURCE.scope(
        (target.clone(), Arc::clone(&entered)),
        CANCEL_STALLED_DELIVERY.scope(cancel.clone(), execution),
    );
    tokio::pin!(scoped);
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            report = &mut scoped => panic!("execution completed before {target} reached the stall hook: {report:?}"),
            () = async {
                while !entered.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        cancel.cancel();
        scoped.await
    }).await.expect("selected resource must reach and leave the delivery hook")
}
