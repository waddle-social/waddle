//! Rate limiting for outbound Web Push sends.
//!
//! Two complementary controls layered in front of any
//! [`WebPushSender`]:
//!
//! 1. **Global concurrency cap** via [`tokio::sync::Semaphore`] — bounds
//!    the total number of in-flight HTTP requests so the publish-job
//!    worker can't drown the host's connection pool when a fan-out hits
//!    a thousand devices at once.
//!
//! 2. **Per-(endpoint, urgency) leaky bucket** — keyed by
//!    `(EndpointHash, Urgency)` so one chatty relay+class pair can't
//!    monopolize the global cap. Implemented as a "next-available
//!    timestamp" map: each acquire either returns immediately and bumps
//!    the timestamp by `min_interval`, or sleeps until the timestamp
//!    passes. Strict rate-limit semantics; no burst — keeps the
//!    implementation tiny.
//!
//! The wrapper preserves the [`WebPushSender`] trait so callers
//! (publish-job worker, tests) see the same shape; the limiter is
//! transparent except for the rate it imposes.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;

use super::sender::{Urgency, WebPushRequest, WebPushSender};
use super::types::{EndpointHash, WebPushOutcome};

/// Default ceiling on concurrent in-flight Web Push HTTPS requests.
/// Picked to comfortably fit inside the default `reqwest` connection
/// pool (each request gets its own slot) without starving the rest of
/// the process.
pub const DEFAULT_GLOBAL_CONCURRENCY: usize = 64;

/// Default minimum interval between two sends to the same
/// `(endpoint, urgency)` pair. 100ms ≈ 10 req/sec/pair, comfortably
/// below any major relay's per-app rate limit while preventing one
/// chatty pair from monopolizing the global cap.
pub const DEFAULT_PER_PAIR_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Configurable knobs for the limiter. Constructed at boot from
/// deployment defaults; tests can override either field.
#[derive(Debug, Clone, Copy)]
pub struct LimiterConfig {
    pub global_concurrency: usize,
    pub per_pair_min_interval: Duration,
}

impl Default for LimiterConfig {
    fn default() -> Self {
        Self {
            global_concurrency: DEFAULT_GLOBAL_CONCURRENCY,
            per_pair_min_interval: DEFAULT_PER_PAIR_MIN_INTERVAL,
        }
    }
}

/// Per-(endpoint, urgency) leaky-bucket state. `next_available` is the
/// earliest `Instant` at which the next acquire is allowed; an acquire
/// updates it to `max(now, next_available) + min_interval`.
#[derive(Debug)]
struct PairState {
    next_available: Instant,
}

/// Rate-limiting state shared by every [`RateLimitedWebPushSender`]
/// wrapping it. Cheap to clone via `Arc`.
#[derive(Debug)]
pub struct Limiter {
    global: Arc<Semaphore>,
    config: LimiterConfig,
    pairs: Mutex<HashMap<(EndpointHash, Urgency), PairState>>,
}

