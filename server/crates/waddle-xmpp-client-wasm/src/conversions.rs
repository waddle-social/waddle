use super::*;

pub(crate) fn upload_slot_to_js(slot: discovery::UploadSlot) -> WaddleUploadSlot {
    WaddleUploadSlot {
        put_url: slot.put_url,
        get_url: slot.get_url,
        put_headers: slot
            .put_headers
            .into_iter()
            .map(|(name, value)| WaddleUploadHeader { name, value })
            .collect(),
    }
}

pub(crate) fn markup_span_type_to_string(span_type: MarkupSpanType) -> String {
    match span_type {
        MarkupSpanType::Bold => "bold",
        MarkupSpanType::Italic => "italic",
        MarkupSpanType::Strikethrough => "strikethrough",
        MarkupSpanType::Code => "code",
        MarkupSpanType::CodeBlock => "code_block",
        MarkupSpanType::Blockquote => "blockquote",
        MarkupSpanType::Link => "link",
    }
    .to_string()
}

pub(crate) fn markup_spans_to_js(spans: Vec<messaging::MarkupSpan>) -> Vec<WaddleMarkupSpan> {
    spans
        .into_iter()
        .map(|span| WaddleMarkupSpan {
            span_type: markup_span_type_to_string(span.span_type),
            start: span.start,
            end: span.end,
            uri: span.uri,
        })
        .collect()
}

pub(crate) fn references_to_js(references: Vec<messaging::ReferenceData>) -> Vec<WaddleReference> {
    references
        .into_iter()
        .map(|reference| WaddleReference {
            ref_type: reference.ref_type,
            uri: reference.uri,
            begin: reference.begin,
            end: reference.end,
            anchor: reference.anchor,
        })
        .collect()
}

fn in_call_signal_to_js(signal: Option<InCallSignal>) -> Option<WaddleInCallSignal> {
    match signal {
        Some(InCallSignal::Reaction(payload)) => Some(WaddleInCallSignal::Reaction {
            sid: payload.sid.as_str().to_string(),
            emoji: payload.emoji.as_str().to_string(),
        }),
        None => None,
    }
}

pub(crate) fn call_thread_to_js(
    anchor: waddle_xmpp_client::xep::call_thread::CallThreadAnchor,
) -> WaddleCallThreadAnchor {
    let kind = match anchor.kind {
        waddle_xmpp_client::xep::call_thread::CallThreadKind::Dm => "dm",
        waddle_xmpp_client::xep::call_thread::CallThreadKind::Muc => "muc",
    };
    let mut media = Vec::new();
    if anchor.media.audio {
        media.push("audio".to_string());
    }
    if anchor.media.video {
        media.push("video".to_string());
    }

    WaddleCallThreadAnchor {
        kind: kind.to_string(),
        sid: anchor.sid.0,
        media,
        initiator: anchor.initiator.to_string(),
        started: anchor.started.to_rfc3339(),
    }
}

pub(crate) fn call_thread_ended_to_js(
    ended: waddle_xmpp_client::xep::call_thread::CallThreadEnded,
) -> WaddleCallThreadEnded {
    WaddleCallThreadEnded {
        anchor_id: ended.anchor_id,
        ended: ended.ended.to_rfc3339(),
        duration: ended.duration.as_str().to_owned(),
    }
}

pub(crate) fn extension_envelope_to_js(
    envelope: messaging::ExtensionEnvelopeData,
) -> WaddleExtensionEnvelope {
    WaddleExtensionEnvelope {
        version: envelope.version,
        enrichments: envelope
            .enrichments
            .into_iter()
            .map(|enrichment| WaddleExtensionEnrichment {
                id: enrichment.id.as_str().to_string(),
                plugin: enrichment.plugin.as_str().to_string(),
                capability: enrichment.capability.as_str().to_string(),
                payload_namespace: enrichment.payload_namespace.as_str().to_string(),
                created: enrichment.created.as_str().to_string(),
                source: enrichment.source.map(extension_source_to_js),
                title: enrichment.title.map(|title| title.as_str().to_string()),
                summary: enrichment
                    .summary
                    .map(|summary| summary.as_str().to_string()),
                payloads: enrichment
                    .payloads
                    .into_iter()
                    .map(extension_payload_element_to_js)
                    .collect(),
                launches: enrichment
                    .launches
                    .into_iter()
                    .map(extension_launch_to_js)
                    .collect(),
            })
            .collect(),
    }
}

fn extension_source_to_js(source: messaging::ExtensionSourceData) -> WaddleExtensionSource {
    WaddleExtensionSource {
        stanza_id: source.stanza_id.as_str().to_string(),
        body_start: source.body_start,
        body_end: source.body_end,
    }
}

fn extension_launch_to_js(launch: messaging::ExtensionLaunchData) -> WaddleExtensionLaunch {
    WaddleExtensionLaunch {
        id: launch.id.as_str().to_string(),
        plugin: launch.plugin.as_str().to_string(),
        action: launch.action.as_str().to_string(),
        command_node: launch.command_node.as_str().to_string(),
        label: launch.label.as_str().to_string(),
        context: WaddleExtensionLaunchContext {
            waddle_id: launch.context.waddle_id.as_str().to_string(),
            room: launch.context.room.map(|room| room.as_str().to_string()),
            source_stanza_id: launch
                .context
                .source_stanza_id
                .map(|stanza_id| stanza_id.as_str().to_string()),
        },
        payloads: launch
            .payloads
            .into_iter()
            .map(extension_payload_element_to_js)
            .collect(),
        expires_at: launch
            .expires_at
            .map(|expires_at| expires_at.as_str().to_string()),
        token: launch.token.map(|token| token.as_str().to_string()),
    }
}

fn extension_payload_element_to_js(
    element: messaging::ExtensionPayloadElementData,
) -> WaddleExtensionPayloadElement {
    WaddleExtensionPayloadElement {
        namespace: element.namespace.as_str().to_string(),
        name: element.name.as_str().to_string(),
        attributes: element
            .attributes
            .into_iter()
            .map(|attribute| WaddleExtensionPayloadAttribute {
                name: attribute.name.as_str().to_string(),
                value: attribute.value,
            })
            .collect(),
        text: element.text,
        children: element
            .children
            .into_iter()
            .map(extension_payload_element_to_js)
            .collect(),
    }
}

