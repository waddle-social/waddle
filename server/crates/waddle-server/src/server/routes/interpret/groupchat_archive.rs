use super::*;

pub(super) async fn archive_groupchat_message(
    mam_storage: &Arc<dyn MamStorage>,
    room: &BareJid,
    message: &Message,
    sender_nickname_generation: u64,
) -> Option<ArchiveStoreResult> {
    let archive_clone = message.clone();
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
            return None;
        }
    };

    finish_archive_groupchat_message(
        mam_storage,
        room,
        archive_clone,
        archive_id,
        sender_nickname_generation,
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
) -> Option<ArchiveStoreResult> {
    // RFC 6121 §5.2.3: `<body>` is optional. Preserve the
    // None-vs-empty distinction so subject-only / reaction-only
    // groupchat messages don't materialize a fake empty body in the
    // archive's denormalized projection.
    let body = prototype_body(&archive_clone);
    let reply = extract_groupchat_reply_reference(&archive_clone, room);
    let origin_id = extract_origin_id(&archive_clone);
    let rich = rich_archive_payload(&archive_clone);
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
        return None;
    };
    let room_jid_full = jid::Jid::from(room.clone());
    let stanza_id = archive_clone
        .id
        .clone()
        .map(|id| waddle_xmpp_core::xep0359::StanzaId::new(id, room_jid_full.clone()));
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

    match mam_storage.store_message(room, &archived).await {
        Ok(stored_id) => Some(ArchiveStoreResult {
            rewrite: ArchiveIdRewrite::from_store_result(
                jid::Jid::from(room.clone()),
                archive_id,
                stored_id.clone(),
            ),
            stored_id,
        }),
        Err(error) => {
            warn!(
                room = %room,
                %error,
                "ArchiveGroupchat: store_message failed; dropping archive write"
            );
            None
        }
    }
}

pub(super) struct ArchiveStoreResult {
    pub stored_id: String,
    pub rewrite: Option<ArchiveIdRewrite>,
}

pub(super) async fn apply_groupchat_retraction_tombstone(
    mam_storage: &Arc<dyn MamStorage>,
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    room: &BareJid,
    target_message_id: &str,
    retraction_message: &Message,
) {
    // XEP-0424 §3.1: the retraction names the target by its wire
    // message id within the room's archive, not by the archive's
    // primary key. Use the same accessor as the XEP-0308 correction
    // path (`validate_groupchat_rich_targets`) so reflection-time
    // validation and tombstone application agree on the lookup key.
    let original = match mam_storage
        .get_message_by_message_id(room, target_message_id)
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            debug!(
                archive = %room,
                target = target_message_id,
                "ApplyGroupchatRetractionTombstone: target not found in room archive; skipping"
            );
            return;
        }
        Err(error) => {
            warn!(
                archive = %room,
                target = target_message_id,
                %error,
                "ApplyGroupchatRetractionTombstone: archive lookup failed; skipping"
            );
            return;
        }
    };
    let Some(retraction_id) = retraction_message.id.clone().and_then(RichMessageId::new) else {
        warn!(
            archive = %room,
            target = target_message_id,
            "ApplyGroupchatRetractionTombstone: retraction stanza missing valid message id; skipping"
        );
        return;
    };
    let tombstone = ArchivedTombstone {
        retraction_id: Some(retraction_id),
        stamp: chrono::Utc::now(),
        moderation: None,
    };
    match mam_storage
        .replace_with_tombstone(&original.id, tombstone)
        .await
    {
        Ok(true) => {
            debug!(
                archive = %room,
                original_id = %original.id,
                "ApplyGroupchatRetractionTombstone: replaced with tombstone"
            );
        }
        Ok(false) => warn!(
            archive = %room,
            original_id = %original.id,
            "ApplyGroupchatRetractionTombstone: target row not found at replace time"
        ),
        Err(error) => warn!(
            archive = %room,
            original_id = %original.id,
            %error,
            "ApplyGroupchatRetractionTombstone: replace_with_tombstone failed"
        ),
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
        target_message_id,
        &room.to_string(),
        "ApplyGroupchatRetractionTombstone",
    )
    .await;
}

