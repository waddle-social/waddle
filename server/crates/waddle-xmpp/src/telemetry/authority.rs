//! Admission, custody, fencing, mailbox and transport-progress metrics.
//!
//! One typed recording function per observation, so a call site names what
//! happened and cannot choose a metric name or an attribute value of its
//! own. Every attribute is a closed enum from
//! [`super::attributes`]; no function takes a JID, a stream id, a node id
//! or a string.
//!
//! Counters whose alert reads "more than N on a healthy pod" are
//! zero-registered through [`register_authority_counters`] for the outcomes
//! that mean something went wrong. Counters that only prove progress
//! (`entered`, `acquired`, `completed`, `acked`) are not: an alert on their
//! absence must see no series, not a flat zero.

use std::time::Duration;

use super::attributes::{
    ActorLane, ActorRetirementCause, ActorTurnEvent, DestroyCompletionOutcome,
    IngressEntryAuthorityKind, IngressEntryOutcome, IoExitCause, IoPump, KeypairLeaseOutcome,
    MailboxRejection, RelayOutcome, RemoteDispatchOutcome, RestrictedStartupStage, RestrictedStore,
    SmEnableOutcome,
};
use super::SECOND_SCALE_BUCKETS;

/// Boundaries for histograms that sample a queue length.
const DEPTH_BUCKETS: [f64; 12] = [
    0.0, 1.0, 2.0, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0,
];

/// Boundaries for histograms that sample retained bytes, 1 KiB to 64 MiB.
const BYTE_BUCKETS: [f64; 9] = [
    1024.0,
    4096.0,
    16384.0,
    65536.0,
    262_144.0,
    1_048_576.0,
    4_194_304.0,
    16_777_216.0,
    67_108_864.0,
];

/// Boundaries for histograms that sample how long work has waited.
const AGE_BUCKETS: [f64; 11] = [
    0.1, 0.5, 1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 900.0, 3600.0,
];

fn add_io_pump_exit(count: u64, pump: IoPump, cause: IoExitCause) {
    crate::counter_add!(
        "waddle.io.pump.exits",
        "{exit}",
        "Supervised transport pumps that ended, by pump and cause.",
        count,
        pump,
        cause,
    );
}

/// One supervised reader or writer pump ended.
pub fn record_io_pump_exit(pump: IoPump, cause: IoExitCause) {
    add_io_pump_exit(1, pump, cause);
}

fn add_io_custodian_retirement(count: u64, cause: IoExitCause) {
    crate::counter_add!(
        "waddle.io.custodian.retirements",
        "{retirement}",
        "Transport custodians that retired their socket and pumps, by cause.",
        count,
        cause,
    );
}

/// The custodian of one transport retired it.
pub fn record_io_custodian_retirement(cause: IoExitCause) {
    add_io_custodian_retirement(1, cause);
}

/// How long one write waited on a peer that was not reading.
pub fn record_io_writer_blocked(blocked: Duration) {
    crate::histogram_record!(
        "waddle.io.writer.blocked.duration",
        "s",
        "Time a transport write spent blocked on the peer before it completed or was abandoned.",
        buckets: SECOND_SCALE_BUCKETS,
        blocked.as_secs_f64(),
    );
}

/// A sample of one connection actor mailbox lane: queued messages, their
/// retained bytes and the age of the oldest.
pub fn record_actor_mailbox(lane: ActorLane, depth: usize, bytes: usize, oldest: Duration) {
    crate::histogram_record!(
        "waddle.connection_actor.mailbox.depth",
        "{message}",
        "Messages queued in a connection actor mailbox lane when sampled.",
        buckets: DEPTH_BUCKETS,
        depth as f64,
        lane,
    );
    crate::histogram_record!(
        "waddle.connection_actor.mailbox.bytes",
        "By",
        "Bytes retained by a connection actor mailbox lane when sampled.",
        buckets: BYTE_BUCKETS,
        bytes as f64,
        lane,
    );
    crate::histogram_record!(
        "waddle.connection_actor.mailbox.oldest_age",
        "s",
        "Age of the oldest message in a connection actor mailbox lane when sampled; zero when empty.",
        buckets: AGE_BUCKETS,
        oldest.as_secs_f64(),
        lane,
    );
}

fn add_actor_mailbox_rejected(count: u64, lane: ActorLane, reason: MailboxRejection) {
    crate::counter_add!(
        "waddle.connection_actor.mailbox.rejected",
        "{message}",
        "Messages a connection actor mailbox refused, by lane and reason.",
        count,
        lane,
        reason,
    );
}

/// A connection actor mailbox refused a message.
pub fn record_actor_mailbox_rejected(lane: ActorLane, reason: MailboxRejection) {
    add_actor_mailbox_rejected(1, lane, reason);
}

