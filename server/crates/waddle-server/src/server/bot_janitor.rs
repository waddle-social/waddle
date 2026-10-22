//! Bot lifecycle janitor (D21): deletes spent bot tokens and keeps each
//! bot's disabled state in line with its owner's health.
//!
//! The janitor is cleanup, not the security boundary. Every token
//! validation already checks the owner live (D24), and every bot action is
//! intersected with the owner's access (D25).
//!
//! Account lifecycle:
//! - **Bot deleted:** the `/api/bots` delete route clears the bot's room and
//!   space affiliations, purges its tuples, deletes the users row (tokens
//!   cascade) and then the owner tuple.
//! - **Owner row or credentials gone:** `validate` rejects at once, because
//!   an owner with no membership fails the D24 gate. The next sweep sets
//!   `owner_missing` and revokes the tokens in the same transaction.
//! - **Owner lost `server#member`, `admin` and `owner`:** `validate` rejects
//!   at once (D24). The sweep sets `owner_not_member` and revokes the
//!   tokens. If the owner later regains membership (an OIDC login
//!   re-provisions it), the sweep re-enables the bot with no valid tokens,
//!   and the owner must mint new ones.
//! - **Owner removed or outcast from a space or channel:** `authorize_bot`
//!   denies through the D25 intersection. No janitor action is needed.
//! - Bots never hold `server#member`: membership reconciliation only
//!   provisions human accounts on the primary domain.
//!
//! Every replica may run the sweep. All of its writes are idempotent.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use jid::BareJid;
use kameo::actor::ActorRef;
use tracing::{debug, info, warn, Instrument};
use waddle_xmpp::telemetry::attributes::{Janitor, SweepOutcome};
use waddle_xmpp::telemetry::reliability::record_janitor_sweep;

use crate::auth::bots::store::BotOwnerHealthRow;
use crate::auth::bots::{BotStore, BotStoreError, BotTokenStore, DisabledReason};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionActor, Subject,
};
use crate::server::routes::websocket::WebSocketState;
use crate::server::session_janitors::janitor_sweep_span;

const BOT_JANITOR_INTERVAL: Duration = Duration::from_secs(60);
/// Applies to each page (its read and every transition in it).
const BOT_JANITOR_PAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// No new page starts once a tick has run this long; the cursor carries
/// the rest over to the next tick.
const BOT_JANITOR_TICK_BUDGET: Duration = Duration::from_secs(45);
const BOT_TOKEN_REVOKED_RETENTION_DAYS: i64 = 7;
const BOT_LIFECYCLE_PAGE: u32 = 200;

#[derive(Debug, thiserror::Error)]
pub(crate) enum BotJanitorError {
    #[error(transparent)]
    Storage(#[from] BotStoreError),
    #[error("bot lifecycle page timed out")]
    PageTimeout,
}

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BotSweepReport {
    pub tokens_deleted: u64,
    pub evaluated: usize,
    pub disabled: usize,
    pub enabled: usize,
    pub tokens_revoked: u64,
    /// The outcome recorded for this tick.
    pub outcome: SweepOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transition {
    None,
    Disable(DisabledReason),
    Enable,
}

/// The pure transition rule. `owner_is_member` is `None` when membership
/// is unknown (the permission backend failed, or the owner was not
/// checked); an unknown answer never disables a bot.
fn decide(row: &BotOwnerHealthRow, owner_is_member: Option<bool>) -> Transition {
    let wanted = if !row.owner_row_exists || !row.owner_has_credential {
        Some(DisabledReason::OwnerMissing)
    } else {
        match owner_is_member {
            None => return Transition::None,
            Some(false) => Some(DisabledReason::OwnerNotMember),
            Some(true) => None,
        }
    };
    match (wanted, row.disabled_reason) {
        (Some(reason), current) if current != Some(reason) => Transition::Disable(reason),
        (None, Some(_)) => Transition::Enable,
        _ => Transition::None,
    }
}

/// Whether an owner still holds `create_bot` on the server, which
/// `server#member`, `admin` and `owner` all grant.
trait OwnerMembership {
    async fn is_member(&self, owner: &BareJid) -> Result<bool, String>;
}

struct PermissionMembership<'a> {
    permissions: &'a ActorRef<PermissionActor>,
    server_id: &'a str,
}

impl OwnerMembership for PermissionMembership<'_> {
    async fn is_member(&self, owner: &BareJid) -> Result<bool, String> {
        self.permissions
            .ask(CheckPermission {
                subject: Subject::user(owner.to_string()),
                permission: Permission::CreateBot,
                object: Object::new(ObjectType::Server, self.server_id),
            })
            .await
            .map(|response| response.allowed)
            .map_err(|error| error.to_string())
    }
}

