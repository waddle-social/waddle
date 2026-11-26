//! Best-effort display-name enrichment before first publication.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use tokio::time::{timeout_at, Instant};
use waddle_xmpp::push::envelope::NotificationClass;
use waddle_xmpp::push::names::PushNames;
use waddle_xmpp_core::xep0359::StanzaId;

use super::{NotificationOutboxJob, NotificationOutboxStore};

const LOOKUP_TIMEOUT: Duration = Duration::from_millis(250);
const PASS_TIMEOUT: Duration = Duration::from_secs(5);

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
    deadline: Instant,
    cutoff: Option<LookupCutoff>,
    affected_jobs: usize,
}

impl Default for PushDisplayNameLookupBudget {
    fn default() -> Self {
        Self {
            deadline: Instant::now() + PASS_TIMEOUT,
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
            let now = Instant::now();
            let lookup_deadline = now + LOOKUP_TIMEOUT;
            let deadline = lookup_deadline.min(budget.deadline);
            if now < deadline {
                let names = timeout_at(
                    deadline,
                    resolver.resolve(PushDisplayNameRequest {
                        recipient: job.recipient_bare_jid(),
                        conversation: job.conversation_jid(),
                        sender,
                        class,
                        archive_stanza_id: job.room_archive_stanza_id.clone(),
                    }),
                )
                .await;
                // Tokio can return the result if the lookup and its timer
                // both become ready before the next poll.
                if let Some(names) = names.ok().filter(|_| Instant::now() < deadline) {
                    return Some(names);
                }
            }
            budget.cutoff = Some(if budget.deadline <= lookup_deadline {
                LookupCutoff::PassCeiling
            } else {
                LookupCutoff::LookupTimeout
            });
        }
        budget.affected_jobs += 1;
        None
    }
}
