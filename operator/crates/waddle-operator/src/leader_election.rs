use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::coordination::v1::{Lease, LeaseSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::MicroTime;
use k8s_openapi::jiff::Timestamp;
use kube::api::{Api, ObjectMeta, PostParams};
use kube::Client;
use tokio::sync::{oneshot, watch};

use crate::error::OperatorError;

const DEFAULT_LEASE_DURATION_SECS: i32 = 15;
const DEFAULT_RENEW_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Configuration for `coordination.k8s.io/v1` `Lease`-based leader election.
#[derive(Debug, Clone)]
pub struct LeaderConfig {
    /// Namespace containing the Lease object.
    pub namespace: String,
    /// Name of the Lease object.
    pub lease_name: String,
    /// Unique holder identity (typically pod name or hostname).
    pub holder_id: String,
    /// Lease duration in seconds.
    pub lease_duration_secs: i32,
    /// How frequently the active leader renews the Lease.
    pub renew_interval: Duration,
    /// How frequently standby replicas attempt acquisition.
    pub retry_interval: Duration,
}

impl LeaderConfig {
    /// Builds a `LeaderConfig` with production defaults (15s lease, 5s renew).
    #[must_use]
    pub fn new(namespace: String, lease_name: String, holder_id: String) -> Self {
        Self {
            namespace,
            lease_name,
            holder_id,
            lease_duration_secs: DEFAULT_LEASE_DURATION_SECS,
            renew_interval: DEFAULT_RENEW_INTERVAL,
            retry_interval: DEFAULT_RETRY_INTERVAL,
        }
    }
}

/// Monotonic write fence preventing stale leaders from issuing Kubernetes writes after lease expiry.
#[derive(Debug, Clone)]
pub struct LeadershipFence {
    inner: Arc<LeadershipFenceInner>,
}

#[derive(Debug)]
struct LeadershipFenceInner {
    always_active: bool,
    active: AtomicBool,
    origin: tokio::time::Instant,
    safe_deadline_ms: AtomicU64,
}

impl LeadershipFence {
    /// Creates an unrestricted fence for single-replica or test setups without leader election.
    #[must_use]
    pub fn always_active() -> Self {
        Self {
            inner: Arc::new(LeadershipFenceInner {
                always_active: true,
                active: AtomicBool::new(true),
                origin: tokio::time::Instant::now(),
                safe_deadline_ms: AtomicU64::new(u64::MAX),
            }),
        }
    }

    fn new_bounded(
        origin: tokio::time::Instant,
        attempt_start: tokio::time::Instant,
        lease_duration: Duration,
        safety_margin: Duration,
    ) -> Self {
        let fence = Self {
            inner: Arc::new(LeadershipFenceInner {
                always_active: false,
                active: AtomicBool::new(true),
                origin,
                safe_deadline_ms: AtomicU64::new(0),
            }),
        };
        fence.renew_from(attempt_start, lease_duration, safety_margin);
        fence
    }

    fn renew_from(
        &self,
        attempt_start: tokio::time::Instant,
        lease_duration: Duration,
        safety_margin: Duration,
    ) {
        let safe_until = attempt_start + lease_duration.saturating_sub(safety_margin);
        let offset_ms = safe_until
            .saturating_duration_since(self.inner.origin)
            .as_millis();
        let clamped_ms = u64::try_from(offset_ms).unwrap_or(u64::MAX);
        self.inner
            .safe_deadline_ms
            .store(clamped_ms, Ordering::Release);
    }

    fn revoke(&self) {
        self.inner.active.store(false, Ordering::Release);
        self.inner.safe_deadline_ms.store(0, Ordering::Release);
    }

    /// Returns `true` while leadership is active and within the monotonic lease safety deadline.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        if self.inner.always_active {
            return true;
        }
        if !self.inner.active.load(Ordering::Acquire) {
            return false;
        }
        let elapsed_ms = tokio::time::Instant::now()
            .saturating_duration_since(self.inner.origin)
            .as_millis();
        let safe_ms = self.inner.safe_deadline_ms.load(Ordering::Acquire);
        elapsed_ms < u128::from(safe_ms)
    }

    /// Ensures leadership is still valid before performing a cluster mutation.
    ///
    /// # Errors
    /// Returns [`OperatorError::LeadershipLost`] when the lease has expired or been revoked.
    pub fn ensure_valid(&self) -> Result<(), OperatorError> {
        if self.is_valid() {
            Ok(())
        } else {
            Err(OperatorError::LeadershipLost)
        }
    }
}

