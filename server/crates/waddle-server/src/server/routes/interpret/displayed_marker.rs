//! Interpreter arm for
//! [`waddle_xmpp::protocol::OutboundEvent::MarkInboxReadFromMarker`].
//!
//! Bridges XEP-0333 chat markers (`displayed`, `acknowledged`) to the inbox
//! read position (XEP-0430 §"Unread Messages"): the marked message and every
//! message before it is read, never later ones.
//!
//! The marked message is resolved in the archive that holds the
//! conversation: by MUC-assigned id in the room archive, or by the peer's
//! origin-id / message id in the sender's personal archive. A marker whose
//! message is not in the conversation is ignored, and so is one that is not
//! ahead of the stored read position (XEP-0333 §"Business Rules": markers
//! only move forward, and a marker for a message not found MUST be ignored).
//!
//! For an accepted marker the unread count of each affected row becomes the
//! number of incoming archived messages after the marked one:
//!
//! - the channel-level inbox row `(owner, conversation, thread_id='')`
//! - the thread-level row `(owner, room, thread_id=<message thread>)` when the
//!   marked room message belongs to a thread.
//!
//! Both rows are pushed through [`push_inbox_update_with`] so the user's
//! other resources fan out the read-state change without waiting for a fresh
//! `urn:xmpp:inbox:1` / `urn:waddle:threads:0` query.
//!
//! XEP-0490 MDS coexistence: MDS keys PEP items by chat JID (the room),
//! which makes thread-keyed mark-read impossible to express through MDS
//! alone. This arm covers the gap by deriving the thread from the marked
//! message id via MAM.

use jid::BareJid;
use tracing::{debug, warn};
use waddle_xmpp::inbox::storage::InboxStorage;
use waddle_xmpp::inbox::{InboxEntry, ReadThrough};
use waddle_xmpp::mam::storage::MamStorageError;
use waddle_xmpp::mam::{ArchiveOrdinal, ArchivedMessage, MamArchiveKind, MamQuery, MamStorage};
use waddle_xmpp::protocol::MarkerConversation;
use waddle_xmpp_core::mam::ThreadId;
use waddle_xmpp_core::xep0359::OriginId;

use super::groupchat_archive::push_inbox_update_with;
use super::Deps;

/// Archive page size when counting the messages after a marked one.
const COUNT_PAGE_SIZE: u32 = 500;

/// A marked message resolved in the conversation archive.
struct MarkedMessage {
    /// Archive position of the message.
    ordinal: ArchiveOrdinal,
    /// Archive id, the cursor for counting later messages.
    archive_id: String,
    /// Waddle thread of a room message, when it belongs to one.
    thread: Option<ThreadId>,
}

/// Drive [`OutboundEvent::MarkInboxReadFromMarker`] effects against MAM (to
/// resolve the marked message and count the messages after it) and the inbox
/// storage (for the read-position writes + push fan-out).
pub(super) async fn mark_inbox_read_from_marker(
    deps: &Deps<'_>,
    owner: BareJid,
    conversation: MarkerConversation,
    marked_message_id: String,
) {
    // A marker of an account at another server reports read state its own
    // server keeps: there is no inbox row here to mark.
    if !deps.serves(&owner) {
        return;
    }
    let marked = match resolve_marked_message(deps, &owner, &conversation, &marked_message_id).await
    {
        Ok(Some(marked)) => marked,
        Ok(None) => {
            debug!(%owner, ?conversation, marked_id = %marked_message_id,
                "MarkInboxReadFromMarker: marked message not in the conversation; ignoring");
            return;
        }
        Err(error) => {
            warn!(%owner, ?conversation, marked_id = %marked_message_id, %error,
                "MarkInboxReadFromMarker: MAM lookup failed");
            deps.effects
                .fail_plan(super::effects::PlanFailure::RichTargetLookup);
            return;
        }
    };
    // Notification activity ingest (slice 2b): a room read marker is a
    // strong "currently engaged" signal for the owner in the named room.
    // Record activity BEFORE the inbox mark-read so an inbox-storage outage
    // cannot mask the typed activity signal — the read marker still
    // happened on the wire.
    if let MarkerConversation::Room(room) = &conversation {
        super::notification_activity_ingest::record_read_marker_activity(deps, &owner, room).await;
    }

    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            %owner,
            ?conversation,
            marked_id = %marked_message_id,
            "MarkInboxReadFromMarker: no inbox_storage in Deps; skipping"
        );
        return;
    };
    apply_mark_read(
        deps,
        inbox_storage.as_ref(),
        &owner,
        &conversation,
        &marked,
        None,
    )
    .await;
    if let Some(ref thread_id) = marked.thread {
        apply_mark_read(
            deps,
            inbox_storage.as_ref(),
            &owner,
            &conversation,
            &marked,
            Some(thread_id),
        )
        .await;
    }
}

