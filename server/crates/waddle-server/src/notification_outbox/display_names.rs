//! Best-effort display-name enrichment before first publication.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use tokio::time::Instant;
use waddle_xmpp::push::envelope::NotificationClass;
use waddle_xmpp::push::names::PushNames;
use waddle_xmpp_core::xep0359::StanzaId;

use super::{NotificationOutboxJob, NotificationOutboxStore};

const LOOKUP_BUDGET: Duration = Duration::from_millis(250);

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

/// Caps the time spent waiting for optional names across a claimed batch.
pub(super) struct PushDisplayNameLookupBudget(Duration);

impl Default for PushDisplayNameLookupBudget {
    fn default() -> Self {
        Self(LOOKUP_BUDGET)
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
        if budget.0.is_zero() || job.approved_payload.is_some() || !class.allows_display_names() {
            return None;
        }
        let sender = job.rich_summary().sender.as_ref()?;
        let resolver = self.push_display_names.0.get()?;
        let start = Instant::now();
        let names = tokio::time::timeout(
            budget.0,
            resolver.resolve(PushDisplayNameRequest {
                recipient: job.recipient_bare_jid(),
                conversation: job.conversation_jid(),
                sender,
                class,
                archive_stanza_id: job.room_archive_stanza_id.clone(),
            }),
        )
        .await;
        match names {
            Ok(names) => {
                budget.0 = budget.0.saturating_sub(start.elapsed());
                Some(names)
            }
            Err(_) => {
                budget.0 = Duration::ZERO;
                tracing::warn!("Push display-name lookup timed out; skipping names for the rest of this outbox drain");
                None
            }
        }
    }
}
