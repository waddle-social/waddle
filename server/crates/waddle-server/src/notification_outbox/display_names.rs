//! Best-effort display-name enrichment before first publication.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp::push::names::PushNames;

use super::{NotificationClass, NotificationOutboxJob, NotificationOutboxStore};

const LOOKUP_TIMEOUT: Duration = Duration::from_millis(250);

/// Resolves names that this recipient is entitled to see.
#[async_trait]
pub trait PushDisplayNames: Send + Sync {
    async fn resolve(
        &self,
        recipient: &BareJid,
        conversation: &BareJid,
        sender: &Jid,
        class: NotificationClass,
    ) -> PushNames;
}

/// The resolver is attached once and shared by store clones.
#[derive(Clone, Default)]
pub(super) struct PushDisplayNamesSlot(Arc<OnceLock<Arc<dyn PushDisplayNames>>>);

impl NotificationOutboxStore {
    /// Attach the optional resolver. Returns false if one is already attached.
    pub fn attach_push_display_names(&self, resolver: Arc<dyn PushDisplayNames>) -> bool {
        self.push_display_names.0.set(resolver).is_ok()
    }

    pub(super) async fn resolve_push_display_names(
        &self,
        job: &NotificationOutboxJob,
    ) -> Option<PushNames> {
        // Reuse the frozen payload on retry, including any absent names.
        if job.approved_payload.is_some() || job.class() == NotificationClass::Call {
            return None;
        }
        let sender = job.rich_summary().sender.as_ref()?;
        let resolver = self.push_display_names.0.get()?;
        tokio::time::timeout(
            LOOKUP_TIMEOUT,
            resolver.resolve(
                job.recipient_bare_jid(),
                job.conversation_jid(),
                sender,
                job.class(),
            ),
        )
        .await
        .ok()
    }
}
