use std::borrow::Cow;

use tracing::warn;
use waddle_xmpp::{
    parser::stanza_to_string,
    protocol::handlers::errors::bad_request_reply,
    protocol::{frame::InboundFrame, InboundEvent, XmppStateMachine},
    xep::{
        add_reference, build_file_sharing_element, build_link_metadata_element,
        decode_link_preview_token, extract_link_preview_request_from_message,
        is_file_sharing_element, parse_fallbacks_from_message, parse_file_sharing_element,
        strip_fallback_ranges, strip_link_metadata, strip_link_preview_requests, Disposition,
        FallbackRegion, FileMetadata, FileSharing, LinkMetadata, Reference, WaddleLinkPreviewError,
        NS_DELAY, NS_REPLY,
    },
    Stanza,
};
use waddle_xmpp_core::first_eligible_https_url_text;
#[cfg(test)]
use waddle_xmpp_core::PreviewImageMediaType;

use super::super::{
    interpret_loop::build_interpret_deps, replay::drive_interpret_loop, WebSocketState,
};
use crate::auth::Session;
use crate::config::LinkPreviewConfig;
use crate::server::routes::websocket::link_preview_telemetry::{
    record_link_preview_event, LinkPreviewTelemetryEvent,
};
use waddle_xmpp::protocol::ConnectionPhase;

/// Thin transport adapter that drives the sans-I/O dispatcher
/// (#229 PR16 + PR18). Every `<message/>` stanza arriving on the
/// WebSocket transport flows through here:
///
/// 1. Wrap the typed [`xmpp_parsers::message::Message`] in
///    [`InboundEvent::FrameReceived`] and feed it to the per-connection
///    [`XmppStateMachine`]. The state machine runs the locked-Q2(a)
///    chain (`BlockingFilter → RichTargetValidation → Canonicalize →
///    EnrichmentDispatch → Archive → CarbonsMessage → Inbox → Route`)
///    for `<message type='chat'>` and emits typed
///    [`waddle_xmpp::protocol::OutboundEvent`]s.
/// 2. For `<message type='groupchat'>` traffic, the chain emits
///    [`waddle_xmpp::protocol::OutboundEvent::DispatchToRoom`]; the
///    interpreter resolves it against the room handler chain
///    (`OccupancyValidation → MucCanonicalize → MucArchive →
///    MucInbox → Reflector`) and recursively interprets the chain's
///    own emitted events.
/// 3. The interpreter ([`crate::server::routes::interpret::interpret`])
///    executes the I/O side effects (route to peer, persist to MAM,
///    project inbox, fan XEP-0280 carbons, etc.) and returns the
///    serialized wire frames the caller writes back to the sender's
///    transport.
///
/// `authenticated_session` is threaded through so the
/// [`OutboundEvent::DispatchToRoom`] arm can perform the managed-room
/// owner check (announcements room admits server owners only).
///
/// [`OutboundEvent::DispatchToRoom`]: waddle_xmpp::protocol::OutboundEvent::DispatchToRoom
pub async fn handle_message(
    mut incoming: xmpp_parsers::message::Message,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    state_machine: Option<&mut XmppStateMachine>,
    authenticated_session: Option<&Session>,
) -> Vec<String> {
    let Some(bound_jid) = phase.bound_jid().cloned() else {
        warn!("Message received without authenticated session");
        return vec![];
    };
    let Some(sm) = state_machine else {
        warn!(
            "Message received before per-connection state machine was initialized; \
             dropping. This indicates a stanza arrived before bind completed."
        );
        return vec![];
    };

    strip_client_authored_delay(&mut incoming);
    consume_link_preview_request(
        &mut incoming,
        &bound_jid,
        state.deps.occupant_id_secret.key(),
        chrono::Utc::now().timestamp(),
        state.deps.auth_state.base_url.as_str(),
        &state.deps.link_preview,
    );

    if incoming.type_ != xmpp_parsers::message::MessageType::Groupchat
        && incoming.type_ != xmpp_parsers::message::MessageType::Error
        && waddle_extensions::message_has_framework_envelope(&incoming)
    {
        let mut stamped = incoming.clone();
        stamped.from = Some(jid::Jid::from(bound_jid));
        remove_framework_envelopes(&mut stamped);
        let reply = bad_request_reply(
            &stamped,
            "Client-authored Waddle extension envelopes are not allowed.",
        );
        return match stanza_to_string(reply) {
            Ok(frame) => vec![frame],
            Err(error) => {
                warn!(error = ?error, "failed to serialize framework-envelope rejection");
                vec![]
            }
        };
    }

    let events = sm.handle(InboundEvent::FrameReceived(InboundFrame::Stanza(Box::new(
        Stanza::Message(incoming),
    ))));
    let deps = build_interpret_deps(state, authenticated_session);
    let (frames, _close) = drive_interpret_loop(events, sm, &deps).await;
    frames
}