pub(crate) fn inbound_to_js(message: InboundMessage) -> WaddleMessage {
    let (reply_fallback_start, reply_fallback_end) = match message.reply_fallback {
        Some((start, end)) => (Some(start), Some(end)),
        None => (None, None),
    };
    let forum_thread_title = if message.forum_post_kind.as_deref() == Some("topic") {
        message
            .forum_title
            .clone()
            .or_else(|| message.subject.clone())
    } else {
        None
    };
    let in_call = in_call_signal_to_js(message.in_call);

    WaddleMessage {
        id: message.id,
        from: message.from,
        to: message.to,
        body: message.body,
        subject: message.subject,
        message_type: message.message_type.clone(),
        timestamp: message.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        stanza_id: message.stanza_id.as_ref().map(ToString::to_string),
        stanza_id_by: message.stanza_id.as_ref().map(|id| id.by.to_string()),
        stanza_ids: message
            .stanza_ids
            .into_iter()
            .map(|stanza_id| WaddleStanzaId {
                id: stanza_id.id,
                by: stanza_id.by.to_string(),
            })
            .collect(),
        origin_id: message.origin_id,
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        retraction_id: message.retraction_id,
        is_retracted: message.is_retracted,
        moderation_target_id: message.moderation_target_id,
        moderated_by: message.moderated_by.map(|jid| jid.to_string()),
        moderation_reason: message.moderation_reason,
        chat_state: message.chat_state,
        displayed_marker_requested: message.displayed_marker_requested,
        displayed_marker_id: message.displayed_marker_id,
        reaction_target_id: message.reaction_target_id,
        reaction_emojis: message.reaction_emojis,
        in_call,
        is_muc: message.message_type == "groupchat",
        thread: message.thread_id.or(message.thread),
        parent_thread_id: message.parent_thread_id,
        reply_to_id: message.reply_to_id,
        reply_to_sender: message.reply_to_sender,
        reply_fallback_start,
        reply_fallback_end,
        markup_spans: markup_spans_to_js(message.markup_spans),
        broadcast_mention: message.broadcast_mention,
        mention_uris: message.mention_uris,
        references: references_to_js(message.references),
        forum_post_kind: message.forum_post_kind,
        forum_title: message.forum_title,
        forum_thread_title,
        is_sticker: message.is_sticker,
        call_thread: message.call_thread.map(call_thread_to_js),
        call_thread_ended: message.call_thread_ended.map(call_thread_ended_to_js),
        shared_files: message
            .shared_files
            .into_iter()
            .map(shared_file_to_js)
            .collect(),
        link_previews: link_previews_to_js(message.link_previews),
        pin_event: message.pin_event.map(pin_event_to_js),
        extension_envelope: message.extension_envelope.map(extension_envelope_to_js),
        extension_body_fallback: message.extension_body_fallback,
        pubsub_events: message
            .pubsub_events
            .into_iter()
            .map(pubsub_event_to_js)
            .collect(),
        inbox_push: None,
        carbon: message.carbon.map(carbon_to_js),
    }
}

/// XEP-0280 typed direction → JS `{ sent, received }` marker (#1243).
fn carbon_to_js(direction: messaging::CarbonDirection) -> WaddleCarbonMarker {
    WaddleCarbonMarker {
        sent: direction == messaging::CarbonDirection::Sent,
        received: direction == messaging::CarbonDirection::Received,
    }
}

pub(crate) fn pubsub_event_to_js(event: waddle_xmpp_client::PubsubEvent) -> WaddlePubsubEvent {
    WaddlePubsubEvent {
        from: event.from.map(|jid| jid.to_string()),
        node: event.node,
        items: event
            .items
            .into_iter()
            .map(|item| WaddlePubsubEventItem {
                id: item.id,
                retracted: item.retracted,
                payload: pubsub_payload_to_js(item.payload),
            })
            .collect(),
    }
}

fn pubsub_payload_to_js(
    payload: waddle_xmpp_client::PubsubEventPayload,
) -> WaddlePubsubEventPayload {
    match payload {
        waddle_xmpp_client::PubsubEventPayload::AttachmentSummary(summary) => {
            WaddlePubsubEventPayload::AttachmentSummary {
                reactions: summary
                    .reactions
                    .into_iter()
                    .map(|reaction| WaddlePubsubReactionSummary {
                        emoji: reaction.emoji,
                        count: reaction.count,
                        reactors: reaction
                            .reactors
                            .into_iter()
                            .map(|reactor| reactor.to_string())
                            .collect(),
                    })
                    .collect(),
                noticed_count: summary.noticed_count,
            }
        }
        waddle_xmpp_client::PubsubEventPayload::Opaque { element } => {
            WaddlePubsubEventPayload::Opaque {
                xml: String::from(&element),
            }
        }
        waddle_xmpp_client::PubsubEventPayload::Empty => WaddlePubsubEventPayload::Empty,
    }
}

fn inbox_entry_to_js(
    entry: waddle_xmpp_client::inbox::InboxStreamEntry,
) -> WaddleInboxConversation {
    WaddleInboxConversation {
        partner: entry.partner,
        kind: entry.kind.unwrap_or_else(|| "direct".to_string()),
        last_stanza_id: entry.last_stanza_id,
        last_updated: entry.last_updated.unwrap_or(0),
        unread: entry.unread,
        preview: entry.preview,
        thread: entry.thread_id,
        thread_title: entry.thread_title,
        reply_count: entry.reply_count,
        author: entry.author,
    }
}

pub(crate) fn inbox_push_to_js(
    entry: waddle_xmpp_client::inbox::InboxStreamEntry,
) -> WaddleMessage {
    WaddleMessage {
        id: None,
        from: None,
        to: None,
        body: None,
        subject: None,
        message_type: "headline".to_string(),
        timestamp: None,
        stanza_id: None,
        stanza_id_by: None,
        stanza_ids: Vec::new(),
        origin_id: None,
        replaces_id: None,
        retracts_id: None,
        retraction_id: None,
        is_retracted: false,
        moderation_target_id: None,
        moderated_by: None,
        moderation_reason: None,
        chat_state: None,
        displayed_marker_requested: false,
        displayed_marker_id: None,
        reaction_target_id: None,
        reaction_emojis: Vec::new(),
        in_call: None,
        is_muc: false,
        thread: None,
        parent_thread_id: None,
        reply_to_id: None,
        reply_to_sender: None,
        reply_fallback_start: None,
        reply_fallback_end: None,
        markup_spans: Vec::new(),
        broadcast_mention: None,
        mention_uris: Vec::new(),
        references: Vec::new(),
        forum_post_kind: None,
        forum_title: None,
        forum_thread_title: None,
        is_sticker: false,
        call_thread: None,
        call_thread_ended: None,
        shared_files: Vec::new(),
        link_previews: Vec::new(),
        pin_event: None,
        extension_envelope: None,
        extension_body_fallback: false,
        pubsub_events: Vec::new(),
        inbox_push: Some(inbox_entry_to_js(entry)),
        carbon: None,
    }
}

