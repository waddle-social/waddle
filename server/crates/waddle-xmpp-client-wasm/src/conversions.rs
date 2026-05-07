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

    WaddleMessage {
        id: message.id,
        from: message.from,
        to: message.to,
        body: message.body,
        subject: message.subject,
        message_type: message.message_type.clone(),
        timestamp: message.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        stanza_id: message.stanza_id,
        origin_id: message.origin_id,
        replaces_id: message.replaces_id,
        retracts_id: message.retracts_id,
        moderation_target_id: message.moderation_target_id,
        moderated_by: message.moderated_by,
        moderation_reason: message.moderation_reason,
        chat_state: message.chat_state,
        displayed_marker_id: message.displayed_marker_id,
        reaction_target_id: message.reaction_target_id,
        reaction_emojis: message.reaction_emojis,
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
        shared_files: message
            .shared_files
            .into_iter()
            .map(shared_file_to_js)
            .collect(),
    }
}

pub(crate) fn archived_to_js(archived: ArchivedMessage) -> WaddleArchivedMessage {
    let parsed = match messaging::parse(&archived.inner) {
        Some(waddle_xmpp_client::MessagingEvent::Message(message)) => Some(message),
        _ => None,
    };
    let (reply_fallback_start, reply_fallback_end) = parsed
        .as_ref()
        .and_then(|message| message.reply_fallback)
        .map(|(start, end)| (Some(start), Some(end)))
        .unwrap_or((None, None));
    let forum_thread_title = parsed.as_ref().and_then(|message| {
        if message.forum_post_kind.as_deref() == Some("topic") {
            message
                .forum_title
                .clone()
                .or_else(|| message.subject.clone())
        } else {
            None
        }
    });

    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        stanza_id: archived.stanza_id,
        timestamp: archived.timestamp.map(|timestamp| timestamp.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        subject: parsed.as_ref().and_then(|message| message.subject.clone()),
        replaces_id: parsed
            .as_ref()
            .and_then(|message| message.replaces_id.clone()),
        retracts_id: parsed
            .as_ref()
            .and_then(|message| message.retracts_id.clone()),
        moderation_target_id: parsed
            .as_ref()
            .and_then(|message| message.moderation_target_id.clone()),
        moderated_by: parsed
            .as_ref()
            .and_then(|message| message.moderated_by.clone()),
        moderation_reason: parsed
            .as_ref()
            .and_then(|message| message.moderation_reason.clone()),
        reaction_target_id: parsed
            .as_ref()
            .and_then(|message| message.reaction_target_id.clone()),
        reaction_emojis: parsed
            .as_ref()
            .map(|message| message.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed
            .as_ref()
            .and_then(|message| message.reply_to_id.clone()),
        reply_to_sender: parsed
            .as_ref()
            .and_then(|message| message.reply_to_sender.clone()),
        reply_fallback_start,
        reply_fallback_end,
        markup_spans: parsed
            .as_ref()
            .map(|message| markup_spans_to_js(message.markup_spans.clone()))
            .unwrap_or_default(),
        broadcast_mention: parsed
            .as_ref()
            .and_then(|message| message.broadcast_mention.clone()),
        mention_uris: parsed
            .as_ref()
            .map(|message| message.mention_uris.clone())
            .unwrap_or_default(),
        references: parsed
            .as_ref()
            .map(|message| references_to_js(message.references.clone()))
            .unwrap_or_default(),
        forum_post_kind: parsed
            .as_ref()
            .and_then(|message| message.forum_post_kind.clone()),
        forum_title: parsed
            .as_ref()
            .and_then(|message| message.forum_title.clone()),
        forum_thread_title,
        is_sticker: parsed.as_ref().is_some_and(|message| message.is_sticker),
        author_real_jid: archived.author_real_jid,
        shared_files: parsed
            .as_ref()
            .map(|message| {
                message
                    .shared_files
                    .iter()
                    .cloned()
                    .map(shared_file_to_js)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn mam_page_to_js(page: waddle_xmpp_client::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page.messages.into_iter().map(archived_to_js).collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

pub(crate) fn inbox_result_to_js(result: discovery::WaddleInboxResult) -> WaddleInboxResult {
    WaddleInboxResult {
        total_unread: result.total_unread.unwrap_or(0),
        conversations: result
            .conversations
            .into_iter()
            .filter_map(|conversation| {
                Some(WaddleInboxConversation {
                    partner: conversation.partner,
                    kind: conversation.kind,
                    last_stanza_id: conversation.last_stanza_id?,
                    last_updated: i64::try_from(conversation.last_updated?).ok()?,
                    unread: conversation.unread,
                    preview: conversation.preview,
                    thread: conversation.thread,
                    thread_title: conversation.thread_title,
                    reply_count: conversation.reply_count,
                    author: conversation.author,
                })
            })
            .collect(),
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
        disposition: file.disposition.as_str().to_string(),
        encrypted: file.encrypted.map(encrypted_file_to_js),
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
        vcard_avatar: presence.vcard_avatar,
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

        let js = archived_to_js(archived);

        assert_eq!(js.references.len(), 1);
        let reference = &js.references[0];
        assert_eq!(reference.ref_type, "data");
        assert_eq!(reference.uri, "https://example.com/");
        assert_eq!(reference.begin, 4);
        assert_eq!(reference.end, 23);
        assert!(reference.anchor.is_none());
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
