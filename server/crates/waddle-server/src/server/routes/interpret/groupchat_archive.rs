use super::*;
use waddle_xmpp::mam::MamStorageError;
#[cfg(any(feature = "clustering", test))]
use waddle_xmpp::muc::RoomClaimFenceContext;

/// Outcome of a groupchat archive write attempt (ADR-0017 Phase 3 Slice 7
/// FIX 1, council-adjudicated). `OwnershipUncertain` means the fenced
/// backstop either disproved ownership or could not prove it. The caller
/// MUST neither treat the message as archived NOR fan it out to occupants.
pub(super) enum ArchiveGroupchatOutcome {
    Stored(ArchiveStoreResult),
    /// Not an error: a chain-bug guard or a non-fencing storage failure
    /// declined the write. The reflection still goes out (today's
    /// pre-existing behavior, unchanged).
    Skipped,
    /// Exact ownership could not be proved. This covers a definitive claim
    /// mismatch, a missing cached claim fence, and a backend failure while
    /// checking or applying a fenced write. The message was NOT archived;
    /// the caller must also suppress fan-out and other effects.
    OwnershipUncertain,
}

/// Resolution of the room-ownership fence for a groupchat archive write.
///
/// `Unfenced` is reserved for deployments where clustered MUC ownership is
/// not configured at all. Once a clustered durable room store exists, a
/// missing cached epoch is not equivalent to single-node operation: it is
/// an inability to prove ownership and must fail closed.
pub(super) enum RoomClaimFenceResolution {
    Unfenced,
    #[cfg(any(feature = "clustering", test))]
    Fenced(RoomClaimFenceContext),
    #[cfg(any(feature = "clustering", test))]
    OwnershipUncertain,
}

impl RoomClaimFenceResolution {
    pub(super) fn is_fenced(&self) -> bool {
        #[cfg(any(feature = "clustering", test))]
        {
            matches!(self, Self::Fenced(_))
        }
        #[cfg(not(any(feature = "clustering", test)))]
        {
            false
        }
    }

    pub(super) fn is_ownership_uncertain(&self) -> bool {
        #[cfg(any(feature = "clustering", test))]
        {
            matches!(self, Self::OwnershipUncertain)
        }
        #[cfg(not(any(feature = "clustering", test)))]
        {
            false
        }
    }
}

/// ADR-0017 Phase 3 Slice 7 FIX 1: resolve the typed `(Entity, ClaimEpoch,
/// NodeIdentity)` fencing context for `room`, the SAME mechanism
/// `dispatch_to_room`'s own `check_fenced_fanout` pre-fan-out check reads
/// from — threaded here rather than re-derived from a second, independent
/// source. `Unfenced` is returned only when clustering/durable MUC ownership
/// is not configured. A configured store with no cached exact claim returns
/// `OwnershipUncertain`; it must never degrade into an unfenced write.
pub(super) fn resolve_room_claim_fence(
    deps: &Deps<'_>,
    room: &BareJid,
) -> RoomClaimFenceResolution {
    #[cfg(feature = "clustering")]
    {
        let Some(_store) = deps.muc_durable_store else {
            return if deps.clustered_muc_ownership_required {
                RoomClaimFenceResolution::OwnershipUncertain
            } else {
                RoomClaimFenceResolution::Unfenced
            };
        };
        if let Some(fence) = deps.room_claim_fence.as_ref() {
            let expected = waddle_xmpp::ownership::Entity::new(
                waddle_xmpp::ownership::EntityType::RoomActor,
                room.to_string(),
            );
            return if fence.entity == expected {
                RoomClaimFenceResolution::Fenced(fence.clone())
            } else {
                RoomClaimFenceResolution::OwnershipUncertain
            };
        }
        // A configured durable MUC store means this archive belongs to a
        // claim-bound RoomActor incarnation. Never recover a missing actor
        // proof from the store's room-scoped "latest epoch" cache: after an
        // E1 actor is retained and E2 is acquired, that would revive the ABA.
        RoomClaimFenceResolution::OwnershipUncertain
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (deps, room);
        RoomClaimFenceResolution::Unfenced
    }
}