struct Sweep<'a, M> {
    store: &'a BotStore,
    tokens: &'a BotTokenStore,
    membership: M,
    page_size: u32,
    page_timeout: Duration,
    tick_budget: Duration,
}

/// Ticks every [`BOT_JANITOR_INTERVAL`], skipping the first. The paging
/// cursor lives in the task, so a tick resumes where the last one stopped.
pub(crate) fn spawn_bot_lifecycle_janitor(websocket_state: &Arc<WebSocketState>) {
    let weak_state = Arc::downgrade(websocket_state);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(BOT_JANITOR_INTERVAL);
        ticker.tick().await;
        let mut cursor = None;
        loop {
            ticker.tick().await;
            let Some(state) = weak_state.upgrade() else {
                break;
            };
            if let Err(error) = run_bot_lifecycle_sweep(&state, &mut cursor).await {
                warn!(%error, "bot janitor: sweep failed; will retry next tick");
            }
        }
    });
}

/// One tick. Records exactly one `waddle.janitor.sweeps` sample.
pub(crate) async fn run_bot_lifecycle_sweep(
    state: &WebSocketState,
    cursor: &mut Option<BareJid>,
) -> Result<BotSweepReport, BotJanitorError> {
    let auth = &state.deps.auth_state;
    let sweep = Sweep {
        store: &auth.bot_store,
        tokens: &auth.bot_tokens,
        membership: PermissionMembership {
            permissions: &auth.permission_actor,
            server_id: auth.bootstrap_membership.server_id.as_str(),
        },
        page_size: BOT_LIFECYCLE_PAGE,
        page_timeout: BOT_JANITOR_PAGE_TIMEOUT,
        tick_budget: BOT_JANITOR_TICK_BUDGET,
    };
    sweep
        .run(cursor, Utc::now())
        .instrument(janitor_sweep_span(Janitor::BotLifecycle))
        .await
}

enum PageEnd {
    /// A full page: more bots may follow.
    Full,
    /// The last page: every bot has been evaluated.
    Exhausted,
}