fn strip_client_authored_delay(message: &mut xmpp_parsers::message::Message) {
    message
        .payloads
        .retain(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY));
}

fn consume_link_preview_request(
    message: &mut xmpp_parsers::message::Message,
    sender_jid: &jid::FullJid,
    secret: &[u8],
    now_unix: i64,
    trusted_media_base_url: &str,
    link_preview: &LinkPreviewConfig,
) {
    if !link_preview.enabled {
        strip_link_preview_requests(message);
        strip_link_metadata(message);
        return;
    }
    // Trust-anchor guard: the server is the sole authority for direct-video
    // previews. A client must not pre-author an XEP-0447 file-share whose
    // source is the previewed link and have it promoted to a "trusted" video
    // card by recipients. Drop any client-authored file-share that targets the
    // message's first eligible link before the server stamps its own. Genuine
    // XEP-0363 uploads point at the Waddle media origin, never the external
    // body link, so this never strips a real attachment.
    strip_client_file_sharing_for_previewed_link(message);
    let expected_sender = sender_jid.to_bare();
    let expected_scope = message.to.as_ref().map(|to| to.to_bare());
    let decoded = extract_link_preview_request_from_message(message).and_then(|token| {
        match decode_link_preview_token(&token, secret, now_unix) {
            Ok(preview) => Some(preview),
            Err(WaddleLinkPreviewError::Expired) => {
                record_link_preview_event(LinkPreviewTelemetryEvent::TokenExpired);
                None
            }
            Err(_) => {
                record_link_preview_event(LinkPreviewTelemetryEvent::TokenInvalid);
                None
            }
        }
    });
    let metadata = decoded
        .filter(|preview| {
            Some(&preview.scope_jid) == expected_scope.as_ref()
                && preview.sender_jid == expected_sender
                && preview.original_url.scheme() == "https"
                && preview.normalized_url.scheme() == "https"
                && link_preview_token_urls_allowed_by_current_policy(preview, link_preview)
                && first_eligible_https_url(message).as_ref() == Some(&preview.original_url)
        })
        .map(|preview| {
            // Direct-video preview source is a remote URL already validated by
            // the token/host policy above. It is stamped as a conformant
            // XEP-0447 inline file-share; the client plays it on user action.
            let video_sharing = preview
                .video
                .filter(|video| {
                    link_preview_url_allowed_by_current_policy(&video.url, link_preview)
                })
                .map(|video| {
                    let mut file = FileMetadata::new().with_media_type(video.media_type.as_str());
                    if let Some(size) = video.size {
                        file = file.with_size(size);
                    }
                    FileSharing::new(file)
                        .with_url(video.url.as_str())
                        .with_disposition(Disposition::Inline)
                });

            let mut metadata =
                LinkMetadata::new(preview.original_url).with_canonical_url(preview.normalized_url);
            if let Some(title) = preview.title {
                metadata = metadata.with_title(title);
            }
            if let Some(description) = preview.description {
                metadata = metadata.with_description(description);
            }
            if let Some(image) = preview.image.filter(|image| {
                is_trusted_cached_preview_image_url(&image.url, trusted_media_base_url)
            }) {
                add_reference(message, &Reference::data(image.url.as_str()));
                let mut preview_image = waddle_xmpp::xep::LinkPreviewImage::new(image.url)
                    .with_media_type(image.media_type);
                if let (Some(width), Some(height)) = (image.width, image.height) {
                    preview_image = preview_image.with_dimensions(width, height);
                }
                if let Some(alt) = image.alt {
                    preview_image = preview_image.with_alt(alt);
                }
                metadata = metadata.with_image(preview_image);
            }
            (metadata, video_sharing)
        });
    strip_link_preview_requests(message);
    strip_link_metadata(message);
    if let Some((metadata, video_sharing)) = metadata {
        message
            .payloads
            .push(build_link_metadata_element(&metadata));
        if let Some(video_sharing) = video_sharing {
            message
                .payloads
                .push(build_file_sharing_element(&video_sharing));
        }
    }
}