pub(crate) fn pin_event_to_js(event: PinEvent) -> WaddlePinEvent {
    WaddlePinEvent {
        action: match event.action {
            PinEventAction::Pinned => "pinned".to_string(),
            PinEventAction::Unpinned => "unpinned".to_string(),
        },
        target_stanza_id: event.target_stanza_id,
        by: event.by,
        reason: event.reason,
        preview: event.preview.map(pin_preview_to_js),
    }
}

pub(crate) fn pin_preview_to_js(preview: PinPreview) -> WaddlePinPreview {
    WaddlePinPreview {
        author_jid: preview.author_jid,
        author_nick: preview.author_nick,
        text: preview.text,
        message_timestamp: preview.message_timestamp.to_rfc3339(),
    }
}

pub(crate) fn pin_entry_to_js(entry: PinEntry) -> WaddlePinEntry {
    WaddlePinEntry {
        target_stanza_id: entry.target_stanza_id,
        pinner_jid: entry.pinner_jid,
        pinned_at: entry.pinned_at.to_rfc3339(),
        preview: pin_preview_to_js(entry.preview),
    }
}

pub(crate) fn archived_to_js(archived: ArchivedMessage) -> Option<WaddleArchivedMessage> {
    let stanza_id_by = archived.stanza_id.as_ref().map(|id| id.by.to_string());
    let stanza_ids = archived
        .stanza_ids
        .iter()
        .map(|stanza_id| WaddleStanzaId {
            id: stanza_id.id.clone(),
            by: stanza_id.by.to_string(),
        })
        .collect();
    let parsed = archived.payload.message.as_deref();
    let call_event = archived
        .payload
        .call
        .as_ref()
        .map(|call| call_event_to_js((**call).clone()));
    if parsed.is_none() && call_event.is_none() {
        return None;
    }
    let (reply_fallback_start, reply_fallback_end) = parsed
        .and_then(|message| message.reply_fallback)
        .map(|(start, end)| (Some(start), Some(end)))
        .unwrap_or((None, None));
    let forum_thread_title = parsed.and_then(|message| {
        if message.forum_post_kind.as_deref() == Some("topic") {
            message
                .forum_title
                .clone()
                .or_else(|| message.subject.clone())
        } else {
            None
        }
    });

    Some(WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        id: archived.id.map(|id| id.to_string()),
        stanza_id: archived.stanza_id.map(|id| id.to_string()),
        stanza_id_by,
        stanza_ids,
        origin_id: archived.origin_id.map(|id| id.to_string()),
        timestamp: archived.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        subject: parsed.and_then(|message| message.subject.clone()),
        replaces_id: parsed.and_then(|message| message.replaces_id.clone()),
        retracts_id: parsed.and_then(|message| message.retracts_id.clone()),
        retraction_id: parsed.and_then(|message| message.retraction_id.clone()),
        is_retracted: parsed.is_some_and(|message| message.is_retracted),
        moderation_target_id: parsed.and_then(|message| message.moderation_target_id.clone()),
        moderated_by: parsed
            .and_then(|message| message.moderated_by.as_ref().map(|jid| jid.to_string())),
        moderation_reason: parsed.and_then(|message| message.moderation_reason.clone()),
        reaction_target_id: parsed.and_then(|message| message.reaction_target_id.clone()),
        reaction_emojis: parsed
            .map(|message| message.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed.and_then(|message| message.reply_to_id.clone()),
        reply_to_sender: parsed.and_then(|message| message.reply_to_sender.clone()),
        reply_fallback_start,
        reply_fallback_end,
        markup_spans: parsed
            .map(|message| markup_spans_to_js(message.markup_spans.clone()))
            .unwrap_or_default(),
        broadcast_mention: parsed.and_then(|message| message.broadcast_mention.clone()),
        mention_uris: parsed
            .map(|message| message.mention_uris.clone())
            .unwrap_or_default(),
        references: parsed
            .map(|message| references_to_js(message.references.clone()))
            .unwrap_or_default(),
        forum_post_kind: parsed.and_then(|message| message.forum_post_kind.clone()),
        forum_title: parsed.and_then(|message| message.forum_title.clone()),
        forum_thread_title,
        is_sticker: parsed.is_some_and(|message| message.is_sticker),
        author_real_jid: archived.author_real_jid,
        call_thread: parsed
            .and_then(|message| message.call_thread.clone())
            .map(call_thread_to_js),
        call_thread_ended: parsed
            .and_then(|message| message.call_thread_ended.clone())
            .map(call_thread_ended_to_js),
        shared_files: parsed
            .map(|message| {
                message
                    .shared_files
                    .iter()
                    .cloned()
                    .map(shared_file_to_js)
                    .collect()
            })
            .unwrap_or_default(),
        link_previews: parsed
            .map(|message| link_previews_to_js(message.link_previews.clone()))
            .unwrap_or_default(),
        extension_envelope: parsed
            .and_then(|message| message.extension_envelope.clone())
            .map(extension_envelope_to_js),
        extension_body_fallback: parsed.is_some_and(|message| message.extension_body_fallback),
        call_event,
    })
}

