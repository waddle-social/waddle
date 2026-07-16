use jid::Jid;
use minidom::Element;

use waddle_xmpp_client::{
    mds::MdsCatchupEntry,
    messaging::{
        self, CallEventKind, CallMedia, CarbonDirection, InboundCallEvent, InboundPresence,
        JingleReason, LinkPreviewData, LinkPreviewLookup, LiveKitJoin, MarkupSpan, MarkupSpanData,
        MarkupSpanType, MdsDisplayedEntry, MujiPresence, ReferenceData, SendMessageOptions,
    },
    pin::{PinEntry, PinEvent, PinEventAction, PinPreview},
    request::StanzaId,
    xep::{
        call_thread::CallThreadEnded,
        reply::{FallbackRange, ReplyMarker},
        thread::ThreadRef,
    },
    ClientEvent, ConnectionEvent, InboundMessage, LifecycleEvent, MessageDeliveryEvent,
    MessagingEvent, SmResumeState,
};

use crate::{
    WaddleArchivedMessage, WaddleCallEvent, WaddleCallEventKind, WaddleCallMedia,
    WaddleCallThreadAnchor, WaddleCallThreadEnded, WaddleCarbonDirection, WaddleChatState,
    WaddleClientEvent, WaddleEncryptedFile, WaddleEncryptedFileHash, WaddleEventListener,
    WaddleForumPostKind, WaddleJingleReason, WaddleLinkPreview, WaddleLinkPreviewImage,
    WaddleLinkPreviewLookup, WaddleLinkPreviewLookupPreview, WaddleLinkPreviewLookupStatus,
    WaddleLinkPreviewPlayer, WaddleLinkPreviewVideo, WaddleLiveKitJoin, WaddleMarkupSpan,
    WaddleMarkupSpanType, WaddleMdsDisplayedEntry, WaddleMessage, WaddleMucAffiliation,
    WaddleMucRole, WaddleMujiPresence, WaddlePinAction, WaddlePinEntry, WaddlePinEvent,
    WaddlePinPreview, WaddlePresence, WaddlePresenceHat, WaddleReference, WaddleReferenceType,
    WaddleSaslCondition, WaddleSendOptions, WaddleSharedFile, WaddleSmResumeState,
    WaddleStanzaErrorType, WaddleStanzaId,
};

// ── Event dispatch ───────────────────────────────────────────────────────────