/// One connection actor turn completed. The histogram's count is the number
/// of turns, so a stalled actor shows as a rate that stops.
pub fn record_actor_turn(event: ActorTurnEvent, duration: Duration) {
    crate::histogram_record!(
        "waddle.connection_actor.turn.duration",
        "s",
        "Duration of one connection actor turn, by the event it handled.",
        buckets: SECOND_SCALE_BUCKETS,
        duration.as_secs_f64(),
        event,
    );
}

fn add_actor_retirement(count: u64, cause: ActorRetirementCause) {
    crate::counter_add!(
        "waddle.connection_actor.retirements",
        "{actor}",
        "Connection actors that retired, by cause.",
        count,
        cause,
    );
}

/// A connection actor retired.
pub fn record_actor_retirement(cause: ActorRetirementCause) {
    add_actor_retirement(1, cause);
}

fn add_restricted_startup_refusal(count: u64, stage: RestrictedStartupStage) {
    crate::counter_add!(
        "waddle.restricted_runtime.startup.refusals",
        "{refusal}",
        "Starts under the restricted database role that refused to serve, by stage.",
        count,
        stage,
    );
}

/// A start under the restricted database role refused at `stage`. The
/// process exits afterwards, so the caller flushes metrics before it does.
pub fn record_restricted_startup_refusal(stage: RestrictedStartupStage) {
    add_restricted_startup_refusal(1, stage);
}

fn add_restricted_attach_failure(count: u64, store: RestrictedStore) {
    crate::counter_add!(
        "waddle.restricted_runtime.attach.failures",
        "{failure}",
        "Store attachments that failed verification under the restricted database role, by store.",
        count,
        store,
    );
}

/// The runtime attachment of `store` failed its verification.
pub fn record_restricted_attach_failure(store: RestrictedStore) {
    add_restricted_attach_failure(1, store);
}

fn add_ingress_entry(
    count: u64,
    authority: IngressEntryAuthorityKind,
    outcome: IngressEntryOutcome,
) {
    crate::counter_add!(
        "waddle.ingress.entry.transactions",
        "{transaction}",
        "Ingress transactions requested through the typed entry, by authority and outcome.",
        count,
        authority,
        outcome,
    );
}

/// One ingress transaction entry ended, with the time from the request to
/// the prepared transaction or the refusal.
pub fn record_ingress_entry(
    authority: IngressEntryAuthorityKind,
    outcome: IngressEntryOutcome,
    preparation: Duration,
) {
    add_ingress_entry(1, authority, outcome);
    crate::histogram_record!(
        "waddle.ingress.entry.preparation.duration",
        "s",
        "Time from an ingress entry request to its prepared transaction or refusal, by authority.",
        buckets: SECOND_SCALE_BUCKETS,
        preparation.as_secs_f64(),
        authority,
    );
}

fn add_destroy_completion(count: u64, outcome: DestroyCompletionOutcome) {
    crate::counter_add!(
        "waddle.muc.destroy_completion.outcomes",
        "{completion}",
        "Leased room-destroy completions, by outcome.",
        count,
        outcome,
    );
}

/// One leased room-destroy completion ended.
pub fn record_destroy_completion(outcome: DestroyCompletionOutcome) {
    add_destroy_completion(1, outcome);
}

/// A successful reading of the room-destroy completion queue. Record every
/// successful reading, including zero; on a failed reading record nothing,
/// so the previous values stand.
pub fn record_destroy_completion_backlog(depth: u64, oldest: Duration) {
    crate::gauge_record!(
        "waddle.muc.destroy_completion.depth",
        "{completion}",
        "Room-destroy completions waiting in the durable queue.",
        depth,
    );
    crate::gauge_record!(
        "waddle.muc.destroy_completion.oldest_age",
        "s",
        "Age of the oldest waiting room-destroy completion; zero when the queue is empty.",
        oldest.as_secs(),
    );
}

fn add_keypair_lease(count: u64, outcome: KeypairLeaseOutcome) {
    crate::counter_add!(
        "waddle.clustering.keypair_lease.outcomes",
        "{operation}",
        "Keypair-slot lease operations, by outcome.",
        count,
        outcome,
    );
}

/// One keypair-slot lease operation ended.
pub fn record_keypair_lease(outcome: KeypairLeaseOutcome) {
    add_keypair_lease(1, outcome);
}

fn add_relay_outcome(count: u64, outcome: RelayOutcome) {
    crate::counter_add!(
        "waddle.relay.ordered.outcomes",
        "{delivery}",
        "Ordered relay deliveries as their sender saw them end, by outcome.",
        count,
        outcome,
    );
}

/// One ordered relay delivery ended for its sender.
pub fn record_relay_outcome(outcome: RelayOutcome) {
    add_relay_outcome(1, outcome);
}