pub(crate) fn mam_page_to_js(page: waddle_xmpp_client::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page
            .messages
            .into_iter()
            .filter_map(archived_to_js)
            .collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

pub(crate) fn inbox_page_to_js(page: crate::state::InboxPage) -> WaddleInboxResult {
    let conversations = page.entries.into_iter().map(inbox_entry_to_js).collect();
    WaddleInboxResult {
        // `all-unread` is the XEP-0430 server-wide unread sum; mirror it
        // under the legacy `total_unread` field the chat layer reads so
        // the JS shape stays additive on this change.
        total_unread: page.fin.counts.all_unread,
        total: page.fin.counts.total,
        unread_conversations: page.fin.counts.unread,
        conversations,
    }
}

pub(crate) fn shared_file_to_js(file: messaging::SharedFile) -> WaddleSharedFile {
    WaddleSharedFile {
        url: file.url,
        name: file.name,
        media_type: file.media_type,
        size: file.size,
        width: file.width,
        height: file.height,
        hashes: file
            .hashes
            .into_iter()
            .map(|hash| WaddleEncryptedFileHash {
                algo: hash.algo.as_str().to_string(),
                value_b64: hash.value_b64,
            })
            .collect(),
        disposition: file.disposition.as_str().to_string(),
        encrypted: file.encrypted.map(encrypted_file_to_js),
    }
}

fn link_previews_to_js(previews: Vec<messaging::LinkPreviewData>) -> Vec<WaddleLinkPreview> {
    previews
        .into_iter()
        .map(|preview| WaddleLinkPreview {
            original_url: preview.original_url.to_string(),
            normalized_url: preview.normalized_url.map(|url| url.to_string()),
            title: preview.title,
            description: preview.description,
            remote_media_unavailable: preview.remote_media_unavailable,
            image: preview.image.map(|image| WaddleLinkPreviewImage {
                url: image.url.to_string(),
                media_type: image.media_type.as_str().to_string(),
                width: image.width,
                height: image.height,
                alt: image.alt,
            }),
            video: preview.video.map(|video| WaddleLinkPreviewVideo {
                url: video.url.to_string(),
                media_type: video.media_type.as_str().to_string(),
            }),
            player_embed: preview.player_embed.map(|player| WaddleLinkPreviewPlayer {
                url: player.url.to_string(),
                width: player.width,
                height: player.height,
            }),
        })
        .collect()
}

pub(crate) fn call_event_to_js(event: InboundCallEvent) -> WaddleCallEvent {
    use waddle_xmpp_client::messaging::CallEventKind;
    let from = event.from.to_string();
    let to = event.to.map(|jid| jid.to_string());
    // The JS surface keeps `sid` as a plain string — boundary
    // serialization is the explicit I/O exception to the typed-
    // payloads rule. Convert the typed `SessionId` once here.
    let sid = event.sid.0;
    match event.kind {
        CallEventKind::Propose { media } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "propose",
            media: Some(WaddleCallMedia {
                audio: media.audio,
                video: media.video,
            }),
            join: None,
            reason: None,
            tie_break: None,
            migrated_to: None,
        },
        CallEventKind::Proceed => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "proceed",
            media: None,
            join: None,
            reason: None,
            tie_break: None,
            migrated_to: None,
        },
        CallEventKind::Ringing => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "ringing",
            media: None,
            join: None,
            reason: None,
            tie_break: None,
            migrated_to: None,
        },
        CallEventKind::Reject { reason, tie_break } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "reject",
            media: None,
            join: None,
            reason: reason
                .map(|r| waddle_xmpp_client::messaging::jingle_reason_wire_name(r).to_string()),
            tie_break: Some(tie_break),
            migrated_to: None,
        },
        CallEventKind::Retract { reason, tie_break } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "retract",
            media: None,
            join: None,
            reason: reason
                .map(|r| waddle_xmpp_client::messaging::jingle_reason_wire_name(r).to_string()),
            tie_break: Some(tie_break),
            migrated_to: None,
        },
        CallEventKind::Finish {
            reason,
            migrated_to,
        } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "finish",
            media: None,
            join: None,
            reason: reason
                .map(|r| waddle_xmpp_client::messaging::jingle_reason_wire_name(r).to_string()),
            tie_break: None,
            migrated_to: migrated_to.map(|sid| sid.0),
        },
        CallEventKind::SessionInitiate { join, media } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "session-initiate",
            media: Some(WaddleCallMedia {
                audio: media.audio,
                video: media.video,
            }),
            join: Some(WaddleLiveKitJoin {
                url: join.url,
                room: join.room,
                identity: join.identity,
                token: join.token,
            }),
            reason: None,
            tie_break: None,
            migrated_to: None,
        },
        CallEventKind::SessionAccept { join, media } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "session-accept",
            media: Some(WaddleCallMedia {
                audio: media.audio,
                video: media.video,
            }),
            join: Some(WaddleLiveKitJoin {
                url: join.url,
                room: join.room,
                identity: join.identity,
                token: join.token,
            }),
            reason: None,
            tie_break: None,
            migrated_to: None,
        },
        CallEventKind::SessionTerminate { reason } => WaddleCallEvent {
            from,
            to,
            sid,
            kind: "session-terminate",
            media: None,
            join: None,
            // Surface the typed reason as its canonical XEP-0166
            // §7.4 wire name. The crate boundary into JS can only
            // carry strings, so the typed-payloads guarantee ends
            // here — but the string is produced from a typed enum,
            // not a free-form passthrough.
            reason: reason
                .map(|r| waddle_xmpp_client::messaging::jingle_reason_wire_name(r).to_string()),
            tie_break: None,
            migrated_to: None,
        },
    }
}

pub(crate) fn presence_to_js(presence: InboundPresence) -> WaddlePresence {
    WaddlePresence {
        from: presence.from,
        to: presence.to,
        presence_type: presence
            .presence_type
            .unwrap_or_else(|| "available".to_string()),
        show: presence.show,
        status: presence.status,
        hats: presence
            .hats
            .into_iter()
            .map(|hat| WaddlePresenceHat {
                uri: hat.uri,
                title: hat.title,
            })
            .collect(),
        muc_affiliation: presence.muc_affiliation.map(muc_affiliation_to_string),
        muc_role: presence.muc_role.map(muc_role_to_string),
        muc_jid: presence.muc_jid,
        muc_status_codes: presence
            .muc_status
            .iter()
            .map(|status| status.code())
            .collect(),
        vcard_avatar: presence.vcard_avatar,
        muji: presence.muji.map(|m| WaddleMujiPresence {
            preparing: m.preparing,
            active: m.active,
            audio: m.audio,
            video: m.video,
        }),
        hand_raised: presence.hand_raised,
        muted: presence.muted,
        // Typed instant → xs:dateTime only at this JS boundary (typed-payloads).
        idle_since: presence.idle_since.map(|since| since.to_rfc3339()),
        error_condition: presence.error.as_ref().map(|err| err.condition.clone()),
        error_type: presence
            .error
            .as_ref()
            .map(|err| err.error_type.as_str().to_string()),
        error_text: presence.error.and_then(|err| err.text),
    }
}

