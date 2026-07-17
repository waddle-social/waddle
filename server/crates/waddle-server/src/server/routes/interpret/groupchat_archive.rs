use super::*;
use waddle_xmpp::mam::{MamStorageError, StoreOutcome, TerminalTombstoneOutcome};
use waddle_xmpp::muc::RoomClaimFenceContext;
use waddle_xmpp::ownership::{CurrentNodeIdentityGuard, SharedNodeIdentity};

/// Archive fencing authority resolved for one room write. `Guarded` owns
/// the identity-rotation read guard through the archive transaction, while
/// `OwnershipLost` distinguishes a configured clustered room with no valid
/// fence from a genuinely unclustered `Unfenced` deployment.
pub(super) enum RoomArchiveFence {
    Unfenced,
    Guarded {
        context: RoomClaimFenceContext,
        _identity_guard: CurrentNodeIdentityGuard,
    },
    OwnershipLost,
}

/// Outcome of a groupchat archive write attempt (ADR-0017 Phase 3 Slice 7
/// FIX 1, council-adjudicated). `OwnershipLost` is the fenced backstop
/// firing: the caller MUST neither treat the message as archived NOR fan
/// it out to occupants — mirroring `dispatch_to_room`'s own
/// `check_fenced_fanout` `Ok(false)` handling one layer closer to the
/// actual write.
pub(super) enum ArchiveGroupchatOutcome {
    Stored(ArchiveStoreResult),
    Deduplicated(ArchiveStoreResult),
    TombstoneHit,
    /// Not an error: a chain-bug guard or a non-fencing storage failure
    /// declined the write. The reflection still goes out (today's
    /// pre-existing behavior, unchanged).
    Skipped,
    /// The fenced write observed that this node no longer holds the
    /// room's ownership claim. The message was NOT archived; the caller
    /// must also suppress fan-out and bounce the sender.
    OwnershipLost,
}

/// ADR-0017 Phase 3 Slice 7 FIX 1: resolve the typed `(Entity, ClaimEpoch,
/// node_id)` fencing context for `room`, the SAME mechanism
/// `dispatch_to_room`'s own `check_fenced_fanout` pre-fan-out check reads
/// from — threaded here rather than re-derived from a second, independent
/// source. A genuinely unclustered deployment resolves to `Unfenced` and
/// retains the portable `MamStorage::store_message` path. Once a clustered
/// durable store is configured, a missing, stale, or rotated fence resolves
/// to `OwnershipLost`; it can never silently become an unfenced write.
pub(super) async fn resolve_room_claim_fence(deps: &Deps<'_>, room: &BareJid) -> RoomArchiveFence {
    #[cfg(feature = "clustering")]
    {
        let Some(state) = deps.web_socket_state else {
            return RoomArchiveFence::Unfenced;
        };
        let clustering = &state.deps.app_state.clustering_claims;
        let Some(store) = clustering.muc_durable_store.as_ref() else {
            return RoomArchiveFence::Unfenced;
        };
        guard_clustered_room_claim_fence(
            store.current_claim_fence(room),
            clustering.node_identity.as_ref(),
        )
        .await
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (deps, room);
        RoomArchiveFence::Unfenced
    }
}

async fn guard_clustered_room_claim_fence(
    fence: Option<RoomClaimFenceContext>,
    node_identity: Option<&SharedNodeIdentity>,
) -> RoomArchiveFence {
    let (Some(context), Some(node_identity)) = (fence, node_identity) else {
        return RoomArchiveFence::OwnershipLost;
    };
    let Some(identity_guard) = node_identity.guard_if_current(&context.owner).await else {
        return RoomArchiveFence::OwnershipLost;
    };
    RoomArchiveFence::Guarded {
        context,
        _identity_guard: identity_guard,
    }
}

