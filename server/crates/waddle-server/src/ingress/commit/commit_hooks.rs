//! Commit fault-injection seams. Production defaults are no-ops; tests opt in
//! with task-local scopes, so concurrent connections remain independent.
use super::IngressDecisionClass;
#[cfg(not(test))]
use crate::ingress_uow::IngressUowError;

pub(super) fn observe_class(_class: IngressDecisionClass) {
    #[cfg(test)]
    let _ = OBSERVED_CLASS.try_with(|observed| observed.set(Some(_class)));
}
#[cfg(not(test))]
pub(super) fn take_failure() -> Option<IngressUowError> {
    None
}
#[cfg(not(test))]
pub(super) fn consume_serialization_failure() -> bool {
    false
}
#[cfg(not(test))]
pub(super) fn ambiguous_commit() -> bool {
    false
}
#[cfg(test)]
pub(crate) use injected::*;
#[cfg(test)]
mod injected {
    use crate::ingress_uow::IngressUowError;
    tokio::task_local! {
        pub static SERIALIZATION_FAILURES: std::cell::Cell<usize>;
        pub static AMBIGUOUS_COMMIT: bool;
        pub static OBSERVED_CLASS: std::cell::Cell<Option<super::IngressDecisionClass>>;
        pub static FAILURE: std::cell::RefCell<Option<IngressUowError>>;
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