pub(super) async fn archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    message: &Message,
    sender_nickname_generation: u64,
    fence: &RoomClaimFenceResolution,
    sender_item: Option<&waddle_xmpp_core::mam::ArchivedMucSender>,
) -> ArchiveGroupchatOutcome {
    if fence.is_ownership_uncertain() {
        warn!(
            room = %room,
            "ArchiveGroupchat: clustered room has no provable claim fence; failing closed"
        );
        return ArchiveGroupchatOutcome::OwnershipUncertain;
    }

    // XEP-0313 §MUC Archives: for non-anonymous rooms the *archived*
    // copy carries a room-authored `<x xmlns='muc#user'><item
    // jid='real-jid' affiliation role/></x>` disclosing the sender's
    // real JID (#1268). The live reflection never carries it — this
    // append happens on the archive clone only, after
    // `MucCanonicalizeHandler` has already stripped any
    // client-supplied muc#user forgery (#1251).
    let mut archive_clone = message.clone();
    if let Some(sender_item) = sender_item {
        archive_clone
            .payloads
            .push(waddle_xmpp_core::mam::build_archived_muc_sender_x(
                sender_item,
            ));
    }
    let archive_id = match extract_room_stanza_id(&archive_clone, room) {
        Some(id) => id,
        None => {
            // Chain bug: `MucCanonicalizeHandler` MUST stamp
            // `<stanza-id by='room'/>` before `MucArchiveHandler`
            // emits `ArchiveGroupchat`. Persisting a fresh archive-
            // only id here would break the "archive id == wire
            // stanza-id" invariant — clients reflecting back the wire
            // stanza-id (XEP-0308 corrections, XEP-0424 retractions)
            // would fail to resolve the archive row. Skip the write;
            // the reflection still goes out, and a separate audit can
            // surface the chain regression (Copilot review on
            // PR #279).
            warn!(
                room = %room,
                "ArchiveGroupchat: message has no `<stanza-id by='room'/>`; \
                 skipping archive write because persisting an archive-only id would \
                 break the wire/archive stanza-id invariant (chain bug)"
            );
            return if fence.is_fenced() {
                // In clustered mode an emitted archive effect without its
                // canonical room stanza-id is not a benign policy skip. The
                // chain cannot prove a safe archive/fan-out identity, so the
                // whole batch must fail closed.
                ArchiveGroupchatOutcome::OwnershipUncertain
            } else {
                ArchiveGroupchatOutcome::Skipped
            };
        }
    };

    finish_archive_groupchat_message(
        mam_storage,
        room,
        archive_clone,
        archive_id,
        sender_nickname_generation,
        fence,
        sender_item,
    )
    .await
}

pub(super) fn extract_room_stanza_id(message: &Message, room: &BareJid) -> Option<String> {
    let room_str = room.to_string();
    message
        .payloads
        .iter()
        .filter(|payload| payload.name() == "stanza-id" && payload.ns() == STANZA_ID_NS)
        .find(|payload| payload.attr("by").is_some_and(|by| by == room_str.as_str()))
        .and_then(|payload| payload.attr("id").map(ToOwned::to_owned))
}