/// Walk the SM session registry and drop every unacked outbound
/// `<message/>` entry that matches a XEP-0424 / XEP-0425 tombstone.
/// `target_id` is matched against either the cached message's wire
/// `id` attribute or any XEP-0359 `<stanza-id id='…'/>` child, scoped
/// to `archive_jid` so cross-conversation collateral damage is
/// impossible. Returns silently on any registry error (logged at
/// WARN) — the archive scrub has already happened, and dropping the
/// in-flight copy is best-effort.
pub(super) async fn scrub_unacked_for_tombstone(
    sm_session_registry: Option<&Arc<InMemorySmSessionRegistry>>,
    target_id: &str,
    archive_jid: &str,
    site: &'static str,
) {
    let Some(sm) = sm_session_registry else {
        return;
    };
    use waddle_xmpp::stream_management::SmSessionRegistry as _;
    match sm.scrub_unacked_for_tombstone(target_id, archive_jid).await {
        Ok(removed) if removed > 0 => {
            debug!(
                target = target_id,
                archive = archive_jid,
                removed,
                "{site}: scrubbed unacked SM queue entries for tombstoned message"
            );
        }
        Ok(_) => {}
        Err(error) => {
            warn!(
                target = target_id,
                archive = archive_jid,
                %error,
                "{site}: scrub_unacked_for_tombstone failed; pre-scrub stanza may still replay on resume"
            );
        }
    }
}

/// Apply the `(owner, room, message)` projection against the inbox
/// storage. Mirrors the legacy
/// `deliver_groupchat_via_room_actor`'s groupchat
/// channel + thread upserts and the XEP-0430 inbox push to the
/// owner's other resources.
#[allow(clippy::too_many_arguments)]
pub(super) async fn project_groupchat_inbox(
    inbox_storage: &Arc<dyn InboxStorage>,
    connection_registry: &waddle_xmpp::registry::ConnectionRegistry,
    owner: &BareJid,
    room: &BareJid,
    message: &Message,
    is_recipient: bool,
    thread: &Option<GroupchatThreadProjection>,
    dispatch_timestamp: i64,
) -> GroupchatInboxProjectionOutcome {
    let mut outcome = GroupchatInboxProjectionOutcome::default();
    let entry = groupchat_entry(room.clone(), message, dispatch_timestamp);
    match inbox_storage.upsert(owner, entry, is_recipient).await {
        Ok(updated) if is_recipient => {
            outcome.channel_committed = true;
            push_inbox_update(connection_registry, owner, &updated).await;
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
    let thread_entry = groupchat_thread_entry(
        room.clone(),
        message,
        dispatch_timestamp,
        &thread.thread_id,
        thread.title.as_deref(),
        thread.author_nick.as_deref(),
    );
    match inbox_storage
        .upsert(owner, thread_entry, is_recipient)
        .await
    {
        Ok(updated) if is_recipient => {
            outcome.thread_committed = true;
            push_inbox_update(connection_registry, owner, &updated).await;
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
    user: &BareJid,
    entry: &InboxEntry,
) {
    let resources = connection_registry.get_resources_for_user(user);
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

pub(super) fn prototype_body(message: &Message) -> Option<String> {
    message
        .bodies
        .get("")
        .or_else(|| message.bodies.values().next())
        .map(|body| body.0.clone())
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

pub(super) fn rich_archive_payload(message: &Message) -> Option<ArchivedRichMessage> {
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
                            retraction_id: message.id.clone().and_then(RichMessageId::new),
                        })
                    }),
                RetractionKind::Tombstone(retracted) => message.id.clone().and_then(|id| {
                    RichMessageId::new(id).map(|target_id| {
                        ArchivedRichPayload::Retraction(ArchivedRetraction {
                            target_id,
                            stamp: retracted
                                .stamp
                                .as_deref()
                                .and_then(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).ok())
                                .map(|stamp| stamp.with_timezone(&chrono::Utc)),
                            retraction_id: RichMessageId::new(retracted.retraction_id),
                        })
                    })
                }),
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
    if payload.is_none() && reply.is_none() && references.is_empty() && mentions.is_empty() {
        None
    } else {
        Some(ArchivedRichMessage {
            payload,
            reply,
            references,
            mentions,
        })
    }
}