/// Active leadership lease guard that renews in the background until shutdown or loss.
pub struct LeaderGuard {
    config: LeaderConfig,
    api: Api<Lease>,
    fence: LeadershipFence,
    stop_tx: watch::Sender<bool>,
    lost_rx: Option<oneshot::Receiver<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl LeaderGuard {
    /// Waits until this process acquires the Kubernetes Lease or `shutdown` triggers.
    ///
    /// Returns `Ok(None)` if shutdown occurred before leadership was acquired.
    ///
    /// # Errors
    /// Returns [`OperatorError`] on non-retryable Kubernetes API errors.
    pub async fn acquire(
        client: &Client,
        config: LeaderConfig,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<Option<Self>, OperatorError> {
        let api: Api<Lease> = Api::namespaced(client.clone(), &config.namespace);
        let lease_duration =
            Duration::from_secs(u64::try_from(config.lease_duration_secs.max(1)).unwrap_or(15));
        let safety_margin = Duration::from_secs(2).min(lease_duration / 3);

        let acquire_start;
        loop {
            if *shutdown.borrow() {
                return Ok(None);
            }

            let attempt_start = tokio::time::Instant::now();
            if try_acquire_once(&api, &config).await? {
                acquire_start = attempt_start;
                tracing::info!(
                    namespace = %config.namespace,
                    lease = %config.lease_name,
                    holder = %config.holder_id,
                    "acquired leader election lease"
                );
                break;
            }

            tokio::select! {
                () = tokio::time::sleep(config.retry_interval) => {}
                res = shutdown.changed() => {
                    if res.is_err() || *shutdown.borrow() {
                        return Ok(None);
                    }
                }
            }
        }

        let (stop_tx, mut stop_rx) = watch::channel(false);
        let (lost_tx, lost_rx) = oneshot::channel();
        let fence = LeadershipFence::new_bounded(
            acquire_start,
            acquire_start,
            lease_duration,
            safety_margin,
        );
        let renew_fence = fence.clone();
        let renew_api = api.clone();
        let renew_cfg = config.clone();

        let task = tokio::spawn(async move {
            let mut lease_deadline = acquire_start + lease_duration;
            let mut interval = tokio::time::interval(renew_cfg.renew_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let _first_tick = interval.tick().await;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let attempt_start = tokio::time::Instant::now();
                        let remaining = lease_deadline
                            .saturating_duration_since(attempt_start)
                            .saturating_sub(safety_margin);
                        if remaining.is_zero() {
                            renew_fence.revoke();
                            tracing::error!(holder = %renew_cfg.holder_id, "leader election lease renewal budget exhausted before response");
                            let _ = lost_tx.send(());
                            break;
                        }

                        match tokio::time::timeout(remaining, renew_once(&renew_api, &renew_cfg)).await {
                            Ok(Ok(true)) => {
                                lease_deadline = attempt_start + lease_duration;
                                renew_fence.renew_from(attempt_start, lease_duration, safety_margin);
                            }
                            Ok(Ok(false)) => {
                                renew_fence.revoke();
                                tracing::error!(holder = %renew_cfg.holder_id, "leader election lease lost to another holder");
                                let _ = lost_tx.send(());
                                break;
                            }
                            Ok(Err(err)) => {
                                renew_fence.revoke();
                                tracing::error!(error = %err, holder = %renew_cfg.holder_id, "failed to renew leader election lease");
                                let _ = lost_tx.send(());
                                break;
                            }
                            Err(_) => {
                                renew_fence.revoke();
                                tracing::error!(holder = %renew_cfg.holder_id, "leader election lease renewal exceeded safety deadline");
                                let _ = lost_tx.send(());
                                break;
                            }
                        }
                    }
                    res = stop_rx.changed() => {
                        if res.is_err() || *stop_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        Ok(Some(Self {
            config,
            api,
            fence,
            stop_tx,
            lost_rx: Some(lost_rx),
            task: Some(task),
        }))
    }

    /// Returns a cloneable handle to the synchronous leadership write fence.
    #[must_use]
    pub fn fence(&self) -> LeadershipFence {
        self.fence.clone()
    }

    /// Takes the oneshot notification channel that resolves if the lease is lost.
    pub fn take_lost_rx(&mut self) -> Option<oneshot::Receiver<()>> {
        self.lost_rx.take()
    }

    /// Stops background renewal and attempts best-effort release of the Lease.
    pub async fn release(mut self) {
        self.fence.revoke();
        let _ = self.stop_tx.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        release_now(&self.api, &self.config).await;
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        self.fence.revoke();
        let _ = self.stop_tx.send(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn release_now(api: &Api<Lease>, config: &LeaderConfig) {
    let Ok(Some(mut lease)) = api.get_opt(&config.lease_name).await else {
        return;
    };
    let Some(spec) = lease.spec.as_mut() else {
        return;
    };
    if spec.holder_identity.as_deref() != Some(config.holder_id.as_str()) {
        return;
    }
    spec.holder_identity = None;
    spec.renew_time = Some(MicroTime(Timestamp::now()));
    let _ = api
        .replace(&config.lease_name, &PostParams::default(), &lease)
        .await;
}

async fn try_acquire_once(api: &Api<Lease>, cfg: &LeaderConfig) -> Result<bool, OperatorError> {
    let now = Timestamp::now();
    match api
        .get_opt(&cfg.lease_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get leader election Lease",
            source,
        })? {
        None => {
            let lease = Lease {
                metadata: ObjectMeta {
                    name: Some(cfg.lease_name.clone()),
                    namespace: Some(cfg.namespace.clone()),
                    ..ObjectMeta::default()
                },
                spec: Some(LeaseSpec {
                    holder_identity: Some(cfg.holder_id.clone()),
                    lease_duration_seconds: Some(cfg.lease_duration_secs),
                    acquire_time: Some(MicroTime(now)),
                    renew_time: Some(MicroTime(now)),
                    lease_transitions: Some(0),
                    ..LeaseSpec::default()
                }),
            };
            match api.create(&PostParams::default(), &lease).await {
                Ok(_) => Ok(true),
                Err(kube::Error::Api(status)) if status.code == 409 => Ok(false),
                Err(source) => Err(OperatorError::Kube {
                    context: "create leader election Lease",
                    source,
                }),
            }
        }
        Some(mut lease) => {
            let spec = lease.spec.get_or_insert_with(LeaseSpec::default);
            let held_by_us = spec.holder_identity.as_deref() == Some(cfg.holder_id.as_str());
            if !held_by_us && !is_lease_expired(spec, now, cfg.lease_duration_secs) {
                return Ok(false);
            }

            if held_by_us {
                spec.renew_time = Some(MicroTime(now));
                spec.lease_duration_seconds = Some(cfg.lease_duration_secs);
            } else {
                let transitions = spec.lease_transitions.unwrap_or(0).saturating_add(1);
                spec.holder_identity = Some(cfg.holder_id.clone());
                spec.lease_duration_seconds = Some(cfg.lease_duration_secs);
                spec.acquire_time = Some(MicroTime(now));
                spec.renew_time = Some(MicroTime(now));
                spec.lease_transitions = Some(transitions);
            }

            match api
                .replace(&cfg.lease_name, &PostParams::default(), &lease)
                .await
            {
                Ok(_) => Ok(true),
                Err(kube::Error::Api(status)) if status.code == 409 => Ok(false),
                Err(source) => Err(OperatorError::Kube {
                    context: "acquire leader election Lease",
                    source,
                }),
            }
        }
    }
}

async fn renew_once(api: &Api<Lease>, cfg: &LeaderConfig) -> Result<bool, OperatorError> {
    let mut lease = api
        .get(&cfg.lease_name)
        .await
        .map_err(|source| OperatorError::Kube {
            context: "get Lease for renewal",
            source,
        })?;
    let spec = lease.spec.get_or_insert_with(LeaseSpec::default);
    if spec.holder_identity.as_deref() != Some(cfg.holder_id.as_str()) {
        return Ok(false);
    }

    spec.renew_time = Some(MicroTime(Timestamp::now()));
    spec.lease_duration_seconds = Some(cfg.lease_duration_secs);
    match api
        .replace(&cfg.lease_name, &PostParams::default(), &lease)
        .await
    {
        Ok(_) => Ok(true),
        Err(kube::Error::Api(status)) if status.code == 409 => Ok(false),
        Err(source) => Err(OperatorError::Kube {
            context: "renew leader election Lease",
            source,
        }),
    }
}

fn is_lease_expired(spec: &LeaseSpec, now: Timestamp, default_duration: i32) -> bool {
    let Some(holder) = spec.holder_identity.as_deref() else {
        return true;
    };
    if holder.is_empty() {
        return true;
    }
    let Some(renew_time) = spec.renew_time.as_ref().or(spec.acquire_time.as_ref()) else {
        return true;
    };
    let duration_secs = i64::from(spec.lease_duration_seconds.unwrap_or(default_duration));
    let elapsed_secs = now.as_second().saturating_sub(renew_time.0.as_second());
    elapsed_secs >= duration_secs
}