pub(super) async fn finish_archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    archive_clone: Message,
    archive_id: String,
    sender_nickname_generation: u64,
    fence: &RoomClaimFenceResolution,
    sender_item: Option<&waddle_xmpp_core::mam::ArchivedMucSender>,
) -> ArchiveGroupchatOutcome {
    // RFC 6121 §5.2.3: `<body>` is optional. Preserve the
    // None-vs-empty distinction so subject-only / reaction-only
    // groupchat messages don't materialize a fake empty body in the
    // archive's denormalized projection.
    let body = super::prototype_body(&archive_clone);
    let reply = extract_groupchat_reply_reference(&archive_clone, room);
    let origin_id = extract_origin_id(&archive_clone);
    let rich = rich_archive_payload(&archive_clone, sender_item);
    let stanza_xml = serialize_groupchat_stanza_xml(&archive_clone);

    // XEP-0201: read the typed thread info (id + optional parent) from the
    // post-reattach payload form. `protocol::frame::parse_stanza` calls
    // `reattach_thread_parent` at the inbound boundary so the parent
    // attribute survives `Message::try_from` here. The collapsed
    // `Option<ThreadInfo>` field on `ArchivedMessage` accepts the
    // helper's result directly.
    let thread = waddle_xmpp::xep0201::thread_info_from_message_in_stanza_ns(
        &archive_clone,
        waddle_xmpp::xep0201::CLIENT_STANZA_NS,
    );

    // XEP-0045 §7.2: groupchat reflections always carry an in-room
    // sender JID; we treat a missing `from` as a malformed reflection
    // and refuse the archive write rather than persisting a sentinel.
    // (The protocol-side handler stamps `from` before reaching this
    // arm, so this guard is defensive.)
    let Some(from_jid) = archive_clone.from.clone() else {
        warn!(
            room = %room,
            "ArchiveGroupchat: missing from JID on reflection; dropping archive write"
        );
        return ArchiveGroupchatOutcome::Skipped;
    };
    let room_jid_full = jid::Jid::from(room.clone());
    let stanza_id = archive_clone
        .id
        .as_ref()
        .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id.0.clone(), room_jid_full.clone()));
    let archived = MamArchivedMessage {
        id: archive_id.clone(),
        timestamp: chrono::Utc::now(),
        from: from_jid,
        to: room_jid_full,
        body,
        stanza_id,
        thread,
        reply,
        origin_id,
        // Typed propagation: see #228 commit 8 — `ArchivedMessage.message_type`
        // is now `xmpp_parsers::message::MessageType`, not `String`. The
        // wire-typed value flows directly through; no lossy stringifier.
        message_type: archive_clone.type_.clone(),
        stanza_xml,
        rich,
        nickname_generation: Some(sender_nickname_generation),
    };

    // ADR-0017 Phase 3 Slice 7 FIX 1: the fenced variant when this room's
    // claim fence is known (clustering enabled + Postgres) — the SAME
    // `store_message`/`store_message_fenced` split
    // `pending_delivery::insert_fenced` establishes one table over,
    // running the `SELECT ... FOR SHARE` INSIDE the same transaction as
    // this insert.
    let (store_result, fenced) = match fence {
        #[cfg(any(feature = "clustering", test))]
        RoomClaimFenceResolution::Fenced(fence) => (
            mam_storage
                .store_message_fenced(room, &archived, fence)
                .await,
            true,
        ),
        RoomClaimFenceResolution::Unfenced => {
            (mam_storage.store_message(room, &archived).await, false)
        }
        #[cfg(any(feature = "clustering", test))]
        RoomClaimFenceResolution::OwnershipUncertain => {
            warn!(
                room = %room,
                "ArchiveGroupchat: clustered room ownership became uncertain before store; \
                 failing closed"
            );
            return ArchiveGroupchatOutcome::OwnershipUncertain;
        }
    };
    match store_result {
        Ok(stored_id) => ArchiveGroupchatOutcome::Stored(ArchiveStoreResult {
            rewrite: ArchiveIdRewrite::from_store_result(
                jid::Jid::from(room.clone()),
                archive_id,
                stored_id.clone(),
            ),
            stored_id,
        }),
        Err(MamStorageError::NotOwner { entity }) => {
            warn!(
                room = %room,
                %entity,
                "ArchiveGroupchat: fenced store failed — this node has been deposed; \
                 not archiving, caller must also suppress fan-out"
            );
            ArchiveGroupchatOutcome::OwnershipUncertain
        }
        Err(error) if fenced => {
            warn!(
                room = %room,
                %error,
                "ArchiveGroupchat: fenced store could not prove and commit under exact room \
                 ownership; failing closed"
            );
            ArchiveGroupchatOutcome::OwnershipUncertain
        }
        Err(error) => {
            warn!(
                room = %room,
                %error,
                "ArchiveGroupchat: store_message failed; dropping archive write"
            );
            ArchiveGroupchatOutcome::Skipped
        }
    }
}

