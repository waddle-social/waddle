//! Typed direct-message writes deferred by ingress planning.
use jid::BareJid;
use waddle_xmpp::{
    inbox::InboxEntry,
    ingress::{InboxProjectionMutation, LinkPreviewMediaRefMutation, NotificationActivityMutation},
    mam::{ArchiveExpectation, ArchivedMessage, ArchivedTombstone},
    tombstone::TombstoneTarget,
};
use waddle_xmpp_core::{mam::ThreadId, xep0359::StanzaId};

#[derive(Debug, Clone)]
pub enum DurableDirectEffect {
    ArchiveDirect {
        archive: BareJid,
        message: Box<ArchivedMessage>,
        archive_expectation: ArchiveExpectation,
    },
    ProjectInbox {
        owner: BareJid,
        entry: Box<InboxEntry>,
        increment_unread: bool,
    },
    MarkInboxRead {
        owner: BareJid,
        channel: BareJid,
        thread: Option<ThreadId>,
    },
    RetractionTombstone {
        archive: BareJid,
        target: StanzaId,
        tombstone: ArchivedTombstone,
    },
    DmCallThreadProjection {
        owner: BareJid,
        mutation: Box<InboxProjectionMutation>,
    },
}

#[derive(Debug, Clone)]
pub enum ExternalDirectEffect {
    NotificationActivity {
        owner: BareJid,
        mutation: NotificationActivityMutation,
    },
    PushInboxUpdate {
        owner: BareJid,
        entry: Box<InboxEntry>,
    },
    LinkPreviewRefs {
        mutations: Vec<LinkPreviewMediaRefMutation>,
    },
    ClearLinkPreviewRefs {
        mutations: Vec<LinkPreviewMediaRefMutation>,
    },
    /// Frozen post-commit call state; subsequent archive passes read this overlay.
    DmCallThreadState {
        state: Box<PlannedDmCallState>,
    },
    ScrubReplayForTombstone {
        target: TombstoneTarget,
    },
}

pub(crate) fn durable(deps: &super::super::Deps<'_>, effect: DurableDirectEffect) {
    deps.effects.record(planned_durable(effect));
}

pub(crate) fn planned_durable(effect: DurableDirectEffect) -> super::PlannedEffect {
    let dependency = match &effect {
        DurableDirectEffect::ProjectInbox { owner, entry, .. } => {
            Some(super::PlanEffectDependency::AfterArchive {
                archive: owner.clone(),
                minted: StanzaId::new(entry.last_stanza_id.clone(), jid::Jid::from(owner.clone())),
            })
        }
        DurableDirectEffect::DmCallThreadProjection { owner, mutation } => {
            match mutation.as_ref() {
                InboxProjectionMutation::DirectCallThreadAnchor {
                    archive_stanza_id, ..
                } => Some(super::PlanEffectDependency::AfterArchive {
                    archive: owner.clone(),
                    minted: archive_stanza_id.clone(),
                }),
                _ => None,
            }
        }
        _ => None,
    };
    let mut planned =
        super::PlannedEffect::new(super::Effect::Durable(super::DurableEffect::Direct(effect)));
    if let Some(dependency) = dependency {
        planned = planned.with_dependency(dependency);
    }
    planned
}

pub(crate) fn external(deps: &super::super::Deps<'_>, effect: ExternalDirectEffect) {
    let dependency = match &effect {
        ExternalDirectEffect::PushInboxUpdate { owner, entry } => {
            let archive = match entry.kind {
                waddle_xmpp::inbox::ConversationKind::Direct => owner,
                waddle_xmpp::inbox::ConversationKind::MucRoom => &entry.partner,
            };
            Some(super::PlanEffectDependency::AfterArchive {
                archive: archive.clone(),
                minted: StanzaId::new(
                    entry.last_stanza_id.clone(),
                    jid::Jid::from(archive.clone()),
                ),
            })
        }

        ExternalDirectEffect::DmCallThreadState { state } => state
            .active
            .as_ref()
            .and_then(|active| active.anchor.as_ref())
            .map(|anchor| super::PlanEffectDependency::AfterArchive {
                archive: anchor.by.to_bare(),
                minted: anchor.clone(),
            }),
        _ => None,
    };
    let message_dependencies = match &effect {
        ExternalDirectEffect::LinkPreviewRefs { mutations }
        | ExternalDirectEffect::ClearLinkPreviewRefs { mutations } => mutations
            .iter()
            .map(|mutation| super::PlanEffectDependency::AfterArchive {
                archive: mutation.archive.clone(),
                minted: mutation.current_archive_stanza_id.clone(),
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let suppression = if matches!(&effect, ExternalDirectEffect::PushInboxUpdate { .. }) {
        super::PlanSuppressionPolicy::SenderOnly
    } else {
        super::PlanSuppressionPolicy::Always
    };
    let mut planned = super::PlannedEffect::new(super::Effect::External(
        super::ExternalEffect::Direct(effect),
    ))
    .with_suppression(suppression);
    if let Some(dependency) = dependency {
        planned = planned.with_dependency(dependency);
    }
    for dependency in message_dependencies {
        if !planned.dependencies.contains(&dependency) {
            planned = planned.with_dependency(dependency);
        }
    }
    deps.effects.record(planned);
}

#[derive(Debug, Clone)]
pub struct PlannedDmCallState {
    pub key: crate::server::routes::websocket::DmCallThreadKey,
    pub pending: Option<crate::server::routes::websocket::PendingDmCallOffer>,
    pub active: Option<PlannedActiveDmCall>,
    pub projected: std::collections::HashSet<BareJid>,
}

#[derive(Debug, Clone)]
pub struct PlannedActiveDmCall {
    pub anchor: Option<StanzaId>,
    pub initiator: BareJid,
    pub media: waddle_xmpp::xep::CallThreadMedia,
    pub started: chrono::DateTime<chrono::Utc>,
    pub thread: ThreadId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::interpret::{effects::PlanSink, Deps};
    use waddle_xmpp::{inbox::ConversationKind, registry::ConnectionRegistry};

    #[test]
    fn inbox_push_dependency_uses_the_conversation_archive_authority() {
        let registry = ConnectionRegistry::new();
        let sink = PlanSink::new();
        let mut deps = Deps::registry_only(&registry);
        deps.effects = &sink;
        let owner: BareJid = "alice@example.com".parse().expect("owner");
        let partner: BareJid = "room@conference.example.com".parse().expect("partner");
        for (kind, archive) in [
            (ConversationKind::Direct, &owner),
            (ConversationKind::MucRoom, &partner),
        ] {
            external(
                &deps,
                ExternalDirectEffect::PushInboxUpdate {
                    owner: owner.clone(),
                    entry: Box::new(InboxEntry::new(partner.clone(), kind, "archive-id", 0)),
                },
            );
            let plan = sink.snapshot();
            assert_eq!(
                plan.last().expect("push").dependencies,
                vec![super::super::PlanEffectDependency::AfterArchive {
                    archive: archive.clone(),
                    minted: StanzaId::new("archive-id", jid::Jid::from(archive.clone())),
                }]
            );
        }
    }
}