impl<M: OwnerMembership> Sweep<'_, M> {
    async fn run(
        &self,
        cursor: &mut Option<BareJid>,
        now: DateTime<Utc>,
    ) -> Result<BotSweepReport, BotJanitorError> {
        let result = self.sweep(cursor, now).await;
        let outcome = match &result {
            Ok(report) => report.outcome,
            Err(_) => SweepOutcome::Failed,
        };
        record_janitor_sweep(Janitor::BotLifecycle, outcome);
        result
    }

    async fn sweep(
        &self,
        cursor: &mut Option<BareJid>,
        now: DateTime<Utc>,
    ) -> Result<BotSweepReport, BotJanitorError> {
        let started = Instant::now();
        let mut report = BotSweepReport {
            tokens_deleted: 0,
            evaluated: 0,
            disabled: 0,
            enabled: 0,
            tokens_revoked: 0,
            outcome: SweepOutcome::Completed,
        };
        let mut failed = false;

        let retention = chrono::Duration::days(BOT_TOKEN_REVOKED_RETENTION_DAYS);
        match self.tokens.sweep(now, retention).await {
            Ok(deleted) => report.tokens_deleted = deleted,
            Err(error) => {
                warn!(%error, "bot janitor: token sweep failed");
                failed = true;
            }
        }

        // One membership check per distinct owner per tick.
        let mut owners = HashMap::new();
        let deferred = loop {
            let page = tokio::time::timeout(
                self.page_timeout,
                self.sweep_page(cursor, &mut owners, &mut report, &mut failed, now),
            )
            .await
            .map_err(|_| BotJanitorError::PageTimeout)??;
            match page {
                PageEnd::Exhausted => {
                    *cursor = None;
                    break false;
                }
                PageEnd::Full if started.elapsed() >= self.tick_budget => break true,
                PageEnd::Full => {}
            }
        };

        report.outcome = if failed {
            SweepOutcome::Failed
        } else if deferred {
            SweepOutcome::Deferred
        } else {
            SweepOutcome::Completed
        };
        debug!(
            tokens_deleted = report.tokens_deleted,
            evaluated = report.evaluated,
            disabled = report.disabled,
            enabled = report.enabled,
            tokens_revoked = report.tokens_revoked,
            "bot janitor: sweep finished"
        );
        Ok(report)
    }

    /// Evaluate one page after `cursor`, advancing it past each bot.
    async fn sweep_page(
        &self,
        cursor: &mut Option<BareJid>,
        owners: &mut HashMap<BareJid, Result<bool, String>>,
        report: &mut BotSweepReport,
        failed: &mut bool,
        now: DateTime<Utc>,
    ) -> Result<PageEnd, BotJanitorError> {
        let rows = self
            .store
            .lifecycle_page(cursor.as_ref(), self.page_size)
            .await?;
        let end = if rows.len() < self.page_size as usize {
            PageEnd::Exhausted
        } else {
            PageEnd::Full
        };
        for row in rows {
            let owner_is_member = if row.owner_row_exists && row.owner_has_credential {
                let member = self.owner_membership(owners, &row.owner).await;
                *failed |= member.is_none();
                member
            } else {
                None
            };
            match decide(&row, owner_is_member) {
                Transition::None => {}
                Transition::Disable(reason) => {
                    match self.store.disable(&row.bot, reason, now).await {
                        Ok(disabled) => {
                            info!(
                                bot = %row.bot,
                                reason = reason.as_str(),
                                tokens_revoked = disabled.tokens_revoked,
                                "bot janitor: disabled bot and revoked its tokens"
                            );
                            report.disabled += usize::from(disabled.disabled);
                            report.tokens_revoked += disabled.tokens_revoked;
                        }
                        Err(error) => {
                            warn!(bot = %row.bot, %error, "bot janitor: disabling bot failed");
                            *failed = true;
                        }
                    }
                }
                Transition::Enable => match self.store.enable(&row.bot, now).await {
                    Ok(enabled) => {
                        info!(bot = %row.bot, "bot janitor: owner healthy again; re-enabled bot");
                        report.enabled += usize::from(enabled);
                    }
                    Err(error) => {
                        warn!(bot = %row.bot, %error, "bot janitor: re-enabling bot failed");
                        *failed = true;
                    }
                },
            }
            report.evaluated += 1;
            *cursor = Some(row.bot.as_bare().clone());
        }
        Ok(end)
    }

    /// The memoized membership of `owner`. `None` means the backend failed;
    /// the failure is logged once per owner per tick.
    async fn owner_membership(
        &self,
        owners: &mut HashMap<BareJid, Result<bool, String>>,
        owner: &BareJid,
    ) -> Option<bool> {
        let check = match owners.entry(owner.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                let check = self.membership.is_member(owner).await;
                if let Err(error) = &check {
                    warn!(
                        owner = %owner,
                        %error,
                        "bot janitor: owner membership check failed; leaving their bots unchanged"
                    );
                }
                entry.insert(check)
            }
        };
        check.as_ref().ok().copied()
    }
}

#[cfg(test)]
mod tests;
