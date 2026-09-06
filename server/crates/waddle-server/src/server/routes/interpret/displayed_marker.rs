//! Interpreter arm for
//! [`waddle_xmpp::protocol::OutboundEvent::MarkInboxReadFromDisplayed`].
//!
//! Bridges XEP-0333 displayed markers to the Waddle inbox: when the
//! sender has displayed a groupchat message, we look the message up in
//! MAM (keyed by `(room, wire_message_id)`), read its `<thread/>`
//! payload, and clear `unread` on:
//!
//! - the channel-level inbox row `(owner, room, thread_id='')`
//! - the thread-level row `(owner, room, thread_id=<message thread>)`
//!   when the displayed message belongs to a thread.
//!
//! Both rows are pushed through [`push_inbox_update`] so the user's
//! other resources fan out the read-state flip without waiting for a
//! fresh `urn:waddle:inbox:0` / `urn:waddle:threads:0` query (XEP-0430
//! §"Mark as read" cross-device sync semantic).
//!
//! XEP-0490 MDS coexistence: MDS keys PEP items by chat JID (the room),
//! which makes thread-keyed mark-read impossible to express through MDS
//! alone. This arm covers the gap by deriving the thread from the
//! displayed message id via MAM.

use jid::BareJid;
use tracing::{debug, warn};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp_core::mam::ThreadId;

use super::groupchat_archive::push_inbox_update;
use super::Deps;

/// Drive [`OutboundEvent::MarkInboxReadFromDisplayed`] effects against
/// MAM (for the thread lookup) and the inbox storage (for the
/// mark-read writes + push fan-out).
pub(super) async fn mark_inbox_read_from_displayed(
    deps: &Deps<'_>,
    owner: BareJid,
    room: BareJid,
    displayed_message_id: String,
) {
    // Notification activity ingest (slice 2b): a XEP-0490 displayed
    // marker advance is a strong "currently engaged" signal for the
    // owner in the named room. Record activity BEFORE the inbox
    // mark-read so an inbox-storage outage cannot mask the typed
    // activity signal — the read marker still happened on the wire.
    super::notification_activity_ingest::record_read_marker_activity(deps, &owner, &room).await;

    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            %owner,
            %room,
            displayed_id = %displayed_message_id,
            "MarkInboxReadFromDisplayed: no inbox_storage in Deps; skipping"
        );
        return;
    };
    let thread_id = resolve_thread_id(deps, &room, &displayed_message_id).await;

    apply_mark_read(deps, inbox_storage.as_ref(), &owner, &room, None).await;
    if let Some(ref thread_id) = thread_id {
        apply_mark_read(deps, inbox_storage.as_ref(), &owner, &room, Some(thread_id)).await;
    }
}

/// Look up the displayed message's `<thread/>` id from MAM. Returns
/// `None` when the message was not found, has no thread, or MAM is not
/// wired (unit-test fixtures). The mark-read still applies to the
/// channel-level row in all cases.
async fn resolve_thread_id(
    deps: &Deps<'_>,
    room: &BareJid,
    displayed_message_id: &str,
) -> Option<ThreadId> {
    let mam_storage = deps.mam_storage?;
    match mam_storage
        .get_message_by_message_id(room, displayed_message_id)
        .await
    {
        Ok(Some(archived)) => archived.thread.map(|thread| thread.id),
        Ok(None) => {
            debug!(
                %room,
                displayed_id = %displayed_message_id,
                "MarkInboxReadFromDisplayed: target message not in MAM; \
                 marking only the channel row read"
            );
            None
        }
        Err(error) => {
            warn!(
                %room,
                displayed_id = %displayed_message_id,
                %error,
                "MarkInboxReadFromDisplayed: MAM lookup failed; \
                 falling back to channel-row mark-read"
            );
            None
        }
    }
}