pub(super) fn dispatch_event(
    event: ClientEvent,
    account_bare_jid: &str,
    listener: &dyn WaddleEventListener,
) {
    match event {
        ClientEvent::Lifecycle(LifecycleEvent::SessionReady(_)) => {
            listener.on_event(WaddleClientEvent::Connected);
        }
        ClientEvent::Messaging(MessagingEvent::Message(msg)) => {
            listener.on_event(WaddleClientEvent::Message {
                message: inbound_to_ffi(trusted_mds_message(*msg, account_bare_jid)),
            });
        }
        ClientEvent::Messaging(MessagingEvent::Presence(pres)) => {
            listener.on_event(WaddleClientEvent::Presence {
                presence: presence_to_ffi(*pres),
            });
        }
        ClientEvent::MamResult(archived) => {
            // `None` = the trusted parse rejected the row (e.g. spoofed
            // moderation) and it carries no call event — drop, like wasm.
            if let Some(message) = archived_to_ffi(*archived) {
                listener.on_event(WaddleClientEvent::MamResult { message });
            }
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked { stanza_id }) => {
            listener.on_event(WaddleClientEvent::DeliveryAcked {
                stanza_id: stanza_id.to_string(),
            });
        }
        ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed { stanza_id }) => {
            listener.on_event(WaddleClientEvent::DeliveryFailed {
                stanza_id: stanza_id.to_string(),
            });
        }
        ClientEvent::Call(call) => {
            listener.on_event(WaddleClientEvent::Call {
                event: call_event_to_ffi(*call),
            });
        }
        ClientEvent::ResumeStateChanged(state) => {
            listener.on_event(WaddleClientEvent::ResumeStateChanged {
                state: state.map(resume_state_to_ffi),
            });
        }
        ClientEvent::Connection(ConnectionEvent::AuthenticationFailed(failure)) => {
            listener.on_event(WaddleClientEvent::AuthenticationFailed {
                condition: sasl_condition_to_ffi(failure.condition),
            });
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

/// XEP-0490 §3.1 catch-up entry → FFI record. Same field mapping as
/// [`mds_entry_to_ffi`] but sourced from the IQ retrieve parser
/// (`mds::MdsCatchupEntry`) instead of the inbound PEP event type.
pub(crate) fn mds_catchup_entry_to_ffi(entry: MdsCatchupEntry) -> WaddleMdsDisplayedEntry {
    WaddleMdsDisplayedEntry {
        chat_id: entry.chat_id.to_string(),
        stanza_id: entry.stanza_id.as_str().to_string(),
        stanza_id_by: entry.stanza_id_by.to_string(),
    }
}

/// Convert a parsed inbound presence into the UniFFI record. Typed
/// values (MUC status codes, idle instant, stanza error) are
/// stringified only here, at the FFI boundary.
fn presence_to_ffi(pres: InboundPresence) -> WaddlePresence {
    WaddlePresence {
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
        muc_jid: pres.muc_jid,
        muc_status_codes: pres.muc_status.iter().map(|status| status.code()).collect(),
        vcard_avatar: pres.vcard_avatar,
        idle_since: pres.idle_since.map(|since| since.to_rfc3339()),
        error_condition: pres.error.as_ref().map(|err| err.condition.clone()),
        error_type: pres
            .error
            .as_ref()
            .map(|err| stanza_error_type_to_ffi(err.error_type)),
        error_text: pres.error.and_then(|err| err.text),
        hand_raised: pres.hand_raised,
        muted: pres.muted,
        muji: pres.muji.map(muji_presence_to_ffi),
    }
}

/// XEP-0085: map the parser-validated state string to the typed FFI
/// enum. Unknown strings surface as `None` rather than an opaque
/// passthrough (typed-payloads hard rule).
fn chat_state_to_ffi(state: &str) -> Option<WaddleChatState> {
    match state {
        "active" => Some(WaddleChatState::Active),
        "composing" => Some(WaddleChatState::Composing),
        "paused" => Some(WaddleChatState::Paused),
        "inactive" => Some(WaddleChatState::Inactive),
        "gone" => Some(WaddleChatState::Gone),
        _ => None,
    }
}

fn forum_post_kind_to_ffi(kind: &str) -> Option<WaddleForumPostKind> {
    match kind {
        "topic" => Some(WaddleForumPostKind::Topic),
        "reply" => Some(WaddleForumPostKind::Reply),
        _ => None,
    }
}

fn stanza_error_type_to_ffi(
    error_type: waddle_xmpp_client::StanzaErrorType,
) -> WaddleStanzaErrorType {
    use waddle_xmpp_client::StanzaErrorType;
    match error_type {
        StanzaErrorType::Auth => WaddleStanzaErrorType::Auth,
        StanzaErrorType::Cancel => WaddleStanzaErrorType::Cancel,
        StanzaErrorType::Continue => WaddleStanzaErrorType::Continue,
        StanzaErrorType::Modify => WaddleStanzaErrorType::Modify,
        StanzaErrorType::Wait => WaddleStanzaErrorType::Wait,
        StanzaErrorType::Unknown => WaddleStanzaErrorType::Unknown,
    }
}

fn markup_span_type_to_ffi(span_type: MarkupSpanType) -> WaddleMarkupSpanType {
    match span_type {
        MarkupSpanType::Bold => WaddleMarkupSpanType::Bold,
        MarkupSpanType::Italic => WaddleMarkupSpanType::Italic,
        MarkupSpanType::Strikethrough => WaddleMarkupSpanType::Strikethrough,
        MarkupSpanType::Code => WaddleMarkupSpanType::Code,
        MarkupSpanType::CodeBlock => WaddleMarkupSpanType::CodeBlock,
        MarkupSpanType::Blockquote => WaddleMarkupSpanType::Blockquote,
        MarkupSpanType::Link => WaddleMarkupSpanType::Link,
    }
}

/// Wire name of a span kind as consumed by the outbound XEP-0394
/// builder (`build_outbound_message` matches on these strings).
fn markup_span_type_wire_name(span_type: WaddleMarkupSpanType) -> &'static str {
    match span_type {
        WaddleMarkupSpanType::Bold => "bold",
        WaddleMarkupSpanType::Italic => "italic",
        WaddleMarkupSpanType::Strikethrough => "strikethrough",
        WaddleMarkupSpanType::Code => "code",
        WaddleMarkupSpanType::CodeBlock => "code_block",
        WaddleMarkupSpanType::Blockquote => "blockquote",
        WaddleMarkupSpanType::Link => "link",
    }
}

fn markup_span_to_ffi(span: MarkupSpan) -> WaddleMarkupSpan {
    WaddleMarkupSpan {
        span_type: markup_span_type_to_ffi(span.span_type),
        // Char offsets; a body long enough to overflow u32 cannot
        // cross a real XMPP stream, so saturation is unreachable in
        // practice but keeps the conversion total.
        start: u32::try_from(span.start).unwrap_or(u32::MAX),
        end: u32::try_from(span.end).unwrap_or(u32::MAX),
        uri: span.uri,
    }
}

fn markup_spans_to_ffi(spans: Vec<MarkupSpan>) -> Vec<WaddleMarkupSpan> {
    spans.into_iter().map(markup_span_to_ffi).collect()
}

fn reference_to_ffi(reference: ReferenceData) -> WaddleReference {
    WaddleReference {
        ref_type: reference_type_to_ffi(reference.ref_type),
        uri: reference.uri,
        begin: reference.begin,
        end: reference.end,
        anchor: reference.anchor,
    }
}

fn sasl_condition_to_ffi(
    condition: waddle_xmpp_client::SaslFailureCondition,
) -> WaddleSaslCondition {
    use waddle_xmpp_client::SaslFailureCondition as C;
    match condition {
        C::Aborted => WaddleSaslCondition::Aborted,
        C::AccountDisabled => WaddleSaslCondition::AccountDisabled,
        C::CredentialsExpired => WaddleSaslCondition::CredentialsExpired,
        C::EncryptionRequired => WaddleSaslCondition::EncryptionRequired,
        C::IncorrectEncoding => WaddleSaslCondition::IncorrectEncoding,
        C::InvalidAuthzid => WaddleSaslCondition::InvalidAuthzid,
        C::InvalidMechanism => WaddleSaslCondition::InvalidMechanism,
        C::MalformedRequest => WaddleSaslCondition::MalformedRequest,
        C::MechanismTooWeak => WaddleSaslCondition::MechanismTooWeak,
        C::NotAuthorized => WaddleSaslCondition::NotAuthorized,
        C::TemporaryAuthFailure => WaddleSaslCondition::TemporaryAuthFailure,
        C::Unknown => WaddleSaslCondition::Unknown,
    }
}

fn reference_type_to_ffi(ref_type: String) -> WaddleReferenceType {
    match ref_type.as_str() {
        "mention" => WaddleReferenceType::Mention,
        "data" => WaddleReferenceType::Data,
        _ => WaddleReferenceType::Other { value: ref_type },
    }
}

fn reference_type_wire_name(ref_type: WaddleReferenceType) -> String {
    match ref_type {
        WaddleReferenceType::Mention => "mention".to_string(),
        WaddleReferenceType::Data => "data".to_string(),
        WaddleReferenceType::Other { value } => value,
    }
}

fn references_to_ffi(references: Vec<ReferenceData>) -> Vec<WaddleReference> {
    references.into_iter().map(reference_to_ffi).collect()
}

fn link_preview_to_ffi(preview: LinkPreviewData) -> WaddleLinkPreview {
    WaddleLinkPreview {
        original_url: preview.original_url.to_string(),
        normalized_url: preview.normalized_url.map(|url| url.to_string()),
        title: preview.title,
        description: preview.description,
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
        remote_media_unavailable: preview.remote_media_unavailable,
    }
}

fn link_previews_to_ffi(previews: Vec<LinkPreviewData>) -> Vec<WaddleLinkPreview> {
    previews.into_iter().map(link_preview_to_ffi).collect()
}

pub(crate) fn link_preview_lookup_to_ffi(lookup: LinkPreviewLookup) -> WaddleLinkPreviewLookup {
    match lookup {
        LinkPreviewLookup::Ready(ready) => WaddleLinkPreviewLookup {
            status: WaddleLinkPreviewLookupStatus::Ready,
            preview: Some(WaddleLinkPreviewLookupPreview {
                token: ready.token.as_str().to_string(),
                original_url: ready.original_url.to_string(),
                normalized_url: ready.normalized_url.to_string(),
                expires_at: ready.expires_at.to_rfc3339(),
                title: ready.title,
                description: ready.description,
                image: ready.image.map(|image| WaddleLinkPreviewImage {
                    url: image.url.to_string(),
                    media_type: image.media_type.as_str().to_string(),
                    width: image.width,
                    height: image.height,
                    alt: image.alt,
                }),
                player_embed: ready.player_embed.map(|player| WaddleLinkPreviewPlayer {
                    url: player.url.to_string(),
                    width: player.width,
                    height: player.height,
                }),
            }),
        },
        LinkPreviewLookup::Unsupported => {
            lookup_status_only(WaddleLinkPreviewLookupStatus::Unsupported)
        }
        LinkPreviewLookup::Blocked => lookup_status_only(WaddleLinkPreviewLookupStatus::Blocked),
        LinkPreviewLookup::Failed => lookup_status_only(WaddleLinkPreviewLookupStatus::Failed),
    }
}

pub(crate) fn lookup_status_only(status: WaddleLinkPreviewLookupStatus) -> WaddleLinkPreviewLookup {
    WaddleLinkPreviewLookup {
        status,
        preview: None,
    }
}

fn pin_preview_to_ffi(preview: PinPreview) -> WaddlePinPreview {
    WaddlePinPreview {
        author_jid: preview.author_jid,
        author_nick: preview.author_nick,
        text: preview.text,
        message_timestamp: preview.message_timestamp.to_rfc3339(),
    }
}

/// `urn:waddle:pin:0` pin-list entry → FFI record. Timestamps are
/// stringified to RFC 3339 only here, at the boundary.
pub(crate) fn pin_entry_to_ffi(entry: PinEntry) -> WaddlePinEntry {
    WaddlePinEntry {
        target_stanza_id: entry.target_stanza_id,
        pinner_jid: entry.pinner_jid,
        pinned_at: entry.pinned_at.to_rfc3339(),
        preview: pin_preview_to_ffi(entry.preview),
    }
}

fn pin_event_to_ffi(event: PinEvent) -> WaddlePinEvent {
    WaddlePinEvent {
        action: match event.action {
            PinEventAction::Pinned => WaddlePinAction::Pinned,
            PinEventAction::Unpinned => WaddlePinAction::Unpinned,
        },
        target_stanza_id: event.target_stanza_id,
        by: event.by,
        reason: event.reason,
        preview: event.preview.map(pin_preview_to_ffi),
    }
}

fn call_thread_ended_to_ffi(ended: CallThreadEnded) -> WaddleCallThreadEnded {
    WaddleCallThreadEnded {
        anchor_id: ended.anchor_id,
        ended: ended.ended.to_rfc3339(),
        duration: ended.duration.as_str().to_owned(),
    }
}

fn carbon_to_ffi(direction: CarbonDirection) -> WaddleCarbonDirection {
    match direction {
        CarbonDirection::Sent => WaddleCarbonDirection::Sent,
        CarbonDirection::Received => WaddleCarbonDirection::Received,
    }
}

// ── XEP-0198 resume snapshot round-trip ──────────────────────────────────────

/// Serialize the typed resume snapshot for opaque persistence on the
/// app side. Queued stanzas cross as XML strings — the message
/// stanza-id is re-derived from each element on restore, so the
/// round trip is lossless for replay identity.
pub(super) fn resume_state_to_ffi(state: SmResumeState) -> WaddleSmResumeState {
    WaddleSmResumeState {
        previd: state.previd().to_string(),
        inbound_h: state.inbound_h(),
        outbound_h: state.outbound_h(),
        max_resume_seconds: state.max_resume_seconds(),
        queued_stanzas_xml: state
            .unhandled_outbound_stanzas()
            .map(String::from)
            .collect(),
    }
}

/// Rebuild the typed resume snapshot from persisted FFI data. Parsing
/// the queued-stanza XML happens exactly once, here at the boundary;
/// malformed persisted state is surfaced as a human-readable error
/// (via the listener) rather than silently dropped.
pub(super) fn resume_state_from_ffi(state: WaddleSmResumeState) -> Result<SmResumeState, String> {
    let stanzas = state
        .queued_stanzas_xml
        .into_iter()
        .map(|xml| {
            xml.parse::<Element>()
                .map_err(|err| format!("invalid resume stanza XML: {err}"))
        })
        .collect::<Result<Vec<_>, String>>()?;
    SmResumeState::from_unhandled_outbound_stanzas(
        state.previd,
        state.inbound_h,
        state.outbound_h,
        stanzas,
    )
    .map(|resume| resume.with_max_resume_seconds(state.max_resume_seconds))
    .map_err(|err| err.to_string())
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
        CallEventKind::Ringing => WaddleCallEventKind::Ringing,
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
        subject: msg.subject,
        message_type: msg.message_type,
        timestamp: msg.timestamp.map(|t| t.to_rfc3339()),
        stanza_id: msg.stanza_id.as_ref().map(ToString::to_string),
        stanza_id_by: msg.stanza_id.map(|id| id.by.to_string()),
        stanza_ids: msg
            .stanza_ids
            .into_iter()
            .map(|sid| WaddleStanzaId {
                id: sid.id,
                by: sid.by.to_string(),
            })
            .collect(),
        origin_id: msg.origin_id,
        replaces_id: msg.replaces_id,
        retracts_id: msg.retracts_id,
        retraction_id: msg.retraction_id,
        is_retracted: msg.is_retracted,
        moderation_target_id: msg.moderation_target_id,
        moderated_by: msg.moderated_by.map(|jid| jid.to_string()),
        moderation_reason: msg.moderation_reason,
        reaction_target_id: msg.reaction_target_id,
        reaction_emojis: msg.reaction_emojis,
        chat_state: msg.chat_state.as_deref().and_then(chat_state_to_ffi),
        displayed_marker_requested: msg.displayed_marker_requested,
        displayed_marker_id: msg.displayed_marker_id,
        is_muc,
        thread: msg.thread_id.or(msg.thread),
        parent_thread_id: msg.parent_thread_id,
        markup_spans: markup_spans_to_ffi(msg.markup_spans),
        broadcast_mention: msg.broadcast_mention,
        mention_uris: msg.mention_uris,
        references: references_to_ffi(msg.references),
        forum_post_kind: msg
            .forum_post_kind
            .as_deref()
            .and_then(forum_post_kind_to_ffi),
        forum_title: msg.forum_title,
        is_sticker: msg.is_sticker,
        link_previews: link_previews_to_ffi(msg.link_previews),
        pin_event: msg.pin_event.map(pin_event_to_ffi),
        call_thread_ended: msg.call_thread_ended.map(call_thread_ended_to_ffi),
        carbon: msg.carbon.map(carbon_to_ffi),
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
/// Returns `None` for archive rows whose inner message failed the trusted
/// parse and that carry no call event — mirroring the wasm client's
/// drop-guard. Without it, a spoofed occupant "moderation" stanza whose
/// payload parse was rejected would still surface its raw `<body>` as a
/// normal, non-retracted chat message.
pub(crate) fn archived_to_ffi(
    archived: waddle_xmpp_client::ArchivedMessage,
) -> Option<WaddleArchivedMessage> {
    if archived.payload.message.is_none() && archived.payload.call.is_none() {
        return None;
    }
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
    Some(WaddleArchivedMessage {
        mam_id: archived.mam_id,
        query_id: archived.query_id,
        id: archived.id.map(|id| id.to_string()),
        stanza_id: archived.stanza_id.as_ref().map(ToString::to_string),
        stanza_id_by: archived.stanza_id.map(|id| id.by.to_string()),
        stanza_ids: archived
            .stanza_ids
            .into_iter()
            .map(|sid| WaddleStanzaId {
                id: sid.id,
                by: sid.by.to_string(),
            })
            .collect(),
        origin_id: archived.origin_id.map(|id| id.to_string()),
        timestamp: archived.timestamp.map(|t| t.to_rfc3339()),
        from: archived.from,
        to: archived.to,
        message_type: archived.message_type,
        body: archived.body,
        subject: parsed.and_then(|m| m.subject.clone()),
        replaces_id: parsed.and_then(|m| m.replaces_id.clone()),
        retracts_id: parsed.and_then(|m| m.retracts_id.clone()),
        retraction_id: parsed.and_then(|m| m.retraction_id.clone()),
        is_retracted: parsed.is_some_and(|m| m.is_retracted),
        moderation_target_id: parsed.and_then(|m| m.moderation_target_id.clone()),
        moderated_by: parsed.and_then(|m| m.moderated_by.as_ref().map(|jid| jid.to_string())),
        moderation_reason: parsed.and_then(|m| m.moderation_reason.clone()),
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
        markup_spans: parsed
            .map(|m| markup_spans_to_ffi(m.markup_spans.clone()))
            .unwrap_or_default(),
        broadcast_mention: parsed.and_then(|m| m.broadcast_mention.clone()),
        mention_uris: parsed.map(|m| m.mention_uris.clone()).unwrap_or_default(),
        references: parsed
            .map(|m| references_to_ffi(m.references.clone()))
            .unwrap_or_default(),
        forum_post_kind: parsed
            .and_then(|m| m.forum_post_kind.as_deref())
            .and_then(forum_post_kind_to_ffi),
        forum_title: parsed.and_then(|m| m.forum_title.clone()),
        is_sticker: parsed.is_some_and(|m| m.is_sticker),
        author_real_jid: archived.author_real_jid,
        call_thread: parsed
            .and_then(|m| m.call_thread.clone())
            .map(call_thread_to_ffi),
        call_thread_ended: parsed
            .and_then(|m| m.call_thread_ended.clone())
            .map(call_thread_ended_to_ffi),
        shared_files: parsed
            .map(|m| {
                m.shared_files
                    .clone()
                    .into_iter()
                    .map(shared_file_to_ffi)
                    .collect()
            })
            .unwrap_or_default(),
        link_previews: parsed
            .map(|m| link_previews_to_ffi(m.link_previews.clone()))
            .unwrap_or_default(),
        call_event,
    })
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
        subject: opts.subject,
        markup_spans: opts
            .markup_spans
            .into_iter()
            .map(|span| MarkupSpanData {
                span_type: markup_span_type_wire_name(span.span_type).to_string(),
                start: span.start,
                end: span.end,
                uri: span.uri,
            })
            .collect(),
        references: opts
            .references
            .into_iter()
            .map(|reference| ReferenceData {
                ref_type: reference_type_wire_name(reference.ref_type),
                uri: reference.uri,
                begin: reference.begin,
                end: reference.end,
                anchor: reference.anchor,
            })
            .collect(),
        request_displayed_marker: opts.request_displayed_marker,
        muc_pm: opts.muc_pm,
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
    fn jmi_ringing_round_trips_through_call_event_to_ffi() {
        let ringing = call_event_to_ffi(parse_jmi(messaging::build_ringing(&sid("c1"))));
        assert_eq!(ringing.sid, "c1");
        assert!(matches!(ringing.kind, WaddleCallEventKind::Ringing));
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

        let ffi = archived_to_ffi(archived).expect("parsed archive row must convert");
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

        let ffi = archived_to_ffi(archived).expect("parsed archive row must convert");
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

    // ── Record-parity conversion coverage (one test per XEP cluster) ────────

    #[test]
    fn xep0359_stanza_ids_survive_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='m1' \
                      from='room@conf.waddle.test/alice'>\
               <body>hi</body>\
               <stanza-id xmlns='urn:xmpp:sid:0' id='sid-archive' by='archive.waddle.test'/>\
               <stanza-id xmlns='urn:xmpp:sid:0' id='sid-room' by='room@conf.waddle.test'/>\
             </message>",
        ));
        assert_eq!(ffi.stanza_id.as_deref(), Some("sid-archive"));
        assert_eq!(ffi.stanza_id_by.as_deref(), Some("archive.waddle.test"));
        assert_eq!(ffi.stanza_ids.len(), 2);
        assert_eq!(ffi.stanza_ids[0].id, "sid-archive");
        assert_eq!(ffi.stanza_ids[0].by, "archive.waddle.test");
        assert_eq!(ffi.stanza_ids[1].id, "sid-room");
        assert_eq!(ffi.stanza_ids[1].by, "room@conf.waddle.test");
    }

    #[test]
    fn xep0308_correction_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='chat' id='m2' from='bob@waddle.test/phone'>\
               <body>fixed typo</body>\
               <replace xmlns='urn:xmpp:message-correct:0' id='orig-1'/>\
             </message>",
        ));
        assert_eq!(ffi.replaces_id.as_deref(), Some("orig-1"));
    }

    #[test]
    fn xep0425_moderation_broadcast_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='retraction-id-1' \
                      from='room@conf.waddle.test'>\
               <retract xmlns='urn:xmpp:message-retract:1' id='target-room-sid'>\
                 <moderated xmlns='urn:xmpp:message-moderate:1' by='room@conf.waddle.test/mod'/>\
                 <reason>spam</reason>\
               </retract>\
             </message>",
        ));
        assert_eq!(ffi.retracts_id.as_deref(), Some("target-room-sid"));
        assert_eq!(ffi.moderation_target_id.as_deref(), Some("target-room-sid"));
        assert_eq!(
            ffi.moderated_by.as_deref(),
            Some("room@conf.waddle.test/mod")
        );
        assert_eq!(ffi.moderation_reason.as_deref(), Some("spam"));
        assert!(!ffi.is_retracted);
    }

    #[test]
    fn xep0424_tombstone_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='target-message-id' \
                      from='room@conf.waddle.test/alice'>\
               <retracted xmlns='urn:xmpp:message-retract:1' id='retraction-message-id' \
                          stamp='2026-05-06T12:00:01Z'>\
                 <moderated xmlns='urn:xmpp:message-moderate:1' by='room@conf.waddle.test/mod'/>\
                 <reason>spam</reason>\
               </retracted>\
             </message>",
        ));
        assert!(ffi.is_retracted);
        assert_eq!(ffi.retraction_id.as_deref(), Some("retraction-message-id"));
        assert_eq!(
            ffi.moderated_by.as_deref(),
            Some("room@conf.waddle.test/mod")
        );
        assert_eq!(ffi.moderation_reason.as_deref(), Some("spam"));
    }

    #[test]
    fn xep0085_chat_state_maps_to_typed_enum() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='chat' from='bob@waddle.test/phone'>\
               <composing xmlns='http://jabber.org/protocol/chatstates'/>\
             </message>",
        ));
        assert_eq!(ffi.chat_state, Some(WaddleChatState::Composing));
    }

    #[test]
    fn xep0085_unknown_chat_state_string_maps_to_none() {
        // The parser already validates states, but the conversion layer
        // must independently refuse to pass unknown strings through as
        // typed values.
        assert_eq!(chat_state_to_ffi("active"), Some(WaddleChatState::Active));
        assert_eq!(chat_state_to_ffi("paused"), Some(WaddleChatState::Paused));
        assert_eq!(
            chat_state_to_ffi("inactive"),
            Some(WaddleChatState::Inactive)
        );
        assert_eq!(chat_state_to_ffi("gone"), Some(WaddleChatState::Gone));
        assert_eq!(chat_state_to_ffi("bogus-state"), None);
        assert_eq!(chat_state_to_ffi(""), None);

        let mut msg = parse_message(
            "<message xmlns='jabber:client' type='chat' from='bob@waddle.test/phone'>\
               <body>hi</body>\
             </message>",
        );
        msg.chat_state = Some("bogus-state".to_string());
        assert_eq!(inbound_to_ffi(msg).chat_state, None);
    }

    #[test]
    fn xep0333_displayed_marker_id_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='chat' from='bob@waddle.test/phone'>\
               <displayed xmlns='urn:xmpp:chat-markers:0' id='m-42'/>\
             </message>",
        ));
        assert_eq!(ffi.displayed_marker_id.as_deref(), Some("m-42"));
    }

    #[test]
    fn xep0394_markup_spans_survive_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='room@conf.waddle.test/alice'>\
               <body>bold block https://example.com</body>\
               <markup xmlns='urn:xmpp:markup:0'>\
                 <span start='0' end='4'><strong/></span>\
                 <bcode start='5' end='10'/>\
                 <span xmlns='urn:waddle:markup:0' start='11' end='30' \
                       uri='https://example.com'/>\
               </markup>\
             </message>",
        ));
        assert_eq!(ffi.markup_spans.len(), 3);
        assert_eq!(ffi.markup_spans[0].span_type, WaddleMarkupSpanType::Bold);
        assert_eq!(ffi.markup_spans[0].start, 0);
        assert_eq!(ffi.markup_spans[0].end, 4);
        assert_eq!(ffi.markup_spans[0].uri, None);
        assert_eq!(
            ffi.markup_spans[1].span_type,
            WaddleMarkupSpanType::CodeBlock
        );
        assert_eq!(ffi.markup_spans[2].span_type, WaddleMarkupSpanType::Link);
        assert_eq!(
            ffi.markup_spans[2].uri.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn xep0372_references_and_broadcast_mention_survive_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='room@conf.waddle.test/alice'>\
               <body>hi @bob and @everyone, see https://example.com</body>\
               <reference xmlns='urn:xmpp:reference:0' type='mention' \
                  uri='xmpp:bob@waddle.test' begin='3' end='7'/>\
               <reference xmlns='urn:xmpp:reference:0' type='mention' \
                  uri='xmpp:@everyone' begin='12' end='21' \
                  anchor='@everyone'/>\
               <reference xmlns='urn:xmpp:reference:0' type='data' \
                  uri='https://example.com' begin='27' end='46'/>\
             </message>",
        ));
        assert_eq!(
            ffi.mention_uris,
            vec![
                "xmpp:bob@waddle.test".to_string(),
                "xmpp:@everyone".to_string(),
            ]
        );
        assert_eq!(ffi.broadcast_mention.as_deref(), Some("xmpp:@everyone"));
        assert_eq!(ffi.references.len(), 3);
        assert_eq!(ffi.references[0].ref_type, WaddleReferenceType::Mention);
        assert_eq!(ffi.references[0].uri, "xmpp:bob@waddle.test");
        assert_eq!(ffi.references[0].begin, 3);
        assert_eq!(ffi.references[0].end, 7);
        assert_eq!(ffi.references[1].anchor.as_deref(), Some("@everyone"));
        assert_eq!(ffi.references[2].ref_type, WaddleReferenceType::Data);
        assert!(ffi.references[2].anchor.is_none());
    }

    #[test]
    fn xep0449_sticker_marker_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='chat' from='bob@waddle.test/phone'>\
               <body>\u{1F980}</body>\
               <sticker xmlns='urn:xmpp:stickers:0' pack='crabs'/>\
               <file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>\
                 <file xmlns='urn:xmpp:file:metadata:0'>\
                   <media-type>image/png</media-type>\
                   <name>crab.png</name>\
                   <size>1234</size>\
                 </file>\
                 <sources>\
                   <url-data xmlns='http://jabber.org/protocol/url-data' \
                             target='https://cdn.waddle.test/crab.png'/>\
                 </sources>\
               </file-sharing>\
             </message>",
        ));
        assert!(ffi.is_sticker);
        assert_eq!(ffi.shared_files.len(), 1);
        assert_eq!(ffi.shared_files[0].url, "https://cdn.waddle.test/crab.png");
    }

    #[test]
    fn xep0280_carbon_direction_survives_inbound_to_ffi() {
        let mut sent = parse_message(
            "<message xmlns='jabber:client' type='chat' id='c1' \
                      from='alice@waddle.test/phone' to='bob@waddle.test'>\
               <body>sent elsewhere</body>\
             </message>",
        );
        sent.carbon = Some(CarbonDirection::Sent);
        assert_eq!(
            inbound_to_ffi(sent).carbon,
            Some(WaddleCarbonDirection::Sent)
        );

        let mut received = parse_message(
            "<message xmlns='jabber:client' type='chat' id='c2' \
                      from='bob@waddle.test/desktop' to='alice@waddle.test/phone'>\
               <body>received elsewhere</body>\
             </message>",
        );
        received.carbon = Some(CarbonDirection::Received);
        assert_eq!(
            inbound_to_ffi(received).carbon,
            Some(WaddleCarbonDirection::Received)
        );

        let direct = parse_message(
            "<message xmlns='jabber:client' type='chat' id='c3' \
                      from='bob@waddle.test/desktop' to='alice@waddle.test/phone'>\
               <body>direct</body>\
             </message>",
        );
        assert_eq!(inbound_to_ffi(direct).carbon, None);
    }

    #[test]
    fn xep0511_link_previews_survive_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='m-link' \
                      from='room@conf.waddle.test/alice'>\
               <body>see https://the.link.example/what-was-linked</body>\
               <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' \
                                xmlns:og='https://ogp.me/ns#' \
                                xmlns:ogi='https://ogp.me/ns#image:' \
                                rdf:about='https://the.link.example/what-was-linked'>\
                 <og:title>The Best Webpage</og:title>\
                 <og:description>Plain text preview</og:description>\
                 <og:url>https://the.link.example/what-was-linked</og:url>\
                 <og:image>https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png</og:image>\
                 <ogi:type>image/png</ogi:type>\
                 <ogi:width>640</ogi:width>\
                 <ogi:height>360</ogi:height>\
                 <ogi:alt>Article screenshot</ogi:alt>\
               </rdf:Description>\
             </message>",
        ));
        assert_eq!(ffi.link_previews.len(), 1);
        let preview = &ffi.link_previews[0];
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
        assert!(!preview.remote_media_unavailable);
        let image = preview.image.as_ref().expect("cached image survives");
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.width, Some(640));
        assert_eq!(image.height, Some(360));
        assert_eq!(image.alt.as_deref(), Some("Article screenshot"));
    }

    #[test]
    fn xep0511_remote_media_flag_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='m-link' \
                      from='room@conf.waddle.test/alice'>\
               <body>see https://the.link.example/what-was-linked</body>\
               <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' \
                                xmlns:og='https://ogp.me/ns#' \
                                xmlns:ogi='https://ogp.me/ns#image:' \
                                rdf:about='https://the.link.example/what-was-linked'>\
                 <og:title>The Best Webpage</og:title>\
                 <og:image>https://remote.example/preview.png</og:image>\
                 <ogi:type>image/png</ogi:type>\
               </rdf:Description>\
             </message>",
        ));
        assert_eq!(ffi.link_previews.len(), 1);
        assert!(ffi.link_previews[0].image.is_none());
        assert!(ffi.link_previews[0].remote_media_unavailable);
    }

    #[test]
    fn pin_event_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='room@conf.waddle.test'>\
               <body>alice pinned a message</body>\
               <pin-event xmlns='urn:waddle:pin:0' action='pinned' target='stanza-1' \
                          by='admin@waddle.test'>\
                 <preview>\
                   <author jid='alice@waddle.test' nick='alice'/>\
                   <text>important update</text>\
                   <ts>2026-05-08T11:55:00+00:00</ts>\
                 </preview>\
               </pin-event>\
             </message>",
        ));
        let pin = ffi.pin_event.expect("pin event survives conversion");
        assert_eq!(pin.action, WaddlePinAction::Pinned);
        assert_eq!(pin.target_stanza_id, "stanza-1");
        assert_eq!(pin.by, "admin@waddle.test");
        assert!(pin.reason.is_none());
        let preview = pin.preview.expect("pinned event carries a preview");
        assert_eq!(preview.author_jid, "alice@waddle.test");
        assert_eq!(preview.author_nick.as_deref(), Some("alice"));
        assert_eq!(preview.text, "important update");
        assert_eq!(preview.message_timestamp, "2026-05-08T11:55:00+00:00");

        let unpin = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='room@conf.waddle.test'>\
               <pin-event xmlns='urn:waddle:pin:0' action='unpinned' target='stanza-1' \
                          by='admin@waddle.test' reason='retracted'/>\
             </message>",
        ))
        .pin_event
        .expect("unpin event survives conversion");
        assert_eq!(unpin.action, WaddlePinAction::Unpinned);
        assert_eq!(unpin.reason.as_deref(), Some("retracted"));
        assert!(unpin.preview.is_none());
    }

    #[test]
    fn call_thread_ended_survives_inbound_to_ffi() {
        let ffi = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' id='ended-1' \
                      from='general@muc.waddle.test'>\
               <apply-to xmlns='urn:xmpp:fasten:0' id='anchor-stanza-id'>\
                 <call-thread-ended xmlns='urn:waddle:call-thread:0' \
                                    ended='2026-06-07T14:35:00Z' duration='PT5M'/>\
               </apply-to>\
             </message>",
        ));
        let ended = ffi
            .call_thread_ended
            .expect("call-thread-ended survives conversion");
        assert_eq!(ended.anchor_id, "anchor-stanza-id");
        assert_eq!(ended.ended, "2026-06-07T14:35:00+00:00");
        assert_eq!(ended.duration, "PT5M");
    }

    #[test]
    fn forum_topic_metadata_and_subject_survive_inbound_to_ffi() {
        let topic = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='forum@conf.waddle.test/alice'>\
               <subject>Release planning</subject>\
               <body>First post of the topic</body>\
               <thread>topic-thread-1</thread>\
             </message>",
        ));
        assert_eq!(topic.subject.as_deref(), Some("Release planning"));
        assert_eq!(topic.forum_post_kind, Some(WaddleForumPostKind::Topic));
        assert_eq!(topic.forum_title.as_deref(), Some("Release planning"));
        assert_eq!(topic.thread.as_deref(), Some("topic-thread-1"));

        let reply = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='forum@conf.waddle.test/bob'>\
               <body>A reply in the topic</body>\
               <thread>topic-thread-1</thread>\
             </message>",
        ));
        assert_eq!(reply.forum_post_kind, Some(WaddleForumPostKind::Reply));
        assert!(reply.forum_title.is_none());

        // Bare room-topic change: subject only, no forum classification.
        let room_topic = inbound_to_ffi(parse_message(
            "<message xmlns='jabber:client' type='groupchat' from='room@conf.waddle.test/alice'>\
               <subject>New room topic</subject>\
             </message>",
        ));
        assert_eq!(room_topic.subject.as_deref(), Some("New room topic"));
        assert!(room_topic.forum_post_kind.is_none());
    }

    #[test]
    fn presence_extras_survive_presence_to_ffi() {
        let stanza: Element = "<presence xmlns='jabber:client' \
                                from='room@muc.waddle.test/alice' \
                                to='alice@waddle.test/desktop'>\
            <x xmlns='http://jabber.org/protocol/muc#user'>\
              <item affiliation='member' role='participant' jid='alice@waddle.test/desktop'/>\
              <status code='100'/>\
              <status code='110'/>\
            </x>\
            <x xmlns='vcard-temp:x:update'><photo>a1b2c3d4</photo></x>\
            <idle xmlns='urn:xmpp:idle:1' since='2026-06-01T12:00:00Z'/>\
            <in-call xmlns='urn:waddle:in-call:0'><hand-raised/><muted/></in-call>\
        </presence>"
            .parse()
            .expect("fixture parses");
        let Some(MessagingEvent::Presence(pres)) = messaging::parse(&stanza) else {
            panic!("expected Presence variant");
        };
        let ffi = presence_to_ffi(*pres);
        assert_eq!(ffi.muc_jid.as_deref(), Some("alice@waddle.test/desktop"));
        assert_eq!(ffi.muc_status_codes, vec![100, 110]);
        assert_eq!(ffi.vcard_avatar.as_deref(), Some("a1b2c3d4"));
        assert_eq!(ffi.idle_since.as_deref(), Some("2026-06-01T12:00:00+00:00"));
        assert!(ffi.hand_raised);
        assert!(ffi.muted);
        assert!(ffi.error_condition.is_none());
        assert!(ffi.error_type.is_none());
        assert!(ffi.error_text.is_none());
    }

    #[test]
    fn presence_error_fields_survive_presence_to_ffi() {
        let stanza: Element = "<presence xmlns='jabber:client' type='error' \
                                from='room@muc.waddle.test/alice'>\
            <error type='auth'>\
              <registration-required xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/>\
              <text xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'>Members only</text>\
            </error>\
        </presence>"
            .parse()
            .expect("fixture parses");
        let Some(MessagingEvent::Presence(pres)) = messaging::parse(&stanza) else {
            panic!("expected Presence variant");
        };
        let ffi = presence_to_ffi(*pres);
        assert_eq!(ffi.presence_type, "error");
        assert_eq!(
            ffi.error_condition.as_deref(),
            Some("registration-required")
        );
        assert_eq!(ffi.error_type, Some(WaddleStanzaErrorType::Auth));
        assert_eq!(ffi.error_text.as_deref(), Some("Members only"));
    }

    #[test]
    fn archived_to_ffi_maps_extended_inner_message_fields() {
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-ext' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='m-ext' \
                            from='forum@conf.waddle.test/alice'>\
                     <subject>Release planning</subject>\
                     <body>bold @bob https://example.com</body>\
                     <thread>topic-thread-1</thread>\
                     <stanza-id xmlns='urn:xmpp:sid:0' id='sid-archive' by='archive.waddle.test'/>\
                     <stanza-id xmlns='urn:xmpp:sid:0' id='sid-room' by='forum@conf.waddle.test'/>\
                     <x xmlns='http://jabber.org/protocol/muc#user'>\
                       <item affiliation='member' role='participant' \
                             jid='alice@waddle.test/desktop'/>\
                     </x>\
                     <markup xmlns='urn:xmpp:markup:0'>\
                       <span start='0' end='4'><strong/></span>\
                     </markup>\
                     <reference xmlns='urn:xmpp:reference:0' type='mention' \
                        uri='xmpp:bob@waddle.test' begin='5' end='9'/>\
                     <sticker xmlns='urn:xmpp:stickers:0' pack='crabs'/>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );

        let ffi = archived_to_ffi(archived).expect("parsed archive row must convert");
        assert_eq!(ffi.mam_id, "mam-ext");
        assert_eq!(ffi.subject.as_deref(), Some("Release planning"));
        assert_eq!(ffi.stanza_id.as_deref(), Some("sid-archive"));
        assert_eq!(ffi.stanza_id_by.as_deref(), Some("archive.waddle.test"));
        assert_eq!(ffi.stanza_ids.len(), 2);
        assert_eq!(ffi.stanza_ids[1].id, "sid-room");
        assert_eq!(ffi.stanza_ids[1].by, "forum@conf.waddle.test");
        assert_eq!(ffi.author_real_jid.as_deref(), Some("alice@waddle.test"));
        assert_eq!(ffi.markup_spans.len(), 1);
        assert_eq!(ffi.markup_spans[0].span_type, WaddleMarkupSpanType::Bold);
        assert_eq!(ffi.mention_uris, vec!["xmpp:bob@waddle.test".to_string()]);
        assert_eq!(ffi.references.len(), 1);
        assert_eq!(ffi.references[0].ref_type, WaddleReferenceType::Mention);
        assert_eq!(ffi.forum_post_kind, Some(WaddleForumPostKind::Topic));
        assert_eq!(ffi.forum_title.as_deref(), Some("Release planning"));
        assert!(ffi.is_sticker);
        assert!(!ffi.is_retracted);
        assert!(ffi.retraction_id.is_none());
    }

    #[test]
    fn archived_to_ffi_maps_tombstone_and_call_thread_ended() {
        let tombstone = archived_to_ffi(parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-tombstone' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='target-message-id' \
                            from='room@conf.waddle.test/alice'>\
                     <retracted xmlns='urn:xmpp:message-retract:1' id='retraction-message-id' \
                                stamp='2026-05-06T12:00:01Z'>\
                       <moderated xmlns='urn:xmpp:message-moderate:1' \
                                  by='room@conf.waddle.test/mod'/>\
                       <reason>spam</reason>\
                     </retracted>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        ))
        .expect("tombstone row must convert");
        assert!(tombstone.is_retracted);
        assert_eq!(
            tombstone.retraction_id.as_deref(),
            Some("retraction-message-id")
        );
        assert_eq!(
            tombstone.moderated_by.as_deref(),
            Some("room@conf.waddle.test/mod")
        );
        assert_eq!(tombstone.moderation_reason.as_deref(), Some("spam"));

        let ended = archived_to_ffi(parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-ended' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-06-07T14:35:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='ended-1' \
                            from='general@muc.waddle.test'>\
                     <apply-to xmlns='urn:xmpp:fasten:0' id='anchor-stanza-id'>\
                       <call-thread-ended xmlns='urn:waddle:call-thread:0' \
                                          ended='2026-06-07T14:35:00Z' duration='PT5M'/>\
                     </apply-to>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        ))
        .expect("ended row must convert")
        .call_thread_ended
        .expect("call-thread-ended survives archive conversion");
        assert_eq!(ended.anchor_id, "anchor-stanza-id");
        assert_eq!(ended.duration, "PT5M");
    }

    /// Wasm-parity drop-guard (XEP-0425 spoof): an occupant-authored
    /// "moderation" carrying a `<body>` fails the trusted parse, so the
    /// row converts to `None` instead of rendering the spoofed body as a
    /// normal, non-retracted chat message.
    #[test]
    fn archived_to_ffi_discards_spoofed_moderation_with_body() {
        let spoofed = archived_to_ffi(parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-spoof' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-06T12:00:00Z'/>\
                   <message xmlns='jabber:client' type='groupchat' id='spoof-1' \
                            from='room@conf.waddle.test/alice'>\
                     <body>this must not render as normal chat</body>\
                     <retract xmlns='urn:xmpp:message-retract:1' id='target-id'>\
                       <moderated xmlns='urn:xmpp:message-moderate:1' \
                                  by='room@conf.waddle.test/alice'/>\
                     </retract>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        ));
        assert!(spoofed.is_none());
    }

    // ── Outbound send-options mapping ────────────────────────────────────────

    #[test]
    fn send_options_map_subject_muc_pm_markup_and_references() {
        let opts = WaddleSendOptions {
            subject: Some("Release planning".to_string()),
            muc_pm: true,
            markup_spans: vec![
                WaddleMarkupSpan {
                    span_type: WaddleMarkupSpanType::Bold,
                    start: 0,
                    end: 4,
                    uri: None,
                },
                WaddleMarkupSpan {
                    span_type: WaddleMarkupSpanType::CodeBlock,
                    start: 5,
                    end: 10,
                    uri: None,
                },
                WaddleMarkupSpan {
                    span_type: WaddleMarkupSpanType::Link,
                    start: 11,
                    end: 30,
                    uri: Some("https://example.com".to_string()),
                },
            ],
            references: vec![WaddleReference {
                ref_type: WaddleReferenceType::Mention,
                uri: "xmpp:bob@waddle.test".to_string(),
                begin: 5,
                end: 9,
                anchor: Some("@bob".to_string()),
            }],
            ..WaddleSendOptions::default()
        };

        let mapped = send_options_from_ffi(opts).expect("options convert");
        assert_eq!(mapped.subject.as_deref(), Some("Release planning"));
        assert!(mapped.muc_pm);
        assert_eq!(mapped.markup_spans.len(), 3);
        // Wire names must match the strings the outbound XEP-0394
        // builder branches on.
        assert_eq!(mapped.markup_spans[0].span_type, "bold");
        assert_eq!(mapped.markup_spans[0].start, 0);
        assert_eq!(mapped.markup_spans[0].end, 4);
        assert_eq!(mapped.markup_spans[1].span_type, "code_block");
        assert_eq!(mapped.markup_spans[2].span_type, "link");
        assert_eq!(
            mapped.markup_spans[2].uri.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(mapped.references.len(), 1);
        assert_eq!(mapped.references[0].ref_type, "mention");
        assert_eq!(mapped.references[0].uri, "xmpp:bob@waddle.test");
        assert_eq!(mapped.references[0].begin, 5);
        assert_eq!(mapped.references[0].end, 9);
        assert_eq!(mapped.references[0].anchor.as_deref(), Some("@bob"));
    }

    #[test]
    fn send_options_markup_span_wire_names_cover_every_variant() {
        // The outbound builder silently skips unknown span strings, so
        // every FFI variant must map to a string the builder accepts.
        let cases = [
            (WaddleMarkupSpanType::Bold, "bold"),
            (WaddleMarkupSpanType::Italic, "italic"),
            (WaddleMarkupSpanType::Strikethrough, "strikethrough"),
            (WaddleMarkupSpanType::Code, "code"),
            (WaddleMarkupSpanType::CodeBlock, "code_block"),
            (WaddleMarkupSpanType::Blockquote, "blockquote"),
            (WaddleMarkupSpanType::Link, "link"),
        ];
        for (variant, wire) in cases {
            assert_eq!(markup_span_type_wire_name(variant), wire);
        }
    }

    // ── XEP-0198 resume snapshot round-trip ──────────────────────────────────

    #[test]
    fn resume_state_round_trips_through_session_config_and_snapshot() {
        use waddle_xmpp_client::stream_management::SmState;

        // Serialize the queued stanza with the same writer the FFI
        // uses so string equality is meaningful.
        let queued: Element = "<message xmlns='jabber:client' id='queued-1' type='chat' \
                                to='bob@waddle.test'>\
                                 <body>unacked</body>\
                                 <delay xmlns='urn:xmpp:delay' stamp='2026-06-01T12:00:00Z'/>\
                               </message>"
            .parse()
            .expect("fixture parses");
        let original = WaddleSmResumeState {
            previd: "prev-stream".to_string(),
            inbound_h: 5,
            outbound_h: 9,
            max_resume_seconds: Some(300),
            queued_stanzas_xml: vec![String::from(&queued)],
        };

        // FFI record → typed SmResumeState (as threaded into
        // SessionConfig.stream_management.resume_state on connect).
        let typed = resume_state_from_ffi(original.clone()).expect("resume state converts");
        assert_eq!(typed.previd(), "prev-stream");
        assert_eq!(typed.inbound_h(), 5);
        assert_eq!(typed.outbound_h(), 9);
        assert_eq!(typed.max_resume_seconds(), Some(300));
        assert_eq!(
            typed.unhandled_message_stanza_ids(),
            vec![StanzaId::new("queued-1").expect("stanza id")],
            "message stanza-id must be re-derived from the persisted XML"
        );

        // Seed a runtime SM state from it and snapshot back — the
        // round trip the native reconnect path performs.
        let snapshot = SmState::from_resume_state(&typed)
            .resume_state()
            .expect("seeded SM state stays resumable");
        assert_eq!(snapshot, typed, "snapshot must preserve the seeded state");

        let round_tripped = resume_state_to_ffi(snapshot);
        assert_eq!(round_tripped, original);
    }

    #[test]
    fn resume_state_from_ffi_rejects_malformed_stanza_xml() {
        let err = resume_state_from_ffi(WaddleSmResumeState {
            previd: "prev-stream".to_string(),
            inbound_h: 0,
            outbound_h: 1,
            max_resume_seconds: None,
            queued_stanzas_xml: vec!["<not-xml".to_string()],
        })
        .expect_err("malformed XML must be rejected");
        assert!(err.contains("invalid resume stanza XML"), "err: {err}");
    }

    /// In-test listener that captures every dispatched event in
    /// order. Used to verify the `dispatch_event` routing without
    /// spinning up the tokio broadcast bus.
    #[derive(Default)]
    struct CapturingListener {
        // Mutex so we can mutate from `&self` callbacks. `parking_lot`
        // would be lighter but the test-only `std::sync::Mutex` is
        // available everywhere the workspace builds.
        events: std::sync::Mutex<Vec<WaddleClientEvent>>,
    }

    impl CapturingListener {
        fn events(&self) -> Vec<WaddleClientEvent> {
            self.events
                .lock()
                .expect("test capture mutex poisoned")
                .clone()
        }
    }

    impl WaddleEventListener for CapturingListener {
        fn on_event(&self, event: WaddleClientEvent) {
            self.events
                .lock()
                .expect("test capture mutex poisoned")
                .push(event);
        }
    }

    #[test]
    fn dispatch_event_maps_client_events_to_typed_variants() {
        use waddle_xmpp_client::SessionBinding;

        let listener = CapturingListener::default();
        let account = "alice@waddle.test";

        let binding = SessionBinding {
            jid: "alice@waddle.test/desktop"
                .parse()
                .expect("full JID parses"),
            stream_id: None,
            resumable: false,
        };
        dispatch_event(
            ClientEvent::Lifecycle(LifecycleEvent::SessionReady(binding)),
            account,
            &listener,
        );

        let msg_xml = "<message xmlns='jabber:client' type='chat' \
                        from='bob@waddle.test/desktop'><body>hi</body></message>";
        let msg = parse_message(msg_xml);
        dispatch_event(
            ClientEvent::Messaging(MessagingEvent::Message(Box::new(msg))),
            account,
            &listener,
        );

        let presence_stanza: Element = "<presence xmlns='jabber:client' \
                                         from='room@muc.waddle.test/bob'/>"
            .parse()
            .expect("fixture parses");
        let Some(MessagingEvent::Presence(pres)) = messaging::parse(&presence_stanza) else {
            panic!("expected Presence variant");
        };
        dispatch_event(
            ClientEvent::Messaging(MessagingEvent::Presence(pres)),
            account,
            &listener,
        );

        dispatch_event(
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Acked {
                stanza_id: StanzaId::new("acked-1").expect("stanza id"),
            }),
            account,
            &listener,
        );
        dispatch_event(
            ClientEvent::MessageDelivery(MessageDeliveryEvent::Failed {
                stanza_id: StanzaId::new("failed-1").expect("stanza id"),
            }),
            account,
            &listener,
        );

        let call_xml = "<message xmlns='jabber:client' from='bob@waddle.test/desktop'>\
            <proceed xmlns='urn:xmpp:jingle-message:0' id='c1'/>\
        </message>";
        let call_stanza: Element = call_xml.parse().expect("fixture parses");
        let call = messaging::parse_call_event(&call_stanza).expect("fixture is a call");
        dispatch_event(ClientEvent::Call(Box::new(call)), account, &listener);

        dispatch_event(
            ClientEvent::ResumeStateChanged(Some(
                SmResumeState::new("prev-1", 3, 7).expect("resume state"),
            )),
            account,
            &listener,
        );
        dispatch_event(ClientEvent::ResumeStateChanged(None), account, &listener);

        let events = listener.events();
        assert_eq!(events.len(), 8);
        assert!(matches!(events[0], WaddleClientEvent::Connected));
        match &events[1] {
            WaddleClientEvent::Message { message } => {
                assert_eq!(message.body.as_deref(), Some("hi"));
            }
            _ => panic!("expected Message variant"),
        }
        match &events[2] {
            WaddleClientEvent::Presence { presence } => {
                assert_eq!(presence.from.as_deref(), Some("room@muc.waddle.test/bob"));
            }
            _ => panic!("expected Presence variant"),
        }
        match &events[3] {
            WaddleClientEvent::DeliveryAcked { stanza_id } => assert_eq!(stanza_id, "acked-1"),
            _ => panic!("expected DeliveryAcked variant"),
        }
        match &events[4] {
            WaddleClientEvent::DeliveryFailed { stanza_id } => assert_eq!(stanza_id, "failed-1"),
            _ => panic!("expected DeliveryFailed variant"),
        }
        match &events[5] {
            WaddleClientEvent::Call { event } => {
                assert!(matches!(event.kind, WaddleCallEventKind::Proceed));
                assert_eq!(event.sid, "c1");
            }
            _ => panic!("expected Call variant"),
        }
        match &events[6] {
            WaddleClientEvent::ResumeStateChanged { state } => {
                let state = state.as_ref().expect("resume snapshot present");
                assert_eq!(state.previd, "prev-1");
                assert_eq!(state.inbound_h, 3);
                assert_eq!(state.outbound_h, 7);
            }
            _ => panic!("expected ResumeStateChanged variant"),
        }
        match &events[7] {
            WaddleClientEvent::ResumeStateChanged { state } => assert!(state.is_none()),
            _ => panic!("expected ResumeStateChanged(None) variant"),
        }
    }

    #[test]
    fn dispatch_event_maps_sasl_failure_to_authentication_failed() {
        use waddle_xmpp_client::{ConnectionEvent, SaslFailure, SaslFailureCondition};
        let listener = CapturingListener::default();
        dispatch_event(
            ClientEvent::Connection(ConnectionEvent::AuthenticationFailed(SaslFailure {
                condition: SaslFailureCondition::NotAuthorized,
            })),
            "icepuma@waddle.test",
            &listener,
        );
        let events = listener.events.lock().expect("listener lock");
        assert_eq!(events.len(), 1);
        match &events[0] {
            WaddleClientEvent::AuthenticationFailed { condition } => {
                assert_eq!(*condition, WaddleSaslCondition::NotAuthorized);
            }
            _ => panic!("expected AuthenticationFailed"),
        }
    }

    #[test]
    fn dispatch_event_maps_mam_results() {
        let listener = CapturingListener::default();
        let archived = parse_mam_archived(
            "<message xmlns='jabber:client'>\
               <result xmlns='urn:xmpp:mam:2' id='mam-1' queryid='q1'>\
                 <forwarded xmlns='urn:xmpp:forward:0'>\
                   <delay xmlns='urn:xmpp:delay' stamp='2026-05-25T10:00:00Z'/>\
                   <message xmlns='jabber:client' type='chat' id='m1' \
                            from='bob@waddle.test/phone'>\
                     <body>archived</body>\
                   </message>\
                 </forwarded>\
               </result>\
             </message>",
        );
        dispatch_event(
            ClientEvent::MamResult(Box::new(archived)),
            "alice@waddle.test",
            &listener,
        );
        let events = listener.events();
        assert_eq!(events.len(), 1);
        match &events[0] {
            WaddleClientEvent::MamResult { message } => {
                assert_eq!(message.mam_id, "mam-1");
                assert_eq!(message.body.as_deref(), Some("archived"));
            }
            _ => panic!("expected MamResult variant"),
        }
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
        assert!(listener.events().is_empty());
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
