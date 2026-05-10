//! Startup backfill for the OIDC → PEP profile bridge.
//!
//! Iterates users with sync-relevant claim state (a non-null
//! `users.avatar_url` or `users.display_name`) and dispatches
//! [`ensure_pep_profile_published`] with bounded concurrency. The
//! publish helper is idempotent at the wire level (PR3 keys avatar
//! data on SHA-1; PR4's removal flow checks the metadata-present
//! probe), so a "best effort" boot-time pass is safe to repeat.
//!
//! Per-user throttle: when a previous attempt's
//! [`FetchError::kind`] indicated a permanent failure (`4xx`,
//! `mime_rejected`, `size_exceeded`, `ssrf_blocked`,
//! `magic_byte_mismatch`, or `invalid_url`) the backfill skips the
//! user for [`PERMANENT_FAILURE_COOLDOWN`] before retrying. State
//! lives in `user_avatar_fetch_state`, keyed on `xmpp_localpart`.
//!
//! Concurrency is serialized per-(BareJid) by
//! [`ensure_pep_profile_published`]'s internal lock — concurrent
//! wire publishes for the same user during a backfill pass do not
//! race the user-managed guard.

use chrono::{DateTime, Duration, Utc};
use futures::stream::{self, StreamExt};
use jid::BareJid;
use kameo::actor::ActorRef;
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use url::Url;

use super::publish::{ensure_pep_profile_published, ProfilePublishDeps};
use super::source::{NameIntent, PhotoIntent, ProfileSource, ProfileSyncError};
use crate::db::actor::{DbActor, DbExecute, DbQuery};

/// Concurrency cap on outbound avatar fetches during backfill.
/// Avoids hammering the IDP / a slow CDN at boot.
const MAX_INFLIGHT: usize = 4;

/// Cool-down after a permanent failure before the backfill retries
/// the same user. Matches the RFC 363 "skip re-attempt for 24h
/// after a 4xx" rule.
const PERMANENT_FAILURE_COOLDOWN: Duration = Duration::hours(24);

/// Error kinds (from `FetchError::kind()` plus a synthesized
/// `invalid_url` for parse-failure rows) that count as permanent
/// for the throttle. Anything not on this list is treated as
/// transient — re-attempted every boot until it succeeds or
/// flips to a permanent kind.
const PERMANENT_KINDS: &[&str] = &[
    "permanent_4xx",
    "mime_rejected",
    "size_exceeded",
    "ssrf_blocked",
    "magic_byte_mismatch",
    "invalid_url",
    "invalid_scheme",
    "missing_host",
];

/// Aggregate counts of a backfill run.
#[derive(Debug, Default, Clone)]
pub struct BackfillReport {
    pub total: usize,
    pub processed: usize,
    pub skipped_throttled: usize,
    pub skipped_user_managed: usize,
    pub failed: usize,
}