pub(crate) fn mds_entry_to_js(entry: MdsCatchupEntry) -> WaddleMdsDisplayedEntry {
    WaddleMdsDisplayedEntry {
        chat_id: entry.chat_id.to_string(),
        stanza_id: entry.stanza_id.as_str().to_string(),
        stanza_id_by: entry.stanza_id_by.to_string(),
    }
}

pub(crate) fn muc_affiliation_to_string(value: MucAffiliation) -> String {
    match value {
        MucAffiliation::Owner => "owner",
        MucAffiliation::Admin => "admin",
        MucAffiliation::Member => "member",
        MucAffiliation::Outcast => "outcast",
        MucAffiliation::None => "none",
    }
    .to_string()
}

pub(crate) fn muc_role_to_string(value: MucRole) -> String {
    match value {
        MucRole::Moderator => "moderator",
        MucRole::Participant => "participant",
        MucRole::Visitor => "visitor",
        MucRole::None => "none",
    }
    .to_string()
}

#[cfg(test)]
mod inbound_to_js_tests {
    use super::*;
    use minidom::Element;
    use waddle_xmpp_client::inbox::{InboxStreamEntry, InboxStreamEntrySource};

    fn sid(value: &str) -> messaging::SessionId {
        messaging::SessionId(value.to_string())
    }

    fn parse_jmi(jmi: Element) -> messaging::InboundCallEvent {
        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .append(jmi)
            .build();
        messaging::parse_call_event(&stanza).expect("JMI event parses")
    }

    #[test]
    fn inbox_push_to_js_serializes_inbox_push_payload() {
        let js = inbox_push_to_js(InboxStreamEntry {
            source: InboxStreamEntrySource::Push,
            query_id: None,
            partner: "room@muc.example.com".to_string(),
            kind: Some("muc".to_string()),
            last_stanza_id: "sid-1".to_string(),
            last_updated: Some(1_700_000_001),
            unread: 2,
            preview: Some("new message".to_string()),
            thread_id: Some("thread-1".to_string()),
            thread_title: Some("Planning".to_string()),
            reply_count: Some(5),
            author: Some("bob".to_string()),
        });
        let value = serde_json::to_value(js).expect("serializes");
        let push = value
            .get("inbox_push")
            .and_then(serde_json::Value::as_object)
            .expect("inbox push payload");

        assert_eq!(
            value
                .get("message_type")
                .and_then(serde_json::Value::as_str),
            Some("headline")
        );
        assert_eq!(
            push.get("partner").and_then(serde_json::Value::as_str),
            Some("room@muc.example.com")
        );
        assert_eq!(
            push.get("kind").and_then(serde_json::Value::as_str),
            Some("muc")
        );
        assert_eq!(
            push.get("last_stanza_id")
                .and_then(serde_json::Value::as_str),
            Some("sid-1")
        );
        assert_eq!(
            push.get("unread").and_then(serde_json::Value::as_u64),
            Some(2)
        );
        assert_eq!(
            push.get("thread").and_then(serde_json::Value::as_str),
            Some("thread-1")
        );
    }

    #[test]
    fn pubsub_event_to_js_serializes_typed_and_opaque_payloads() {
        let event = waddle_xmpp_client::PubsubEvent {
            from: Some("community.example.com".parse().expect("valid event jid")),
            node: "urn:xmpp:pubsub-attachments:summary:1/urn:xmpp:pubsub-social-feed:stories:0"
                .to_string(),
            items: vec![
                waddle_xmpp_client::PubsubEventItem {
                    id: Some("story-1".to_string()),
                    retracted: false,
                    payload: waddle_xmpp_client::PubsubEventPayload::AttachmentSummary(
                        waddle_xmpp_client::AttachmentSummaryEvent {
                            reactions: vec![waddle_xmpp_client::ReactionSummaryEvent {
                                emoji: "👍".to_string(),
                                count: 2,
                                reactors: vec!["alice@example.com"
                                    .parse()
                                    .expect("valid reactor jid")],
                            }],
                            noticed_count: Some(1),
                        },
                    ),
                },
                waddle_xmpp_client::PubsubEventItem {
                    id: Some("future".to_string()),
                    retracted: false,
                    payload: waddle_xmpp_client::PubsubEventPayload::Opaque {
                        element: Element::builder("future", "urn:example:future")
                            .append("payload")
                            .build(),
                    },
                },
                waddle_xmpp_client::PubsubEventItem {
                    id: Some("empty".to_string()),
                    retracted: true,
                    payload: waddle_xmpp_client::PubsubEventPayload::Empty,
                },
            ],
        };

        let value = serde_json::to_value(pubsub_event_to_js(event)).expect("serializes");
        let items = value
            .get("items")
            .and_then(serde_json::Value::as_array)
            .expect("items array");
        assert_eq!(
            value.get("from").and_then(serde_json::Value::as_str),
            Some("community.example.com")
        );
        assert_eq!(
            items[0]
                .get("payload")
                .and_then(|payload| payload.get("kind"))
                .and_then(serde_json::Value::as_str),
            Some("attachmentSummary")
        );
        assert_eq!(
            items[0]
                .get("payload")
                .and_then(|payload| payload.get("noticedCount"))
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            items[0]
                .get("payload")
                .and_then(|payload| payload.get("reactions"))
                .and_then(serde_json::Value::as_array)
                .and_then(|reactions| reactions.first())
                .and_then(|reaction| reaction.get("reactors"))
                .and_then(serde_json::Value::as_array)
                .and_then(|reactors| reactors.first())
                .and_then(serde_json::Value::as_str),
            Some("alice@example.com")
        );
        let opaque_xml = items[1]
            .get("payload")
            .and_then(|payload| payload.get("xml"))
            .and_then(serde_json::Value::as_str)
            .expect("opaque xml");
        assert!(opaque_xml.contains("urn:example:future"), "{opaque_xml}");
        assert_eq!(
            items[2]
                .get("retracted")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            items[2]
                .get("payload")
                .and_then(|payload| payload.get("kind"))
                .and_then(serde_json::Value::as_str),
            Some("empty")
        );
    }

