use super::Deps;
use jid::BareJid;
use tracing::{debug, warn};
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};

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

    let entry = InboxEntry::new(
        peer.clone(),
        ConversationKind::Direct,
        stanza_id,
        last_updated,
    )
    .with_thread(thread_id)
    .with_call_thread(CallThreadKind::Dm, media);
    if let Err(error) = inbox_storage.upsert(&owner, entry, false).await {
        warn!(
            owner = %owner,
            peer = %peer,
            %error,
            "ProjectDirectCallThreadAnchor: inbox upsert failed"
        );
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

    for (owner, partner) in [(peer_a.clone(), peer_b.clone()), (peer_b, peer_a)] {
        if let Err(error) = inbox_storage
            .mark_direct_call_thread_ended(&owner, &partner, &thread_id, ended, &duration)
            .await
        {
            warn!(
                owner = %owner,
                partner = %partner,
                %error,
                "MarkDirectCallThreadEnded: inbox update failed"
            );
        }
    }
}