pub(super) struct ArchiveStoreResult {
    pub stored_id: String,
    pub rewrite: Option<ArchiveIdRewrite>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GroupchatRetractionTombstoneOutcome {
    Applied,
    Skipped,
    NotOwner,
    OwnershipUncertain,
    PersistFailed,
}

pub(super) async fn apply_groupchat_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    target_message_id: &str,
    retraction_message: &Message,
    fence: &RoomClaimFenceResolution,
) -> GroupchatRetractionTombstoneOutcome {
    if fence.is_ownership_uncertain() {
        return GroupchatRetractionTombstoneOutcome::OwnershipUncertain;
    }
    // XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // retraction names the target by the room-assigned XEP-0359
    // stanza-id, which is persisted as the archive primary key. Resolve
    // strictly by that id via `get_message` (a PK lookup), scoped to the
    // room (the archived `to` is the room JID) — never by the wire `id`
    // attribute or the origin-id. Keyed identically to the
    // validation-time `lookup_groupchat_retraction_target` so both sites
    // agree.
    let original = match mam_storage
        .get_message_by_archive_or_stanza_id(room, target_message_id)
        .await
    {
        Ok(Some(row)) if row.to.to_bare() == *room => row,
        Ok(_) => {
            debug!(
                archive = %room,
                target = target_message_id,
                "ApplyGroupchatRetractionTombstone: target not found in room archive; skipping"
            );
            return GroupchatRetractionTombstoneOutcome::Skipped;
        }
        Err(error) => {
            warn!(
                archive = %room,
                target = target_message_id,
                %error,
                "ApplyGroupchatRetractionTombstone: archive lookup failed; skipping"
            );
            return if fence.is_fenced() {
                GroupchatRetractionTombstoneOutcome::OwnershipUncertain
            } else {
                GroupchatRetractionTombstoneOutcome::PersistFailed
            };
        }
    };
    let Some(retraction_archive_id) = extract_room_stanza_id(retraction_message, room) else {
        warn!(
            archive = %room,
            target = target_message_id,
            "ApplyGroupchatRetractionTombstone: retraction stanza missing canonical room stanza-id"
        );
        return if fence.is_fenced() {
            GroupchatRetractionTombstoneOutcome::OwnershipUncertain
        } else {
            GroupchatRetractionTombstoneOutcome::Skipped
        };
    };
    // XEP-0424 tombstones cite the retraction message's wire `id`, while
    // the fenced storage proof above/below uses its room-assigned canonical
    // stanza-id. Keep those two identities deliberately distinct.
    let Some(retraction_id) = retraction_message
        .id
        .as_ref()
        .map(|id| id.0.clone())
        .and_then(RichMessageId::new)
    else {
        return if fence.is_fenced() {
            GroupchatRetractionTombstoneOutcome::OwnershipUncertain
        } else {
            GroupchatRetractionTombstoneOutcome::Skipped
        };
    };
    let Some(retraction_from) = retraction_message.from.as_ref() else {
        return if fence.is_fenced() {
            GroupchatRetractionTombstoneOutcome::OwnershipUncertain
        } else {
            GroupchatRetractionTombstoneOutcome::Skipped
        };
    };
    let tombstone = ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
    };
    let replace_result = match fence {
        #[cfg(any(feature = "clustering", test))]
        RoomClaimFenceResolution::Fenced(fence) => {
            mam_storage
                .replace_with_tombstone_fenced(
                    room,
                    &original.id,
                    &retraction_archive_id,
                    retraction_from,
                    tombstone,
                    fence,
                )
                .await
        }
        RoomClaimFenceResolution::Unfenced => {
            // XEP-0424 requires the retraction message itself to be stored.
            // Prove it belongs to this room and exact occupant before
            // replacing the target even in a single-node deployment.
            let archived_retraction = mam_storage
                .get_message_by_archive_or_stanza_id(room, &retraction_archive_id)
                .await;
            match archived_retraction {
                Ok(Some(row)) if row.to.to_bare() == *room && row.from == *retraction_from => {
                    mam_storage
                        .replace_with_tombstone(&original.id, tombstone)
                        .await
                }
                Ok(_) => Ok(false),
                Err(error) => Err(error),
            }
        }
        #[cfg(any(feature = "clustering", test))]
        RoomClaimFenceResolution::OwnershipUncertain => {
            unreachable!("ownership-uncertain fence returned before archive access")
        }
    };
    match replace_result {
        Ok(true) => {
            debug!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: replaced with tombstone"
            );
        }
        Ok(false) => {
            warn!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: target row not found at replace time"
            );
            return GroupchatRetractionTombstoneOutcome::PersistFailed;
        }
        Err(MamStorageError::NotOwner { .. }) => {
            return GroupchatRetractionTombstoneOutcome::NotOwner;
        }
        Err(error) => {
            warn!(
                archive = %room,
                original_id = %original.id,
                %error,
                "ApplyGroupchatRetractionTombstone: replace_with_tombstone failed"
            );
            return if fence.is_fenced() {
                GroupchatRetractionTombstoneOutcome::OwnershipUncertain
            } else {
                GroupchatRetractionTombstoneOutcome::PersistFailed
            };
        }
    }
    GroupchatRetractionTombstoneOutcome::Applied
}

/// Walk the SM session registry AND the pending-delivery store and
/// drop every cached `<message/>` copy that matches a XEP-0424 /
/// XEP-0425 tombstone. `target` carries the typed identity
/// ([`waddle_xmpp::tombstone::TombstoneTarget`]): groupchat scrubs
/// match only the room-assigned XEP-0359 stanza-id; 1:1 scrubs match
/// the author's wire id only for messages FROM that author. Both are
/// scoped to the conversation archive so cross-conversation (and
/// cross-sender wire-id-collision) collateral damage is impossible.
/// Returns silently on any storage error (logged at WARN) — the
/// archive scrub has already happened, and dropping the in-flight
/// copies is best-effort.
///
/// Both layers are scrubbed from the same call sites because
/// promotion (#1097/#1098) moves unacked SM stanzas into
/// pending_delivery: scrubbing only the SM registry would let a
/// promoted copy deliver the retracted content verbatim at the
/// recipient's next login.
pub(super) async fn scrub_unacked_for_tombstone(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    pending_storage: Option<
        &Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
    >,
    target: &waddle_xmpp::tombstone::TombstoneTarget,
    site: &'static str,
) {
    if let Some(sm) = sm_session_registry {
        use waddle_xmpp::stream_management::SmSessionRegistry as _;
        match sm.scrub_unacked_for_tombstone(target).await {
            Ok(removed) if removed > 0 => {
                debug!(
                    target = target.id(),
                    archive = %target.archive_jid(),
                    removed,
                    "{site}: scrubbed unacked SM queue entries for tombstoned message"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    target = target.id(),
                    archive = %target.archive_jid(),
                    %error,
                    "{site}: scrub_unacked_for_tombstone failed; pre-scrub stanza may still replay on resume"
                );
            }
        }
    }
    if let Some(pending) = pending_storage {
        match pending.scrub_for_tombstone(target).await {
            Ok(removed) if removed > 0 => {
                debug!(
                    target = target.id(),
                    archive = %target.archive_jid(),
                    removed,
                    "{site}: scrubbed pending_delivery rows for tombstoned message"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    target = target.id(),
                    archive = %target.archive_jid(),
                    %error,
                    "{site}: pending_delivery scrub_for_tombstone failed; retracted \
                     content may still deliver at the recipient's next login"
                );
            }
        }
    }
}