/// Typed error from the backfill helpers — the storage seam can't
/// be statically typed past the kameo `ask` boundary, but the rest
/// of the chain returns typed `ProfileSyncError`. Wrapping the
/// stringly-typed actor errors here keeps the boundary explicit.
#[derive(Debug, Error)]
pub enum BackfillError {
    #[error("backfill storage failure: {0}")]
    Storage(String),
    #[error(transparent)]
    Sync(#[from] ProfileSyncError),
}

/// One row of backfill input — the OIDC claim state plus the
/// throttle adjuncts.
#[derive(Debug, Clone)]
struct BackfillRow {
    xmpp_localpart: String,
    avatar_url: Option<String>,
    display_name: Option<String>,
    last_attempt_at: Option<String>,
    last_error: Option<String>,
}

/// Run a one-shot backfill. `xmpp_domain` is the server's
/// authoritative XMPP domain (used to construct each user's
/// `BareJid`). `cancel` is checked between rows so SIGTERM
/// short-circuits the run instead of letting an N-row pass block
/// the graceful-shutdown drain past the deployment grace period.
///
/// Returns aggregate counters useful for boot-time telemetry.
/// Per-user failures are logged and persisted but never abort the
/// run; cancellation does abort (with the partial counts up to that
/// point reflected in the report's `processed`/`failed` fields).
pub async fn run_startup_backfill(
    deps: &ProfilePublishDeps,
    xmpp_domain: &str,
    cancel: CancellationToken,
) -> BackfillReport {
    let db_actor = deps.state.deps.app_state.db_pool.global_actor();

    let rows = match load_users(db_actor).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(error = %error, "Profile backfill aborted at user load");
            return BackfillReport::default();
        }
    };
    let mut report = BackfillReport {
        total: rows.len(),
        ..BackfillReport::default()
    };
    if rows.is_empty() {
        debug!("Profile backfill: no rows to process");
        return report;
    }
    info!(total = rows.len(), "Profile backfill: starting");

    let now = Utc::now();
    let cancel_for_workers = cancel.clone();
    let outcomes = stream::iter(rows)
        .map(|row| {
            let cancel = cancel_for_workers.clone();
            async move {
                if cancel.is_cancelled() {
                    return ProcessOutcome::Cancelled;
                }
                process_row(deps, xmpp_domain, row, now).await
            }
        })
        .buffer_unordered(MAX_INFLIGHT)
        .collect::<Vec<_>>()
        .await;

    let mut cancelled_count = 0usize;
    for outcome in outcomes {
        match outcome {
            ProcessOutcome::Ran => report.processed += 1,
            ProcessOutcome::ThrottledSkip => report.skipped_throttled += 1,
            ProcessOutcome::UserManagedSkip => report.skipped_user_managed += 1,
            ProcessOutcome::Failed => report.failed += 1,
            ProcessOutcome::Cancelled => cancelled_count += 1,
        }
    }

    if cancelled_count > 0 {
        info!(
            cancelled = cancelled_count,
            total = report.total,
            processed = report.processed,
            "Profile backfill: cancelled mid-pass (graceful shutdown)"
        );
    } else {
        info!(
            total = report.total,
            processed = report.processed,
            skipped_throttled = report.skipped_throttled,
            skipped_user_managed = report.skipped_user_managed,
            failed = report.failed,
            "Profile backfill: complete"
        );
    }
    report
}

/// What `process_row` decided to do for a single user.
#[derive(Debug, Clone, Copy)]
enum ProcessOutcome {
    /// The publish chain ran (success or no-op).
    Ran,
    /// Skipped because of the permanent-failure cool-down.
    ThrottledSkip,
    /// Skipped because the user has self-published an avatar; the
    /// guard would suppress `RemoveIfOidcOwned` and the bridge has
    /// no work to do for this user.
    UserManagedSkip,
    /// The publish chain returned an error.
    Failed,
    /// The cancellation token fired before this row started.
    Cancelled,
}

