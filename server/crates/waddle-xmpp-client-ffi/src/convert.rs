use jid::Jid;

use waddle_xmpp_client::{
    messaging::{
        self, CallEventKind, CallMedia, InboundCallEvent, JingleReason, LiveKitJoin,
        MdsDisplayedEntry, MujiPresence, SendMessageOptions,
    },
    request::StanzaId,
    xep::{
        reply::{FallbackRange, ReplyMarker},
        thread::ThreadRef,
    },
    ClientEvent, InboundMessage, LifecycleEvent, MessageDeliveryEvent, MessagingEvent,
};

use crate::{
    WaddleArchivedMessage, WaddleCallEvent, WaddleCallEventKind, WaddleCallMedia,
    WaddleCallThreadAnchor, WaddleEncryptedFile, WaddleEncryptedFileHash, WaddleEventListener,
    WaddleJingleReason, WaddleLiveKitJoin, WaddleMdsDisplayedEntry, WaddleMessage,
    WaddleMucAffiliation, WaddleMucRole, WaddleMujiPresence, WaddlePresence, WaddlePresenceHat,
    WaddleSendOptions, WaddleSharedFile,
};

// ── Event dispatch ───────────────────────────────────────────────────────────

pub(super) fn dispatch_event(
    event: ClientEvent,
    account_bare_jid: &str,
    listener: &dyn WaddleEventListener,
) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => listener.on_connected(),
        ClientEvent::Messaging(MessagingEvent::Message(msg)) => {
            listener.on_message(inbound_to_ffi(trusted_mds_message(*msg, account_bare_jid)));
        }
        ClientEvent::Messaging(MessagingEvent::Presence(pres)) => {
            let pres = *pres;
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
                muji: pres.muji.map(muji_presence_to_ffi),
            });
        }
        ClientEvent::MamResult(archived) => {
            listener.on_mam_result(archived_to_ffi(*archived));
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            listener.on_message_delivery_acked(stanza_id.to_string());
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            listener.on_message_delivery_failed(stanza_id.to_string());
        }
        ClientEvent::Call(call) => {
            listener.on_call(call_event_to_ffi(*call));
        }
        _ => {}
    }
}