/// Run the inbox `mark_read` write for one `(owner, room, thread_id)`
/// key and push the post-update entry to the owner's other resources.
async fn apply_mark_read(
    deps: &Deps<'_>,
    inbox_storage: &dyn InboxStorage,
    owner: &BareJid,
    room: &BareJid,
    thread_id: Option<&ThreadId>,
) {
    if deps.effects.is_planning() {
        plan_mark_read(deps, inbox_storage, owner, room, thread_id).await;
        return;
    }
    match inbox_storage
        .mark_read(owner, room, thread_id.map(ThreadId::as_str))
        .await
    {
        Ok(Some(entry)) => {
            let push_recipients =
                push_inbox_update(deps.connection_registry, deps.user_registry, owner, &entry)
                    .await;
            capture_displayed_marker_pushes(deps, owner, push_recipients);
            let mutation = match thread_id {
                Some(thread_id) => {
                    waddle_xmpp::ingress::InboxProjectionMutation::GroupchatThreadRead {
                        room: room.clone(),
                        thread_id: thread_id.clone(),
                    }
                }
                None => waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelRead {
                    room: room.clone(),
                },
            };
            deps.capture_intent(waddle_xmpp::ingress::IngressEffectIntent::InboxProject {
                owner: owner.clone(),
                mutation,
            });
        }
        Ok(None) => {
            debug!(
                %owner,
                %room,
                thread = thread_id.map_or("", ThreadId::as_str),
                "MarkInboxReadFromDisplayed: no matching inbox row; no-op"
            );
        }
        Err(error) => {
            warn!(
                %owner,
                %room,
                thread = thread_id.map_or("", ThreadId::as_str),
                %error,
                "MarkInboxReadFromDisplayed: mark_read failed"
            );
        }
    }
}

fn capture_displayed_marker_pushes(deps: &Deps<'_>, owner: &BareJid, fanout: Vec<jid::FullJid>) {
    let Some(capture) = deps.ingress_effect_capture.as_ref() else {
        return;
    };
    if fanout.is_empty() {
        return;
    }
    capture.record_intent(waddle_xmpp::ingress::IngressEffectIntent::RouteDirect {
        recipient: owner.clone(),
        fanout,
        route_identity: capture.next_route_identity(),
    });
}