fn link_preview_token_urls_allowed_by_current_policy(
    preview: &waddle_xmpp::xep::LinkPreviewTokenData,
    link_preview: &LinkPreviewConfig,
) -> bool {
    link_preview_url_allowed_by_current_policy(&preview.original_url, link_preview)
        && link_preview_url_allowed_by_current_policy(&preview.normalized_url, link_preview)
}

/// Remove client-authored XEP-0447 file-shares whose source is the link this
/// message previews. See the call site for the trust rationale.
fn strip_client_file_sharing_for_previewed_link(message: &mut xmpp_parsers::message::Message) {
    let Some(body_url) = first_eligible_https_url(message) else {
        return;
    };
    message.payloads.retain(|payload| {
        if !is_file_sharing_element(payload) {
            return true;
        }
        let targets_previewed_link = parse_file_sharing_element(payload)
            .and_then(|sharing| sharing.first_url().map(str::to_string))
            .and_then(|raw| url::Url::parse(&raw).ok())
            .is_some_and(|source| source == body_url);
        !targets_previewed_link
    });
}

fn link_preview_url_allowed_by_current_policy(
    url: &url::Url,
    link_preview: &LinkPreviewConfig,
) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    if !link_preview.video_enabled && looks_like_direct_video_url(url) {
        return false;
    }
    let host = match url.host() {
        Some(url::Host::Domain(host)) if !is_dot_local_domain(host) => host,
        Some(url::Host::Domain(_)) | Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)) | None => {
            return false;
        }
    };
    if link_preview
        .blocked_hosts
        .iter()
        .any(|pattern| pattern.matches(host))
    {
        return false;
    }
    link_preview.allowed_hosts.is_empty()
        || link_preview
            .allowed_hosts
            .iter()
            .any(|pattern| pattern.matches(host))
}

fn looks_like_direct_video_url(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    matches!(
        path.rsplit('/').next(),
        Some(filename)
            if filename.ends_with(".mp4")
                || filename.ends_with(".webm")
                || filename.ends_with(".mov")
                || filename.ends_with(".m4v")
                || filename.ends_with(".ogv")
    )
}

fn is_dot_local_domain(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "local" || host.ends_with(".local")
}

fn is_trusted_cached_preview_image_url(url: &url::Url, trusted_media_base_url: &str) -> bool {
    let Ok(trusted) = url::Url::parse(trusted_media_base_url) else {
        return false;
    };
    trusted_preview_schemes_match(url, &trusted)
        && url.host_str() == trusted.host_str()
        && url.port_or_known_default() == trusted.port_or_known_default()
        && is_link_preview_xep0363_file_path(url.path())
}