/// A sample of one ordered relay channel: queued deliveries and the age of
/// the oldest.
pub fn record_relay_queue(depth: usize, oldest: Duration) {
    crate::histogram_record!(
        "waddle.relay.ordered.queue.depth",
        "{delivery}",
        "Deliveries queued on an ordered relay channel when sampled.",
        buckets: DEPTH_BUCKETS,
        depth as f64,
    );
    crate::histogram_record!(
        "waddle.relay.ordered.queue.oldest_age",
        "s",
        "Age of the oldest delivery queued on an ordered relay channel when sampled; zero when empty.",
        buckets: AGE_BUCKETS,
        oldest.as_secs_f64(),
    );
}

fn add_sm_enable(count: u64, outcome: SmEnableOutcome) {
    crate::counter_add!(
        "xmpp.sm.enable.results",
        "{request}",
        "XEP-0198 enable requests, by outcome.",
        count,
        outcome,
    );
}

/// One XEP-0198 enable request ended.
pub fn record_sm_enable(outcome: SmEnableOutcome) {
    add_sm_enable(1, outcome);
}

/// A successful XEP-0198 resumption replayed `stanzas` unacknowledged
/// stanzas.
pub fn record_sm_resume_replay(stanzas: usize) {
    crate::histogram_record!(
        "xmpp.sm.resume.replayed",
        "{stanza}",
        "Unacknowledged stanzas replayed by a successful XEP-0198 resumption.",
        buckets: DEPTH_BUCKETS,
        stanzas as f64,
    );
}

/// The unacknowledged queue of one stream held `depth` stanzas when the
/// client's acknowledgement was processed.
pub fn record_sm_unacked_depth(depth: usize) {
    crate::histogram_record!(
        "xmpp.sm.unacked.depth",
        "{stanza}",
        "Stanzas still unacknowledged on a stream after an acknowledgement was processed.",
        buckets: DEPTH_BUCKETS,
        depth as f64,
    );
}

fn add_remote_dispatch(count: u64, outcome: RemoteDispatchOutcome) {
    crate::counter_add!(
        "waddle.muc.remote_dispatch.outcomes",
        "{submission}",
        "Submissions to a room owned by another node, by outcome.",
        count,
        outcome,
    );
}

/// One submission to a room owned by another node ended.
pub fn record_remote_dispatch(outcome: RemoteDispatchOutcome) {
    add_remote_dispatch(1, outcome);
}

/// A sample of the submissions one stream has sent to a remote room owner
/// and not yet seen resolved: how many, and the age of the oldest.
pub fn record_pending_acceptance(pending: usize, oldest: Duration) {
    crate::histogram_record!(
        "waddle.muc.remote_acceptance.pending",
        "{submission}",
        "Remote-room submissions awaiting the owner's acceptance when sampled.",
        buckets: DEPTH_BUCKETS,
        pending as f64,
    );
    crate::histogram_record!(
        "waddle.muc.remote_acceptance.oldest_pending_age",
        "s",
        "Age of the oldest remote-room submission awaiting acceptance when sampled; zero with none.",
        buckets: AGE_BUCKETS,
        oldest.as_secs_f64(),
    );
}

