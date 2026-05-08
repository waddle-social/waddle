use jid::Jid;

use waddle_xmpp_client::{
    messaging::{self, SendMessageOptions},
    request::StanzaId,
    xep::{
        reply::{FallbackRange, ReplyMarker},
        thread::ThreadRef,
    },
    ClientEvent, InboundMessage, LifecycleEvent, MessageDeliveryEvent, MessagingEvent,
};

use crate::{
    WaddleArchivedMessage, WaddleChannel, WaddleEncryptedFile, WaddleEncryptedFileHash,
    WaddleEventListener, WaddleMamPage, WaddleMessage, WaddleMucAffiliation, WaddleMucRole,
    WaddlePresence, WaddlePresenceHat, WaddleSendOptions, WaddleSharedFile, WaddleSpace,
    WaddleTopology, WaddleUploadHeader, WaddleUploadSlot,
};

// ── Event dispatch ───────────────────────────────────────────────────────────

pub(super) fn dispatch_event(event: ClientEvent, listener: &dyn WaddleEventListener) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => listener.on_connected(),
        ClientEvent::Messaging(MessagingEvent::Message(msg)) => {
            listener.on_message(inbound_to_ffi(*msg));
        }
        ClientEvent::Messaging(MessagingEvent::Presence(pres)) => {
            listener.on_presence(WaddlePresence {
                from: pres.from,
                to: pres.to,
                presence_type: pres
                    .presence_type
                    .unwrap_or_else(|| "available".to_string()),
                show: pres.show,
                status: pres.status,
                hats: pres.hats.into_iter().map(presence_hat_to_ffi).collect(),
                muc_affiliation: pres.muc_affiliation.map(muc_affiliation_to_ffi),
                muc_role: pres.muc_role.map(muc_role_to_ffi),
            });
        }
        ClientEvent::MamResult(archived) => {
            listener.on_mam_result(archived_to_ffi(archived));
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            listener.on_message_delivery_acked(stanza_id.to_string());
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            listener.on_message_delivery_failed(stanza_id.to_string());
        }
        _ => {}
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the domain part from a JID like `user@domain` or `domain`.
pub(super) fn jid_domain(jid: &str) -> &str {
    jid.split('@').next_back().unwrap_or(jid)
}

pub(super) fn empty_topology() -> WaddleTopology {
    WaddleTopology {
        spaces: Vec::new(),
        channels: Vec::new(),
    }
}

pub(super) fn topology_to_ffi(
    topology: waddle_xmpp_client::discovery::DiscoveredTopology,
) -> WaddleTopology {
    WaddleTopology {
        spaces: topology
            .spaces
            .into_iter()
            .map(|space| WaddleSpace {
                id: space.id.as_str().to_string(),
                service_jid: space.service_jid.to_string(),
                name: space.name,
                description: space.description,
            })
            .collect(),
        channels: topology
            .channels
            .into_iter()
            .map(|channel| WaddleChannel {
                id: channel.id,
                room_jid: channel.room_jid.to_string(),
                name: channel.name,
                description: channel.description,
                channel_type: channel.channel_type.as_str().to_string(),
                position: channel.position,
                space_id: channel.space_id.as_str().to_string(),
            })
            .collect(),
    }
}

pub(super) fn mam_page_to_ffi(page: waddle_xmpp_client::mam::MamPage) -> WaddleMamPage {
    WaddleMamPage {
        messages: page.messages.into_iter().map(archived_to_ffi).collect(),
        first_id: page.rsm.first,
        last_id: page.rsm.last,
        is_complete: page.is_complete,
    }
}

fn shared_file_to_ffi(file: waddle_xmpp_client::messaging::SharedFile) -> WaddleSharedFile {
    WaddleSharedFile {
        url: file.url,
        name: file.name,
        media_type: file.media_type,
        size: file.size,
        width: file.width,
        height: file.height,
        disposition: file.disposition.as_str().to_string(),
        encrypted: file.encrypted.map(encrypted_file_to_ffi),
    }
}