async fn process_row(
    deps: &ProfilePublishDeps,
    xmpp_domain: &str,
    row: BackfillRow,
    now: DateTime<Utc>,
) -> ProcessOutcome {
    if should_throttle(&row, now) {
        debug!(
            jid = %row.xmpp_localpart,
            last_error = ?row.last_error,
            "Profile backfill: throttled (permanent-failure cool-down)"
        );
        return ProcessOutcome::ThrottledSkip;
    }

    let bare = match format!("{}@{}", row.xmpp_localpart, xmpp_domain).parse::<BareJid>() {
        Ok(bare) => bare,
        Err(error) => {
            warn!(
                error = %error,
                localpart = %row.xmpp_localpart,
                "Profile backfill: skipping user with invalid JID"
            );
            return ProcessOutcome::Failed;
        }
    };

    let db_actor = deps.state.deps.app_state.db_pool.global_actor();

    // Provenance is checked authoritatively inside
    // `ensure_pep_profile_published` — it now consults
    // `user_avatar_source` under the per-(BareJid) lock for both
    // `SetFromUrl` and `RemoveIfOidcOwned`. We don't pre-read here
    // because the publish layer's read is the authoritative one
    // (any pre-check would race a concurrent wire publish that
    // flips the row between the two reads).

    // Track whether the malformed-URL persist needs to fire later —
    // we don't want to abort the whole row just because the PHOTO
    // axis is unhealthy. A user with a bad `avatar_url` claim but a
    // valid `display_name` should still get their FN backfilled.
    let mut photo_url_was_invalid = false;

    let photo = match row.avatar_url.as_deref() {
        Some(s) => match Url::parse(s) {
            Ok(url) => PhotoIntent::SetFromUrl(url),
            Err(error) => {
                warn!(
                    error = %error,
                    jid = %bare,
                    raw_url = %s,
                    "Profile backfill: malformed avatar_url; skipping PHOTO axis and continuing with NAME"
                );
                photo_url_was_invalid = true;
                PhotoIntent::Skip
            }
        },
        // No avatar claim. The publish layer's `RemoveIfOidcOwned`
        // path handles all three provenance states (`'oidc'` →
        // remove, `'user'` → suppress, `Unknown` → idempotent
        // no-op via the metadata-present probe).
        None => PhotoIntent::RemoveIfOidcOwned,
    };

    let name = match row.display_name.clone() {
        Some(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                NameIntent::Skip
            } else {
                NameIntent::Set(trimmed.to_string())
            }
        }
        None => NameIntent::Skip,
    };

    if matches!(photo, PhotoIntent::Skip) && matches!(name, NameIntent::Skip) {
        // Nothing to do for this user via the publish chain.
        // Persist the invalid_url state if that's why PHOTO was
        // skipped, so the throttle prevents re-attempting the bad
        // URL every boot.
        if photo_url_was_invalid {
            if let Err(error) =
                persist_attempt(db_actor, &row.xmpp_localpart, now, Some("invalid_url")).await
            {
                warn!(
                    error = %error,
                    jid = %bare,
                    "Profile backfill: failed to persist invalid_url state"
                );
            }
            return ProcessOutcome::Failed;
        }
        return ProcessOutcome::Ran;
    }

    let source = ProfileSource::Oidc { photo, name };
    match ensure_pep_profile_published(deps, &bare, source).await {
        Ok(outcome) => {
            // Honor the "invalid_url" persistent throttle even when
            // the FN axis succeeds — re-fetching the same bad URL
            // every boot is the failure mode the throttle exists to
            // prevent.
            let kind = if photo_url_was_invalid {
                Some("invalid_url")
            } else {
                None
            };
            if let Err(error) = persist_attempt(db_actor, &row.xmpp_localpart, now, kind).await {
                warn!(
                    error = %error,
                    jid = %bare,
                    "Profile backfill: failed to persist success state (continuing)"
                );
            }
            if photo_url_was_invalid {
                // Half-fail: FN succeeded but PHOTO URL was bad.
                ProcessOutcome::Failed
            } else if outcome.photo_removal_guarded_by_user_managed {
                ProcessOutcome::UserManagedSkip
            } else {
                ProcessOutcome::Ran
            }
        }
        Err(error) => {
            let kind = match &error {
                ProfileSyncError::Fetch(fetch_error) => fetch_error.kind(),
                _ => "publish_failed",
            };
            warn!(
                error = %error,
                jid = %bare,
                kind,
                "Profile backfill: per-user failure (continuing)"
            );
            if let Err(persist_err) =
                persist_attempt(db_actor, &row.xmpp_localpart, now, Some(kind)).await
            {
                warn!(
                    error = %persist_err,
                    jid = %bare,
                    "Profile backfill: failed to persist failure state"
                );
            }
            ProcessOutcome::Failed
        }
    }
}

/// Pure throttle decision — exposed at module scope so a unit test
/// can exercise it without touching the database.
///
/// Semantics:
/// - No `last_attempt_at` row → not throttled.
/// - `last_attempt_at` parses, last error is a permanent kind, and
///   `now - last_attempt <= PERMANENT_FAILURE_COOLDOWN` → throttled.
/// - `last_attempt_at` parses, last error is transient OR `None` →
///   not throttled (transient errors retry every boot; success
///   never throttles).
/// - `last_attempt_at` FAILS to parse AND last error is a permanent
///   kind → **throttled** (fail-closed). A corrupted timestamp is
///   indistinguishable from a recent permanent failure to the
///   throttle, so we err on the side of not re-hammering the IDP
///   until a human resets the row. Transient kinds with a corrupt
///   timestamp still proceed (re-attempt is the safe default).
/// - Negative `now - last_attempt` (clock skew backward) is treated
///   as "within the cool-down" — falls through to the kind check.
fn should_throttle(row: &BackfillRow, now: DateTime<Utc>) -> bool {
    let last_error_is_permanent = matches!(
        row.last_error.as_deref(),
        Some(kind) if PERMANENT_KINDS.contains(&kind)
    );
    let Some(parsed) = row
        .last_attempt_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
    else {
        // No row OR unparseable timestamp.
        if row.last_attempt_at.is_some() && last_error_is_permanent {
            // Corrupted permanent-failure row → fail-closed.
            return true;
        }
        return false;
    };
    let elapsed = now.signed_duration_since(parsed);
    if elapsed > PERMANENT_FAILURE_COOLDOWN {
        return false;
    }
    last_error_is_permanent
}

