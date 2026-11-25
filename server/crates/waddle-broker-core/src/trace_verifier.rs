//! Secret-free JSONL execution trace schema and runtime monitor verifier.
//!
//! Mirrors the 3 PObserve monitor state machines in `server/specs/pobserve/`:
//! - `AdmissionFenceMonitor`
//! - `PlacementLeaseMonitor`
//! - `StreamCancellationMonitor`

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One secret-free structured runtime trace event emitted as a JSONL line.
///
/// Deliberately contains NO fields for prompts, model responses, provider API
/// keys, ciphertext envelopes, auth tokens, or raw JIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRecord {
    pub ts_logical: u64,
    /// Opaque replica/cell identifier (never raw JID or secret).
    pub actor_id: String,
    /// Opaque tenant slug or hash.
    pub tenant_hash: String,
    pub event: TraceEvent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum TraceEvent {
    FenceClose {
        fence_kind: String,
        new_gen: u64,
    },
    FenceReopen {
        fence_kind: String,
        gen: u64,
    },
    ReplicaFenceAck {
        replica: String,
        gen: u64,
    },
    RevokeAck {
        gen: u64,
    },
    AdmissionCommit {
        attempt_id: String,
        payer: String,
        requested_payer: String,
        cred_gen: u64,
        room_gen: u64,
        place_gen: u64,
    },
    ProviderDispatch {
        attempt_id: String,
        cred_gen: u64,
    },
    LeaseClaim {
        holder_key: String,
        gen: u64,
    },
    LeaseTransfer {
        from_key: String,
        to_key: String,
        new_gen: u64,
    },
    LeaseRelease {
        holder_key: String,
        gen: u64,
    },
    TenantWrite {
        writer_key: String,
        gen: u64,
    },
    StreamOpen {
        stream_id: String,
        permits_after: u64,
        max_permits: u64,
    },
    StreamCancelReq {
        stream_id: String,
        tick: u64,
    },
    StreamAbortDone {
        stream_id: String,
        tick: u64,
        permits_after: u64,
    },
    StreamQuarantine {
        stream_id: String,
        permits_after: u64,
    },
    StreamClose {
        stream_id: String,
        permits_after: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MonitorViolation {
    #[error("fence generation did not strictly increase for ({tenant}, {fence_kind}): prev={prev}, new={new_gen}")]
    NonMonotonicFenceGen {
        tenant: String,
        fence_kind: String,
        prev: u64,
        new_gen: u64,
    },
    #[error("silent payer fallback detected for attempt {attempt_id}: requested={requested}, actual={actual}")]
    SilentPayerFallback {
        attempt_id: String,
        requested: String,
        actual: String,
    },
    #[error("duplicate dispatch for attempt {attempt_id}")]
    DuplicateAttemptDispatch { attempt_id: String },
    #[error("post-revoke-ack dispatch for tenant {tenant} attempt {attempt_id}: cred_gen={cred_gen} < revoked_gen={revoked_gen}")]
    PostRevokeDispatch {
        tenant: String,
        attempt_id: String,
        cred_gen: u64,
        revoked_gen: u64,
    },
    #[error("lease generation did not strictly increase for tenant {tenant}: prev={prev}, new={new_gen}")]
    NonMonotonicLeaseGen {
        tenant: String,
        prev: u64,
        new_gen: u64,
    },
    #[error(
        "lease transfer holder mismatch for tenant {tenant}: expected={expected}, got={actual}"
    )]
    LeaseTransferMismatch {
        tenant: String,
        expected: String,
        actual: String,
    },
    #[error(
        "stale or unauthorized tenant write for tenant {tenant}: writer={writer_key} gen={gen}"
    )]
    StaleTenantWrite {
        tenant: String,
        writer_key: String,
        gen: u64,
    },
    #[error("quarantined stream {stream_id} attempted re-open/retry")]
    QuarantinedStreamRetry { stream_id: String },
    #[error(
        "stream permit bound exceeded for {stream_id}: permits={permits_after} > max={max_permits}"
    )]
    StreamPermitBoundExceeded {
        stream_id: String,
        permits_after: u64,
        max_permits: u64,
    },
    #[error("stream abort without open or cancel request for {stream_id}")]
    InvalidStreamAbort { stream_id: String },
    #[error("stream quarantine on non-open stream {stream_id}")]
    InvalidStreamQuarantine { stream_id: String },
}