async fn plan_mark_read(
    deps: &Deps<'_>,
    storage: &dyn InboxStorage,
    owner: &BareJid,
    room: &BareJid,
    thread: Option<&ThreadId>,
) {
    use super::effects::direct::{durable, external, DurableDirectEffect, ExternalDirectEffect};
    durable(
        deps,
        DurableDirectEffect::MarkInboxRead {
            owner: owner.clone(),
            channel: room.clone(),
            thread: thread.cloned(),
        },
    );
    let mutation = match thread {
        Some(thread_id) => waddle_xmpp::ingress::InboxProjectionMutation::GroupchatThreadRead {
            room: room.clone(),
            thread_id: thread_id.clone(),
        },
        None => waddle_xmpp::ingress::InboxProjectionMutation::GroupchatChannelRead {
            room: room.clone(),
        },
    };
    let entries = match thread {
        Some(_) => storage.list_threads(owner, room).await,
        None => storage.list(owner).await,
    };
    if let Ok(entries) = entries {
        for mut entry in entries {
            if entry.partner == *room && entry.thread_id.as_deref() == thread.map(ThreadId::as_str)
            {
                deps.capture_intent(waddle_xmpp::ingress::IngressEffectIntent::InboxProject {
                    owner: owner.clone(),
                    mutation: mutation.clone(),
                });
                entry.unread = 0;
                external(
                    deps,
                    ExternalDirectEffect::PushInboxUpdate {
                        owner: owner.clone(),
                        entry: Box::new(entry),
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
    use waddle_xmpp::mam::storage::{InMemoryMamStorage, MamStorage};
    use waddle_xmpp::mam::ArchivedMessage as MamArchivedMessage;
    use waddle_xmpp::registry::ConnectionRegistry;
    use waddle_xmpp_core::mam::ThreadId;
    use waddle_xmpp_core::xep0359::StanzaId as Xep0359StanzaId;
    use waddle_xmpp_core::ThreadInfo;
    use xmpp_parsers::message::MessageType;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare jid")
    }

    fn make_archive_with_thread(
        room: &BareJid,
        wire_id: &str,
        thread: Option<&str>,
    ) -> MamArchivedMessage {
        let archive_jid = jid::Jid::from(room.clone());
        let thread_info = thread.and_then(|raw| {
            ThreadId::new(raw.to_string()).map(|id| ThreadInfo { id, parent: None })
        });
        let stanza_id = Some(Xep0359StanzaId::new(
            wire_id.to_string(),
            archive_jid.clone(),
        ));
        MamArchivedMessage {
            id: wire_id.to_string(),
            timestamp: chrono::Utc::now(),
            from: jid::Jid::from(bare("alice@example.com")),
            to: archive_jid,
            body: Some("hi".into()),
            stanza_id,
            thread: thread_info,
            reply: None,
            origin_id: None,
            message_type: MessageType::Groupchat,
            stanza_xml: None,
            rich: None,
            nickname_generation: Some(0),
        }
    }

    async fn seed_inbox(
        storage: &Arc<dyn InboxStorage>,
        owner: &BareJid,
        room: &BareJid,
        thread: Option<&str>,
    ) {
        let mut entry = InboxEntry::new(room.clone(), ConversationKind::MucRoom, "sid", 0);
        if let Some(thread_id) = thread {
            entry = entry.with_thread(thread_id);
        }
        storage
            .upsert(owner, entry, true)
            .await
            .expect("seed inbox row with unread=1");
    }

    fn deps_for_test<'a>(
        registry: &'a ConnectionRegistry,
        mam_storage: &'a Arc<dyn MamStorage>,
        inbox_storage: &'a Arc<dyn InboxStorage>,
    ) -> Deps<'a> {
        Deps::test_with_storage(registry, mam_storage, inbox_storage)
    }

    #[tokio::test]
    async fn mark_read_clears_channel_row_when_message_has_no_thread() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        seed_inbox(&inbox, &owner, &room, None).await;

        let archived = make_archive_with_thread(&room, "msg-1", None);
        mam.store_message(&room, &archived).await.expect("archive");

        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_displayed(&deps, owner.clone(), room.clone(), "msg-1".into()).await;

        let rows = inbox.list(&owner).await.expect("list inbox");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].unread, 0, "channel row unread must be cleared");
    }

    #[tokio::test]
    async fn mark_read_clears_thread_and_channel_rows_when_message_has_thread() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let thread = "t-roadmap";
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        seed_inbox(&inbox, &owner, &room, None).await;
        seed_inbox(&inbox, &owner, &room, Some(thread)).await;

        let archived = make_archive_with_thread(&room, "msg-7", Some(thread));
        mam.store_message(&room, &archived).await.expect("archive");

        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_displayed(&deps, owner.clone(), room.clone(), "msg-7".into()).await;

        let channels = inbox.list(&owner).await.expect("list channels");
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].unread, 0, "channel row must clear");
        let threads = inbox
            .list_threads(&owner, &room)
            .await
            .expect("list threads");
        assert_eq!(threads.len(), 1);
        assert_eq!(
            threads[0].unread, 0,
            "thread row must clear when the displayed message belongs to it"
        );
    }

    #[tokio::test]
    async fn mark_read_is_a_no_op_when_message_is_missing_from_mam() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        seed_inbox(&inbox, &owner, &room, None).await;
        seed_inbox(&inbox, &owner, &room, Some("t-1")).await;

        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        // MAM has no message → channel row still clears, thread row stays.
        mark_inbox_read_from_displayed(&deps, owner.clone(), room.clone(), "missing-id".into())
            .await;

        let channels = inbox.list(&owner).await.expect("list");
        assert_eq!(channels[0].unread, 0, "channel still clears");
        let threads = inbox.list_threads(&owner, &room).await.expect("list");
        assert_eq!(
            threads[0].unread, 1,
            "thread row stays unread when MAM lookup fails"
        );
    }
}
