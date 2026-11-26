//! Best-effort display-name enrichment before first publication.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use tokio::time::{timeout, Instant};
use waddle_xmpp::push::envelope::NotificationClass;
use waddle_xmpp::push::names::PushNames;
use waddle_xmpp_core::xep0359::StanzaId;

use super::{NotificationOutboxJob, NotificationOutboxStore};

const LOOKUP_TIMEOUT: Duration = Duration::from_millis(250);
/// Time a pass may spend inside lookups. The rest of a pass, storage and
/// publishing, is not charged, so a large batch keeps its names.
const PASS_LOOKUP_BUDGET: Duration = Duration::from_secs(5);

/// Names are resolved for an opted-in recipient and the job's latest message.
pub struct PushDisplayNameRequest<'a> {
    pub recipient: &'a BareJid,
    pub conversation: &'a BareJid,
    pub sender: &'a Jid,
    pub class: NotificationClass,
    pub archive_stanza_id: Option<StanzaId>,
}

/// Resolves names that this recipient is entitled to see.
#[async_trait]
pub trait PushDisplayNameResolver: Send + Sync {
    async fn resolve(&self, request: PushDisplayNameRequest<'_>) -> PushNames;
}

/// The resolver is attached once and shared by store clones.
#[derive(Clone, Default)]
pub(super) struct PushDisplayNamesSlot(Arc<OnceLock<Arc<dyn PushDisplayNameResolver>>>);

#[derive(Debug)]
enum LookupCutoff {
    LookupTimeout,
    PassCeiling,
}

/// Bounds optional name lookups while a claimed batch is being published.
pub(super) struct PushDisplayNameLookupBudget {
    remaining: Duration,
    cutoff: Option<LookupCutoff>,
    affected_jobs: usize,
}

impl Default for PushDisplayNameLookupBudget {
    fn default() -> Self {
        Self {
            remaining: PASS_LOOKUP_BUDGET,
            cutoff: None,
            affected_jobs: 0,
        }
    }
}

impl Drop for PushDisplayNameLookupBudget {
    fn drop(&mut self) {
        if let Some(cutoff) = &self.cutoff {
            tracing::warn!(
                ?cutoff,
                affected_jobs = self.affected_jobs,
                "Push display-name lookup limit reached"
            );
        }
    }
}

impl NotificationOutboxStore {
    /// Attach the optional resolver. Returns false if one is already attached.
    pub fn attach_push_display_names(&self, resolver: Arc<dyn PushDisplayNameResolver>) -> bool {
        self.push_display_names.0.set(resolver).is_ok()
    }

    pub(super) async fn resolve_push_display_names(
        &self,
        job: &NotificationOutboxJob,
        budget: &mut PushDisplayNameLookupBudget,
    ) -> Option<PushNames> {
        let class = NotificationClass::from(job.class());
        // Reuse the frozen payload on retry, including any absent names.
        if job.approved_payload.is_some() || !class.allows_display_names() {
            return None;
        }
        let sender = job.rich_summary().sender.as_ref()?;
        let resolver = self.push_display_names.0.get()?;
        if budget.cutoff.is_none() {
            let allowed = LOOKUP_TIMEOUT.min(budget.remaining);
            let started = Instant::now();
            let names = timeout(
                allowed,
                resolver.resolve(PushDisplayNameRequest {
                    recipient: job.recipient_bare_jid(),
                    conversation: job.conversation_jid(),
                    sender,
                    class,
                    archive_stanza_id: job.room_archive_stanza_id.clone(),
                }),
            )
            .await;
            budget.remaining = budget.remaining.saturating_sub(started.elapsed());
            if let Ok(names) = names {
                return Some(names);
            }
            budget.cutoff = Some(if allowed < LOOKUP_TIMEOUT {
                LookupCutoff::PassCeiling
            } else {
                LookupCutoff::LookupTimeout
            });
        }
        budget.affected_jobs += 1;
        None
    }
}