#[derive(Debug, Error)]
pub enum TraceVerifyError {
    #[error("invalid JSONL trace at line {line}: {source}")]
    JsonParse {
        line: usize,
        source: serde_json::Error,
    },
    #[error("monitor violation at line {line}: {violation}")]
    Violation {
        line: usize,
        violation: MonitorViolation,
    },
    #[error("secret sentinel leaked into trace output")]
    SecretSentinelLeaked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TraceSummary {
    pub records_processed: usize,
    pub dispatches_observed: usize,
    pub tenant_writes_observed: usize,
    pub streams_opened: usize,
}

#[derive(Debug, Default)]
pub struct AdmissionFenceMonitor {
    fence_gens: HashMap<(String, String), u64>,
    cred_revoked_gen: HashMap<String, u64>,
    dispatched_attempts: HashSet<String>,
    dispatches_observed: usize,
}

impl AdmissionFenceMonitor {
    pub fn observe(&mut self, rec: &TraceRecord) -> Result<(), MonitorViolation> {
        match &rec.event {
            TraceEvent::FenceClose {
                fence_kind,
                new_gen,
            } => {
                let key = (rec.tenant_hash.clone(), fence_kind.clone());
                if let Some(&prev) = self.fence_gens.get(&key) {
                    if *new_gen <= prev {
                        return Err(MonitorViolation::NonMonotonicFenceGen {
                            tenant: rec.tenant_hash.clone(),
                            fence_kind: fence_kind.clone(),
                            prev,
                            new_gen: *new_gen,
                        });
                    }
                }
                self.fence_gens.insert(key, *new_gen);
            }
            TraceEvent::RevokeAck { gen } => {
                let entry = self
                    .cred_revoked_gen
                    .entry(rec.tenant_hash.clone())
                    .or_insert(0);
                if *gen > *entry {
                    *entry = *gen;
                }
            }
            TraceEvent::AdmissionCommit {
                attempt_id,
                payer,
                requested_payer,
                ..
            } => {
                if payer != requested_payer {
                    return Err(MonitorViolation::SilentPayerFallback {
                        attempt_id: attempt_id.clone(),
                        requested: requested_payer.clone(),
                        actual: payer.clone(),
                    });
                }
                if self.dispatched_attempts.contains(attempt_id) {
                    return Err(MonitorViolation::DuplicateAttemptDispatch {
                        attempt_id: attempt_id.clone(),
                    });
                }
            }
            TraceEvent::ProviderDispatch {
                attempt_id,
                cred_gen,
            } => {
                if !self.dispatched_attempts.insert(attempt_id.clone()) {
                    return Err(MonitorViolation::DuplicateAttemptDispatch {
                        attempt_id: attempt_id.clone(),
                    });
                }
                if let Some(&revoked_gen) = self.cred_revoked_gen.get(&rec.tenant_hash) {
                    if *cred_gen < revoked_gen {
                        return Err(MonitorViolation::PostRevokeDispatch {
                            tenant: rec.tenant_hash.clone(),
                            attempt_id: attempt_id.clone(),
                            cred_gen: *cred_gen,
                            revoked_gen,
                        });
                    }
                }
                self.dispatches_observed += 1;
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct PlacementLeaseMonitor {
    active_holder: HashMap<String, String>,
    active_gen: HashMap<String, u64>,
    tenant_writes_observed: usize,
}

impl PlacementLeaseMonitor {
    pub fn observe(&mut self, rec: &TraceRecord) -> Result<(), MonitorViolation> {
        match &rec.event {
            TraceEvent::LeaseClaim { holder_key, gen } => {
                if let Some(&prev) = self.active_gen.get(&rec.tenant_hash) {
                    if *gen <= prev {
                        return Err(MonitorViolation::NonMonotonicLeaseGen {
                            tenant: rec.tenant_hash.clone(),
                            prev,
                            new_gen: *gen,
                        });
                    }
                }
                self.active_holder
                    .insert(rec.tenant_hash.clone(), holder_key.clone());
                self.active_gen.insert(rec.tenant_hash.clone(), *gen);
            }
            TraceEvent::LeaseTransfer {
                from_key,
                to_key,
                new_gen,
            } => {
                let current_holder = self
                    .active_holder
                    .get(&rec.tenant_hash)
                    .cloned()
                    .unwrap_or_default();
                if current_holder != *from_key {
                    return Err(MonitorViolation::LeaseTransferMismatch {
                        tenant: rec.tenant_hash.clone(),
                        expected: current_holder,
                        actual: from_key.clone(),
                    });
                }
                let prev = self.active_gen.get(&rec.tenant_hash).copied().unwrap_or(0);
                if *new_gen <= prev {
                    return Err(MonitorViolation::NonMonotonicLeaseGen {
                        tenant: rec.tenant_hash.clone(),
                        prev,
                        new_gen: *new_gen,
                    });
                }
                self.active_holder
                    .insert(rec.tenant_hash.clone(), to_key.clone());
                self.active_gen.insert(rec.tenant_hash.clone(), *new_gen);
            }
            TraceEvent::LeaseRelease { holder_key, gen } => {
                if self.active_holder.get(&rec.tenant_hash) == Some(holder_key)
                    && self.active_gen.get(&rec.tenant_hash) == Some(gen)
                {
                    self.active_holder.remove(&rec.tenant_hash);
                }
            }
            TraceEvent::TenantWrite { writer_key, gen } => {
                let holder_ok = self.active_holder.get(&rec.tenant_hash) == Some(writer_key);
                let gen_ok = self.active_gen.get(&rec.tenant_hash) == Some(gen);
                if !holder_ok || !gen_ok {
                    return Err(MonitorViolation::StaleTenantWrite {
                        tenant: rec.tenant_hash.clone(),
                        writer_key: writer_key.clone(),
                        gen: *gen,
                    });
                }
                self.tenant_writes_observed += 1;
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
pub struct StreamCancellationMonitor {
    open_streams: HashSet<String>,
    quarantined_streams: HashSet<String>,
    cancel_req_tick: HashMap<String, u64>,
    streams_opened: usize,
}

impl StreamCancellationMonitor {
    pub fn observe(&mut self, rec: &TraceRecord) -> Result<(), MonitorViolation> {
        match &rec.event {
            TraceEvent::StreamOpen {
                stream_id,
                permits_after,
                max_permits,
            } => {
                if self.quarantined_streams.contains(stream_id) {
                    return Err(MonitorViolation::QuarantinedStreamRetry {
                        stream_id: stream_id.clone(),
                    });
                }
                if permits_after > max_permits {
                    return Err(MonitorViolation::StreamPermitBoundExceeded {
                        stream_id: stream_id.clone(),
                        permits_after: *permits_after,
                        max_permits: *max_permits,
                    });
                }
                self.open_streams.insert(stream_id.clone());
                self.streams_opened += 1;
            }
            TraceEvent::StreamCancelReq { stream_id, tick } => {
                self.cancel_req_tick.insert(stream_id.clone(), *tick);
            }
            TraceEvent::StreamAbortDone {
                stream_id, tick, ..
            } => {
                let was_open = self.open_streams.remove(stream_id);
                let req_tick = self.cancel_req_tick.remove(stream_id);
                match req_tick {
                    Some(req) if was_open && *tick >= req => {}
                    _ => {
                        return Err(MonitorViolation::InvalidStreamAbort {
                            stream_id: stream_id.clone(),
                        });
                    }
                }
            }
            TraceEvent::StreamQuarantine { stream_id, .. } => {
                if !self.open_streams.remove(stream_id) {
                    return Err(MonitorViolation::InvalidStreamQuarantine {
                        stream_id: stream_id.clone(),
                    });
                }
                self.quarantined_streams.insert(stream_id.clone());
            }
            TraceEvent::StreamClose { stream_id, .. } => {
                self.open_streams.remove(stream_id);
                self.cancel_req_tick.remove(stream_id);
            }
            _ => {}
        }
        Ok(())
    }
}

/// Ensures no secret sentinels (API keys, prompts, bearer tokens, raw JIDs)
/// appear anywhere in a serialized JSONL trace.
pub fn assert_secret_free(jsonl: &str, sentinels: &[&str]) -> Result<(), TraceVerifyError> {
    for sentinel in sentinels {
        if !sentinel.is_empty() && jsonl.contains(sentinel) {
            return Err(TraceVerifyError::SecretSentinelLeaked);
        }
    }
    Ok(())
}

/// Parses a JSONL trace line-by-line and verifies it against all 3 monitors.
pub fn verify_jsonl_trace(jsonl: &str) -> Result<TraceSummary, TraceVerifyError> {
    let mut admission = AdmissionFenceMonitor::default();
    let mut placement = PlacementLeaseMonitor::default();
    let mut stream = StreamCancellationMonitor::default();
    let mut records_processed = 0usize;

    for (idx, raw_line) in jsonl.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line = idx + 1;
        let rec: TraceRecord = serde_json::from_str(trimmed)
            .map_err(|source| TraceVerifyError::JsonParse { line, source })?;
        admission
            .observe(&rec)
            .map_err(|violation| TraceVerifyError::Violation { line, violation })?;
        placement
            .observe(&rec)
            .map_err(|violation| TraceVerifyError::Violation { line, violation })?;
        stream
            .observe(&rec)
            .map_err(|violation| TraceVerifyError::Violation { line, violation })?;
        records_processed += 1;
    }

    Ok(TraceSummary {
        records_processed,
        dispatches_observed: admission.dispatches_observed,
        tenant_writes_observed: placement.tenant_writes_observed,
        streams_opened: stream.streams_opened,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_jsonl(records: &[TraceRecord]) -> String {
        records
            .iter()
            .map(|r| serde_json::to_string(r).expect("serialize record"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rec(ts: u64, actor: &str, event: TraceEvent) -> TraceRecord {
        TraceRecord {
            ts_logical: ts,
            actor_id: actor.to_owned(),
            tenant_hash: "t-acme".to_owned(),
            event,
        }
    }

    #[test]
    fn valid_end_to_end_trace_passes_all_three_monitors() {
        let trace = to_jsonl(&[
            rec(
                1,
                "cell-1",
                TraceEvent::LeaseClaim {
                    holder_key: "k1".to_owned(),
                    gen: 1,
                },
            ),
            rec(
                2,
                "cell-1",
                TraceEvent::TenantWrite {
                    writer_key: "k1".to_owned(),
                    gen: 1,
                },
            ),
            rec(
                3,
                "broker-1",
                TraceEvent::AdmissionCommit {
                    attempt_id: "att-1".to_owned(),
                    payer: "user".to_owned(),
                    requested_payer: "user".to_owned(),
                    cred_gen: 1,
                    room_gen: 1,
                    place_gen: 1,
                },
            ),
            rec(
                4,
                "broker-1",
                TraceEvent::ProviderDispatch {
                    attempt_id: "att-1".to_owned(),
                    cred_gen: 1,
                },
            ),
            rec(
                5,
                "broker-1",
                TraceEvent::StreamOpen {
                    stream_id: "s-1".to_owned(),
                    permits_after: 1,
                    max_permits: 2,
                },
            ),
            rec(
                6,
                "broker-1",
                TraceEvent::StreamCancelReq {
                    stream_id: "s-1".to_owned(),
                    tick: 10,
                },
            ),
            rec(
                7,
                "broker-1",
                TraceEvent::StreamAbortDone {
                    stream_id: "s-1".to_owned(),
                    tick: 11,
                    permits_after: 0,
                },
            ),
        ]);

        assert!(assert_secret_free(&trace, &["sk-live-secret", "alice@acme.example"]).is_ok());
        let summary = verify_jsonl_trace(&trace).expect("valid trace");
        assert_eq!(summary.records_processed, 7);
        assert_eq!(summary.dispatches_observed, 1);
        assert_eq!(summary.tenant_writes_observed, 1);
        assert_eq!(summary.streams_opened, 1);
    }

    #[test]
    fn catches_post_revoke_dispatch() {
        let trace = to_jsonl(&[
            rec(
                1,
                "broker-1",
                TraceEvent::FenceClose {
                    fence_kind: "cred".to_owned(),
                    new_gen: 2,
                },
            ),
            rec(2, "broker-1", TraceEvent::RevokeAck { gen: 2 }),
            rec(
                3,
                "broker-2",
                TraceEvent::ProviderDispatch {
                    attempt_id: "att-stale".to_owned(),
                    cred_gen: 1,
                },
            ),
        ]);
        let err = verify_jsonl_trace(&trace).expect_err("must catch post-revoke dispatch");
        assert!(matches!(
            err,
            TraceVerifyError::Violation {
                violation: MonitorViolation::PostRevokeDispatch { .. },
                ..
            }
        ));
    }

    #[test]
    fn catches_stale_tenant_write_after_transfer() {
        let trace = to_jsonl(&[
            rec(
                1,
                "cell-1",
                TraceEvent::LeaseClaim {
                    holder_key: "k1".to_owned(),
                    gen: 1,
                },
            ),
            rec(
                2,
                "cell-1",
                TraceEvent::LeaseTransfer {
                    from_key: "k1".to_owned(),
                    to_key: "k2".to_owned(),
                    new_gen: 2,
                },
            ),
            rec(
                3,
                "cell-1",
                TraceEvent::TenantWrite {
                    writer_key: "k1".to_owned(),
                    gen: 1,
                },
            ),
        ]);
        let err = verify_jsonl_trace(&trace).expect_err("must catch stale write");
        assert!(matches!(
            err,
            TraceVerifyError::Violation {
                violation: MonitorViolation::StaleTenantWrite { .. },
                ..
            }
        ));
    }

    #[test]
    fn catches_quarantined_stream_retry() {
        let trace = to_jsonl(&[
            rec(
                1,
                "broker-1",
                TraceEvent::StreamOpen {
                    stream_id: "s-q".to_owned(),
                    permits_after: 1,
                    max_permits: 2,
                },
            ),
            rec(
                2,
                "broker-1",
                TraceEvent::StreamQuarantine {
                    stream_id: "s-q".to_owned(),
                    permits_after: 0,
                },
            ),
            rec(
                3,
                "broker-1",
                TraceEvent::StreamOpen {
                    stream_id: "s-q".to_owned(),
                    permits_after: 1,
                    max_permits: 2,
                },
            ),
        ]);
        let err = verify_jsonl_trace(&trace).expect_err("must catch quarantined retry");
        assert!(matches!(
            err,
            TraceVerifyError::Violation {
                violation: MonitorViolation::QuarantinedStreamRetry { .. },
                ..
            }
        ));
    }
}