    fn parse_jmi_to(jmi: Element, to: &str) -> messaging::InboundCallEvent {
        let stanza = Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
            .append(jmi)
            .build();
        messaging::parse_call_event(&stanza).expect("JMI event parses")
    }

    fn parse_message_element(xml: &str) -> InboundMessage {
        let el: Element = xml.parse().expect("invalid XML");
        match messaging::parse(&el).expect("expected message") {
            messaging::MessagingEvent::Message(msg) => *msg,
            _ => panic!("expected MessagingEvent::Message"),
        }
    }

    fn parse_mam_archived(xml: &str) -> waddle_xmpp_client::ArchivedMessage {
        let el: Element = xml.parse().expect("invalid XML");
        waddle_xmpp_client::mam::parse_mam_result(&el).expect("expected MAM result")
    }

    #[test]
    fn call_event_to_js_preserves_jmi_tie_break_metadata() {
        let reject = call_event_to_js(parse_jmi(messaging::build_reject_with_options(
            &sid("c1"),
            Some(messaging::JingleReason::Expired),
            true,
        )));
        assert_eq!(reject.kind, "reject");
        assert_eq!(reject.reason.as_deref(), Some("expired"));
        assert_eq!(reject.tie_break, Some(true));
        assert_eq!(reject.migrated_to, None);

        let retract = call_event_to_js(parse_jmi(messaging::build_retract_with_options(
            &sid("c2"),
            Some(messaging::JingleReason::Expired),
            true,
        )));
        assert_eq!(retract.kind, "retract");
        assert_eq!(retract.reason.as_deref(), Some("expired"));
        assert_eq!(retract.tie_break, Some(true));
        assert_eq!(retract.migrated_to, None);
    }

    #[test]
    fn call_event_to_js_preserves_jmi_to_peer() {
        let propose = call_event_to_js(parse_jmi_to(
            messaging::build_propose(&sid("c1"), messaging::CallMedia::audio_only()),
            "bob@waddle.test/phone",
        ));

        assert_eq!(propose.kind, "propose");
        assert_eq!(propose.to.as_deref(), Some("bob@waddle.test/phone"));
    }

    #[test]
    fn presence_to_js_maps_muc_status_codes_to_numbers() {
        use waddle_xmpp_client::messaging::MucStatus;
        let presence = InboundPresence {
            from: Some("room@muc.test/alice".to_string()),
            to: None,
            presence_type: None,
            status: None,
            show: None,
            hats: vec![],
            muc_affiliation: None,
            muc_role: None,
            muc_jid: None,
            muc_status: vec![
                MucStatus::NonAnonymous,
                MucStatus::SelfPresence,
                MucStatus::Other(210),
            ],
            vcard_avatar: None,
            muji: None,
            hand_raised: false,
            muted: false,
            idle_since: None,
            error: None,
        };
        let js = presence_to_js(presence);
        assert_eq!(js.muc_status_codes, vec![100, 110, 210]);
    }

    #[test]
    fn call_event_to_js_preserves_finish_migration_metadata() {
        let finish = call_event_to_js(parse_jmi(messaging::build_finish_migrated(
            &sid("old"),
            messaging::JingleReason::Expired,
            &sid("new"),
        )));
        assert_eq!(finish.kind, "finish");
        assert_eq!(finish.reason.as_deref(), Some("expired"));
        assert_eq!(finish.tie_break, None);
        assert_eq!(finish.migrated_to.as_deref(), Some("new"));
    }

    #[test]
    fn archived_to_js_preserves_jmi_only_call_events_for_dm_reload() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-call' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-25T10:00:00Z'/>\
                   <message xmlns='jabber:client' type='chat' id='call-propose' \
                            from='bob@waddle.test/phone' to='alice@waddle.test/web'>\
                     <propose xmlns='urn:xmpp:jingle-message:0' id='call-1'>\
                       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>\
                     </propose>\
                     <store xmlns='urn:xmpp:hints'/>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("JMI-only MAM row should convert");