fn trusted_mds_message(mut msg: InboundMessage, account_bare_jid: &str) -> InboundMessage {
    if msg.mds_displayed.is_some()
        && !waddle_xmpp_client::mds::mds_event_from_matches_account(
            msg.from.as_deref(),
            account_bare_jid,
        )
    {
        msg.mds_displayed = None;
    }
    msg
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

fn muji_presence_to_ffi(muji: MujiPresence) -> WaddleMujiPresence {
    WaddleMujiPresence {
        preparing: muji.preparing,
        active: muji.active,
        audio: muji.audio,
        video: muji.video,
    }
}

fn mds_entry_to_ffi(entry: MdsDisplayedEntry) -> WaddleMdsDisplayedEntry {
    WaddleMdsDisplayedEntry {
        chat_id: entry.chat_id.to_string(),
        stanza_id: entry.stanza_id.as_str().to_string(),
        stanza_id_by: entry.stanza_id_by.to_string(),
    }
}

fn call_media_to_ffi(media: CallMedia) -> WaddleCallMedia {
    WaddleCallMedia {
        audio: media.audio,
        video: media.video,
    }
}

fn livekit_join_to_ffi(join: LiveKitJoin) -> WaddleLiveKitJoin {
    WaddleLiveKitJoin {
        url: join.url,
        room: join.room,
        identity: join.identity,
        token: join.token,
    }
}

pub(super) fn jingle_reason_to_ffi(reason: JingleReason) -> WaddleJingleReason {
    match reason {
        JingleReason::AlternativeSession { .. } => WaddleJingleReason::AlternativeSession,
        JingleReason::Busy => WaddleJingleReason::Busy,
        JingleReason::Cancel => WaddleJingleReason::Cancel,
        JingleReason::ConnectivityError => WaddleJingleReason::ConnectivityError,
        JingleReason::Decline => WaddleJingleReason::Decline,
        JingleReason::Expired => WaddleJingleReason::Expired,
        JingleReason::FailedApplication => WaddleJingleReason::FailedApplication,
        JingleReason::FailedTransport => WaddleJingleReason::FailedTransport,
        JingleReason::GeneralError => WaddleJingleReason::GeneralError,
        JingleReason::Gone => WaddleJingleReason::Gone,
        JingleReason::IncompatibleParameters => WaddleJingleReason::IncompatibleParameters,
        JingleReason::MediaError => WaddleJingleReason::MediaError,
        JingleReason::SecurityError => WaddleJingleReason::SecurityError,
        JingleReason::Success => WaddleJingleReason::Success,
        JingleReason::Timeout => WaddleJingleReason::Timeout,
        JingleReason::UnsupportedApplications => WaddleJingleReason::UnsupportedApplications,
        JingleReason::UnsupportedTransports => WaddleJingleReason::UnsupportedTransports,
    }
}

pub(super) fn jingle_reason_from_ffi(reason: WaddleJingleReason) -> JingleReason {
    match reason {
        WaddleJingleReason::AlternativeSession => JingleReason::AlternativeSession { sid: None },
        WaddleJingleReason::Busy => JingleReason::Busy,
        WaddleJingleReason::Cancel => JingleReason::Cancel,
        WaddleJingleReason::ConnectivityError => JingleReason::ConnectivityError,
        WaddleJingleReason::Decline => JingleReason::Decline,
        WaddleJingleReason::Expired => JingleReason::Expired,
        WaddleJingleReason::FailedApplication => JingleReason::FailedApplication,
        WaddleJingleReason::FailedTransport => JingleReason::FailedTransport,
        WaddleJingleReason::GeneralError => JingleReason::GeneralError,
        WaddleJingleReason::Gone => JingleReason::Gone,
        WaddleJingleReason::IncompatibleParameters => JingleReason::IncompatibleParameters,
        WaddleJingleReason::MediaError => JingleReason::MediaError,
        WaddleJingleReason::SecurityError => JingleReason::SecurityError,
        WaddleJingleReason::Success => JingleReason::Success,
        WaddleJingleReason::Timeout => JingleReason::Timeout,
        WaddleJingleReason::UnsupportedApplications => JingleReason::UnsupportedApplications,
        WaddleJingleReason::UnsupportedTransports => JingleReason::UnsupportedTransports,
    }
}

/// Convert an [`InboundCallEvent`] into its FFI mirror. The
/// session-terminate reason is already a typed `JingleReason` at
/// this point (the upstream `messaging::call` parser handled the
/// untyped → typed transition at the wire boundary) so this layer
/// is a pure variant rewrite.
pub(super) fn call_event_to_ffi(event: InboundCallEvent) -> WaddleCallEvent {
    let kind = match event.kind {
        CallEventKind::Propose { media } => WaddleCallEventKind::Propose {
            media: call_media_to_ffi(media),
        },
        CallEventKind::Proceed => WaddleCallEventKind::Proceed,
        CallEventKind::Reject { reason, tie_break } => WaddleCallEventKind::Reject {
            reason: reason.map(jingle_reason_to_ffi),
            tie_break,
        },
        CallEventKind::Retract { reason, tie_break } => WaddleCallEventKind::Retract {
            reason: reason.map(jingle_reason_to_ffi),
            tie_break,
        },
        CallEventKind::Finish {
            reason,
            migrated_to,
        } => WaddleCallEventKind::Finish {
            reason: reason.map(jingle_reason_to_ffi),
            migrated_to: migrated_to.map(|sid| sid.0),
        },
        CallEventKind::SessionInitiate { join, media } => WaddleCallEventKind::SessionInitiate {
            join: livekit_join_to_ffi(join),
            media: call_media_to_ffi(media),
        },
        CallEventKind::SessionAccept { join, media } => WaddleCallEventKind::SessionAccept {
            join: livekit_join_to_ffi(join),
            media: call_media_to_ffi(media),
        },
        CallEventKind::SessionTerminate { reason } => WaddleCallEventKind::SessionTerminate {
            reason: reason.map(jingle_reason_to_ffi),
        },
    };
    WaddleCallEvent {
        from: event.from.to_string(),
        to: event.to.map(|jid| jid.to_string()),
        sid: event.sid.0,
        kind,
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
        stanza_id: msg.stanza_id.map(|id| id.to_string()),
        origin_id: msg.origin_id,
        replaces_id: msg.replaces_id,
        retracts_id: msg.retracts_id,
        reaction_target_id: msg.reaction_target_id,
        reaction_emojis: msg.reaction_emojis,
        displayed_marker_requested: msg.displayed_marker_requested,
        is_muc,
        thread: msg.thread_id.or(msg.thread),
        parent_thread_id: msg.parent_thread_id,
        reply_to_id: msg.reply_to_id,
        reply_to_sender: msg.reply_to_sender,
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        call_thread: msg.call_thread.map(call_thread_to_ffi),
        shared_files: msg
            .shared_files
            .into_iter()
            .map(shared_file_to_ffi)
            .collect(),
        mds_displayed: msg
            .mds_displayed
            .map(|entries| entries.into_iter().map(mds_entry_to_ffi).collect()),
    }
}

fn call_thread_to_ffi(
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

/// Convert an archived MAM message into the UniFFI record. Re-parses the
/// wrapped inner element through the full messaging parser so that replies
/// and fallback ranges survive history loads. The XEP-0201 nested-thread
/// `parent_thread_id` is read directly from the typed `archived` value
/// (the client parser extracts it via `crate::xep::thread::parse_thread`)
/// instead of being recovered from the re-parse - closes the parent-leak
/// path when `inner` is unparseable downstream.
pub(crate) fn archived_to_ffi(
    archived: waddle_xmpp_client::ArchivedMessage,
) -> WaddleArchivedMessage {
    let parsed = archived.payload.message.as_deref();
    let call_event = archived
        .payload
        .call
        .as_ref()
        .map(|call| call_event_to_ffi((**call).clone()));
    let (fb_start, fb_end) = parsed
        .and_then(|m| m.reply_fallback)
        .map(|(s, e)| (Some(s), Some(e)))
        .unwrap_or((None, None));
    WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        id: archived.id.map(|id| id.to_string()),
        stanza_id: archived.stanza_id.map(|id| id.to_string()),
        origin_id: archived.origin_id.map(|id| id.to_string()),
        timestamp: archived.timestamp.map(|t| t.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        reaction_target_id: parsed.and_then(|m| m.reaction_target_id.clone()),
        reaction_emojis: parsed
            .map(|m| m.reaction_emojis.clone())
            .unwrap_or_default(),
        thread: archived.thread,
        parent_thread_id: archived.parent_thread_id,
        reply_to_id: parsed.and_then(|m| m.reply_to_id.clone()),
        reply_to_sender: parsed.and_then(|m| m.reply_to_sender.clone()),
        reply_fallback_start: fb_start,
        reply_fallback_end: fb_end,
        call_thread: parsed
            .and_then(|m| m.call_thread.clone())
            .map(call_thread_to_ffi),
        shared_files: parsed
            .map(|m| {
                m.shared_files
                    .clone()
                    .into_iter()
                    .map(shared_file_to_ffi)
                    .collect()
            })
            .unwrap_or_default(),
        call_event,
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
        request_displayed_marker: opts.request_displayed_marker,
        link_preview_token: opts
            .link_preview_token
            .map(messaging::LinkPreviewToken::new)
            .transpose()
            .map_err(|err| err.to_string())?,
    })
}

#[cfg(test)]
mod tests {
    //! Unit tests for the FFI-side conversion layer.
    //!
    //! These pin down two contracts the Swift bindings rely on:
    //!
    //! 1. Inbound XMPP stanzas that the wasm client surfaces as
    //!    typed events also surface on Swift via the same shape —
    //!    XEP-0353 JMI + XEP-0166 Jingle session control map to
    //!    `WaddleCallEvent` variants, and XEP-0490 MDS payloads
    //!    survive the `InboundMessage` → `WaddleMessage` hop.
    //! 2. The XEP-0272 `<muji xmlns='urn:xmpp:jingle:muji:0'/>`
    //!    presence extension carries the `preparing` / `active`
    //!    flags through unchanged.
    use super::*;
    use minidom::Element;

    fn assert_audio_video(media: &WaddleCallMedia) {
        assert!(media.audio, "audio flag survives the conversion");
        assert!(media.video, "video flag survives the conversion");
    }

    #[test]
    fn jmi_propose_round_trips_through_call_event_to_ffi() {
        // Canonical wire shape: server stamps the propose with a
        // *full* JID so the responder can reply to the originating
        // resource (XEP-0353 §0.6). The FFI must preserve that
        // verbatim — collapsing to a bare JID would defeat the
        // routing guarantee.
        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop' to='bob@waddle.test'>
            <propose xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='video'/>
            </propose>
        </message>"#;
        let stanza: Element = xml.parse().expect("fixture parses");
        let parsed = messaging::parse_call_event(&stanza).expect("propose parses");
        let ffi = call_event_to_ffi(parsed);
        assert_eq!(ffi.from, "alice@waddle.test/desktop");
        assert_eq!(ffi.to.as_deref(), Some("bob@waddle.test"));
        assert_eq!(ffi.sid, "c1");
        match ffi.kind {
            WaddleCallEventKind::Propose { media } => assert_audio_video(&media),
            other => panic!("expected Propose, got {other:?}"),
        }
    }

    #[test]
    fn session_initiate_carries_livekit_join_through_ffi() {
        // The server-side Jingle handler rewrites the empty
        // `urn:waddle:transports:livekit:0` transport with a
        // populated one before forwarding; the FFI must surface all
        // four credentials (url, room, identity, token) so the
        // Swift LiveKit SDK can connect.
        let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/desktop' to='bob@waddle.test/desktop' id='i1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1' initiator='alice@waddle.test/desktop'>
              <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                <transport xmlns='urn:waddle:transports:livekit:0'
                           url='wss://livekit.waddle.test'
                           room='alice@waddle.test::c1'
                           identity='bob@waddle.test/desktop'>
                  <token xmlns='urn:waddle:transports:livekit:0'>eyJhbGc.payload.sig</token>
                </transport>
              </content>
            </jingle>
        </iq>"#;
        let stanza: Element = xml.parse().expect("fixture parses");
        let parsed = messaging::parse_call_event(&stanza).expect("session-initiate parses");
        let ffi = call_event_to_ffi(parsed);
        match ffi.kind {
            WaddleCallEventKind::SessionInitiate { join, media } => {
                assert_eq!(join.url, "wss://livekit.waddle.test");
                assert_eq!(join.room, "alice@waddle.test::c1");
                assert_eq!(join.identity, "bob@waddle.test/desktop");
                assert_eq!(join.token, "eyJhbGc.payload.sig");
                assert!(media.audio);
                assert!(!media.video);
            }
            other => panic!("expected SessionInitiate, got {other:?}"),
        }
    }

    #[test]
    fn session_terminate_reason_survives_round_trip() {
        let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/desktop' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><success/></reason>
            </jingle>
        </iq>"#;
        let stanza: Element = xml.parse().expect("fixture parses");
        let parsed = messaging::parse_call_event(&stanza).expect("session-terminate parses");
        let ffi = call_event_to_ffi(parsed);
        match ffi.kind {
            WaddleCallEventKind::SessionTerminate { reason } => {
                // The wire condition `<success/>` must resolve into
                // the typed `Success` variant — no string passthrough.
                assert_eq!(reason, Some(WaddleJingleReason::Success));
            }
            other => panic!("expected SessionTerminate, got {other:?}"),
        }
    }

    #[test]
    fn session_terminate_with_unknown_reason_surfaces_as_none() {
        // The upstream `messaging::call` parser already drops
        // unknown conditions to `None` at the wire boundary
        // (typed-payloads hard rule). The FFI just carries that
        // `None` through unchanged.
        let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/desktop' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><not-a-real-condition/></reason>
            </jingle>
        </iq>"#;
        let stanza: Element = xml.parse().expect("fixture parses");
        let parsed = messaging::parse_call_event(&stanza).expect("session-terminate parses");
        let ffi = call_event_to_ffi(parsed);
        match ffi.kind {
            WaddleCallEventKind::SessionTerminate { reason } => assert_eq!(reason, None),
            other => panic!("expected SessionTerminate, got {other:?}"),
        }
    }

    fn sid(value: &str) -> messaging::SessionId {
        messaging::SessionId(value.to_string())
    }

    fn parse_jmi(jmi: Element) -> InboundCallEvent {
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
    fn jmi_tie_break_metadata_survives_call_event_to_ffi() {
        let reject = call_event_to_ffi(parse_jmi(messaging::build_reject_with_options(
            &sid("c1"),
            Some(JingleReason::Expired),
            true,
        )));
        match reject.kind {
            WaddleCallEventKind::Reject { reason, tie_break } => {
                assert_eq!(reason, Some(WaddleJingleReason::Expired));
                assert!(tie_break);
            }
            other => panic!("expected Reject, got {other:?}"),
        }

        let retract = call_event_to_ffi(parse_jmi(messaging::build_retract_with_options(
            &sid("c2"),
            Some(JingleReason::Expired),
            true,
        )));
        match retract.kind {
            WaddleCallEventKind::Retract { reason, tie_break } => {
                assert_eq!(reason, Some(WaddleJingleReason::Expired));
                assert!(tie_break);
            }
            other => panic!("expected Retract, got {other:?}"),
        }
    }

    #[test]
    fn finish_migration_metadata_survives_call_event_to_ffi() {
        let finish = call_event_to_ffi(parse_jmi(messaging::build_finish_migrated(
            &sid("old"),
            JingleReason::Expired,
            &sid("new"),
        )));
        match finish.kind {
            WaddleCallEventKind::Finish {
                reason,
                migrated_to,
            } => {
                assert_eq!(reason, Some(WaddleJingleReason::Expired));
                assert_eq!(migrated_to.as_deref(), Some("new"));
            }
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    /// Parse a wire-shape stanza through the real messaging parser
    /// and extract its `InboundMessage`. Using the real parser
    /// rather than a hand-built struct literal makes the test
    /// resilient to fields being added to `InboundMessage` upstream.
    fn parse_message(xml: &str) -> InboundMessage {
        let stanza: Element = xml.parse().expect("fixture parses");
        match messaging::parse(&stanza) {
            Some(MessagingEvent::Message(msg)) => *msg,
            other => panic!("expected Message variant, got {other:?}"),
        }
    }

    fn parse_mam_archived(xml: &str) -> waddle_xmpp_client::ArchivedMessage {
        let stanza: Element = xml.parse().expect("fixture parses");
        waddle_xmpp_client::mam::parse_mam_result(&stanza).expect("expected MAM result")
    }

    #[test]
    fn archived_to_ffi_preserves_jmi_only_call_events_for_dm_reload() {
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

        let ffi = archived_to_ffi(archived);
        let call_event = ffi.call_event.expect("call event should be present");

        assert_eq!(ffi.mam_id, "mam-call");
        assert_eq!(ffi.body, None);
        assert_eq!(call_event.from, "bob@waddle.test/phone");
        assert_eq!(call_event.to.as_deref(), Some("alice@waddle.test/web"));
        assert_eq!(call_event.sid, "call-1");
        match call_event.kind {
            WaddleCallEventKind::Propose { media } => assert!(media.audio),
            other => panic!("expected Propose, got {other:?}"),
        }
    }

    #[test]
    fn archived_to_ffi_preserves_call_thread_anchor_for_room_reload() {
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

        let ffi = archived_to_ffi(archived);
        let anchor = ffi
            .call_thread
            .expect("call-thread should survive archive conversion");

        assert_eq!(ffi.mam_id, "mam-anchor");
        assert_eq!(ffi.thread.as_deref(), Some("call-thread-uuid"));
        assert_eq!(anchor.kind, "muc");
        assert_eq!(anchor.sid, "session-uuid");
        assert_eq!(anchor.media, vec!["audio".to_owned(), "video".to_owned()]);
        assert_eq!(anchor.initiator, "alice@waddle.test");
        assert_eq!(anchor.started, "2026-06-07T14:30:00+00:00");
    }

    #[test]
    fn mds_displayed_entries_survive_inbound_to_ffi() {
        // XEP-0490 §3 PEP event payload pushed by the user's own
        // server. The FFI must distinguish "not an MDS event"
        // (`None`) from "an MDS event with N entries" (`Some(_)`);
        // collapsing to `Vec` would lose the signal at the Swift
        // boundary (an empty publish on an MDS node would become
        // indistinguishable from an unrelated message).
        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test' to='alice@waddle.test/desktop'>
            <event xmlns='http://jabber.org/protocol/pubsub#event'>
                <items node='urn:xmpp:mds:displayed:0'>
                    <item id='room@conf.waddle.test'>
                        <displayed xmlns='urn:xmpp:mds:displayed:0'>
                            <stanza-id xmlns='urn:xmpp:sid:0' id='s-42' by='conf.waddle.test'/>
                        </displayed>
                    </item>
                </items>
            </event>
        </message>"#;
        let ffi = inbound_to_ffi(parse_message(xml));
        let entries = ffi.mds_displayed.expect("MDS event surfaces as Some(...)");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].chat_id, "room@conf.waddle.test");
        assert_eq!(entries[0].stanza_id, "s-42");
        assert_eq!(entries[0].stanza_id_by, "conf.waddle.test");
    }

    #[test]
    fn non_mds_message_surfaces_as_none() {
        let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test' to='bob@waddle.test' type='chat'>
            <body>hi</body>
        </message>"#;
        assert!(inbound_to_ffi(parse_message(xml)).mds_displayed.is_none());
    }

    #[test]
    fn mds_displayed_entries_are_stripped_for_foreign_event_origin() {
        let xml = r#"<message xmlns='jabber:client' from='mallory@waddle.test' to='alice@waddle.test/desktop'>
            <event xmlns='http://jabber.org/protocol/pubsub#event'>
                <items node='urn:xmpp:mds:displayed:0'>
                    <item id='bob@waddle.test'>
                        <displayed xmlns='urn:xmpp:mds:displayed:0'>
                            <stanza-id xmlns='urn:xmpp:sid:0' id='s-42' by='waddle.test'/>
                        </displayed>
                    </item>
                </items>
            </event>
        </message>"#;
        let trusted = trusted_mds_message(parse_message(xml), "alice@waddle.test");
        assert!(inbound_to_ffi(trusted).mds_displayed.is_none());
    }

    /// In-test listener that captures every callback invocation in
    /// order. Used to verify the `dispatch_event` routing without
    /// spinning up the tokio broadcast bus.
    #[derive(Default)]
    struct CapturingListener {
        // Mutex so we can mutate from `&self` callbacks. `parking_lot`
        // would be lighter but the test-only `std::sync::Mutex` is
        // available everywhere the workspace builds.
        events: std::sync::Mutex<Vec<&'static str>>,
        calls: std::sync::Mutex<Vec<WaddleCallEvent>>,
    }

    impl CapturingListener {
        fn record(&self, tag: &'static str) {
            self.events
                .lock()
                .expect("test capture mutex poisoned")
                .push(tag);
        }
    }

    impl WaddleEventListener for CapturingListener {
        fn on_message(&self, _message: WaddleMessage) {
            self.record("message");
        }
        fn on_presence(&self, _presence: WaddlePresence) {
            self.record("presence");
        }
        fn on_mam_result(&self, _message: WaddleArchivedMessage) {
            self.record("mam");
        }
        fn on_message_delivery_acked(&self, _stanza_id: String) {
            self.record("delivery_acked");
        }
        fn on_message_delivery_failed(&self, _stanza_id: String) {
            self.record("delivery_failed");
        }
        fn on_connected(&self) {
            self.record("connected");
        }
        fn on_disconnected(&self) {
            self.record("disconnected");
        }
        fn on_error(&self, _description: String) {
            self.record("error");
        }
        fn on_call(&self, event: WaddleCallEvent) {
            self.record("call");
            self.calls
                .lock()
                .expect("test capture mutex poisoned")
                .push(event);
        }
    }

    #[test]
    fn typed_call_event_routes_to_on_call() {
        let xml = "<message xmlns='jabber:client' from='bob@waddle.test/desktop'>\
            <proceed xmlns='urn:xmpp:jingle-message:0' id='c1'/>\
        </message>";
        let stanza: Element = xml.parse().expect("fixture parses");
        let call = messaging::parse_call_event(&stanza).expect("fixture is a call");
        let listener = CapturingListener::default();
        dispatch_event(
            ClientEvent::Call(Box::new(call)),
            "alice@waddle.test",
            &listener,
        );
        assert_eq!(
            &*listener.events.lock().expect("test capture mutex poisoned"),
            &["call"]
        );
        let calls = listener.calls.lock().expect("test capture mutex poisoned");
        assert_eq!(calls.len(), 1);
        assert!(matches!(calls[0].kind, WaddleCallEventKind::Proceed));
    }

    #[test]
    fn unhandled_stanza_silently_drops_unrecognised_xml() {
        // Anything that isn't a JMI / Jingle envelope must not
        // synthesise a spurious call event. The Swift app has no
        // general escape hatch and a misclassified call would
        // surface as a ringing UI for a non-existent session.
        let xml =
            "<message xmlns='jabber:client' from='alice@waddle.test'><body>hi</body></message>";
        let stanza: Element = xml.parse().expect("fixture parses");
        let listener = CapturingListener::default();
        dispatch_event(
            ClientEvent::UnhandledStanza(stanza),
            "alice@waddle.test",
            &listener,
        );
        assert!(listener
            .events
            .lock()
            .expect("test capture mutex poisoned")
            .is_empty());
    }

    #[test]
    fn muji_presence_maps_active_and_preparing_flags() {
        let active = muji_presence_to_ffi(MujiPresence {
            preparing: false,
            active: true,
            audio: true,
            video: true,
        });
        assert!(active.active);
        assert!(!active.preparing);
        assert!(active.audio);
        assert!(active.video);

        let preparing = muji_presence_to_ffi(MujiPresence {
            preparing: true,
            active: false,
            audio: false,
            video: false,
        });
        assert!(preparing.preparing);
        assert!(!preparing.active);
        assert!(!preparing.audio);
        assert!(!preparing.video);
    }
}
