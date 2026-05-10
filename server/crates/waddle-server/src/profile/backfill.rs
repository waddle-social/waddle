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

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use futures::stream::{self, StreamExt};
use jid::BareJid;
use thiserror::Error;
use tracing::{debug, info, warn};
use url::Url;

use super::avatar_source::{read_avatar_source, AvatarSource};
use super::publish::{ensure_pep_profile_published, ProfilePublishDeps};
use super::source::{NameIntent, PhotoIntent, ProfileSource, ProfileSyncError};
use crate::db::actor::{DbActor, DbExecute, DbQuery};
use crate::server::routes::websocket::WebSocketState;
use kameo::actor::ActorRef;

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
/// `BareJid`).
///
/// Returns aggregate counters useful for boot-time telemetry.
/// Per-user failures are logged and persisted but never abort the
/// run.
pub async fn run_startup_backfill(deps: &ProfilePublishDeps, xmpp_domain: &str) -> BackfillReport {
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
    let outcomes = stream::iter(rows)
        .map(|row| process_row(deps, xmpp_domain, row, now))
        .buffer_unordered(MAX_INFLIGHT)
        .collect::<Vec<_>>()
        .await;

    for outcome in outcomes {
        match outcome {
            ProcessOutcome::Ran => report.processed += 1,
            ProcessOutcome::ThrottledSkip => report.skipped_throttled += 1,
            ProcessOutcome::UserManagedSkip => report.skipped_user_managed += 1,
            ProcessOutcome::Failed => report.failed += 1,
        }
    }

    info!(
        total = report.total,
        processed = report.processed,
        skipped_throttled = report.skipped_throttled,
        skipped_user_managed = report.skipped_user_managed,
        failed = report.failed,
        "Profile backfill: complete"
    );
    report
}

/// Convenience for callers that want to spawn the backfill
/// registered with the WebSocket state's `profile_publish_tracker`,
/// so the graceful-shutdown drain awaits it.
///
/// Returns the `JoinHandle` so the caller can `let _handle = ...`
/// and detach without tripping `let_underscore_future`. The
/// tracker holds the strong reference for `wait()`.
pub fn spawn_startup_backfill(
    state: Arc<WebSocketState>,
    vcard_store: crate::vcard::VCardStore,
    xmpp_domain: String,
) -> tokio::task::JoinHandle<BackfillReport> {
    let tracker = state.deps.protocol.profile_publish_tracker.clone();
    let fetch_policy = super::FetchPolicy::default();
    tracker.spawn(async move {
        let deps = ProfilePublishDeps {
            state,
            vcard_store,
            fetch_policy,
        };
        run_startup_backfill(&deps, &xmpp_domain).await
    })
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

    // Provenance check up front. A user who has self-published their
    // avatar should not have the bridge re-fetch and re-publish on
    // every boot — the user-managed guard would suppress
    // `RemoveIfOidcOwned`, and we shouldn't waste an HTTP fetch + PEP
    // publish for `SetFromUrl` when the user has explicitly opted out.
    let db_actor = deps.state.deps.app_state.db_pool.global_actor();
    let provenance = match read_avatar_source(db_actor, &bare).await {
        Ok(source) => source,
        Err(error) => {
            warn!(
                error = %error,
                jid = %bare,
                "Profile backfill: avatar_source lookup failed; treating as unknown and continuing"
            );
            AvatarSource::Unknown
        }
    };

    // Track whether the malformed-URL persist needs to fire later —
    // we don't want to abort the whole row just because the PHOTO
    // axis is unhealthy. A user with a bad `avatar_url` claim but a
    // valid `display_name` should still get their FN backfilled.
    let mut photo_url_was_invalid = false;

    let photo = match (row.avatar_url.as_deref(), provenance) {
        // User self-published; bridge stays out of the PHOTO axis
        // entirely. (Setting via OIDC after the user opted into
        // self-management is a deliberate choice the user makes by
        // wire-publishing empty `<metadata/>` to opt back in — see
        // PR4's `record_oidc_managed` hook.)
        (_, AvatarSource::User) => PhotoIntent::Skip,
        (Some(s), _) => match Url::parse(s) {
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
        // No avatar claim. If OIDC owned the previous avatar this
        // becomes a removal; otherwise leave well enough alone.
        (None, AvatarSource::Oidc) => PhotoIntent::RemoveIfOidcOwned,
        (None, AvatarSource::Unknown) => PhotoIntent::Skip,
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
            let _ = persist_attempt(db_actor, &row.xmpp_localpart, now, Some("invalid_url")).await;
            return ProcessOutcome::Failed;
        }
        return if matches!(provenance, AvatarSource::User) {
            ProcessOutcome::UserManagedSkip
        } else {
            ProcessOutcome::Ran
        };
    }

    let source = ProfileSource::Oidc { photo, name };
    match ensure_pep_profile_published(deps, &bare, source).await {
        Ok(_) => {
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
            // If only the FN axis ran (PHOTO skipped on invalid
            // URL), surface that as Failed so the boot-time
            // counters reflect the half-fail.
            if photo_url_was_invalid {
                ProcessOutcome::Failed
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
fn should_throttle(row: &BackfillRow, now: DateTime<Utc>) -> bool {
    let Some(last_attempt) = row
        .last_attempt_at
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
    else {
        return false;
    };
    if now.signed_duration_since(last_attempt) > PERMANENT_FAILURE_COOLDOWN {
        return false;
    }
    matches!(row.last_error.as_deref(), Some(kind) if PERMANENT_KINDS.contains(&kind))
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
}
