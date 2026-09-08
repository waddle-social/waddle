use super::*;
use waddle_xmpp_core::xep0359::StanzaId as ArchiveStanzaId;

pub(super) async fn project_direct_inbox(
    deps: &Deps<'_>,
    owner: BareJid,
    peer: BareJid,
    message: Box<Message>,
    archive_ref: ArchiveStanzaId,
    increment_unread: bool,
) {
    let Some(_) = deps.inbox_storage else {
        debug!(
            owner = %owner,
            peer = %peer,
            "ProjectInbox: no inbox_storage in Deps; skipping (test fixture?)"
        );
        return;
    };
    // Build the inbox entry from the typed message, then
    // overwrite its stanza-id with the typed `archive_ref`
    // so the inbox row links to the canonicalized MAM
    // entry the handler stamped (rather than re-deriving
    // from the wire `<message id=...>`).
    let timestamp = chrono::Utc::now().timestamp();
    let mut entry = direct_message_entry(peer.clone(), &message, timestamp);
    entry.last_stanza_id = archive_ref.as_str().to_string();
    let planned = super::effects::direct::planned_durable(
        super::effects::direct::DurableDirectEffect::ProjectInbox {
            owner: owner.clone(),
            entry: Box::new(entry.clone()),
            increment_unread,
        },
    );
    let result = match deps.effects.execute(planned, deps).await {
        super::effects::EffectOutcome::Inbox(result) => result,
        super::effects::EffectOutcome::PlannedInbox(_) => Ok(entry),
        _ => return,
    };
    match result {
        Ok(entry) => {
            deps.capture_intent(IngressEffectIntent::InboxProject {
                owner: owner.clone(),
                mutation: waddle_xmpp::ingress::InboxProjectionMutation::Direct {
                    entry,
                    increment_unread,
                },
            });
            debug!(
                owner = %owner,
                peer = %peer,
                archive_ref = archive_ref.as_str(),
                increment_unread,
                "ProjectInbox: persisted"
            );
        }
        Err(error) => {
            warn!(
                owner = %owner,
                peer = %peer,
                %error,
                "ProjectInbox: inbox upsert failed; dropping projection"
            );
        }
    }
}