/// Inputs for [`project_groupchat_inbox`]: one `(owner, room, message)`
/// projection against the inbox storage plus the delivery context it
/// needs to push XEP-0430 updates and persist notification recovery.
pub(super) struct GroupchatInboxProjectionInputs<'a> {
    pub inbox_storage: &'a Arc<dyn InboxStorage>,
    pub connection_registry: &'a waddle_xmpp::registry::ConnectionRegistry,
    pub user_registry: Option<&'a kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    pub owner: &'a BareJid,
    pub room: &'a BareJid,
    pub message: &'a Message,
    pub is_recipient: bool,
    pub thread: &'a Option<GroupchatThreadProjection>,
    pub dispatch_timestamp: i64,
    pub notification_recovery: Option<waddle_xmpp::inbox::storage::GroupchatNotificationRecovery>,
}

/// Apply the `(owner, room, message)` projection against the inbox
/// storage. Mirrors the legacy
/// `deliver_groupchat_via_room_actor`'s groupchat
/// channel + thread upserts and the XEP-0430 inbox push to the
/// owner's other resources.
pub(super) async fn project_groupchat_inbox(
    inputs: GroupchatInboxProjectionInputs<'_>,
) -> GroupchatInboxProjectionOutcome {
    let GroupchatInboxProjectionInputs {
        inbox_storage,
        connection_registry,
        user_registry,
        owner,
        room,
        message,
        is_recipient,
        thread,
        dispatch_timestamp,
        notification_recovery,
    } = inputs;
    let mut outcome = GroupchatInboxProjectionOutcome::default();
    let entry = groupchat_entry(room.clone(), message, dispatch_timestamp);
    let channel_recovery = if thread.is_none() {
        notification_recovery.clone()
    } else {
        None
    };
    match inbox_storage
        .upsert_with_groupchat_notification_recovery(owner, entry, is_recipient, channel_recovery)
        .await
    {
        Ok(updated) if is_recipient => {
            outcome.channel_committed = true;
            push_inbox_update(connection_registry, user_registry, owner, &updated).await;
        }
        Ok(_) => {
            outcome.channel_committed = true;
        }
        Err(error) => {
            warn!(
                jid = %owner,
                room = %room,
                %error,
                "ProjectGroupchatInbox: channel-row upsert failed"
            );
        }
    }
    let Some(thread) = thread else {
        return outcome;
    };
    let mut thread_entry = groupchat_thread_entry(
        room.clone(),
        message,
        dispatch_timestamp,
        &thread.thread_id,
        thread.title.as_deref(),
        thread.author_nick.as_deref(),
    );
    // Persist the call-thread anchor metadata (Task 2 storage supports
    // it). The MUC call anchor's `kind`/`media` ride along on the
    // thread-root projection; replies carry neither, so a later reply's
    // projection leaves the stored anchor metadata untouched.
    if let (Some(kind), Some(media)) = (thread.call_thread_kind, thread.call_thread_media) {
        thread_entry = thread_entry.with_call_thread(kind, media);
    }
    match inbox_storage
        .upsert_with_groupchat_notification_recovery(
            owner,
            thread_entry,
            is_recipient,
            notification_recovery,
        )
        .await
    {
        Ok(updated) if is_recipient => {
            outcome.thread_committed = true;
            push_inbox_update(connection_registry, user_registry, owner, &updated).await;
        }
        Ok(_) => {
            outcome.thread_committed = true;
        }
        Err(error) => {
            warn!(
                jid = %owner,
                room = %room,
                %error,
                "ProjectGroupchatInbox: thread-row upsert failed"
            );
        }
    }
    outcome
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct GroupchatInboxProjectionOutcome {
    pub channel_committed: bool,
    pub thread_committed: bool,
}

/// XEP-0430 inbox push to all live resources of `user`. Decoupled
/// from `WebSocketState` (Copilot review on PR #279) so unit tests
/// and non-WebSocket callers can drive the projection without
/// standing up the full route stack.
///
/// Used by the new-message archive path (which observes a fresh
/// inbox row and broadcasts it) *and* the mark-read IQ handler
/// (which broadcasts the read-state flip so the user's other
/// devices clear their unread badges in real time — XEP-0430
/// §"Mark as read" cross-device sync).
pub(crate) async fn push_inbox_update(
    connection_registry: &waddle_xmpp::registry::ConnectionRegistry,
    user_registry: Option<&kameo::actor::ActorRef<waddle_xmpp::registry::UserRegistryActor>>,
    user: &BareJid,
    entry: &InboxEntry,
) {
    let resources = match user_registry {
        Some(user_registry) => {
            waddle_xmpp::registry::get_resources_for_user(user_registry, user).await
        }
        None => Vec::new(),
    };
    for resource_jid in resources {
        let msg = build_inbox_push(Jid::from(resource_jid.clone()), entry);
        let _ = connection_registry
            .send_to(&resource_jid, Stanza::Message(msg))
            .await;
    }
}

pub(super) fn extract_groupchat_reply_reference(
    message: &Message,
    room: &BareJid,
) -> Option<waddle_xmpp_core::mam::ArchivedReply> {
    use waddle_xmpp_core::mam::{ArchivedReply, RichMessageId};
    let reply = message
        .payloads
        .iter()
        .find(|payload| payload.name() == "reply" && payload.ns() == NS_REPLY)?;
    let id = RichMessageId::new(reply.attr("id")?)?;
    // XEP-0461 §3 makes `id` MUST and `to` SHOULD; for groupchat we
    // additionally restrict `to` to a room-scoped JID. A `to` that
    // fails the scope check is dropped (the reply still carries the
    // id) rather than rejecting the entire reply reference.
    let to = reply
        .attr("to")
        .and_then(|value| room_scoped_reply_to_attr(value, room));
    Some(ArchivedReply { id, to })
}

pub(crate) fn room_scoped_reply_to_attr(value: &str, room: &BareJid) -> Option<Jid> {
    value
        .parse::<Jid>()
        .ok()
        .filter(|jid| jid.to_bare() == *room)
}

pub(super) fn extract_origin_id(message: &Message) -> Option<waddle_xmpp_core::xep0359::OriginId> {
    waddle_xmpp_core::xep0359::extract_origin_id(message)
}

pub(super) fn serialize_groupchat_stanza_xml(message: &Message) -> Option<String> {
    let mut msg = message.clone();
    msg.to = None;
    match message_to_string(&msg) {
        Ok(xml) => Some(xml),
        Err(error) => {
            warn!(%error, "Failed to serialize groupchat stanza XML for MAM archive");
            None
        }
    }
}

pub(super) fn rich_archive_payload(
    message: &Message,
    muc_sender: Option<&waddle_xmpp_core::mam::ArchivedMucSender>,
) -> Option<ArchivedRichMessage> {
    let payload = extract_correction_from_message(message)
        .and_then(|correction| {
            RichMessageId::new(correction.replaces_id)
                .map(|replaces_id| ArchivedRichPayload::Correction { replaces_id })
        })
        .or_else(|| {
            extract_retraction_from_message(message).and_then(|kind| match kind {
                RetractionKind::Request(retraction) => RichMessageId::new(retraction.retracts_id)
                    .map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: None,
                            retraction_id: message
                                .id
                                .as_ref()
                                .map(|id| id.0.clone())
                                .and_then(RichMessageId::new),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => {
                    message.id.as_ref().map(|id| id.0.clone()).and_then(|id| {
                        RichMessageId::new(id).map(|target_id| {
                            ArchivedRichPayload::Retraction(ArchivedRetraction {
                                target_id,
                                stamp: retracted
                                    .stamp
                                    .as_deref()
                                    .and_then(|stamp| {
                                        chrono::DateTime::parse_from_rfc3339(stamp).ok()
                                    })
                                    .map(|stamp| stamp.with_timezone(&chrono::Utc)),
                                retraction_id: RichMessageId::new(retracted.retraction_id),
                            })
                        })
                    })
                }
            })
        })
        .or_else(|| {
            extract_reactions_from_message(message).and_then(|reactions| {
                RichMessageId::new(reactions.message_id).map(|target_id| {
                    ArchivedRichPayload::Reactions(ArchivedReactionSet {
                        target_id,
                        emojis: reactions
                            .emojis
                            .into_iter()
                            .filter_map(RichText::new)
                            .collect(),
                    })
                })
            })
        });
    let reply = parse_reply_from_message(message).and_then(|reply| {
        RichMessageId::new(reply.id).map(|id| ArchivedReply { id, to: reply.to })
    });
    let references = extract_references_from_message(message)
        .into_iter()
        .filter_map(|reference| {
            let ref_type = RichText::new(reference.ref_type.as_str())?;
            Some(ArchivedReference {
                ref_type,
                begin: reference.begin.and_then(|value| value.try_into().ok()),
                end: reference.end.and_then(|value| value.try_into().ok()),
                uri: RichText::new(reference.uri),
                anchor: reference.anchor.and_then(RichText::new),
            })
        })
        .collect::<Vec<_>>();
    let mentions = extract_explicit_mentions(message)
        .map(|mentions| {
            mentions
                .mentions
                .into_iter()
                .map(|mention| ArchivedMention {
                    begin: mention.begin,
                    end: mention.end,
                    jid: mention.jid,
                    occupant_id: mention.occupant_id.and_then(RichText::new),
                    mentions: mention.mentions.and_then(RichText::new),
                    uri: mention.uri.and_then(RichText::new),
                    active: mention.active,
                    noping: mention.noping,
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // XEP-0421: capture the server-stamped occupant-id into the typed
    // projection so the non-`stanza_xml` fallback reconstruction can
    // re-emit it (#1268). `MucCanonicalizeHandler` stamped it (and
    // stripped any client-supplied one) before the archive event was
    // emitted, so this value is server-authored.
    let occupant_id = waddle_xmpp::xep::xep0421::extract_occupant_id_from_message(message)
        .and_then(|id| waddle_xmpp_core::mam::ArchivedOccupantId::new(id.as_str()));
    let muc_sender = muc_sender.cloned();
    if payload.is_none()
        && reply.is_none()
        && references.is_empty()
        && mentions.is_empty()
        && occupant_id.is_none()
        && muc_sender.is_none()
    {
        None
    } else {
        Some(ArchivedRichMessage {
            payload,
            reply,
            references,
            mentions,
            occupant_id,
            muc_sender,
        })
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use waddle_xmpp::mam::SqlxMamStorage;
    #[cfg(feature = "clustering")]
    use waddle_xmpp::muc::{DurableRoomState, MucDurableFuture, MucDurableStore, RoomConfig};
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};
    use waddle_xmpp_core::xep0359::{add_stanza_id, StanzaId};

    fn room() -> BareJid {
        "archive-fence@muc.example.com"
            .parse()
            .expect("valid room JID")
    }

    fn fence(room: &BareJid) -> RoomClaimFenceResolution {
        RoomClaimFenceResolution::Fenced(RoomClaimFenceContext {
            entity: Entity::new(EntityType::RoomActor, room.to_string()),
            epoch: ClaimEpoch(7),
            owner: NodeIdentity::new("node-a", "epoch-a"),
        })
    }

    fn groupchat_message(room: &BareJid, with_stanza_id: bool) -> Message {
        let mut message = Message::new(Some(Jid::from(room.clone())));
        message.from = Some(
            format!("{room}/alice")
                .parse::<Jid>()
                .expect("valid room occupant JID"),
        );
        message.type_ = XmppMessageType::Groupchat;
        message.id = Some(xmpp_parsers::message::Id("wire-id".to_string()));
        message
            .bodies
            .insert(xmpp_parsers::message::Lang::new(), "hello".to_string());
        if with_stanza_id {
            add_stanza_id(
                &mut message,
                &StanzaId::new("archive-id", Jid::from(room.clone())),
            );
        }
        message
    }

    #[cfg(feature = "clustering")]
    struct MissingCachedFenceStore;

    #[cfg(feature = "clustering")]
    impl MucDurableStore for MissingCachedFenceStore {
        fn load_room_state<'a>(
            &'a self,
            _room_jid: &'a BareJid,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            Box::pin(async { Ok(None) })
        }

        fn save_config<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _waddle_id: &'a str,
            _channel_id: &'a str,
            _config: &'a RoomConfig,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_subject<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }

        fn save_affiliation<'a>(
            &'a self,
            _room_jid: &'a BareJid,
            _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
        ) -> MucDurableFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn active_cluster_without_muc_store_is_not_treated_as_unfenced() {
        let registry = ConnectionRegistry::new();
        let mut deps = Deps::registry_only(&registry);
        deps.clustered_muc_ownership_required = true;

        assert!(matches!(
            resolve_room_claim_fence(&deps, &room()),
            RoomClaimFenceResolution::OwnershipUncertain
        ));
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn clustered_store_without_cached_claim_is_ownership_uncertain() {
        let registry = ConnectionRegistry::new();
        let store: Arc<dyn MucDurableStore> = Arc::new(MissingCachedFenceStore);
        let mut deps = Deps::registry_only(&registry);
        deps.clustered_muc_ownership_required = true;
        deps.muc_durable_store = Some(&store);

        assert!(matches!(
            resolve_room_claim_fence(&deps, &room()),
            RoomClaimFenceResolution::OwnershipUncertain
        ));
    }

    #[cfg(feature = "clustering")]
    #[test]
    fn durable_archive_without_actor_fence_never_uses_a_room_scoped_latest_claim() {
        use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

        let registry = ConnectionRegistry::new();
        let room = room();
        let latest = RoomClaimFenceContext {
            entity: Entity::new(EntityType::RoomActor, room.to_string()),
            epoch: ClaimEpoch(2),
            owner: NodeIdentity::new("node-a", "node-epoch-a"),
        };
        struct LatestClaimStore(RoomClaimFenceContext);
        impl MucDurableStore for LatestClaimStore {
            fn load_room_state<'a>(
                &'a self,
                _room_jid: &'a BareJid,
            ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
                Box::pin(async { Ok(None) })
            }

            fn save_config<'a>(
                &'a self,
                _room_jid: &'a BareJid,
                _waddle_id: &'a str,
                _channel_id: &'a str,
                _config: &'a RoomConfig,
            ) -> MucDurableFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn save_subject<'a>(
                &'a self,
                _room_jid: &'a BareJid,
                _subject: Option<&'a waddle_xmpp::muc::SubjectState>,
            ) -> MucDurableFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn save_affiliation<'a>(
                &'a self,
                _room_jid: &'a BareJid,
                _entry: &'a waddle_xmpp::muc::affiliation::AffiliationEntry,
            ) -> MucDurableFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn current_claim_fence(&self, _room_jid: &BareJid) -> Option<RoomClaimFenceContext> {
                Some(self.0.clone())
            }
        }

        let latest_store: Arc<dyn MucDurableStore> = Arc::new(LatestClaimStore(latest));
        let mut deps = Deps::registry_only(&registry);
        deps.muc_durable_store = Some(&latest_store);

        assert!(matches!(
            resolve_room_claim_fence(&deps, &room),
            RoomClaimFenceResolution::OwnershipUncertain
        ));
    }

    #[tokio::test]
    async fn ownership_uncertainty_overrides_archive_policy_skip() {
        let room = room();
        let storage: Arc<dyn MamStorage> = Arc::new(
            SqlxMamStorage::open_in_memory()
                .await
                .expect("open in-memory MAM"),
        );
        let outcome = archive_groupchat_message(
            &storage,
            &room,
            &groupchat_message(&room, false),
            0,
            &RoomClaimFenceResolution::OwnershipUncertain,
            None,
        )
        .await;

        assert!(matches!(
            outcome,
            ArchiveGroupchatOutcome::OwnershipUncertain
        ));
    }

    #[tokio::test]
    async fn fenced_malformed_archive_event_is_ownership_uncertain() {
        let room = room();
        let storage: Arc<dyn MamStorage> = Arc::new(
            SqlxMamStorage::open_in_memory()
                .await
                .expect("open in-memory MAM"),
        );
        let outcome = archive_groupchat_message(
            &storage,
            &room,
            &groupchat_message(&room, false),
            0,
            &fence(&room),
            None,
        )
        .await;

        assert!(matches!(
            outcome,
            ArchiveGroupchatOutcome::OwnershipUncertain
        ));
    }

    #[tokio::test]
    async fn fenced_archive_without_fencing_backend_fails_closed() {
        let room = room();
        let storage: Arc<dyn MamStorage> = Arc::new(
            SqlxMamStorage::open_in_memory()
                .await
                .expect("open in-memory MAM"),
        );
        let outcome = archive_groupchat_message(
            &storage,
            &room,
            &groupchat_message(&room, true),
            0,
            &fence(&room),
            None,
        )
        .await;

        assert!(matches!(
            outcome,
            ArchiveGroupchatOutcome::OwnershipUncertain
        ));
        assert!(storage
            .query_messages(&room, &Default::default())
            .await
            .expect("query archive")
            .messages
            .is_empty());
    }

    #[tokio::test]
    async fn unfenced_single_node_archive_uses_portable_store() {
        let room = room();
        let storage: Arc<dyn MamStorage> = Arc::new(
            SqlxMamStorage::open_in_memory()
                .await
                .expect("open in-memory MAM"),
        );
        let outcome = archive_groupchat_message(
            &storage,
            &room,
            &groupchat_message(&room, true),
            0,
            &RoomClaimFenceResolution::Unfenced,
            None,
        )
        .await;

        assert!(matches!(outcome, ArchiveGroupchatOutcome::Stored(_)));
        assert_eq!(
            storage
                .query_messages(&room, &Default::default())
                .await
                .expect("query archive")
                .messages
                .len(),
            1
        );
    }
}