fn trusted_preview_schemes_match(url: &url::Url, trusted: &url::Url) -> bool {
    if url.scheme() == "https" && trusted.scheme() == "https" {
        return true;
    }
    url.scheme() == "http" && trusted.scheme() == "http" && is_loopback_host(url.host_str())
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost"))
        || host
            .and_then(|host| host.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
}

fn is_link_preview_xep0363_file_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/api/files/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let Some(slot_id) = parts.next() else {
        return false;
    };
    let Some(filename) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || uuid::Uuid::parse_str(slot_id).is_err() {
        return false;
    }
    let Some(name) = filename.strip_prefix("link-preview-") else {
        return false;
    };
    let Some((hash, extension)) = name.rsplit_once('.') else {
        return false;
    };
    matches!(extension, "png" | "jpg" | "gif" | "webp")
        && hash.len() == 64
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn first_eligible_https_url(message: &xmpp_parsers::message::Message) -> Option<url::Url> {
    let body = message.bodies.get("")?;
    let body = body_without_reply_fallback(message, body);
    first_eligible_https_url_text(&body).and_then(|candidate| url::Url::parse(candidate).ok())
}

fn body_without_reply_fallback<'a>(
    message: &xmpp_parsers::message::Message,
    body: &'a str,
) -> Cow<'a, str> {
    let mut ranges = Vec::new();
    for fallback in parse_fallbacks_from_message(message) {
        if fallback.for_ns.as_deref() != Some(NS_REPLY) {
            continue;
        }
        match fallback.body {
            Some(FallbackRegion::Whole) => return Cow::Borrowed(""),
            Some(FallbackRegion::Ranges(body_ranges)) => ranges.extend(body_ranges),
            None => {}
        }
    }
    if ranges.is_empty() {
        Cow::Borrowed(body)
    } else {
        Cow::Owned(strip_fallback_ranges(body, &ranges))
    }
}