fn encrypted_file_to_ffi(
    enc: waddle_xmpp_client::xep::encrypted_file::EncryptedFile,
) -> WaddleEncryptedFile {
    WaddleEncryptedFile {
        cipher: enc.cipher.as_uri().to_string(),
        key_b64: enc.key_b64,
        iv_b64: enc.iv_b64,
        hashes: enc
            .hashes
            .into_iter()
            .map(|h| WaddleEncryptedFileHash {
                algo: h.algo,
                value_b64: h.value_b64,
            })
            .collect(),
        sources: enc.sources,
    }
}

fn encrypted_file_from_ffi(
    enc: WaddleEncryptedFile,
) -> Result<waddle_xmpp_client::xep::encrypted_file::EncryptedFile, String> {
    use waddle_xmpp_client::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash};
    let cipher = Cipher::from_uri(&enc.cipher).ok_or_else(|| {
        format!(
            "encrypted attachment has unknown cipher: {cipher}",
            cipher = enc.cipher
        )
    })?;
    if enc.sources.is_empty() {
        return Err(
            "encrypted attachment has no sources; recipients would receive ciphertext with no decryption metadata"
                .to_string(),
        );
    }
    Ok(EncryptedFile {
        cipher,
        key_b64: enc.key_b64,
        iv_b64: enc.iv_b64,
        hashes: enc
            .hashes
            .into_iter()
            .map(|h| EncryptedHash {
                algo: h.algo,
                value_b64: h.value_b64,
            })
            .collect(),
        sources: enc.sources,
    })
}