/// The partner JID of the inbox row a conversation's marker updates.
fn partner(conversation: &MarkerConversation) -> &BareJid {
    match conversation {
        MarkerConversation::Room(room) | MarkerConversation::Direct(room) => room,
    }
}

/// Resolve the marked message in the conversation archive. Returns
/// `Ok(None)` when the message is not in the conversation (including MAM not
/// being wired, as in unit-test fixtures); lookup errors refuse the plan.
async fn resolve_marked_message(
    deps: &Deps<'_>,
    owner: &BareJid,
    conversation: &MarkerConversation,
    marked_message_id: &str,
) -> Result<Option<MarkedMessage>, MamStorageError> {
    let Some(mam_storage) = deps.mam_storage else {
        return Ok(None);
    };
    let archived = match conversation {
        // XEP-0333 §"Group Chats": the MUC-assigned id, which is the
        // archive id the room stamps as its stanza-id.
        MarkerConversation::Room(room) => {
            mam_storage
                .get_message_by_archive_or_stanza_id(room, marked_message_id)
                .await?
        }
        // XEP-0333 §"Sending Displayed Markers": the id of the peer's
        // message, which is its origin-id when the client sent one. Only a
        // message the peer authored can be marked.
        MarkerConversation::Direct(peer) => {
            mam_storage
                .get_message_by_sender_and_origin_id(
                    owner,
                    MamArchiveKind::Personal,
                    &jid::Jid::from(peer.clone()),
                    &OriginId::new(marked_message_id),
                )
                .await?
        }
    };
    Ok(archived.and_then(|archived| {
        let ordinal = archived.ordinal?;
        Some(MarkedMessage {
            ordinal,
            archive_id: archived.id,
            thread: match conversation {
                MarkerConversation::Room(_) => archived.thread.map(|thread| thread.id),
                MarkerConversation::Direct(_) => None,
            },
        })
    }))
}

/// Whether the owner authored an archived message, so it never counts as
/// unread for them. Room rows are archived under the occupant JID; the
/// room-stamped real JID identifies the author.
fn authored_by(message: &ArchivedMessage, owner: &BareJid) -> bool {
    let author = message
        .rich
        .as_ref()
        .and_then(|rich| rich.muc_sender.as_ref())
        .map_or(&message.from, |sender| &sender.jid);
    author.to_bare() == *owner
}

/// Count the incoming messages archived after the marked one for one inbox
/// row (`thread` narrows a room row to its thread).
async fn count_incoming_after(
    mam_storage: &dyn MamStorage,
    owner: &BareJid,
    conversation: &MarkerConversation,
    marked: &MarkedMessage,
    thread: Option<&ThreadId>,
) -> Result<u32, MamStorageError> {
    let (archive, kind, with) = match conversation {
        MarkerConversation::Room(room) => (room, MamArchiveKind::Room, None),
        MarkerConversation::Direct(peer) => (
            owner,
            MamArchiveKind::Personal,
            Some(jid::Jid::from(peer.clone())),
        ),
    };
    let mut cursor = marked.archive_id.clone();
    let mut count = 0u32;
    loop {
        let query = MamQuery {
            max: Some(COUNT_PAGE_SIZE),
            after_id: Some(cursor),
            with: with.clone(),
            thread_id: thread.cloned(),
            ..MamQuery::default()
        };
        let page = mam_storage.query_messages(archive, kind, &query).await?;
        let incoming = page
            .messages
            .iter()
            .filter(|message| !authored_by(message, owner))
            .count();
        count = count.saturating_add(u32::try_from(incoming).unwrap_or(u32::MAX));
        match page.messages.last() {
            Some(last) if page.messages.len() >= COUNT_PAGE_SIZE as usize => {
                cursor = last.id.clone();
            }
            _ => return Ok(count),
        }
    }
}