fn remove_framework_envelopes(message: &mut xmpp_parsers::message::Message) {
    message
        .payloads
        .retain(|payload| !payload.ns().starts_with("urn:waddle:"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::link_preview_telemetry::recorded_events;
    const SECRET: &[u8] = b"test-link-preview-secret";

    fn sender() -> jid::FullJid {
        "alice@example.com/desktop".parse().expect("jid")
    }
    use xmpp_parsers::message::Message;
    use xmpp_parsers::minidom::rxml::xml_ncname;
    use xmpp_parsers::minidom::Element;

    #[test]
    fn strips_client_supplied_delay_without_touching_other_payloads() {
        let xml = "<message xmlns='jabber:client' type='chat'>\
                    <body>Hello</body>\
                    <delay xmlns='urn:xmpp:delay' from='evil.example' stamp='2024-06-01T09:30:00Z'>forged</delay>\
                    <envelope xmlns='urn:waddle:test'/>\
                    </message>";
        let mut message =
            Message::try_from(xml.parse::<Element>().expect("valid xml")).expect("message");

        strip_client_authored_delay(&mut message);

        assert!(message
            .payloads
            .iter()
            .all(|payload| payload.ns() != NS_DELAY));
        assert!(message
            .payloads
            .iter()
            .any(|payload| payload.ns().starts_with("urn:waddle:")));
    }

    #[test]
    fn cached_preview_image_url_trust_allows_loopback_http_only_for_matching_origin() {
        let loopback = url::Url::parse(
            "http://localhost:3000/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        )
        .expect("url");
        let non_loopback = url::Url::parse(
            "http://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        )
        .expect("url");

        assert!(is_trusted_cached_preview_image_url(
            &loopback,
            "http://localhost:3000"
        ));
        assert!(!is_trusted_cached_preview_image_url(
            &non_loopback,
            "http://waddle.example"
        ));
    }

    #[test]
    fn trusts_cached_preview_image_urls_with_explicit_default_ports() {
        let https_with_default_port = url::Url::parse(
            "https://waddle.example:443/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        )
        .expect("url");
        let loopback_with_default_port = url::Url::parse(
            "http://localhost:80/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        )
        .expect("url");

        assert!(is_trusted_cached_preview_image_url(
            &https_with_default_port,
            "https://waddle.example"
        ));
        assert!(is_trusted_cached_preview_image_url(
            &loopback_with_default_port,
            "http://localhost"
        ));
    }

    #[test]
    fn consumes_link_preview_request_and_stamps_xep0511_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://the.link.example.com/what-was-linked-to")
                .expect("url"),
            normalized_url: url::Url::parse(
                "https://example.com/canonical-url/for/what-was-linked-to",
            )
            .expect("url"),
            title: Some("The Best Webpage".to_string()),
            description: Some("This is a great webpage and you will really like it".to_string()),
            image: Some(waddle_xmpp::xep::LinkPreviewTokenImage {
                url: url::Url::parse(
                    "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
                )
                .expect("url"),
                media_type: PreviewImageMediaType::Png,
                width: Some(640),
                height: Some(360),
                alt: Some("Article screenshot".to_string()),
            }),
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://the.link.example.com/what-was-linked-to".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message
            .payloads
            .iter()
            .all(|payload| payload.ns() != waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW));
        let parsed = waddle_xmpp::xep::extract_link_metadata_from_message(&message);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].about, preview.original_url);
        assert_eq!(parsed[0].canonical_url, Some(preview.normalized_url));
        assert_eq!(parsed[0].title.as_deref(), Some("The Best Webpage"));
        assert_eq!(parsed[0].images.len(), 1);
        assert_eq!(
            parsed[0].images[0].url.as_str(),
            "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"
        );
        assert_eq!(
            parsed[0].images[0].media_type,
            Some(PreviewImageMediaType::Png)
        );
        let references = waddle_xmpp::xep::extract_references_from_message(&message);
        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].uri,
            "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"
        );
        assert_eq!(
            references[0].ref_type,
            waddle_xmpp::xep::ReferenceType::Data
        );
    }

    fn direct_video_preview_token() -> waddle_xmpp::xep::LinkPreviewTokenData {
        waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://cdn.example.com/clip.mp4").expect("url"),
            normalized_url: url::Url::parse("https://cdn.example.com/clip.mp4").expect("url"),
            title: None,
            description: None,
            image: None,
            video: Some(waddle_xmpp::xep::LinkPreviewTokenVideo {
                url: url::Url::parse("https://cdn.example.com/clip.mp4").expect("url"),
                media_type: waddle_xmpp_core::DirectVideoMediaType::Mp4,
                size: Some(4096),
            }),
            expires_at_unix: 1_900_000_000,
        }
    }

    fn message_with_direct_video_request(
        preview: &waddle_xmpp::xep::LinkPreviewTokenData,
    ) -> Message {
        let token = waddle_xmpp::xep::encode_link_preview_token(preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "watch https://cdn.example.com/clip.mp4".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));
        message
    }

    #[test]
    fn consumes_direct_video_request_and_stamps_xep0447_inline_file_sharing() {
        let preview = direct_video_preview_token();
        let mut message = message_with_direct_video_request(&preview);

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        let sharing = waddle_xmpp::xep::extract_file_sharing_from_message(&message)
            .expect("XEP-0447 file-sharing stamped for trusted direct video");
        assert!(sharing.is_inline(), "direct video preview is inline");
        assert_eq!(sharing.metadata.media_type.as_deref(), Some("video/mp4"));
        assert_eq!(sharing.metadata.size, Some(4096));
        assert_eq!(
            sharing.first_url(),
            Some("https://cdn.example.com/clip.mp4")
        );

        // XEP-0511 link metadata is stamped alongside the direct-media element.
        let parsed = waddle_xmpp::xep::extract_link_metadata_from_message(&message);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].about, preview.original_url);
    }

    #[test]
    fn client_authored_file_sharing_for_previewed_link_is_stripped() {
        // A sender hand-crafts an inline-video file-share whose source is the
        // very link they posted, hoping recipients promote it to a trusted
        // direct-video card. The server must drop it; only server-stamped
        // direct-video file-shares may reach recipients.
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "look https://attacker.example.com/article".to_string(),
        );
        let forged = FileSharing::new(
            FileMetadata::new()
                .with_media_type("video/mp4")
                .with_size(9000),
        )
        .with_url("https://attacker.example.com/article")
        .with_disposition(Disposition::Inline);
        message.payloads.push(build_file_sharing_element(&forged));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(
            waddle_xmpp::xep::extract_file_sharing_from_message(&message).is_none(),
            "client-authored file-share targeting the previewed link must be stripped"
        );
    }

    #[test]
    fn genuine_upload_file_sharing_survives_link_preview_consumption() {
        // A real XEP-0363 upload points at the Waddle media origin, not the
        // external body link, so it must NOT be stripped.
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "see https://example.com/article and my clip".to_string(),
        );
        let upload = FileSharing::new(
            FileMetadata::new()
                .with_media_type("video/mp4")
                .with_size(1234),
        )
        .with_url("https://waddle.example/api/files/abc/clip.mp4")
        .with_disposition(Disposition::Inline);
        message.payloads.push(build_file_sharing_element(&upload));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        let sharing = waddle_xmpp::xep::extract_file_sharing_from_message(&message)
            .expect("genuine upload file-share survives");
        assert_eq!(
            sharing.first_url(),
            Some("https://waddle.example/api/files/abc/clip.mp4")
        );
    }

    #[test]
    fn disabled_video_policy_suppresses_file_sharing_but_strips_request() {
        let preview = direct_video_preview_token();
        let mut message = message_with_direct_video_request(&preview);
        let config = LinkPreviewConfig {
            video_enabled: false,
            ..LinkPreviewConfig::default()
        };

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &config,
        );

        assert!(
            waddle_xmpp::xep::extract_file_sharing_from_message(&message).is_none(),
            "disable-video policy must not stamp direct-video file sharing"
        );
        assert!(
            message
                .payloads
                .iter()
                .all(|payload| payload.ns() != waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW),
            "private preview request is always stripped"
        );
    }

    #[test]
    fn disabled_policy_strips_link_preview_request_without_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/").expect("url"),
            normalized_url: url::Url::parse("https://example.com/").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig {
                enabled: false,
                ..LinkPreviewConfig::default()
            },
        );

        assert!(message
            .payloads
            .iter()
            .all(|payload| payload.ns() != waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW));
        assert!(waddle_xmpp::xep::extract_link_metadata_from_message(&message).is_empty());
    }

    #[test]
    fn tightened_policy_rechecks_token_urls_before_stamping_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://blocked.example/article").expect("url"),
            normalized_url: url::Url::parse("https://blocked.example/article").expect("url"),
            title: Some("Blocked".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://blocked.example/article".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig {
                blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
                ..LinkPreviewConfig::default()
            },
        );

        assert!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).is_empty(),
            "send-time validation must reject tokens that no longer satisfy current host policy"
        );
    }

    #[test]
    fn send_time_policy_rejects_unconditional_resolver_host_bans() {
        for blocked_url in [
            "https://127.0.0.1/article",
            "https://[::1]/article",
            "https://internal.local/article",
            "https://printer.local./article",
            "https://Mixed.Local/article",
        ] {
            let preview = waddle_xmpp::xep::LinkPreviewTokenData {
                sender_jid: "alice@example.com".parse().expect("jid"),
                scope_jid: "room@muc.example.com".parse().expect("jid"),
                original_url: url::Url::parse(blocked_url).expect("url"),
                normalized_url: url::Url::parse(blocked_url).expect("url"),
                title: Some("Blocked".to_string()),
                description: None,
                image: None,
                video: None,
                expires_at_unix: 1_900_000_000,
            };
            let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
            let mut message = Message::new(None::<jid::Jid>);
            message.to = Some("room@muc.example.com".parse().expect("jid"));
            message.bodies.insert(
                xmpp_parsers::message::Lang::new(),
                format!("read {blocked_url}"),
            );
            message
                .payloads
                .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

            consume_link_preview_request(
                &mut message,
                &sender(),
                SECRET,
                1_800_000_000,
                "https://waddle.example",
                &LinkPreviewConfig::default(),
            );

            assert!(
                waddle_xmpp::xep::extract_link_metadata_from_message(&message).is_empty(),
                "send-time validation must reject resolver-blocked host {blocked_url}"
            );
        }
    }

    #[test]
    fn send_time_policy_rechecks_normalized_url_before_stamping_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://allowed.example/article").expect("url"),
            normalized_url: url::Url::parse("https://blocked.example/canonical").expect("url"),
            title: Some("Blocked canonical".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://allowed.example/article".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig {
                blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
                ..LinkPreviewConfig::default()
            },
        );

        assert!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).is_empty(),
            "send-time validation must reject a token with a now-blocked canonical URL"
        );
    }

    #[test]
    fn send_time_policy_rejects_direct_video_tokens_when_video_is_disabled() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://video.example/clip.mp4").expect("url"),
            normalized_url: url::Url::parse("https://video.example/clip.mp4").expect("url"),
            title: Some("Video".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "watch https://video.example/clip.mp4".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig {
                video_enabled: false,
                ..LinkPreviewConfig::default()
            },
        );

        assert!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).is_empty(),
            "send-time validation must reject direct video tokens while video previews are disabled"
        );
    }

    #[test]
    fn correction_link_preview_request_stamps_xep0511_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/edited").expect("url"),
            normalized_url: url::Url::parse("https://example.com/edited").expect("url"),
            title: Some("Edited Link".to_string()),
            description: None,
            image: Some(waddle_xmpp::xep::LinkPreviewTokenImage {
                url: url::Url::parse(
                    "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
                )
                .expect("url"),
                media_type: PreviewImageMediaType::Png,
                width: Some(640),
                height: Some(360),
                alt: None,
            }),
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "edited to https://example.com/edited".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_replace_element(
                "original-message-id",
            ));
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert_eq!(
            waddle_xmpp::xep::extract_replaces_id(&message),
            Some("original-message-id".to_string())
        );
        let parsed = waddle_xmpp::xep::extract_link_metadata_from_message(&message);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].about, preview.original_url);
        assert_eq!(parsed[0].title.as_deref(), Some("Edited Link"));
        let references = waddle_xmpp::xep::extract_references_from_message(&message);
        assert_eq!(references.len(), 1);
        assert_eq!(
            references[0].uri,
            "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"
        );
    }

    #[test]
    fn link_preview_request_with_foreign_cached_image_origin_stamps_text_metadata_only() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://the.link.example.com/what-was-linked-to")
                .expect("url"),
            normalized_url: url::Url::parse(
                "https://example.com/canonical-url/for/what-was-linked-to",
            )
            .expect("url"),
            title: Some("The Best Webpage".to_string()),
            description: Some("This is a great webpage and you will really like it".to_string()),
            image: Some(waddle_xmpp::xep::LinkPreviewTokenImage {
                url: url::Url::parse(
                    "https://attacker.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
                )
                .expect("url"),
                media_type: PreviewImageMediaType::Png,
                width: Some(640),
                height: Some(360),
                alt: Some("Article screenshot".to_string()),
            }),
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://the.link.example.com/what-was-linked-to".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        let parsed = waddle_xmpp::xep::extract_link_metadata_from_message(&message);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title.as_deref(), Some("The Best Webpage"));
        assert!(parsed[0].images.is_empty());
        assert!(waddle_xmpp::xep::extract_references_from_message(&message).is_empty());
    }

    #[test]
    fn expired_link_preview_request_is_stripped_without_metadata() {
        let _events_guard = recorded_events::lock();
        recorded_events::clear();
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/").expect("url"),
            normalized_url: url::Url::parse("https://example.com/").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 10,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            11,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::TokenExpired),
            "expired send-time token path must emit token_expired telemetry"
        );
    }

    #[test]
    fn invalid_link_preview_request_is_stripped_without_metadata() {
        let _events_guard = recorded_events::lock();
        recorded_events::clear();
        let token =
            waddle_xmpp::xep::LinkPreviewToken::new("not-a-signed-preview-token").expect("token");
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::TokenInvalid),
            "invalid send-time token path must emit token_invalid telemetry"
        );
    }

    #[test]
    fn oversized_link_preview_request_is_stripped_without_metadata() {
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message.payloads.push(
            Element::builder(
                waddle_xmpp::xep::ELEMENT_PREVIEW_REQUEST,
                waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW,
            )
            .attr(
                xml_ncname!("token").to_owned(),
                "x".repeat(waddle_xmpp::xep::MAX_LINK_PREVIEW_TOKEN_BYTES + 1),
            )
            .build(),
        );

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
    }

    #[test]
    fn wrong_scope_link_preview_request_is_stripped_without_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "other@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/").expect("url"),
            normalized_url: url::Url::parse("https://example.com/").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
    }

    #[test]
    fn wrong_sender_link_preview_request_is_stripped_without_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "mallory@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/").expect("url"),
            normalized_url: url::Url::parse("https://example.com/").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
    }

    #[test]
    fn request_for_non_first_body_url_is_stripped_without_metadata() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://second.example.com/").expect("url"),
            normalized_url: url::Url::parse("https://second.example.com/").expect("url"),
            title: Some("Second".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "first https://first.example.com/ then https://second.example.com/".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
    }

    #[test]
    fn wrapped_first_body_url_is_eligible_for_preview_stamp() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/a").expect("url"),
            normalized_url: url::Url::parse("https://example.com/a").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read (https://example.com/a).".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert_eq!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).len(),
            1
        );
    }

    #[test]
    fn inline_punctuation_prefixed_body_url_is_eligible_for_preview_stamp() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/a").expect("url"),
            normalized_url: url::Url::parse("https://example.com/a").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read:https://example.com/a".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert_eq!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).len(),
            1
        );
    }

    #[test]
    fn quoted_host_only_body_url_is_eligible_for_preview_stamp() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://example.com/").expect("url"),
            normalized_url: url::Url::parse("https://example.com/").expect("url"),
            title: Some("Example".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read \"https://example.com\"".to_string(),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert_eq!(
            waddle_xmpp::xep::extract_link_metadata_from_message(&message).len(),
            1
        );
    }

    #[test]
    fn reply_fallback_url_is_ignored_when_validating_preview_stamp() {
        let preview = waddle_xmpp::xep::LinkPreviewTokenData {
            sender_jid: "alice@example.com".parse().expect("jid"),
            scope_jid: "room@muc.example.com".parse().expect("jid"),
            original_url: url::Url::parse("https://current.example.com/").expect("url"),
            normalized_url: url::Url::parse("https://current.example.com/").expect("url"),
            title: Some("Current".to_string()),
            description: None,
            image: None,
            video: None,
            expires_at_unix: 1_900_000_000,
        };
        let token = waddle_xmpp::xep::encode_link_preview_token(&preview, SECRET);
        let fallback_prefix = "> earlier https://quoted.example.com/\n";
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            format!("{fallback_prefix}see https://current.example.com/"),
        );
        message
            .payloads
            .push(waddle_xmpp::xep::build_fallback_element(
                &waddle_xmpp::xep::FallbackIndication::for_range(
                    waddle_xmpp::xep::NS_REPLY,
                    0,
                    fallback_prefix.encode_utf16().count(),
                ),
            ));
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_preview_request_element(&token));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        let parsed = waddle_xmpp::xep::extract_link_metadata_from_message(&message);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].about, preview.original_url);
    }

    #[test]
    fn client_authored_xep0511_metadata_is_stripped_before_stamp() {
        let mut message = Message::new(None::<jid::Jid>);
        message.to = Some("room@muc.example.com".parse().expect("jid"));
        message.bodies.insert(
            xmpp_parsers::message::Lang::new(),
            "read https://example.com/".to_string(),
        );
        let forged = waddle_xmpp::xep::LinkMetadata::new(
            url::Url::parse("https://evil.example/").expect("url"),
        )
        .with_title("Forged");
        message
            .payloads
            .push(waddle_xmpp::xep::build_link_metadata_element(&forged));

        consume_link_preview_request(
            &mut message,
            &sender(),
            SECRET,
            1_800_000_000,
            "https://waddle.example",
            &LinkPreviewConfig::default(),
        );

        assert!(message.payloads.is_empty());
    }
}
