//! Production [`DndReader`] backed by the durable
//! [`crate::dnd_projection::DndProjectionStore`] projection of
//! `urn:waddle:dnd:0`.
//!
//! Reads the recipient's projected DND state and evaluates it
//! against a `Clock::now_utc()` reading. When no projection row
//! exists the user is treated as not-in-DND; that matches
//! [`NoopDndReader`]'s behavior and means a fresh user without a
//! published DND state defaults to receiving push notifications.
//!
//! The evaluator boundary is pure (see
//! [`waddle_xmpp::xep::xep_waddle_dnd::WaddleDnd::evaluate`]), so the
//! reader does no schedule arithmetic itself — it just pairs the
//! projection read with a clock read.
//!
//! Per the typed-payloads hard rule, no string-typed time or state
//! crosses this boundary: the wall clock is `DateTime<Utc>`, the
//! evaluator result is the typed
//! [`waddle_xmpp::xep::xep_waddle_dnd::DndEvaluation`], and the
//! returned value is the existing
//! [`crate::notification_outbox::DndState`].

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use jid::BareJid;
use tracing::warn;
use waddle_xmpp::xep::xep_waddle_dnd::DndEvaluation;

use crate::dnd_projection::DndProjectionStore;
use crate::notification_outbox::{DndReader, DndState, NotificationOutboxError};

/// Reads the recipient's projected DND state via
/// [`DndProjectionStore`] and resolves it through the pure
/// evaluator in [`waddle_xmpp::xep::xep_waddle_dnd`].
///
/// Wall-clock reads go through the [`Clock`] trait so deterministic
/// tests can inject a frozen instant without globally replacing the
/// system clock. Production wiring uses [`SystemClock`].
#[derive(Clone)]
pub struct PepDndReader {
    store: Arc<DndProjectionStore>,
    clock: Arc<dyn Clock>,
}

impl PepDndReader {
    pub fn new(store: Arc<DndProjectionStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
    }

    /// Convenience constructor that pairs the store with the
    /// production [`SystemClock`].
    pub fn with_system_clock(store: Arc<DndProjectionStore>) -> Self {
        Self::new(store, Arc::new(SystemClock))
    }
}

#[async_trait]
impl DndReader for PepDndReader {
    async fn dnd_state(&self, user: &BareJid) -> Result<DndState, NotificationOutboxError> {
        let now = self.clock.now_utc();
        let projection = match self.store.get(user).await {
            Ok(value) => value,
            Err(error) => {
                // A projection read failure MUST NOT cause a push
                // dispatch failure — the gate consults DND as one of
                // many parallel suppression checks. Log AND bump the
                // alert-worthy `waddle_dnd_projection_read_errored_total`
                // counter so SREs can detect the silent-fail-open
                // pattern (a DND-active user receiving push because
                // we couldn't read their projection), then default
                // to Inactive so push goes through.
                warn!(
                    user = %user,
                    error = %error,
                    "dnd_projection read failed; defaulting recipient to Inactive"
                );
                waddle_xmpp::telemetry::reliability::increment_dnd_projection_read_errored();
                return Ok(DndState::Inactive);
            }
        };
        let Some(projection) = projection else {
            return Ok(DndState::Inactive);
        };
        Ok(match projection.state.evaluate(now) {
            DndEvaluation::Inactive => DndState::Inactive,
            DndEvaluation::Active => DndState::Active,
        })
    }
}

/// Abstracts the wall clock so tests can inject deterministic
/// instants. The production impl is [`SystemClock`].
pub trait Clock: Send + Sync + 'static {
    fn now_utc(&self) -> DateTime<Utc>;
}