/// Snapshot the inbox row a marker would update, `None` when the owner has
/// no such row.
async fn read_row(
    inbox_storage: &dyn InboxStorage,
    owner: &BareJid,
    conversation: &MarkerConversation,
    thread: Option<&ThreadId>,
) -> Result<Option<InboxEntry>, waddle_xmpp::inbox::storage::InboxStorageError> {
    let partner = partner(conversation);
    let entries = match thread {
        Some(_) => inbox_storage.list_threads(owner, partner).await?,
        None => inbox_storage.list(owner).await?,
    };
    Ok(entries.into_iter().find(|entry| {
        entry.partner == *partner && entry.thread_id.as_deref() == thread.map(ThreadId::as_str)
    }))
}

/// Decide the read cursor for one `(owner, partner, thread_id)` row: `None`
/// when the marker must be ignored (no row, or not ahead of the stored read
/// position), otherwise the unread messages the marker reads.
///
/// Read failures mark a planning sink as incomplete.
async fn plan_read_through(
    deps: &Deps<'_>,
    inbox_storage: &dyn InboxStorage,
    owner: &BareJid,
    conversation: &MarkerConversation,
    marked: &MarkedMessage,
    thread: Option<&ThreadId>,
) -> Option<ReadThrough> {
    let partner = partner(conversation);
    // RFC 0018 §2: snapshot reads must succeed before the plan can commit.
    let snapshot = async {
        let entry = read_row(inbox_storage, owner, conversation, thread).await?;
        let position = inbox_storage
            .read_position(owner, partner, thread.map(ThreadId::as_str))
            .await?;
        Ok::<_, waddle_xmpp::inbox::storage::InboxStorageError>((entry, position))
    }
    .await;
    let (entry, position) = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            warn!(%owner, %partner, %error, "chat-marker inbox read failed");
            deps.effects
                .fail_plan(super::effects::PlanFailure::InboxSnapshotRead);
            return None;
        }
    };
    let Some(entry) = entry else {
        debug!(%owner, %partner, thread = thread.map_or("", ThreadId::as_str),
            "MarkInboxReadFromMarker: no matching inbox row; no-op");
        return None;
    };
    if position.is_some_and(|stored| stored >= marked.ordinal) {
        debug!(%owner, %partner, thread = thread.map_or("", ThreadId::as_str),
            "MarkInboxReadFromMarker: marker is not ahead of the read position; ignoring");
        return None;
    }
    let mam_storage = deps.mam_storage?;
    let unread_after =
        match count_incoming_after(mam_storage.as_ref(), owner, conversation, marked, thread).await
        {
            Ok(count) => count,
            Err(error) => {
                warn!(%owner, %partner, %error, "chat-marker unread count failed");
                deps.effects
                    .fail_plan(super::effects::PlanFailure::RichTargetLookup);
                return None;
            }
        };
    Some(ReadThrough {
        ordinal: marked.ordinal,
        unread_cleared: entry.unread.saturating_sub(unread_after.min(entry.unread)),
    })
}

fn read_mutation(
    conversation: &MarkerConversation,
    thread: Option<&ThreadId>,
) -> waddle_xmpp::ingress::InboxProjectionMutation {
    use waddle_xmpp::ingress::InboxProjectionMutation;
    match (conversation, thread) {
        (MarkerConversation::Room(room), Some(thread_id)) => {
            InboxProjectionMutation::GroupchatThreadRead {
                room: room.clone(),
                thread_id: thread_id.clone(),
            }
        }
        (MarkerConversation::Room(room), None) => {
            InboxProjectionMutation::GroupchatChannelRead { room: room.clone() }
        }
        (MarkerConversation::Direct(peer), _) => {
            InboxProjectionMutation::DirectRead { peer: peer.clone() }
        }
    }
}