/// Create, at zero, every series of this module that an alert reads as
/// "above N". Called once at startup through
/// [`super::reliability::register_reliability_counters`].
pub(super) fn register_authority_counters() {
    for pump in IoPump::ALL {
        for cause in [
            IoExitCause::Panicked,
            IoExitCause::Abandoned,
            IoExitCause::Deadline,
        ] {
            add_io_pump_exit(0, pump, cause);
        }
    }
    for cause in [
        IoExitCause::Panicked,
        IoExitCause::Abandoned,
        IoExitCause::Deadline,
    ] {
        add_io_custodian_retirement(0, cause);
    }
    for lane in ActorLane::ALL {
        for reason in MailboxRejection::ALL {
            add_actor_mailbox_rejected(0, lane, reason);
        }
    }
    for cause in [
        ActorRetirementCause::Panic,
        ActorRetirementCause::Abandoned,
        ActorRetirementCause::Deadline,
    ] {
        add_actor_retirement(0, cause);
    }
    for stage in RestrictedStartupStage::ALL {
        add_restricted_startup_refusal(0, stage);
    }
    for store in RestrictedStore::ALL {
        add_restricted_attach_failure(0, store);
    }
    for authority in IngressEntryAuthorityKind::ALL {
        for outcome in IngressEntryOutcome::ALL {
            if outcome != IngressEntryOutcome::Entered {
                add_ingress_entry(0, authority, outcome);
            }
        }
    }
    for outcome in [
        DestroyCompletionOutcome::LeaseTakenOver,
        DestroyCompletionOutcome::Failed,
    ] {
        add_destroy_completion(0, outcome);
    }
    for outcome in [
        KeypairLeaseOutcome::Exhausted,
        KeypairLeaseOutcome::FencingLost,
    ] {
        add_keypair_lease(0, outcome);
    }
    for outcome in [
        RelayOutcome::Nacked,
        RelayOutcome::Uncertain,
        RelayOutcome::TimedOut,
        RelayOutcome::IncompatiblePeer,
    ] {
        add_relay_outcome(0, outcome);
    }
    add_sm_enable(0, SmEnableOutcome::Failed);
    for outcome in [
        RemoteDispatchOutcome::Rejected,
        RemoteDispatchOutcome::Uncertain,
        RemoteDispatchOutcome::OwnerMoved,
        RemoteDispatchOutcome::TimedOut,
    ] {
        add_remote_dispatch(0, outcome);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::telemetry::test_support::MetricsTestGuard;

    /// The exported values of the series of `metric` carrying every pair in
    /// `attributes`. A counter that shares its name with zero-registered
    /// series still exports, so the presence of one series is read from its
    /// own samples and never from a sum over the metric.
    fn series_values(
        guard: &MetricsTestGuard,
        metric: &str,
        attributes: &[(&str, &str)],
    ) -> Vec<u64> {
        guard
            .counter_samples(metric)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, sample)| {
                attributes.iter().all(|(key, value)| {
                    sample.iter().any(|(sample_key, sample_value)| {
                        sample_key == key && sample_value == value
                    })
                })
            })
            .map(|(value, _)| value)
            .collect()
    }

    #[tokio::test]
    async fn failure_series_exist_at_zero_and_progress_series_do_not() {
        let guard = crate::telemetry::test_support::acquire().await;
        crate::telemetry::reliability::register_reliability_counters();
        for (metric, attributes) in [
            (
                "waddle.io.pump.exits",
                vec![("pump", "writer"), ("cause", "panicked")],
            ),
            (
                "waddle.connection_actor.retirements",
                vec![("cause", "abandoned")],
            ),
            (
                "waddle.restricted_runtime.attach.failures",
                vec![("store", "mam")],
            ),
            (
                "waddle.ingress.entry.transactions",
                vec![
                    ("authority", "installed_guard"),
                    ("outcome", "preparation_timeout"),
                ],
            ),
            (
                "waddle.clustering.keypair_lease.outcomes",
                vec![("outcome", "fencing_lost")],
            ),
            (
                "waddle.relay.ordered.outcomes",
                vec![("outcome", "uncertain")],
            ),
            (
                "waddle.muc.remote_dispatch.outcomes",
                vec![("outcome", "uncertain")],
            ),
        ] {
            let values = series_values(&guard, metric, &attributes);
            assert!(!values.is_empty(), "{metric} {attributes:?} is registered");
            assert!(
                values.iter().all(|value| *value == 0),
                "{metric} {attributes:?} starts at zero: {values:?}"
            );
        }
        for (metric, attributes) in [
            (
                "waddle.ingress.entry.transactions",
                vec![("authority", "unattested"), ("outcome", "entered")],
            ),
            (
                "waddle.clustering.keypair_lease.outcomes",
                vec![("outcome", "acquired")],
            ),
            ("waddle.relay.ordered.outcomes", vec![("outcome", "acked")]),
        ] {
            assert_eq!(
                series_values(&guard, metric, &attributes),
                Vec::<u64>::new(),
                "{metric} {attributes:?} has no series before progress"
            );
        }
    }

    #[tokio::test]
    async fn recorded_observations_are_exported_with_bounded_attributes() {
        let guard = crate::telemetry::test_support::acquire().await;
        record_ingress_entry(
            IngressEntryAuthorityKind::InstalledGuard,
            IngressEntryOutcome::Entered,
            Duration::from_millis(3),
        );
        record_keypair_lease(KeypairLeaseOutcome::FencingLost);
        record_actor_mailbox(ActorLane::Data, 7, 2048, Duration::from_secs(2));
        record_destroy_completion_backlog(4, Duration::from_secs(90));
        assert_eq!(
            guard.counter_sum(
                "waddle.ingress.entry.transactions",
                &[("authority", "installed_guard"), ("outcome", "entered")]
            ),
            Some(1)
        );
        assert_eq!(
            guard.histogram_count(
                "waddle.ingress.entry.preparation.duration",
                &[("authority", "installed_guard")]
            ),
            Some(1)
        );
        assert_eq!(
            guard.counter_sum(
                "waddle.clustering.keypair_lease.outcomes",
                &[("outcome", "fencing_lost")]
            ),
            Some(1)
        );
        for metric in [
            "waddle.connection_actor.mailbox.depth",
            "waddle.connection_actor.mailbox.bytes",
            "waddle.connection_actor.mailbox.oldest_age",
        ] {
            assert_eq!(guard.histogram_count(metric, &[("lane", "data")]), Some(1));
        }
    }
}
