//! Apply typed direct effects against the production handles.
use super::super::Deps;
use super::{
    direct::{DurableDirectEffect, ExternalDirectEffect},
    EffectOutcome,
};
use tracing::warn;
use waddle_xmpp::{
    inbox::{ConversationKind, InboxEntry},
    ingress::{InboxProjectionMutation, NotificationActivityMutation},
};

pub(super) async fn execute_durable(effect: DurableDirectEffect, deps: &Deps<'_>) -> EffectOutcome {
    match effect {
        DurableDirectEffect::ArchiveDirect {
            archive, message, ..
        } => {
            let Some(storage) = deps.mam_storage else {
                return EffectOutcome::Unavailable;
            };
            EffectOutcome::Archive(storage.store_message(&archive, &message).await)
        }
        DurableDirectEffect::ProjectInbox {
            owner,
            entry,
            increment_unread,
        } => {
            let Some(storage) = deps.inbox_storage else {
                return EffectOutcome::Unavailable;
            };
            EffectOutcome::Inbox(storage.upsert(&owner, *entry, increment_unread).await)
        }
        DurableDirectEffect::MarkInboxRead {
            owner,
            channel,
            thread,
        } => {
            let Some(storage) = deps.inbox_storage else {
                return EffectOutcome::Unavailable;
            };
            match storage
                .mark_read(&owner, &channel, thread.as_ref().map(|id| id.as_str()))
                .await
            {
                Ok(Some(entry)) => EffectOutcome::Inbox(Ok(entry)),
                Ok(None) => EffectOutcome::Completed,
                Err(error) => EffectOutcome::Inbox(Err(error)),
            }
        }
        DurableDirectEffect::RetractionTombstone {
            target, tombstone, ..
        } => {
            let Some(storage) = deps.mam_storage else {
                return EffectOutcome::Unavailable;
            };
            if let Err(error) = storage.replace_with_tombstone(&target.id, tombstone).await {
                warn!(%error, "planned direct tombstone write failed");
                return EffectOutcome::Unavailable;
            }
            EffectOutcome::Completed
        }
        DurableDirectEffect::DmCallThreadProjection { owner, mutation } => {
            let Some(storage) = deps.inbox_storage else {
                return EffectOutcome::Unavailable;
            };
            match *mutation {
                InboxProjectionMutation::DirectCallThreadAnchor {
                    peer,
                    thread_id,
                    archive_stanza_id,
                    media,
                    last_updated,
                } => {
                    let entry = InboxEntry::new(
                        peer,
                        ConversationKind::Direct,
                        archive_stanza_id.id,
                        last_updated,
                    )
                    .with_thread(thread_id.as_str())
                    .with_call_thread(waddle_xmpp::xep::CallThreadKind::Dm, media);
                    EffectOutcome::Inbox(storage.upsert(&owner, entry, false).await)
                }
                InboxProjectionMutation::DirectCallThreadEnded {
                    peer,
                    thread_id,
                    ended,
                    duration,
                } => {
                    if let Err(error) = storage
                        .mark_direct_call_thread_ended(
                            &owner,
                            &peer,
                            thread_id.as_str(),
                            ended,
                            &duration,
                        )
                        .await
                    {
                        return EffectOutcome::Inbox(Err(error));
                    }
                    EffectOutcome::Completed
                }
                _ => EffectOutcome::Unavailable,
            }
        }
    }
}

pub(super) async fn execute_external(
    effect: ExternalDirectEffect,
    deps: &Deps<'_>,
    applied: &super::AppliedDurableEffects,
) -> EffectOutcome {
    match effect {
        ExternalDirectEffect::PushInboxUpdate { owner, projection } => {
            let Some(entry) = applied.inbox(projection) else {
                return EffectOutcome::Unavailable;
            };
            super::super::groupchat_archive::push_inbox_update(
                deps.connection_registry,
                deps.user_registry,
                &owner,
                entry,
            )
            .await;
            EffectOutcome::Completed
        }
        ExternalDirectEffect::ScrubReplayForTombstone { target } => {
            super::super::groupchat_archive::scrub_unacked_for_tombstone(
                deps.sm_session_registry,
                deps.pending_delivery_storage,
                &target,
                "PlannedRetraction",
                deps.ingress_effect_capture.as_ref(),
            )
            .await;
            EffectOutcome::Completed
        }
        ExternalDirectEffect::DmCallThreadState { state } => {
            let Some(socket) = deps.web_socket_state else {
                return EffectOutcome::Unavailable;
            };
            let protocol = &socket.deps.protocol;
            match state.pending {
                Some(pending) => {
                    protocol
                        .pending_dm_call_offers
                        .insert(state.key.clone(), pending);
                }
                None => {
                    protocol.pending_dm_call_offers.remove(&state.key);
                }
            }
            match state.active {
                Some(active) => {
                    protocol.dm_call_threads.insert(
                        state.key.clone(),
                        crate::server::routes::websocket::ActiveCallThread {
                            anchor_origin_id: active
                                .anchor
                                .map(|anchor| anchor.id)
                                .unwrap_or_default(),
                            initiator: active.initiator,
                            media: active.media,
                            started: active.started,
                            thread_id: active.thread.as_str().to_owned(),
                        },
                    );
                }
                None => {
                    protocol.dm_call_threads.remove(&state.key);
                }
            }
            for owner in [&state.key.low_peer, &state.key.high_peer] {
                let key = (owner.clone(), state.key.clone());
                if state.projected.contains(owner) {
                    protocol.dm_call_thread_projections.insert(key);
                } else {
                    protocol.dm_call_thread_projections.remove(&key);
                }
            }
            EffectOutcome::Completed
        }
        ExternalDirectEffect::NotificationActivity { owner, mutation } => {
            notification_activity(deps, &owner, mutation).await
        }
        ExternalDirectEffect::LinkPreviewRefs { mutations }
        | ExternalDirectEffect::ClearLinkPreviewRefs { mutations } => {
            super::super::preview_plan::execute(deps, mutations).await
        }
    }
}

async fn notification_activity(
    deps: &Deps<'_>,
    owner: &jid::BareJid,
    mutation: NotificationActivityMutation,
) -> EffectOutcome {
    let Some(state) = deps.web_socket_state else {
        return EffectOutcome::Unavailable;
    };
    let store = &state.deps.protocol.notification_activity;
    let result = match mutation {
        NotificationActivityMutation::ChatState {
            conversation,
            state,
            committed_at_ms,
        } => {
            store
                .record_chat_state(
                    owner,
                    &conversation,
                    crate::notification_activity::NotificationChatState::from_xep0085(state),
                    committed_at_ms,
                )
                .await
        }
        NotificationActivityMutation::ChatStateGone {
            conversation,
            committed_at_ms,
        } => {
            store
                .record_chat_state_gone(owner, &conversation, committed_at_ms)
                .await
        }
        NotificationActivityMutation::ReadMarker {
            conversation,
            committed_at_ms,
        } => {
            store
                .record_read_marker(owner, &conversation, committed_at_ms)
                .await
        }
        NotificationActivityMutation::OutboundMessage {
            conversation,
            committed_at_ms,
        } => {
            store
                .record_outbound_message(owner, &conversation, committed_at_ms)
                .await
        }
        NotificationActivityMutation::OfflineDelivery { .. }
        | NotificationActivityMutation::NotificationCandidate { .. } => {
            return EffectOutcome::Unavailable
        }
    };
    match result {
        Ok(()) => EffectOutcome::Completed,
        Err(error) => {
            warn!(%error, "planned notification activity failed");
            EffectOutcome::Unavailable
        }
    }
}