impl Limiter {
    pub fn new(config: LimiterConfig) -> Self {
        Self {
            global: Arc::new(Semaphore::new(config.global_concurrency)),
            config,
            pairs: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(LimiterConfig::default())
    }

    /// Block until both the global cap and the per-pair bucket allow
    /// the caller to send. The returned guard holds the global permit
    /// for as long as it is alive — drop it after the HTTP request
    /// completes.
    async fn acquire(&self, endpoint_hash: EndpointHash, urgency: Urgency) -> GlobalPermit<'_> {
        // Per-pair gate first: cheap mutex pop + optional sleep. Doing
        // this before the global permit means a backed-up pair waits
        // without holding a slot the rest of the process could use.
        let sleep_until = {
            let mut pairs = self.pairs.lock().expect("limiter pair map poisoned");
            let entry = pairs
                .entry((endpoint_hash, urgency))
                .or_insert_with(|| PairState {
                    next_available: Instant::now(),
                });
            let now = Instant::now();
            let start = entry.next_available.max(now);
            entry.next_available = start + self.config.per_pair_min_interval;
            // `start` is when this acquire is permitted; sleep up to
            // `start` if the bucket is ahead of the clock.
            if start > now {
                Some(start)
            } else {
                None
            }
        };
        if let Some(deadline) = sleep_until {
            tokio::time::sleep_until(deadline).await;
        }

        let permit = self
            .global
            .clone()
            .acquire_owned()
            .await
            .expect("limiter semaphore must not close");
        GlobalPermit {
            _permit: permit,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// RAII handle for the global concurrency slot taken by
/// [`Limiter::acquire`]. Drop releases the slot for the next waiter.
struct GlobalPermit<'a> {
    _permit: tokio::sync::OwnedSemaphorePermit,
    _phantom: std::marker::PhantomData<&'a Limiter>,
}

/// Wraps any [`WebPushSender`] with the [`Limiter`] gating.
pub struct RateLimitedWebPushSender {
    inner: Arc<dyn WebPushSender>,
    limiter: Arc<Limiter>,
}

impl RateLimitedWebPushSender {
    pub fn new(inner: Arc<dyn WebPushSender>, limiter: Arc<Limiter>) -> Self {
        Self { inner, limiter }
    }
}

impl WebPushSender for RateLimitedWebPushSender {
    fn send(
        &self,
        request: WebPushRequest<'_>,
    ) -> Pin<Box<dyn std::future::Future<Output = WebPushOutcome> + Send + '_>> {
        // Bucket key on relay host (scheme + host + port), NOT the
        // full per-device endpoint URL. Otherwise a 1000-device fan-
        // out to FCM would key on 1000 distinct buckets and the
        // per-pair rate limit becomes per-device — defeating the
        // "one chatty relay+class can't monopolize the global cap"
        // semantic the doc-comment promises.
        let endpoint_hash =
            EndpointHash::of(&request.endpoint.origin().ascii_serialization());
        let urgency = request.urgency;
        let endpoint = request.endpoint.clone();
        let payload = request.payload.clone();
        let jwt = request.vapid_jwt.clone();
        let key = request.vapid_public_key_b64u.to_string();
        let topic = request.topic.cloned();
        let ttl = request.ttl;
        Box::pin(async move {
            let _permit = self.limiter.acquire(endpoint_hash, urgency).await;
            self.inner
                .send(WebPushRequest {
                    endpoint: &endpoint,
                    payload: &payload,
                    vapid_jwt: &jwt,
                    vapid_public_key_b64u: &key,
                    topic: topic.as_ref(),
                    ttl,
                    urgency,
                })
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push::types::{EncryptedPayload, PushTopic, VapidJwt};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use url::Url;

    /// Test sender that records concurrent in-flight calls so we can
    /// verify the global cap is enforced.
    #[derive(Default, Clone)]
    struct PeakSender {
        inflight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
        delay_ms: u64,
    }

    impl PeakSender {
        fn with_delay(delay_ms: u64) -> Self {
            Self {
                inflight: Arc::new(AtomicUsize::new(0)),
                peak: Arc::new(AtomicUsize::new(0)),
                delay_ms,
            }
        }
    }

    impl WebPushSender for PeakSender {
        fn send(
            &self,
            _request: WebPushRequest<'_>,
        ) -> Pin<Box<dyn std::future::Future<Output = WebPushOutcome> + Send + '_>> {
            let inflight = Arc::clone(&self.inflight);
            let peak = Arc::clone(&self.peak);
            let delay_ms = self.delay_ms;
            Box::pin(async move {
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                inflight.fetch_sub(1, Ordering::SeqCst);
                WebPushOutcome::Delivered { status: 201 }
            })
        }
    }

    fn jwt() -> VapidJwt {
        VapidJwt::new("eyJhbGciOiJFUzI1NiJ9.eyJhdWQiOiJodHRwczovL2EifQ.sig").expect("jwt")
    }

    fn payload() -> EncryptedPayload {
        EncryptedPayload::new(vec![0u8; 32])
    }

    fn endpoint(path: &str) -> Url {
        Url::parse(&format!("https://relay.example.com{path}")).unwrap()
    }

    fn make_request<'a>(
        endpoint: &'a Url,
        payload: &'a EncryptedPayload,
        jwt: &'a VapidJwt,
        urgency: Urgency,
    ) -> WebPushRequest<'a> {
        WebPushRequest {
            endpoint,
            payload,
            vapid_jwt: jwt,
            vapid_public_key_b64u: "BFoo",
            topic: None,
            ttl: 60,
            urgency,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn global_cap_bounds_concurrent_sends() {
        let inner = PeakSender::with_delay(100);
        let inner_arc: Arc<dyn WebPushSender> = Arc::new(inner.clone());
        let limiter = Arc::new(Limiter::new(LimiterConfig {
            global_concurrency: 3,
            per_pair_min_interval: Duration::ZERO,
        }));
        let sender = RateLimitedWebPushSender::new(inner_arc, limiter);

        let p = payload();
        let j = jwt();
        let urls: Vec<_> = (0..10).map(|i| endpoint(&format!("/{i}"))).collect();
        let futures: Vec<_> = urls
            .iter()
            .map(|u| sender.send(make_request(u, &p, &j, Urgency::Normal)))
            .collect();
        let outcomes = futures::future::join_all(futures).await;
        assert_eq!(outcomes.len(), 10);
        assert!(
            inner.peak.load(Ordering::SeqCst) <= 3,
            "global cap must bound peak concurrency to 3, observed {}",
            inner.peak.load(Ordering::SeqCst)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn per_pair_bucket_spaces_same_endpoint_class() {
        let inner = PeakSender::with_delay(0);
        let inner_arc: Arc<dyn WebPushSender> = Arc::new(inner.clone());
        let limiter = Arc::new(Limiter::new(LimiterConfig {
            global_concurrency: 64,
            per_pair_min_interval: Duration::from_millis(50),
        }));
        let sender = RateLimitedWebPushSender::new(inner_arc, limiter);

        let p = payload();
        let j = jwt();
        let url = endpoint("/same");
        let start = Instant::now();
        for _ in 0..4 {
            sender
                .send(make_request(&url, &p, &j, Urgency::Normal))
                .await;
        }
        let elapsed = start.elapsed();
        // 4 acquires at 50ms apart → ≥150ms elapsed (the first runs
        // immediately, the next three each wait at least 50ms).
        assert!(
            elapsed >= Duration::from_millis(150),
            "per-pair bucket must serialize same-(endpoint,urgency); elapsed {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn per_pair_bucket_keys_by_urgency() {
        // Same endpoint, two different urgencies → two independent
        // buckets, so the second urgency doesn't wait on the first.
        let inner = PeakSender::with_delay(0);
        let inner_arc: Arc<dyn WebPushSender> = Arc::new(inner.clone());
        let limiter = Arc::new(Limiter::new(LimiterConfig {
            global_concurrency: 64,
            per_pair_min_interval: Duration::from_millis(500),
        }));
        let sender = RateLimitedWebPushSender::new(inner_arc, limiter);
        let p = payload();
        let j = jwt();
        let url = endpoint("/dual");
        let start = Instant::now();
        // Two sends to the same URL with different urgency — should
        // both go through with negligible wait (each bucket only sees
        // one acquire).
        sender.send(make_request(&url, &p, &j, Urgency::High)).await;
        sender
            .send(make_request(&url, &p, &j, Urgency::Normal))
            .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "different urgencies must use independent buckets; elapsed {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn different_relay_hosts_dont_share_buckets() {
        let inner = PeakSender::with_delay(0);
        let inner_arc: Arc<dyn WebPushSender> = Arc::new(inner.clone());
        let limiter = Arc::new(Limiter::new(LimiterConfig {
            global_concurrency: 64,
            per_pair_min_interval: Duration::from_millis(500),
        }));
        let sender = RateLimitedWebPushSender::new(inner_arc, limiter);
        let p = payload();
        let j = jwt();
        let url_fcm = url::Url::parse("https://fcm.googleapis.com/fcm/send/abc").unwrap();
        let url_moz = url::Url::parse("https://updates.push.services.mozilla.com/wpush/v1/xyz")
            .unwrap();
        let start = Instant::now();
        sender
            .send(make_request(&url_fcm, &p, &j, Urgency::Normal))
            .await;
        sender
            .send(make_request(&url_moz, &p, &j, Urgency::Normal))
            .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(50),
            "different relay hosts must use independent buckets; elapsed {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn same_relay_different_paths_share_one_bucket() {
        // Two devices behind the same relay share one (host, urgency)
        // bucket — that's the whole point of per-pair rate limiting:
        // a 1000-device fan-out to FCM should NOT bypass the spacing.
        let inner = PeakSender::with_delay(0);
        let inner_arc: Arc<dyn WebPushSender> = Arc::new(inner.clone());
        let limiter = Arc::new(Limiter::new(LimiterConfig {
            global_concurrency: 64,
            per_pair_min_interval: Duration::from_millis(200),
        }));
        let sender = RateLimitedWebPushSender::new(inner_arc, limiter);
        let p = payload();
        let j = jwt();
        let url_a = endpoint("/device-a");
        let url_b = endpoint("/device-b");
        let start = Instant::now();
        sender
            .send(make_request(&url_a, &p, &j, Urgency::Normal))
            .await;
        sender
            .send(make_request(&url_b, &p, &j, Urgency::Normal))
            .await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(200),
            "two devices on the same relay must serialize via one shared bucket; elapsed {elapsed:?}"
        );
    }

    #[test]
    fn topic_clone_through_wrapper() {
        // Compile-time check: PushTopic must be Clone-able since the
        // wrapper clones the borrowed Option<&PushTopic> into an owned
        // Option<PushTopic> before re-borrowing for the inner sender.
        fn _is_clone<T: Clone>() {}
        _is_clone::<PushTopic>();
    }
}
