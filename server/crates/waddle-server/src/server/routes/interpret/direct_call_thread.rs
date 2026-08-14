use super::Deps;
use jid::BareJid;
use tracing::{debug, warn};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::{
    ingress::{InboxProjectionMutation, IngressEffectIntent},
    xep::{CallThreadDuration, CallThreadKind, CallThreadMedia},
};
use waddle_xmpp_core::{mam::ThreadId, xep0359::StanzaId};

pub(super) async fn project_direct_call_thread_anchor(
    deps: &Deps<'_>,
    owner: BareJid,
    peer: BareJid,
    thread_id: String,
    stanza_id: String,
    media: CallThreadMedia,
    last_updated: i64,
) {
    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            owner = %owner,
            peer = %peer,
            "ProjectDirectCallThreadAnchor: no inbox_storage in Deps; skipping"
        );
        return;
    };

    let Some(thread_id) = ThreadId::new(thread_id) else {
        warn!(owner = %owner, peer = %peer, "ProjectDirectCallThreadAnchor: invalid thread id");
        return;
    };
    let entry = InboxEntry::new(
        peer.clone(),
        ConversationKind::Direct,
        stanza_id.clone(),
        last_updated,
    )
    .with_thread(thread_id.as_str())
    .with_call_thread(CallThreadKind::Dm, media);
    match inbox_storage.upsert(&owner, entry, false).await {
        Ok(_) => deps.capture_intent(IngressEffectIntent::InboxProject {
            owner: owner.clone(),
            mutation: InboxProjectionMutation::DirectCallThreadAnchor {
                peer: peer.clone(),
                thread_id,
                archive_stanza_id: StanzaId::new(stanza_id, jid::Jid::from(owner.clone())),
                media,
                last_updated,
            },
        }),
        Err(error) => warn!(
            owner = %owner,
            peer = %peer,
            %error,
            "ProjectDirectCallThreadAnchor: inbox upsert failed"
        ),
    }
}

pub(super) async fn mark_direct_call_thread_ended(
    deps: &Deps<'_>,
    peer_a: BareJid,
    peer_b: BareJid,
    thread_id: String,
    ended: chrono::DateTime<chrono::Utc>,
    duration: CallThreadDuration,
) {
    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            peer_a = %peer_a,
            peer_b = %peer_b,
            "MarkDirectCallThreadEnded: no inbox_storage in Deps; skipping"
        );
        return;
    };

    let Some(thread_id) = ThreadId::new(thread_id) else {
        warn!("MarkDirectCallThreadEnded: invalid thread id");
        return;
    };
    for (owner, partner) in [(peer_a.clone(), peer_b.clone()), (peer_b, peer_a)] {
        if let Err(error) = inbox_storage
            .mark_direct_call_thread_ended(&owner, &partner, thread_id.as_str(), ended, &duration)
            .await
        {
            warn!(
                owner = %owner,
                partner = %partner,
                %error,
                "MarkDirectCallThreadEnded: inbox update failed"
            );
        } else {
            deps.capture_intent(IngressEffectIntent::InboxProject {
                owner: owner.clone(),
                mutation: InboxProjectionMutation::DirectCallThreadEnded {
                    peer: partner,
                    thread_id: thread_id.clone(),
                    ended,
                    duration: duration.clone(),
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ingress_shadow::IngressEffectCapture;
    use waddle_xmpp::{
        inbox::storage::{InMemoryInboxStorage, InboxStorage},
        mam::{storage::InMemoryMamStorage, MamStorage},
        registry::ConnectionRegistry,
        xep::CallThreadDuration,
    };

    #[tokio::test]
    async fn records_committed_direct_call_thread_inbox_mutations() {
        let registry = ConnectionRegistry::new();
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let capture = IngressEffectCapture::new(None);
        let deps = Deps::test_with_storage(&registry, &mam, &inbox)
            .with_ingress_effect_capture(Some(capture.clone()));
        let owner: BareJid = "alice@example.com".parse().expect("owner");
        let peer: BareJid = "bob@example.com".parse().expect("peer");

        project_direct_call_thread_anchor(
            &deps,
            owner.clone(),
            peer.clone(),
            "call-thread-1".to_owned(),
            "archive-1".to_owned(),
            CallThreadMedia::audio_video(),
            1_752_768_000,
        )
        .await;
        mark_direct_call_thread_ended(
            &deps,
            owner.clone(),
            peer.clone(),
            "call-thread-1".to_owned(),
            chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                .expect("timestamp")
                .with_timezone(&chrono::Utc),
            CallThreadDuration::parse("PT1M").expect("duration"),
        )
        .await;

        let intents = capture.snapshot().intents;
        assert!(intents.iter().any(|intent| matches!(
            intent,
            IngressEffectIntent::InboxProject {
                owner: intent_owner,
                mutation: InboxProjectionMutation::DirectCallThreadAnchor {
                    peer: intent_peer,
                    thread_id,
                    archive_stanza_id,
                    media,
                    last_updated,
                },
            } if intent_owner == &owner
                && intent_peer == &peer
                && thread_id.as_str() == "call-thread-1"
                && archive_stanza_id.id == "archive-1"
                && *media == CallThreadMedia::audio_video()
                && *last_updated == 1_752_768_000
        )));
        assert_eq!(
            intents
                .iter()
                .filter(|intent| matches!(
                    intent,
                    IngressEffectIntent::InboxProject {
                        mutation: InboxProjectionMutation::DirectCallThreadEnded { .. },
                        ..
                    }
                ))
                .count(),
            2,
            "each successful owner update is captured separately",
        );
    }
}