/// Production [`Clock`] reading `chrono::Utc::now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dnd_projection::{DndProjection, DndProjectionStore};
    use chrono::{NaiveTime, TimeZone, Weekday};
    use chrono_tz::Tz;
    use jid::BareJid;
    use std::str::FromStr;
    use std::sync::Mutex;
    use waddle_xmpp::xep::xep_waddle_dnd::{ScheduleRule, WaddleDnd, WeekdaySet};

    fn alice() -> BareJid {
        BareJid::from_str("alice@waddle.example").expect("test JID parses")
    }

    /// [`Clock`] that returns whatever instant the test plants. The
    /// PR's per-XEP custom test suite (xep_waddle_dnd) covers the
    /// evaluation arithmetic; this reader-level suite covers the
    /// projection-read × clock-read × evaluator composition.
    struct FixedClock {
        now: Mutex<DateTime<Utc>>,
    }

    impl FixedClock {
        fn new(now: DateTime<Utc>) -> Self {
            Self {
                now: Mutex::new(now),
            }
        }

        fn advance_to(&self, when: DateTime<Utc>) {
            *self.now.lock().expect("clock lock") = when;
        }
    }

    impl Clock for FixedClock {
        fn now_utc(&self) -> DateTime<Utc> {
            *self.now.lock().expect("clock lock")
        }
    }

    async fn migrated_store() -> Arc<DndProjectionStore> {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage");
        Arc::new(DndProjectionStore::new(storage.database()))
    }

    fn snooze_state(until: DateTime<Utc>) -> WaddleDnd {
        WaddleDnd {
            timezone: Tz::UTC,
            snooze: Some(until),
            rules: vec![],
        }
    }

    fn weekly_rule_state(tz: Tz, day: Weekday, start: NaiveTime, end: NaiveTime) -> WaddleDnd {
        WaddleDnd {
            timezone: tz,
            snooze: None,
            rules: vec![ScheduleRule {
                days: WeekdaySet::from_iter([day].iter().copied()),
                start,
                end,
            }],
        }
    }

    #[tokio::test]
    async fn no_projection_row_returns_inactive() {
        let store = migrated_store().await;
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 23, 12, 0, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store, clock);
        let state = reader.dnd_state(&alice()).await.expect("read");
        assert_eq!(state, DndState::Inactive);
    }

    #[tokio::test]
    async fn snooze_future_returns_active() {
        let store = migrated_store().await;
        let until = Utc.with_ymd_and_hms(2026, 5, 23, 17, 0, 0).unwrap();
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: snooze_state(until),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 23, 16, 30, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store, clock);
        let state = reader.dnd_state(&alice()).await.expect("read");
        assert_eq!(state, DndState::Active);
    }

    #[tokio::test]
    async fn snooze_elapsed_returns_inactive() {
        let store = migrated_store().await;
        let until = Utc.with_ymd_and_hms(2026, 5, 23, 17, 0, 0).unwrap();
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: snooze_state(until),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 23, 17, 30, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store, clock);
        let state = reader.dnd_state(&alice()).await.expect("read");
        assert_eq!(state, DndState::Inactive);
    }

    #[tokio::test]
    async fn schedule_inside_window_returns_active() {
        let store = migrated_store().await;
        // Mon 22:00 → Tue 07:00 in Oslo.
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: weekly_rule_state(
                    Tz::Europe__Oslo,
                    Weekday::Mon,
                    NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                    NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
                ),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        // 2026-05-25 23:00 Oslo CEST = 21:00 UTC.
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 25, 21, 0, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store, clock);
        let state = reader.dnd_state(&alice()).await.expect("read");
        assert_eq!(state, DndState::Active);
    }

    #[tokio::test]
    async fn schedule_outside_window_returns_inactive() {
        let store = migrated_store().await;
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: weekly_rule_state(
                    Tz::Europe__Oslo,
                    Weekday::Mon,
                    NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                    NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
                ),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        // 2026-05-25 14:00 Oslo CEST = 12:00 UTC (Monday afternoon, well outside).
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 25, 12, 0, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store, clock);
        let state = reader.dnd_state(&alice()).await.expect("read");
        assert_eq!(state, DndState::Inactive);
    }

    #[tokio::test]
    async fn clock_movement_flips_state_without_projection_change() {
        let store = migrated_store().await;
        let until = Utc.with_ymd_and_hms(2026, 5, 23, 17, 0, 0).unwrap();
        store
            .upsert(&DndProjection {
                owner_bare_jid: alice(),
                state: snooze_state(until),
                source_version: 1,
                updated_at_ms: 1_700_000_000_000,
            })
            .await
            .expect("upsert");
        let clock = Arc::new(FixedClock::new(
            Utc.with_ymd_and_hms(2026, 5, 23, 16, 30, 0).unwrap(),
        ));
        let reader = PepDndReader::new(store.clone(), clock.clone());
        assert_eq!(
            reader.dnd_state(&alice()).await.expect("read"),
            DndState::Active
        );
        clock.advance_to(Utc.with_ymd_and_hms(2026, 5, 23, 17, 30, 0).unwrap());
        assert_eq!(
            reader.dnd_state(&alice()).await.expect("read"),
            DndState::Inactive
        );
    }
}