async fn load_users(db_actor: &ActorRef<DbActor>) -> Result<Vec<BackfillRow>, BackfillError> {
    let rows = db_actor
        .ask(DbQuery {
            sql: r#"
                SELECT u.xmpp_localpart, u.avatar_url, u.display_name,
                       s.last_attempt_at, s.last_error
                FROM users u
                LEFT JOIN user_avatar_fetch_state s
                  ON s.xmpp_localpart = u.xmpp_localpart
                WHERE u.avatar_url IS NOT NULL OR u.display_name IS NOT NULL
            "#
            .to_string(),
            params: vec![],
        })
        .await
        .map_err(|e| BackfillError::Storage(e.to_string()))?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let xmpp_localpart = match row.first() {
            Some(crate::db::Value::Text(s)) => s.clone(),
            _ => continue,
        };
        let avatar_url = row.get(1).and_then(|v| match v {
            crate::db::Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        let display_name = row.get(2).and_then(|v| match v {
            crate::db::Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        let last_attempt_at = row.get(3).and_then(|v| match v {
            crate::db::Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        let last_error = row.get(4).and_then(|v| match v {
            crate::db::Value::Text(s) => Some(s.clone()),
            _ => None,
        });
        out.push(BackfillRow {
            xmpp_localpart,
            avatar_url,
            display_name,
            last_attempt_at,
            last_error,
        });
    }
    Ok(out)
}

async fn persist_attempt(
    db_actor: &ActorRef<DbActor>,
    xmpp_localpart: &str,
    now: DateTime<Utc>,
    error_kind: Option<&str>,
) -> Result<(), BackfillError> {
    let now_str = now.to_rfc3339();
    let error_value: crate::db::Value = match error_kind {
        Some(k) => k.into(),
        None => crate::db::Value::Null,
    };
    db_actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO user_avatar_fetch_state
                  (xmpp_localpart, last_attempt_at, last_error, updated_at)
                VALUES (?, ?, ?, ?)
                ON CONFLICT(xmpp_localpart) DO UPDATE
                  SET last_attempt_at = excluded.last_attempt_at,
                      last_error = excluded.last_error,
                      updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                xmpp_localpart.into(),
                now_str.clone().into(),
                error_value,
                now_str.into(),
            ],
        })
        .await
        .map_err(|e| BackfillError::Storage(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_with(last_attempt: Option<&str>, last_error: Option<&str>) -> BackfillRow {
        BackfillRow {
            xmpp_localpart: "alice".into(),
            avatar_url: Some("https://example.com/a.png".into()),
            display_name: None,
            last_attempt_at: last_attempt.map(str::to_string),
            last_error: last_error.map(str::to_string),
        }
    }

    #[test]
    fn should_throttle_returns_false_when_no_prior_attempt() {
        let now = Utc::now();
        assert!(!should_throttle(&row_with(None, None), now));
        assert!(!should_throttle(
            &row_with(None, Some("permanent_4xx")),
            now
        ));
    }

    #[test]
    fn should_throttle_returns_false_after_cooldown() {
        let now = Utc::now();
        let twenty_five_hours_ago = (now - Duration::hours(25)).to_rfc3339();
        assert!(!should_throttle(
            &row_with(Some(&twenty_five_hours_ago), Some("permanent_4xx")),
            now
        ));
    }

    #[test]
    fn should_throttle_returns_true_for_recent_permanent_failures() {
        let now = Utc::now();
        let one_hour_ago = (now - Duration::hours(1)).to_rfc3339();
        for kind in [
            "permanent_4xx",
            "mime_rejected",
            "size_exceeded",
            "ssrf_blocked",
            "magic_byte_mismatch",
            "invalid_url",
        ] {
            assert!(
                should_throttle(&row_with(Some(&one_hour_ago), Some(kind)), now),
                "kind {kind} must throttle"
            );
        }
    }

    #[test]
    fn should_throttle_returns_false_for_recent_transient_failures() {
        let now = Utc::now();
        let one_hour_ago = (now - Duration::hours(1)).to_rfc3339();
        for kind in ["network", "transient_5xx", "dns", "publish_failed"] {
            assert!(
                !should_throttle(&row_with(Some(&one_hour_ago), Some(kind)), now),
                "kind {kind} must NOT throttle (transient)"
            );
        }
    }

    #[test]
    fn should_throttle_returns_false_when_recent_success() {
        let now = Utc::now();
        let one_hour_ago = (now - Duration::hours(1)).to_rfc3339();
        assert!(!should_throttle(&row_with(Some(&one_hour_ago), None), now));
    }

    #[test]
    fn should_throttle_at_exact_cooldown_boundary() {
        // Exactly 24h ago — cool-down comparison is `>` not `>=`,
        // so this still throttles. Verify the boundary so future
        // refactors don't silently flip the semantics.
        let now = Utc::now();
        let exactly_24h_ago = (now - Duration::hours(24)).to_rfc3339();
        assert!(should_throttle(
            &row_with(Some(&exactly_24h_ago), Some("permanent_4xx")),
            now
        ));
    }

    #[test]
    fn should_throttle_for_clock_skew_backward() {
        // last_attempt > now (negative elapsed). Treat as "within the
        // cool-down" → throttle if kind is permanent.
        let now = Utc::now();
        let an_hour_in_the_future = (now + Duration::hours(1)).to_rfc3339();
        assert!(should_throttle(
            &row_with(Some(&an_hour_in_the_future), Some("permanent_4xx")),
            now
        ));
        assert!(!should_throttle(
            &row_with(Some(&an_hour_in_the_future), Some("network")),
            now
        ));
    }

    #[test]
    fn should_throttle_fail_closed_on_unparseable_permanent() {
        // Corrupted timestamp + permanent kind → fail-closed.
        let now = Utc::now();
        assert!(should_throttle(
            &row_with(Some("not-an-rfc3339-timestamp"), Some("permanent_4xx")),
            now
        ));
    }

    #[test]
    fn should_throttle_fail_open_on_unparseable_transient() {
        // Corrupted timestamp + transient kind → fail-open (retry).
        let now = Utc::now();
        assert!(!should_throttle(
            &row_with(Some("garbage"), Some("network")),
            now
        ));
    }

    #[test]
    fn permanent_kinds_subset_of_fetch_error_kinds() {
        // Pin the contract: every `PERMANENT_KINDS` entry must
        // either be a real `FetchError::kind()` value OR the
        // synthesized `"invalid_url"` (which the backfill
        // emits itself for `Url::parse` failures). A future rename
        // in `fetch.rs` would silently break the throttle without
        // this guard.
        use super::super::fetch::FetchError;
        use std::net::{IpAddr, Ipv4Addr};
        let representatives: &[(FetchError, &str)] = &[
            (FetchError::InvalidScheme("http".into()), "invalid_scheme"),
            (FetchError::MissingHost, "missing_host"),
            (
                FetchError::SsrfBlocked(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
                "ssrf_blocked",
            ),
            (FetchError::Http(404), "permanent_4xx"),
            (FetchError::MimeRejected(None), "mime_rejected"),
            (FetchError::MagicByteMismatch, "magic_byte_mismatch"),
            (FetchError::SizeExceeded(100), "size_exceeded"),
        ];
        for (err, expected_kind) in representatives {
            assert_eq!(err.kind(), *expected_kind);
            assert!(
                PERMANENT_KINDS.contains(expected_kind),
                "FetchError::kind() value {expected_kind:?} should be in PERMANENT_KINDS"
            );
        }
        assert!(
            PERMANENT_KINDS.contains(&"invalid_url"),
            "synthesized 'invalid_url' kind must be in PERMANENT_KINDS"
        );
    }
}