/// Run the inbox read-position write for one `(owner, partner, thread_id)`
/// key and push the post-update entry to the owner's other resources.
async fn apply_mark_read(
    deps: &Deps<'_>,
    inbox_storage: &dyn InboxStorage,
    owner: &BareJid,
    conversation: &MarkerConversation,
    marked: &MarkedMessage,
    thread: Option<&ThreadId>,
) {
    let Some(through) =
        plan_read_through(deps, inbox_storage, owner, conversation, marked, thread).await
    else {
        return;
    };
    if deps.effects.is_planning() {
        plan_mark_read(deps, owner, conversation, thread, through).await;
        return;
    }
    let partner = partner(conversation);
    match inbox_storage
        .mark_read_through(owner, partner, thread.map(ThreadId::as_str), through)
        .await
    {
        Ok(Some(entry)) => {
            let push_recipients = push_inbox_update_with(deps, owner, &entry).await;
            capture_displayed_marker_pushes(deps, owner, push_recipients);
            deps.capture_intent(waddle_xmpp::ingress::IngressEffectIntent::InboxProject {
                owner: owner.clone(),
                mutation: read_mutation(conversation, thread),
            });
        }
        Ok(None) => {
            debug!(
                %owner,
                %partner,
                thread = thread.map_or("", ThreadId::as_str),
                "MarkInboxReadFromMarker: no matching inbox row or stale marker; no-op"
            );
        }
        Err(error) => {
            warn!(
                %owner,
                %partner,
                thread = thread.map_or("", ThreadId::as_str),
                %error,
                "MarkInboxReadFromMarker: mark_read_through failed"
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
        prepared: None,
        recipient: owner.clone(),
        fanout,
        route_identity: capture.next_route_identity(),
    });
}

async fn plan_mark_read(
    deps: &Deps<'_>,
    owner: &BareJid,
    conversation: &MarkerConversation,
    thread: Option<&ThreadId>,
    through: ReadThrough,
) {
    use super::effects::direct::{
        external, planned_durable, DurableDirectEffect, ExternalDirectEffect,
    };
    let channel = partner(conversation).clone();
    let super::effects::EffectOutcome::PlannedInbox(projection) = deps
        .effects
        .execute(
            planned_durable(DurableDirectEffect::MarkInboxRead {
                owner: owner.clone(),
                channel,
                thread: thread.cloned(),
                through,
            }),
            deps,
        )
        .await
    else {
        return;
    };
    deps.capture_intent(waddle_xmpp::ingress::IngressEffectIntent::InboxProject {
        owner: owner.clone(),
        mutation: read_mutation(conversation, thread),
    });
    external(
        deps,
        ExternalDirectEffect::PushInboxUpdate {
            owner: owner.clone(),
            projection,
            receipt: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use waddle_xmpp::inbox::storage::{InMemoryInboxStorage, InboxStorage};
    use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
    use waddle_xmpp::mam::storage::{InMemoryMamStorage, MamStorage};
    use waddle_xmpp::mam::ArchivedMessage as MamArchivedMessage;
    use waddle_xmpp::protocol::MarkerConversation;
    use waddle_xmpp::registry::ConnectionRegistry;
    use waddle_xmpp_core::mam::ThreadId;
    use waddle_xmpp_core::xep0359::OriginId;
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
            ThreadId::try_new(raw.to_string()).map(|id| ThreadInfo { id, parent: None })
        });
        let stanza_id = Some(Xep0359StanzaId::new(
            wire_id.to_string(),
            archive_jid.clone(),
        ));
        MamArchivedMessage {
            ordinal: None,
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

    fn room_marker(room: &BareJid) -> MarkerConversation {
        MarkerConversation::Room(room.clone())
    }

    /// An archived message from `sender` in `archive`, stored under `wire_id`.
    async fn archive_from(
        mam: &Arc<dyn MamStorage>,
        archive: &BareJid,
        sender: &str,
        wire_id: &str,
        thread: Option<&str>,
    ) {
        let mut archived = make_archive_with_thread(archive, wire_id, thread);
        archived.from = jid::Jid::from(bare(sender));
        mam.store_message(archive, &archived)
            .await
            .expect("archive");
    }

    /// Seed `count` unread increments on one inbox row.
    async fn seed_unread(
        storage: &Arc<dyn InboxStorage>,
        owner: &BareJid,
        partner: &BareJid,
        kind: ConversationKind,
        thread: Option<&str>,
        count: usize,
    ) {
        for _ in 0..count {
            let mut entry = InboxEntry::new(partner.clone(), kind, "sid", 0);
            if let Some(thread_id) = thread {
                entry = entry.with_thread(thread_id);
            }
            storage.upsert(owner, entry, true).await.expect("seed");
        }
    }

    async fn unread(storage: &Arc<dyn InboxStorage>, owner: &BareJid) -> u32 {
        storage.list(owner).await.expect("list")[0].unread
    }

    #[tokio::test]
    async fn marker_reads_only_through_the_marked_message() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        for index in 1..=5 {
            archive_from(&mam, &room, "bob@example.com", &format!("m{index}"), None).await;
        }
        seed_unread(&inbox, &owner, &room, ConversationKind::MucRoom, None, 5).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m2".into()).await;

        assert_eq!(unread(&inbox, &owner).await, 3, "m3..m5 stay unread");
    }

    #[tokio::test]
    async fn own_messages_after_the_marked_one_are_not_counted_unread() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        archive_from(&mam, &room, "bob@example.com", "m1", None).await;
        archive_from(&mam, &room, "bob@example.com", "m2", None).await;
        archive_from(&mam, &room, "alice@example.com", "own", None).await;
        archive_from(&mam, &room, "bob@example.com", "m4", None).await;
        seed_unread(&inbox, &owner, &room, ConversationKind::MucRoom, None, 3).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m2".into()).await;

        assert_eq!(
            unread(&inbox, &owner).await,
            1,
            "only m4 is after the marker"
        );
    }

    #[tokio::test]
    async fn marker_for_a_message_not_in_the_chat_is_ignored() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        archive_from(&mam, &room, "bob@example.com", "m1", Some("t-1")).await;
        seed_unread(&inbox, &owner, &room, ConversationKind::MucRoom, None, 1).await;
        seed_unread(
            &inbox,
            &owner,
            &room,
            ConversationKind::MucRoom,
            Some("t-1"),
            1,
        )
        .await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_marker(
            &deps,
            owner.clone(),
            room_marker(&room),
            "missing-id".into(),
        )
        .await;

        assert_eq!(unread(&inbox, &owner).await, 1, "channel row untouched");
        let threads = inbox.list_threads(&owner, &room).await.expect("list");
        assert_eq!(threads[0].unread, 1, "thread row untouched");
        assert_eq!(
            inbox
                .read_position(&owner, &room, None)
                .await
                .expect("read"),
            None,
            "no read position is recorded for an unknown id"
        );
    }

    #[tokio::test]
    async fn stale_marker_is_ignored_and_markers_only_move_forward() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        for index in 1..=4 {
            archive_from(&mam, &room, "bob@example.com", &format!("m{index}"), None).await;
        }
        seed_unread(&inbox, &owner, &room, ConversationKind::MucRoom, None, 4).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m3".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 1);
        // A delayed marker for an older message changes nothing, and neither
        // does repeating the newer one.
        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m1".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 1);
        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m3".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 1);
        // A later marker still moves forward.
        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "m4".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 0);
    }

    #[tokio::test]
    async fn threaded_marker_moves_the_thread_and_channel_rows_through_the_message() {
        let owner = bare("alice@example.com");
        let room = bare("team@conf.example.com");
        let thread = "t-roadmap";
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        archive_from(&mam, &room, "bob@example.com", "a1", Some(thread)).await;
        archive_from(&mam, &room, "bob@example.com", "plain", None).await;
        archive_from(&mam, &room, "bob@example.com", "a2", Some(thread)).await;
        seed_unread(&inbox, &owner, &room, ConversationKind::MucRoom, None, 3).await;
        seed_unread(
            &inbox,
            &owner,
            &room,
            ConversationKind::MucRoom,
            Some(thread),
            2,
        )
        .await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);

        mark_inbox_read_from_marker(&deps, owner.clone(), room_marker(&room), "a1".into()).await;

        assert_eq!(unread(&inbox, &owner).await, 2, "plain and a2 stay unread");
        let threads = inbox.list_threads(&owner, &room).await.expect("threads");
        assert_eq!(threads[0].unread, 1, "a2 stays unread in the thread");
    }

    #[tokio::test]
    async fn direct_marker_resolves_the_peers_origin_id_and_reads_through_it() {
        let owner = bare("alice@example.com");
        let peer = bare("bob@example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        for (wire_id, origin) in [("w1", "o1"), ("w2", "o2"), ("w3", "o3")] {
            let mut archived = make_archive_with_thread(&owner, wire_id, None);
            archived.from = jid::Jid::from(peer.clone());
            archived.to = jid::Jid::from(owner.clone());
            archived.message_type = MessageType::Chat;
            archived.origin_id = Some(OriginId::new(origin));
            mam.store_message(&owner, &archived).await.expect("archive");
        }
        seed_unread(&inbox, &owner, &peer, ConversationKind::Direct, None, 3).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);
        let direct = MarkerConversation::Direct(peer.clone());

        mark_inbox_read_from_marker(&deps, owner.clone(), direct.clone(), "o2".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 1, "origin id resolves");

        // The wire message id resolves too, but not forward past the cursor.
        mark_inbox_read_from_marker(&deps, owner.clone(), direct.clone(), "w1".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 1, "older marker ignored");
        mark_inbox_read_from_marker(&deps, owner.clone(), direct, "w3".into()).await;
        assert_eq!(unread(&inbox, &owner).await, 0, "wire id resolves");
    }

    #[tokio::test]
    async fn direct_marker_for_an_unknown_id_or_own_message_is_ignored() {
        let owner = bare("alice@example.com");
        let peer = bare("bob@example.com");
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let mut own = make_archive_with_thread(&owner, "mine", None);
        own.from = jid::Jid::from(owner.clone());
        own.message_type = MessageType::Chat;
        mam.store_message(&owner, &own).await.expect("archive");
        seed_unread(&inbox, &owner, &peer, ConversationKind::Direct, None, 2).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);
        let direct = MarkerConversation::Direct(peer);

        mark_inbox_read_from_marker(&deps, owner.clone(), direct.clone(), "nope".into()).await;
        mark_inbox_read_from_marker(&deps, owner.clone(), direct, "mine".into()).await;

        assert_eq!(unread(&inbox, &owner).await, 2);
    }

    async fn snapshot_read_failure_prevents_ingress_commit(
        fixture: crate::ingress::test_support::IngressFixture,
    ) {
        use crate::ingress::commit::commit_submission;
        use crate::ingress::{IngressDecisionClass, IngressEffectCapture};
        use crate::server::routes::interpret::effects::{PlanFailure, PlanSink};
        use crate::server::routes::interpret::message_plan::finish_plan;
        use crate::server::routes::interpret::{build_plan_deps, interpret};
        let owner = fixture.principal.bare_jid().clone();
        let room = bare("team@conf.example.com");
        let inbox: Arc<dyn InboxStorage> = Arc::new(
            crate::inbox::DatabaseInboxStorage::open(Some(fixture.db.database_url()))
                .await
                .expect("inbox storage"),
        );
        seed_inbox(&inbox, &owner, &room, None).await;
        let mam: Arc<dyn MamStorage> = Arc::new(InMemoryMamStorage::new());
        archive_from(&mam, &room, "bob@example.com", "displayed-message", None).await;
        let registry = ConnectionRegistry::new();
        let deps = deps_for_test(&registry, &mam, &inbox);
        let sink = PlanSink::new();
        let capture = IngressEffectCapture::new();
        let planned =
            build_plan_deps(&deps, &sink).with_ingress_effect_capture(Some(capture.clone()));
        let mut submission = fixture.submission(None, "displayed marker");
        // A real database read fails during Phase A and recovers before Phase B.
        fixture
            .execute(
                "ALTER TABLE inbox_entries RENAME TO unavailable_inbox_entries",
                (),
            )
            .await;
        interpret(
            vec![
                waddle_xmpp::protocol::OutboundEvent::MarkInboxReadFromMarker {
                    owner: owner.clone(),
                    conversation: room_marker(&room),
                    marked_message_id: "displayed-message".to_owned(),
                },
            ],
            &planned,
        )
        .await;
        submission.plan = finish_plan(
            &sink,
            &capture,
            submission.plan.sanitized_message.clone(),
            Some(submission.sender.clone()),
        );
        assert_eq!(
            submission.plan.failure,
            Some(PlanFailure::InboxSnapshotRead)
        );
        fixture
            .execute(
                "ALTER TABLE unavailable_inbox_entries RENAME TO inbox_entries",
                (),
            )
            .await;
        let failure = commit_submission(&fixture.uow, &submission, 3)
            .await
            .expect_err("incomplete plan cannot commit after storage recovery");
        assert_eq!(failure.class(), IngressDecisionClass::Storage);
        assert!(!failure.class().advances());
        assert!(matches!(
            failure.source,
            crate::ingress_uow::IngressUowError::Plan(PlanFailure::InboxSnapshotRead)
        ));
        assert_eq!(fixture.count("ingress_messages").await, 0);
        assert_eq!(fixture.count("ingress_effect_intents").await, 0);
        assert_eq!(
            inbox.list(&owner).await.expect("recovered inbox")[0].unread,
            1
        );
        drop(inbox);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_displayed_marker_snapshot_failure_is_nonadvancing_storage() {
        snapshot_read_failure_prevents_ingress_commit(
            crate::ingress::test_support::IngressFixture::sqlite().await,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_displayed_marker_snapshot_failure_is_nonadvancing_storage() {
        let Some(fixture) = crate::ingress::test_support::IngressFixture::postgres(
            "displayed_marker_snapshot_failure",
        )
        .await
        else {
            return;
        };
        snapshot_read_failure_prevents_ingress_commit(fixture).await;
    }
}

#[cfg(test)]
#[path = "displayed_marker_plan_tests.rs"]
mod plan_tests;