pub(super) fn upload_slot_to_ffi(
    slot: waddle_xmpp_client::discovery::UploadSlot,
) -> WaddleUploadSlot {
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

fn presence_hat_to_ffi(hat: waddle_xmpp_client::messaging::PresenceHat) -> WaddlePresenceHat {
    WaddlePresenceHat {
        uri: hat.uri,
        title: hat.title,
    }
}

fn muc_affiliation_to_ffi(
    affiliation: waddle_xmpp_client::messaging::MucAffiliation,
) -> WaddleMucAffiliation {
    use waddle_xmpp_client::messaging::MucAffiliation;
    match affiliation {
        MucAffiliation::Owner => WaddleMucAffiliation::Owner,
        MucAffiliation::Admin => WaddleMucAffiliation::Admin,
        MucAffiliation::Member => WaddleMucAffiliation::Member,
        MucAffiliation::Outcast => WaddleMucAffiliation::Outcast,
        MucAffiliation::None => WaddleMucAffiliation::None,
    }
}

fn muc_role_to_ffi(role: waddle_xmpp_client::messaging::MucRole) -> WaddleMucRole {
    use waddle_xmpp_client::messaging::MucRole;
    match role {
        MucRole::Moderator => WaddleMucRole::Moderator,
        MucRole::Participant => WaddleMucRole::Participant,
        MucRole::Visitor => WaddleMucRole::Visitor,
        MucRole::None => WaddleMucRole::None,
    }
}

/// Convert a parsed inbound message into the UniFFI record, flattening the
/// XEP-0428 char range into two optional `u32` fields (UniFFI has no tuple
/// support).
fn inbound_to_ffi(msg: InboundMessage) -> WaddleMessage {
    let is_muc = msg.message_type == "groupchat";
    let (fb_start, fb_end) = match msg.reply_fallback {
        Some((s, e)) => (Some(s), Some(e)),
        None => (None, None),
    };
    WaddleMessage {
        id: msg.id,
        from: msg.from,
        to: msg.to,
        body: msg.body,
        message_type: msg.message_type,
        timestamp: msg.timestamp.map(|t| t.to_rfc3339()),
        stanza_id: msg.stanza_id,
        origin_id: msg.origin_id,
        replaces_id: msg.replaces_id,
        retracts_id: msg.retracts_id,
        reaction_target_id: msg.reaction_target_id,
        reaction_emojis: msg.reaction_emojis,
        is_muc,
        thread: msg.thread_id.or(msg.thread),
        parent_thread_id: msg.parent_thread_id,
        reply_to_id: msg.reply_to_id,
        reply_to_sender: msg.reply_to_sender,
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        shared_files: msg
            .shared_files
            .into_iter()
            .map(shared_file_to_ffi)
            .collect(),
    }
}

/// Convert an archived MAM message into the UniFFI record. Re-parses the
/// wrapped inner element through the full messaging parser so that replies
/// and fallback ranges survive history loads. The XEP-0201 nested-thread
/// `parent_thread_id` is read directly from the typed `archived` value
/// (the client parser extracts it via `crate::xep::thread::parse_thread`)
/// instead of being recovered from the re-parse - closes the parent-leak
/// path when `inner` is unparseable downstream.
fn archived_to_ffi(archived: waddle_xmpp_client::ArchivedMessage) -> WaddleArchivedMessage {
    let parsed = match messaging::parse(&archived.inner) {
        Some(MessagingEvent::Message(m)) => Some(m),
        _ => None,
    };
    let (fb_start, fb_end) = parsed
        .as_ref()
        .and_then(|m| m.reply_fallback)
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));
    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        stanza_id: archived.stanza_id,
        timestamp: archived.timestamp.map(|t| t.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        reaction_target_id: parsed.as_ref().and_then(|m| m.reaction_target_id.clone()),
        reaction_emojis: parsed
            .as_ref()
            .map(|m| m.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed.as_ref().and_then(|m| m.reply_to_id.clone()),
        reply_to_sender: parsed.as_ref().and_then(|m| m.reply_to_sender.clone()),
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        shared_files: parsed
            .as_ref()
            .map(|m| {
                m.shared_files
                    .clone()
                    .into_iter()
                    .map(shared_file_to_ffi)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(super) fn empty_mam_page() -> WaddleMamPage {
    WaddleMamPage {
        messages: vec![],
        first_id: None,
        last_id: None,
        is_complete: false,
    }
}

/// Convert the FFI options record into the typed `SendMessageOptions`. JIDs
/// are parsed here (the earliest boundary) so the rest of the send path flows
/// through typed values per the typed-payloads hard rule. Returns a
/// human-readable error string on malformed input - surfaced via the listener.
pub(super) fn send_options_from_ffi(opts: WaddleSendOptions) -> Result<SendMessageOptions, String> {
    let reply = match opts.reply {
        Some(target) => {
            let to = target
                .author_jid
                .parse::<Jid>()
                .map_err(|e| format!("Invalid reply author JID '{}': {e}", target.author_jid))?;
            Some(ReplyMarker {
                to,
                id: target.message_id,
            })
        }
        None => None,
    };

    let fallback = opts.fallback.map(|r| FallbackRange {
        start: r.start,
        end: r.end,
    });

    let thread = opts.thread.map(|t| ThreadRef {
        id: t.id,
        parent: t.parent,
    });

    // Surface invalid encryption metadata (unknown cipher, empty sources)
    // as an explicit error rather than silently sending a ciphertext URL
    // without the matching XEP-0448 envelope - that produces hard-to-debug
    // broken attachments for recipients.
    let shared_files = opts
        .shared_files
        .into_iter()
        .map(|file| {
            let disposition = messaging::SharedFileDisposition::from_text_or_infer(
                Some(file.disposition.as_str()),
                file.media_type.as_deref(),
            );
            let encrypted = file.encrypted.map(encrypted_file_from_ffi).transpose()?;
            Ok(messaging::SharedFile {
                url: file.url,
                name: file.name,
                media_type: file.media_type,
                size: file.size,
                width: file.width,
                height: file.height,
                disposition,
                encrypted,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let stanza_id = opts
        .stanza_id
        .map(StanzaId::new)
        .transpose()
        .map_err(|e| e.to_string())?;

    Ok(SendMessageOptions {
        stanza_id,
        reply,
        fallback,
        thread,
        shared_files,
        subject: None,
        markup_spans: vec![],
        references: vec![],
    })
}
