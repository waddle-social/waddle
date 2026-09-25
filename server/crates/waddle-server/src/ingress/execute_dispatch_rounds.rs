//! Revisit deferred effects fairly without repeating completed or uncertain work.
use tokio::time::Instant;

use super::{ExternalEffect, ExternalOutcome};
use crate::ingress::DispatchProbeBudget;

pub(super) async fn resume_deferred(
    outcomes: &[(ExternalEffect, ExternalOutcome)],
    recheck: &[bool],
    completed: &mut [Option<bool>],
    budget: &DispatchProbeBudget,
    deadline: Instant,
) -> bool {
    let pending: Vec<_> = outcomes
        .iter()
        .zip(recheck)
        .enumerate()
        .filter_map(|(index, ((_, outcome), eligible))| {
            (*eligible && *outcome == ExternalOutcome::AwaitingPredecessor).then_some(index)
        })
        .collect();
    if pending.is_empty() || Instant::now() >= deadline {
        return false;
    }
    let Some(delay) = budget.next_backoff() else {
        return false;
    };
    if tokio::time::timeout_at(deadline, tokio::time::sleep(delay))
        .await
        .is_err()
        || Instant::now() >= deadline
    {
        return false;
    }
    for index in pending {
        completed[index] = None;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use waddle_xmpp::Stanza;

    fn effect() -> ExternalEffect {
        ExternalEffect::Frame(Box::new(Stanza::Message(
            xmpp_parsers::message::Message::new(None),
        )))
    }

    #[tokio::test(start_paused = true)]
    async fn retry_sleep_cannot_renew_the_execution_deadline() {
        let outcomes = vec![(effect(), ExternalOutcome::AwaitingPredecessor)];
        let mut completed = vec![Some(false)];
        let budget = DispatchProbeBudget::default();
        let deadline = Instant::now() + Duration::from_millis(1);
        assert!(!resume_deferred(&outcomes, &[true], &mut completed, &budget, deadline).await);
        assert_eq!(
            completed,
            vec![Some(false)],
            "expired retries remain deferred"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn pending_only_barriers_do_not_consume_retry_rounds() {
        let outcomes = vec![(effect(), ExternalOutcome::AwaitingPredecessor)];
        let mut completed = vec![Some(false)];
        let budget = DispatchProbeBudget::default();
        let now = Instant::now();
        assert!(
            !resume_deferred(
                &outcomes,
                &[false],
                &mut completed,
                &budget,
                now + Duration::from_secs(5),
            )
            .await
        );
        assert_eq!(Instant::now(), now);
        assert_eq!(budget.consumed_backoffs(), 0);
    }
}