        assert_eq!(js.mam_id, "mam-call");
        assert_eq!(js.body, None);
        let call_event = js.call_event.expect("call event should be present");
        assert_eq!(call_event.kind, "propose");
        assert_eq!(call_event.sid, "call-1");
        assert_eq!(call_event.from, "bob@waddle.test/phone");
        assert!(call_event.media.expect("media").audio);
    }

    #[test]
    fn archived_to_js_preserves_call_thread_anchor_for_room_reload() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-anchor' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-06-07T14:30:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='anchor-1' \
                            from='general@muc.waddle.test' to='alice@waddle.test/web'>\
                     <body>Alice started a call</body>\
                     <thread>call-thread-uuid</thread>\
                     <call-thread xmlns='urn:waddle:call-thread:0' \
                                  kind='muc' \
                                  sid='session-uuid' \
                                  media='audio video' \
                                  initiator='alice@waddle.test' \
                                  started='2026-06-07T14:30:00Z'/>\
                     <store xmlns='urn:xmpp:hints'/>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("call-thread MAM row should convert");
        let anchor = js
            .call_thread
            .expect("call-thread should survive archive conversion");

        assert_eq!(js.mam_id, "mam-anchor");
        assert_eq!(js.thread.as_deref(), Some("call-thread-uuid"));
        assert_eq!(anchor.kind, "muc");
        assert_eq!(anchor.sid, "session-uuid");
        assert_eq!(anchor.media, vec!["audio".to_owned(), "video".to_owned()]);
        assert_eq!(anchor.initiator, "alice@waddle.test");
        assert_eq!(anchor.started, "2026-06-07T14:30:00+00:00");
    }

    #[test]
    fn archived_to_js_preserves_call_thread_ended_for_room_reload() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-ended' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-06-07T14:35:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='ended-1' \
                            from='general@muc.waddle.test' to='alice@waddle.test/web'>\
                     <apply-to xmlns='urn:xmpp:fasten:0' id='anchor-stanza-id'>\
                       <call-thread-ended xmlns='urn:waddle:call-thread:0' \
                                          ended='2026-06-07T14:35:00Z' \
                                          duration='PT5M'/>\
                     </apply-to>\
                     <store xmlns='urn:xmpp:hints'/>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("call-thread-ended MAM row should convert");
        let ended = js
            .call_thread_ended
            .expect("call-thread-ended should survive archive conversion");

        assert_eq!(js.mam_id, "mam-ended");
        assert_eq!(ended.anchor_id, "anchor-stanza-id");
        assert_eq!(ended.ended, "2026-06-07T14:35:00+00:00");
        assert_eq!(ended.duration, "PT5M");
    }

    #[test]
    fn inbound_to_js_propagates_data_reference_with_anchor() {
        let inbound = parse_message_element(
            "<message xmlns='jabber:client' type='groupchat' id='m-data'>\
               <body>see https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com' begin='4' end='23' \
                  anchor='https://example.com'/>\
             </message>",
        );

        let js = inbound_to_js(inbound);

        assert_eq!(js.references.len(), 1);
        let reference = &js.references[0];
        assert_eq!(reference.ref_type, "data");
        assert_eq!(reference.uri, "https://example.com");
        assert_eq!(reference.begin, 4);
        assert_eq!(reference.end, 23);
        assert_eq!(reference.anchor.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn archived_to_js_propagates_mam_data_references_for_reload_rendering() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-1' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='m-data' \
                            from='room@conf.example/alice'>\
                     <body>see https://example.com</body>\
                     <reference xmlns='urn:xmpp:reference:0' type='data' \
                        uri='https://example.com/' begin='4' end='23'/>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("valid archived message should convert");

        assert_eq!(js.references.len(), 1);
        let reference = &js.references[0];
        assert_eq!(reference.ref_type, "data");
        assert_eq!(reference.uri, "https://example.com/");
        assert_eq!(reference.begin, 4);
        assert_eq!(reference.end, 23);
        assert!(reference.anchor.is_none());
    }

    #[test]
    fn archived_to_js_propagates_xep0511_link_previews_for_reload_rendering() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-link' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='m-link' \
                            from='room@conf.example/alice'>\
                     <body>see https://the.link.example/what-was-linked</body>\
                     <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example/what-was-linked'>\
                       <og:title>The Best Webpage</og:title>\
                       <og:description>Plain text preview</og:description>\
                       <og:url>https://the.link.example/what-was-linked</og:url>\
                       <og:image>https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png</og:image>\
                       <ogi:type>image/png</ogi:type>\
                       <ogi:width>640</ogi:width>\
                       <ogi:height>360</ogi:height>\
                       <ogi:alt>Article screenshot</ogi:alt>\
                     </rdf:Description>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("valid archived message should convert");

        assert_eq!(js.link_previews.len(), 1);
        let preview = &js.link_previews[0];
        assert_eq!(
            preview.original_url,
            "https://the.link.example/what-was-linked"
        );
        assert_eq!(
            preview.normalized_url.as_deref(),
            Some("https://the.link.example/what-was-linked")
        );
        assert_eq!(preview.title.as_deref(), Some("The Best Webpage"));
        assert_eq!(preview.description.as_deref(), Some("Plain text preview"));
        let image = preview.image.as_ref().expect("cached image");
        assert_eq!(
            image.url,
            "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"
        );
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.width, Some(640));
        assert_eq!(image.height, Some(360));
        assert_eq!(image.alt.as_deref(), Some("Article screenshot"));
        assert!(!preview.remote_media_unavailable);
    }

    #[test]
    fn archived_to_js_marks_remote_xep0511_media_unavailable() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-link-remote-media' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='m-link' \
                            from='room@conf.example/alice'>\
                     <body>see https://the.link.example/what-was-linked</body>\
                     <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example/what-was-linked'>\
                       <og:title>The Best Webpage</og:title>\
                       <og:image>https://remote.example/preview.png</og:image>\
                       <ogi:type>image/png</ogi:type>\
                     </rdf:Description>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("valid archived message should convert");

        assert_eq!(js.link_previews.len(), 1);
        let preview = &js.link_previews[0];
        assert_eq!(preview.title.as_deref(), Some("The Best Webpage"));
        assert!(preview.image.is_none());
        assert!(preview.remote_media_unavailable);
    }

    #[test]
    fn archived_to_js_keeps_bodyless_xep0511_link_preview_rows() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-link-bodyless' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='m-link' \
                            from='room@conf.example/alice'>\
                     <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://example.com/article'>\
                       <og:title>Example Article</og:title>\
                     </rdf:Description>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("bodyless archived preview should convert");

        assert_eq!(js.body, None);
        assert_eq!(js.link_previews.len(), 1);
        assert_eq!(
            js.link_previews[0].original_url,
            "https://example.com/article"
        );
        assert_eq!(
            js.link_previews[0].title.as_deref(),
            Some("Example Article")
        );
    }

    #[test]
    fn archived_to_js_preserves_direct_message_business_ids_for_xep0444_refresh() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-dm-original' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='chat' id='dm-original-wire-id' \
                            from='alice@example.com/desktop' to='bob@example.com'>\
                     <origin-id xmlns='urn:xmpp:sid:0' id='dm-original-origin-id'/>\
                     <body>direct reaction survives refresh</body>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let value = serde_json::to_value(
            archived_to_js(archived).expect("valid archived message should convert"),
        )
        .expect("archived DTO should serialize");

        assert_eq!(
            value.get("id").and_then(serde_json::Value::as_str),
            Some("dm-original-wire-id")
        );
        assert_eq!(
            value.get("origin_id").and_then(serde_json::Value::as_str),
            Some("dm-original-origin-id")
        );
    }

    #[test]
    fn inbound_to_js_serializes_moderation_broadcast_metadata() {
        let inbound = parse_message_element(
            "<message xmlns='jabber:client' type='groupchat' id='retraction-id-1' \
                      from='room@conf.example' to='room@conf.example/macbeth'>\
               <stanza-id xmlns='urn:xmpp:sid:0' id='retraction-room-sid' by='room@conf.example'/>\
               <retract xmlns='urn:xmpp:message-retract:1' id='target-room-sid'>\
                 <moderated xmlns='urn:xmpp:message-moderate:1' by='room@conf.example/moderator'/>\
                 <reason>spam</reason>\
               </retract>\
             </message>",
        );

        let js = inbound_to_js(inbound);

        assert_eq!(js.id.as_deref(), Some("retraction-id-1"));
        assert_eq!(js.retracts_id.as_deref(), Some("target-room-sid"));
        assert_eq!(js.moderation_target_id.as_deref(), Some("target-room-sid"));
        assert_eq!(
            js.moderated_by.as_deref(),
            Some("room@conf.example/moderator")
        );
        assert_eq!(js.moderation_reason.as_deref(), Some("spam"));
        assert_eq!(js.stanza_id.as_deref(), Some("retraction-room-sid"));
        assert_eq!(js.stanza_id_by.as_deref(), Some("room@conf.example"));
        assert_eq!(js.stanza_ids.len(), 1);
        assert_eq!(js.stanza_ids[0].id, "retraction-room-sid");
        assert_eq!(js.stanza_ids[0].by, "room@conf.example");
        assert!(!js.is_retracted);
    }

    #[test]
    fn archived_to_js_serializes_tombstone_stanza_ids_and_moderation_metadata() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-tombstone' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='target-message-id' \
                            from='room@conf.example/alice' to='room@conf.example/macbeth'>\
                     <stanza-id xmlns='urn:xmpp:sid:0' id='archive-sid' by='archive.example'/>\
                     <stanza-id xmlns='urn:xmpp:sid:0' id='room-sid' by='room@conf.example'/>\
                     <retracted xmlns='urn:xmpp:message-retract:1' id='retraction-message-id' \
                                stamp='2026-05-06T12:00:01Z'>\
                       <moderated xmlns='urn:xmpp:message-moderate:1' by='room@conf.example/moderator'/>\
                       <reason>spam</reason>\
                     </retracted>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let js = archived_to_js(archived).expect("valid archived tombstone should convert");

        assert_eq!(js.id.as_deref(), Some("target-message-id"));
        assert_eq!(js.stanza_id.as_deref(), Some("archive-sid"));
        assert_eq!(js.stanza_id_by.as_deref(), Some("archive.example"));
        assert_eq!(js.stanza_ids.len(), 2);
        assert_eq!(js.stanza_ids[0].id, "archive-sid");
        assert_eq!(js.stanza_ids[0].by, "archive.example");
        assert_eq!(js.stanza_ids[1].id, "room-sid");
        assert_eq!(js.stanza_ids[1].by, "room@conf.example");
        assert!(js.is_retracted);
        assert_eq!(js.retraction_id.as_deref(), Some("retraction-message-id"));
        assert_eq!(js.retracts_id, None);
        assert_eq!(js.moderation_target_id, None);
        assert_eq!(
            js.moderated_by.as_deref(),
            Some("room@conf.example/moderator")
        );
        assert_eq!(js.moderation_reason.as_deref(), Some("spam"));
        assert_eq!(js.body, None);
    }

    #[test]
    fn archived_to_js_discards_spoofed_moderation_with_body() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-spoofed-moderation' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='spoofed-moderation' \
                            from='room@conf.example/alice' to='room@conf.example/macbeth'>\
                     <body>this must not render as normal chat</body>\
                     <retract xmlns='urn:xmpp:message-retract:1' id='target-room-sid'>\
                       <moderated xmlns='urn:xmpp:message-moderate:1' by='room@conf.example/moderator'/>\
                       <reason>spam</reason>\
                     </retract>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        assert!(archived_to_js(archived).is_none());
    }

    /// XEP-0280 (#1243): the typed carbon direction crosses the JS
    /// boundary as `{ sent, received }` so the chat layer can dedupe and
    /// attribute unwrapped copies; direct messages carry no marker.
    #[test]
    fn inbound_to_js_serializes_carbon_direction_marker() {
        let mut sent = parse_message_element(
            "<message xmlns='jabber:client' type='chat' id='c1' \
                      from='alice@example.com/phone' to='bob@example.com'>\
               <body>sent elsewhere</body>\
             </message>",
        );
        sent.carbon = Some(messaging::CarbonDirection::Sent);
        let value = serde_json::to_value(inbound_to_js(sent)).expect("serializes");
        assert_eq!(
            value.get("carbon"),
            Some(&serde_json::json!({ "sent": true, "received": false }))
        );

        let mut received = parse_message_element(
            "<message xmlns='jabber:client' type='chat' id='c2' \
                      from='bob@example.com/desktop' to='alice@example.com/phone'>\
               <body>received elsewhere</body>\
             </message>",
        );
        received.carbon = Some(messaging::CarbonDirection::Received);
        let value = serde_json::to_value(inbound_to_js(received)).expect("serializes");
        assert_eq!(
            value.get("carbon"),
            Some(&serde_json::json!({ "sent": false, "received": true }))
        );

        let direct = parse_message_element(
            "<message xmlns='jabber:client' type='chat' id='c3' \
                      from='bob@example.com/desktop' to='alice@example.com/phone'>\
               <body>direct</body>\
             </message>",
        );
        let value = serde_json::to_value(inbound_to_js(direct)).expect("serializes");
        assert_eq!(value.get("carbon"), None);
    }

    #[test]
    fn inbound_to_js_propagates_mention_reference_in_addition_to_mention_uris() {
        let inbound = parse_message_element(
            "<message xmlns='jabber:client' type='groupchat' id='m-mention'>\
               <body>hi @bob</body>\
               <reference xmlns='urn:xmpp:reference:0' type='mention' \
                  uri='xmpp:bob@example.com' begin='3' end='7'/>\
             </message>",
        );

        let js = inbound_to_js(inbound);

        assert_eq!(js.mention_uris, vec!["xmpp:bob@example.com".to_string()]);
        assert_eq!(js.references.len(), 1);
        assert_eq!(js.references[0].ref_type, "mention");
        assert_eq!(js.references[0].uri, "xmpp:bob@example.com");
        assert!(js.references[0].anchor.is_none());
    }

    #[test]
    fn references_to_js_preserves_order_type_and_anchor() {
        let references = references_to_js(vec![
            messaging::ReferenceData {
                ref_type: "data".to_string(),
                uri: "https://example.com".to_string(),
                begin: 4,
                end: 23,
                anchor: Some("example.com".to_string()),
            },
            messaging::ReferenceData {
                ref_type: "mention".to_string(),
                uri: "xmpp:bob@example.com".to_string(),
                begin: 0,
                end: 4,
                anchor: None,
            },
        ]);

        assert_eq!(references.len(), 2);
        assert_eq!(references[0].ref_type, "data");
        assert_eq!(references[0].anchor.as_deref(), Some("example.com"));
        assert_eq!(references[1].ref_type, "mention");
        assert!(references[1].anchor.is_none());
    }
}
