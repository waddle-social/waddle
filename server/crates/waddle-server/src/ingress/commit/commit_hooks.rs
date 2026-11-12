//! Commit fault-injection seams. Production defaults are no-ops; tests opt in
//! with task-local scopes, so concurrent connections remain independent.
use super::IngressDecisionClass;
#[cfg(not(test))]
use crate::ingress_uow::IngressUowError;

pub(super) fn observe_class(_class: IngressDecisionClass) {
    #[cfg(test)]
    let _ = OBSERVED_CLASS.try_with(|observed| observed.set(Some(_class)));
}
pub(super) fn fail_after_manifest() -> bool {
    #[cfg(test)]
    {
        FAIL_AFTER_MANIFEST
            .try_with(|enabled| *enabled)
            .unwrap_or(false)
    }
    #[cfg(not(test))]
    {
        false
    }
}
#[cfg(all(test, feature = "clustering"))]
pub(in crate::ingress) fn allow_clustered_room_manifests() -> bool {
    ALLOW_CLUSTERED_ROOM_MANIFESTS
        .try_with(|enabled| *enabled)
        .unwrap_or(false)
}
#[cfg(not(test))]
pub(super) fn take_failure() -> Option<IngressUowError> {
    None
}
#[cfg(not(test))]
pub(super) fn consume_serialization_failure() -> bool {
    false
}
#[cfg(all(not(test), not(feature = "test-support")))]
pub(super) fn ambiguous_commit() -> bool {
    false
}
#[cfg(all(not(test), feature = "test-support"))]
tokio::task_local! {
    static TEST_SUPPORT_AMBIGUOUS_COMMIT: bool;
}
#[cfg(all(not(test), feature = "test-support"))]
pub(super) fn ambiguous_commit() -> bool {
    TEST_SUPPORT_AMBIGUOUS_COMMIT
        .try_with(|value| *value)
        .unwrap_or(false)
}

/// Simulate losing only an acknowledged commit reply for an isolated test.
#[cfg(feature = "test-support")]
pub(crate) async fn with_simulated_lost_commit_ack<F: std::future::Future>(future: F) -> F::Output {
    #[cfg(test)]
    {
        AMBIGUOUS_COMMIT.scope(true, future).await
    }
    #[cfg(not(test))]
    {
        TEST_SUPPORT_AMBIGUOUS_COMMIT.scope(true, future).await
    }
}
#[cfg(test)]
pub(crate) use injected::*;
#[cfg(test)]
mod injected {
    use crate::ingress_uow::IngressUowError;
    tokio::task_local! {
        pub static SERIALIZATION_FAILURES: std::cell::Cell<usize>;
        pub static AMBIGUOUS_COMMIT: bool;
        pub static FAIL_AFTER_MANIFEST: bool;
        pub static ALLOW_CLUSTERED_ROOM_MANIFESTS: bool;
        pub static OBSERVED_CLASS: std::cell::Cell<Option<super::IngressDecisionClass>>;
        pub static FAILURE: std::cell::RefCell<Option<IngressUowError>>;
        pub static SEARCH_PATH: String;
        pub static ALIAS_TEST_BOUNDS: (std::time::Duration, std::time::Duration);
        pub static ALIAS_OBSERVED_BOUNDS: std::sync::Arc<std::sync::Mutex<Option<(i64, i64)>>>;
    }
    pub(crate) async fn observe_alias_bounds(
        tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    ) -> Result<(), IngressUowError> {
        let Ok(observed) = ALIAS_OBSERVED_BOUNDS.try_with(std::sync::Arc::clone) else {
            return Ok(());
        };
        let mut rows = tx.transaction_mut().query(
            "SELECT (extract(epoch FROM current_setting('lock_timeout')::interval) * 1000)::bigint, (extract(epoch FROM current_setting('statement_timeout')::interval) * 1000)::bigint",
            (),
        ).await?;
        let row = rows
            .next()
            .await?
            .ok_or(IngressUowError::EpochProofMissing)?;
        *observed.lock().expect("test timeout observation lock") = Some((row.get(0)?, row.get(1)?));
        Ok(())
    }
    pub(crate) async fn apply_search_path(
        tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    ) -> Result<(), IngressUowError> {
        let Ok(path) = SEARCH_PATH.try_with(Clone::clone) else {
            return Ok(());
        };
        tx.transaction_mut()
            .execute(
                "SELECT pg_catalog.set_config('search_path', ?, true)",
                crate::db_params![path],
            )
            .await?;
        Ok(())
    }
    pub(crate) fn consume_serialization_failure() -> bool {
        SERIALIZATION_FAILURES
            .try_with(|remaining| {
                let count = remaining.get();
                remaining.set(count.saturating_sub(1));
                count > 0
            })
            .unwrap_or(false)
    }
    pub(crate) fn take_failure() -> Option<IngressUowError> {
        FAILURE
            .try_with(|failure| failure.borrow_mut().take())
            .ok()
            .flatten()
    }
    pub(crate) fn ambiguous_commit() -> bool {
        AMBIGUOUS_COMMIT.try_with(|value| *value).unwrap_or(false)
    }
}