pub(super) async fn archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    message: &Message,
    sender_nickname_generation: u64,
    fence: &RoomArchiveFence,
    sender_item: Option<&waddle_xmpp_core::mam::ArchivedMucSender>,
) -> ArchiveGroupchatOutcome {
    if matches!(fence, RoomArchiveFence::OwnershipLost) {
        return ArchiveGroupchatOutcome::OwnershipLost;
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
            return ArchiveGroupchatOutcome::Skipped;
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
    fence: &RoomArchiveFence,
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
    let store_result = match fence {
        RoomArchiveFence::Guarded { context, .. } => {
            mam_storage
                .store_message_fenced(room, &archived, context)
                .await
        }
        RoomArchiveFence::Unfenced => mam_storage.store_message(room, &archived).await,
        RoomArchiveFence::OwnershipLost => return ArchiveGroupchatOutcome::OwnershipLost,
    };
    match store_result {
        Ok(StoreOutcome::Stored(stored_id)) => {
            ArchiveGroupchatOutcome::Stored(ArchiveStoreResult {
                rewrite: ArchiveIdRewrite::from_store_result(
                    jid::Jid::from(room.clone()),
                    archive_id,
                    stored_id.clone(),
                ),
                stored_id,
            })
        }
        Ok(StoreOutcome::Deduplicated(stored_id)) => {
            ArchiveGroupchatOutcome::Deduplicated(ArchiveStoreResult {
                rewrite: ArchiveIdRewrite::from_store_result(
                    jid::Jid::from(room.clone()),
                    archive_id,
                    stored_id.clone(),
                ),
                stored_id,
            })
        }
        Ok(StoreOutcome::TombstoneHit(existing_id)) => {
            debug!(
                room = %room,
                archive_id = %existing_id,
                "ArchiveGroupchat: origin-id retry matched a tombstone"
            );
            ArchiveGroupchatOutcome::TombstoneHit
        }
        Err(MamStorageError::NotOwner { entity }) => {
            warn!(
                room = %room,
                %entity,
                "ArchiveGroupchat: fenced store failed — this node has been deposed; \
                 not archiving, caller must also suppress fan-out"
            );
            ArchiveGroupchatOutcome::OwnershipLost
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

#[cfg(all(test, feature = "clustering"))]
mod room_archive_fence_tests {
    use super::*;
    use std::time::Duration;
    use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

    fn fence(owner: NodeIdentity) -> RoomClaimFenceContext {
        RoomClaimFenceContext::new(
            Entity::new(EntityType::RoomActor, "room@muc.example.com"),
            owner,
            ClaimEpoch(7),
        )
    }

    #[tokio::test]
    async fn configured_cluster_without_a_room_fence_fails_closed() {
        let identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-a"));
        assert!(matches!(
            guard_clustered_room_claim_fence(None, Some(&identity)).await,
            RoomArchiveFence::OwnershipLost
        ));
    }

    #[tokio::test]
    async fn stale_room_fence_fails_closed_after_identity_rotation() {
        let old = NodeIdentity::new("node-a", "epoch-a");
        let identity = SharedNodeIdentity::new(NodeIdentity::new("node-a", "epoch-b"));
        assert!(matches!(
            guard_clustered_room_claim_fence(Some(fence(old)), Some(&identity)).await,
            RoomArchiveFence::OwnershipLost
        ));
    }

    #[tokio::test]
    async fn guarded_room_fence_blocks_rotation_through_the_archive_boundary() {
        let old = NodeIdentity::new("node-a", "epoch-a");
        let replacement = NodeIdentity::new("node-a", "epoch-b");
        let identity = SharedNodeIdentity::new(old.clone());
        let guarded = guard_clustered_room_claim_fence(Some(fence(old)), Some(&identity)).await;
        assert!(matches!(guarded, RoomArchiveFence::Guarded { .. }));

        let rotating_identity = identity.clone();
        let mut rotation = tokio::spawn(async move {
            rotating_identity.rotate(replacement).await;
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), &mut rotation)
                .await
                .is_err(),
            "identity rotation must wait while the MAM fence authority is live"
        );

        drop(guarded);
        tokio::time::timeout(Duration::from_secs(1), rotation)
            .await
            .expect("rotation unblocks when archive authority drops")
            .expect("rotation task completes");
    }
}

pub(super) struct ArchiveStoreResult {
    pub stored_id: String,
    pub rewrite: Option<ArchiveIdRewrite>,
}

pub(super) async fn apply_groupchat_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    pending_storage: Option<
        &Arc<dyn waddle_xmpp::pending_delivery::storage::PendingDeliveryStorage>,
    >,
    room: &BareJid,
    target_message_id: &str,
    retraction_message: &Message,
) -> bool {
    // XEP-0424 §3 (xep-0424.xml lines 158, 230-232): a groupchat
    // retraction names the target by the room-assigned XEP-0359
    // stanza-id, which is persisted as the archive primary key. Resolve
    // strictly by that id via `get_message` (a PK lookup), scoped to the
    // room (the archived `to` is the room JID) — never by the wire `id`
    // attribute or the origin-id. Keyed identically to the
    // validation-time `lookup_groupchat_retraction_target` so both sites
    // agree. The scrub target mirrors this: cached reflections match
    // ONLY on `<stanza-id by=room id=target/>`, never on the
    // client-chosen wire id (which any occupant could mint to collide).
    let scrub_target = waddle_xmpp::tombstone::TombstoneTarget::Groupchat {
        stanza_id: target_message_id.to_string(),
        room: room.clone(),
    };
    let original = match mam_storage.get_message(target_message_id).await {
        Ok(Some(row)) if row.to.to_bare() == *room => row,
        Ok(_) => {
            debug!(
                archive = %room,
                target = target_message_id,
                "ApplyGroupchatRetractionTombstone: target not found in room archive; skipping"
            );
            return false;
        }
        Err(error) => {
            warn!(
                archive = %room,
                target = target_message_id,
                %error,
                "ApplyGroupchatRetractionTombstone: archive lookup failed; skipping"
            );
            return false;
        }
    };
    // Tombstones are terminal. A heal-retry of an XEP-0424 author
    // retraction must never downgrade the attribution or reason on an
    // existing XEP-0425 moderation tombstone.
    if original
        .rich
        .as_ref()
        .is_some_and(ArchivedRichMessage::is_tombstoned)
    {
        debug!(
            archive = %room,
            original_id = %original.id,
            "ApplyGroupchatRetractionTombstone: target is already tombstoned; preserving terminal tombstone"
        );
        // A crash between the tombstone persist and the scrub leaves
        // pre-tombstone reflections replayable from SM queues /
        // pending_delivery. The scrub is idempotent, so re-running it
        // on the heal path closes that window (Qodo review on PR #1412).
        scrub_unacked_for_tombstone(
            sm_session_registry,
            pending_storage,
            &scrub_target,
            "ApplyGroupchatRetractionTombstone",
        )
        .await;
        return true;
    }
    let Some(retraction_id) = retraction_message
        .id
        .as_ref()
        .map(|id| id.0.clone())
        .and_then(RichMessageId::new)
    else {
        warn!(
            archive = %room,
            target = target_message_id,
            "ApplyGroupchatRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return false;
    };
    let tombstone = ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
        // Retain the original sender's identity for the internal
        // tombstone-retry match only; never emitted to the wire.
        sender_scope: original
            .rich
            .as_ref()
            .and_then(|rich| rich.muc_sender.as_ref())
            .map(|sender| sender.jid.to_bare()),
    };
    match mam_storage
        .replace_with_terminal_tombstone(&original.id, tombstone)
        .await
    {
        Ok(TerminalTombstoneOutcome::Replaced) => {
            debug!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: replaced with tombstone"
            );
        }
        Ok(TerminalTombstoneOutcome::AlreadyTombstoned) => {
            debug!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: concurrent tombstone won; preserving terminal tombstone"
            );
            // Same crash-window heal as the pre-check exit: the winner
            // may not have completed its scrub yet (Qodo review on
            // PR #1412).
            scrub_unacked_for_tombstone(
                sm_session_registry,
                pending_storage,
                &scrub_target,
                "ApplyGroupchatRetractionTombstone",
            )
            .await;
            return true;
        }
        Ok(TerminalTombstoneOutcome::NotFound) => {
            warn!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: target row not found at replace time"
            );
            scrub_unacked_for_tombstone(
                sm_session_registry,
                pending_storage,
                &scrub_target,
                "ApplyGroupchatRetractionTombstone",
            )
            .await;
            return false;
        }
        Err(error) => {
            warn!(
                archive = %room,
                original_id = %original.id,
                %error,
                "ApplyGroupchatRetractionTombstone: terminal tombstone replacement failed"
            );
            scrub_unacked_for_tombstone(
                sm_session_registry,
                pending_storage,
                &scrub_target,
                "ApplyGroupchatRetractionTombstone",
            )
            .await;
            return false;
        }
    }
    // Drop matching unacked groupchat reflections from detached
    // XEP-0198 session queues. The reflection is what occupants see;
    // scrubbing here closes the resume-side replay leak for groupchat
    // retractions identically to the 1:1 case. Scope by the room JID
    // so the matcher's stanza-id branch can find groupchat reflections
    // that key by the room's XEP-0359 stamp, and so a colliding wire
    // id in another conversation is not accidentally scrubbed
    // (Codex P1, Copilot review on PR #305).
    scrub_unacked_for_tombstone(
        sm_session_registry,
        pending_storage,
        &scrub_target,
        "ApplyGroupchatRetractionTombstone",
    )
    .await;
    true
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
    let subjects = message
        .subjects
        .iter()
        .map(|(lang, text)| (lang.0.clone(), text.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
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
        && subjects.is_empty()
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
            subjects,
            occupant_id,
            muc_sender,
        })
    }
}
