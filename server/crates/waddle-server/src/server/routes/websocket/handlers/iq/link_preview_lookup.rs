//! Waddle plaintext link-preview composer lookup.
//!
//! The lookup resolves OpenGraph metadata through the bounded HTTPS resolver,
//! then mints a scoped private token for the send-time XEP-0511 payload.

use super::*;
use chrono::{Duration, SecondsFormat, Utc};
use minidom::rxml::xml_ncname;
use url::Url;
use waddle_xmpp::xep::{
    encode_link_preview_token_checked, LinkPreviewTokenData, LinkPreviewTokenImage,
    LinkPreviewTokenPlayer, LinkPreviewTokenVideo, NS_WADDLE_LINK_PREVIEW,
};
#[cfg(test)]
use waddle_xmpp_core::PreviewImageMediaType;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

use super::link_preview_resolver::{
    resolve_link_preview, LinkPreviewMediaCache, LinkPreviewResolverOutcome,
    LinkPreviewResolverPolicy, LinkPreviewResolverStatus,
};
use crate::server::routes::websocket::link_preview_telemetry::{
    record_link_preview_event, LinkPreviewTelemetryEvent,
};

struct LinkPreviewLookupDeps<'a> {
    muc_domain: &'a str,
    response_from: Option<&'a str>,
    response_to: Option<&'a str>,
    secret: &'a [u8],
    resolver_policy: Option<&'a LinkPreviewResolverPolicy>,
}

pub(super) fn is_link_preview_lookup_iq(iq: &Iq) -> bool {
    let Iq::Get { payload, .. } = iq else {
        return false;
    };
    payload.name() == "lookup" && payload.ns() == NS_WADDLE_LINK_PREVIEW
}

pub(super) async fn handle_link_preview_lookup_iq(
    iq: &Iq,
    sender_jid: Option<&FullJid>,
    state: &WebSocketState,
    muc_domain: &str,
    response_from: Option<&str>,
    response_to: Option<&str>,
    secret: &[u8],
) -> Vec<String> {
    handle_link_preview_lookup_iq_with_policy(
        iq,
        sender_jid,
        state,
        LinkPreviewLookupDeps {
            muc_domain,
            response_from,
            response_to,
            secret,
            resolver_policy: None,
        },
    )
    .await
}

async fn handle_link_preview_lookup_iq_with_policy(
    iq: &Iq,
    sender_jid: Option<&FullJid>,
    state: &WebSocketState,
    deps: LinkPreviewLookupDeps<'_>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let owned_resolver_policy;
    let resolver_policy = match deps.resolver_policy {
        Some(policy) => policy,
        None => {
            owned_resolver_policy = LinkPreviewResolverPolicy::from_config(
                &state.deps.link_preview,
                Some(LinkPreviewMediaCache::new(
                    state.deps.app_state.blob_storage.clone(),
                    state.deps.auth_state.base_url.as_str(),
                    state.deps.app_state.db_pool.global_actor().clone(),
                    sender_jid.to_bare(),
                )),
            );
            &owned_resolver_policy
        }
    };
    let Iq::Get { payload, .. } = iq else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup must be an IQ get."),
        )];
    };

    let Some(original_url) = lookup_url(payload) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup requires a URL."),
        )];
    };
    let Some(scope_jid) = lookup_scope(payload).and_then(|scope| scope.parse().ok()) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            bad_request_iq_error("Link preview lookup requires a conversation scope JID."),
        )];
    };
    if let Some(error) =
        authorize_link_preview_scope(state, sender_jid, &scope_jid, deps.muc_domain).await
    {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            deps.response_from,
            deps.response_to,
            error,
        )];
    }

    let Ok(original_url) = Url::parse(&original_url) else {
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Unsupported,
            None,
        )];
    };
    if !resolver_policy.enabled {
        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverBlocked);
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Blocked,
            None,
        )];
    }
    let outcome = resolve_link_preview(&original_url, resolver_policy).await;
    let LinkPreviewResolverOutcome::Ready(metadata) = outcome else {
        record_link_preview_event(telemetry_event_for_status(outcome.status()));
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            outcome.status(),
            None,
        )];
    };
    let expires_at = Utc::now() + Duration::minutes(5);
    let data = LinkPreviewTokenData {
        sender_jid: sender_jid.to_bare(),
        scope_jid,
        original_url: metadata.original_url,
        normalized_url: metadata.normalized_url,
        title: metadata.title,
        description: metadata.description,
        image: metadata.image.map(|image| LinkPreviewTokenImage {
            url: image.url,
            media_type: image.media_type,
            width: image.width,
            height: image.height,
            alt: image.alt,
        }),
        video: metadata.video.map(|video| LinkPreviewTokenVideo {
            url: video.url,
            media_type: video.media_type,
            size: video.size,
        }),
        player: metadata.player_embed.map(|player| LinkPreviewTokenPlayer {
            url: player.url,
            width: player.width,
            height: player.height,
        }),
        expires_at_unix: expires_at.timestamp(),
    };
    let Some(token) = encode_link_preview_token_checked(&data, deps.secret) else {
        record_link_preview_event(LinkPreviewTelemetryEvent::ResolverFailed);
        return vec![build_link_preview_lookup_result(
            iq,
            deps.response_from,
            deps.response_to,
            LinkPreviewResolverStatus::Failed,
            None,
        )];
    };

    record_link_preview_event(LinkPreviewTelemetryEvent::ResolverReady);
    vec![build_link_preview_lookup_result(
        iq,
        deps.response_from,
        deps.response_to,
        LinkPreviewResolverStatus::Ready,
        Some((
            data,
            token.as_str(),
            expires_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        )),
    )]
}

fn telemetry_event_for_status(status: LinkPreviewResolverStatus) -> LinkPreviewTelemetryEvent {
    match status {
        LinkPreviewResolverStatus::Ready => LinkPreviewTelemetryEvent::ResolverReady,
        LinkPreviewResolverStatus::Blocked => LinkPreviewTelemetryEvent::ResolverBlocked,
        LinkPreviewResolverStatus::Failed => LinkPreviewTelemetryEvent::ResolverFailed,
        LinkPreviewResolverStatus::Unsupported => LinkPreviewTelemetryEvent::ResolverUnsupported,
    }
}

async fn authorize_link_preview_scope(
    state: &WebSocketState,
    sender_jid: &FullJid,
    scope_jid: &BareJid,
    muc_domain: &str,
) -> Option<xmpp_parsers::stanza_error::StanzaError> {
    if scope_jid.domain().as_str() != muc_domain {
        return None;
    }

    let Some(room_actor) = get_room_actor(state, scope_jid).await else {
        return Some(forbidden_iq_error(
            "Link preview lookup is visible only to current room occupants.",
        ));
    };
    match room_actor
        .ask(GetOccupantByJid {
            jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(_)) => None,
        Ok(None) => Some(forbidden_iq_error(
            "Link preview lookup is visible only to current room occupants.",
        )),
        Err(error) => {
            warn!(
                scope = %scope_jid,
                ?error,
                "Link preview lookup: failed to verify room occupancy"
            );
            Some(internal_server_error_iq_error("Internal server error."))
        }
    }
}

fn lookup_url(payload: &Element) -> Option<String> {
    payload
        .get_child("url", NS_WADDLE_LINK_PREVIEW)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn lookup_scope(payload: &Element) -> Option<String> {
    payload
        .get_child("scope", NS_WADDLE_LINK_PREVIEW)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn build_link_preview_lookup_result(
    iq: &Iq,
    response_from: Option<&str>,
    response_to: Option<&str>,
    status: LinkPreviewResolverStatus,
    preview: Option<(LinkPreviewTokenData, &str, String)>,
) -> String {
    let mut lookup = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW).attr(
        xml_ncname!("status").to_owned(),
        if preview.is_some() {
            "ready"
        } else {
            status.as_lookup_status()
        },
    );
    if let Some((data, token, expires_at)) = preview {
        let mut preview = Element::builder("preview", NS_WADDLE_LINK_PREVIEW)
            .attr(xml_ncname!("token").to_owned(), token)
            .attr(
                xml_ncname!("original-url").to_owned(),
                data.original_url.as_str(),
            )
            .attr(
                xml_ncname!("normalized-url").to_owned(),
                data.normalized_url.as_str(),
            )
            .attr(xml_ncname!("expires-at").to_owned(), expires_at)
            .build();
        append_text(&mut preview, "title", data.title.as_deref());
        append_text(&mut preview, "description", data.description.as_deref());
        if let Some(image) = &data.image {
            let mut image_elem = Element::builder("image", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), image.url.as_str())
                .attr(
                    xml_ncname!("media-type").to_owned(),
                    image.media_type.as_str(),
                );
            if let Some(width) = image.width {
                image_elem = image_elem.attr(xml_ncname!("width").to_owned(), width.to_string());
            }
            if let Some(height) = image.height {
                image_elem = image_elem.attr(xml_ncname!("height").to_owned(), height.to_string());
            }
            if let Some(alt) = image.alt.as_deref().filter(|alt| !alt.trim().is_empty()) {
                image_elem = image_elem.attr(xml_ncname!("alt").to_owned(), alt.trim());
            }
            preview.append_child(image_elem.build());
        }
        if let Some(video) = &data.video {
            let mut video_elem = Element::builder("video", NS_WADDLE_LINK_PREVIEW)
                .attr(xml_ncname!("url").to_owned(), video.url.as_str())
                .attr(
                    xml_ncname!("media-type").to_owned(),
                    video.media_type.as_str(),
                );
            if let Some(size) = video.size {
                video_elem = video_elem.attr(xml_ncname!("size").to_owned(), size.to_string());
            }
            preview.append_child(video_elem.build());
        }
        lookup = lookup.append(preview);
    }
    build_iq_result_xml(iq.id(), response_from, response_to, Some(lookup.build()))
}

fn append_text(parent: &mut Element, name: &str, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    parent.append_child(
        Element::builder(name, NS_WADDLE_LINK_PREVIEW)
            .append(value.trim())
            .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::routes::websocket::link_preview_telemetry::recorded_events;
    use crate::server::routes::websocket::tests::create_test_websocket_state;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn iq_get_with_payload(payload: Element) -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: "lookup-1".into(),
            payload,
        }
    }

    fn sender() -> FullJid {
        "alice@example.com/desktop".parse().expect("jid")
    }

    fn secret() -> &'static [u8] {
        b"test-link-preview-secret"
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handles_https_lookup_with_scoped_token_text_and_cached_image_metadata() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        let image_bytes = bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\nlookup preview");
        Mock::given(method("GET"))
            .and(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"<html><head>
                    <meta property="og:title" content="Example Article">
                    <meta property="og:description" content="Plain text summary">
                    <meta property="og:image" content="{}/preview.png">
                    <meta property="og:image:width" content="640">
                    <meta property="og:image:height" content="360">
                    <meta property="og:image:alt" content="Article screenshot">
                  </head></html>"#,
                    server.uri()
                ),
                "text/html",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/preview.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(image_bytes),
            )
            .mount(&server)
            .await;
        let storage_dir =
            std::env::temp_dir().join(format!("waddle-link-preview-{}", uuid::Uuid::new_v4()));
        let storage: std::sync::Arc<dyn crate::storage::BlobStorage> =
            std::sync::Arc::new(crate::storage::LocalStorage::new(storage_dir));
        let state = create_test_websocket_state().await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            media_cache: Some(LinkPreviewMediaCache::new(
                storage,
                "https://waddle.example",
                state.deps.app_state.db_pool.global_actor().clone(),
                "alice@example.com".parse().expect("jid"),
            )),
            ..Default::default()
        };
        let lookup_url = format!("{}/a", server.uri());
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(&resolver_policy),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("ready"));
        let preview = lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .expect("preview");
        assert_eq!(preview.attr("original-url"), Some(lookup_url.as_str()));
        assert_eq!(preview.attr("normalized-url"), Some(lookup_url.as_str()));
        assert!(preview.attr("token").is_some_and(|token| !token.is_empty()));
        assert!(preview.attr("expires-at").is_some());
        let image = preview
            .get_child("image", NS_WADDLE_LINK_PREVIEW)
            .expect("image");
        assert_eq!(image.attr("media-type"), Some("image/png"));
        assert_eq!(image.attr("width"), Some("640"));
        assert_eq!(image.attr("height"), Some("360"));
        assert_eq!(image.attr("alt"), Some("Article screenshot"));
        assert!(image.attr("url").is_some_and(|url| {
            url.starts_with("https://waddle.example/api/files/")
                && url.ends_with(".png")
                && url.contains("/link-preview-")
        }));
        let token = waddle_xmpp::xep::LinkPreviewToken::new(
            preview.attr("token").expect("token").to_string(),
        )
        .expect("token");
        let decoded =
            waddle_xmpp::xep::decode_link_preview_token(&token, secret(), i64::MIN).expect("token");
        assert_eq!(decoded.sender_jid.to_string(), "alice@example.com");
        assert_eq!(decoded.scope_jid.to_string(), "alice@example.com");
        let decoded_image = decoded.image.expect("token image");
        assert_eq!(decoded_image.media_type, PreviewImageMediaType::Png);
        assert_eq!(decoded_image.width, Some(640));
        assert_eq!(decoded_image.height, Some(360));
        assert_eq!(decoded_image.alt.as_deref(), Some("Article screenshot"));
        assert!(decoded_image
            .url
            .as_str()
            .starts_with("https://waddle.example/api/files/"));
        assert_eq!(
            preview
                .get_child("title", NS_WADDLE_LINK_PREVIEW)
                .map(Element::text)
                .as_deref(),
            Some("Example Article")
        );
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverReady),
            "ready lookup path must emit ready telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn direct_video_lookup_mints_token_and_advertises_video() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/clip.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 4096], "video/mp4"))
            .mount(&server)
            .await;
        let state = create_test_websocket_state().await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let lookup_url = format!("{}/clip.mp4", server.uri());
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(&resolver_policy),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("ready"));
        let preview = lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .expect("preview");
        let video = preview
            .get_child("video", NS_WADDLE_LINK_PREVIEW)
            .expect("video element");
        assert_eq!(video.attr("media-type"), Some("video/mp4"));
        assert_eq!(video.attr("url"), Some(lookup_url.as_str()));
        assert_eq!(video.attr("size"), Some("4096"));
        assert!(
            preview.get_child("image", NS_WADDLE_LINK_PREVIEW).is_none(),
            "direct video preview has no cached image"
        );

        let token = waddle_xmpp::xep::LinkPreviewToken::new(
            preview.attr("token").expect("token").to_string(),
        )
        .expect("token");
        let decoded =
            waddle_xmpp::xep::decode_link_preview_token(&token, secret(), i64::MIN).expect("token");
        let decoded_video = decoded.video.expect("token video");
        assert_eq!(
            decoded_video.media_type,
            waddle_xmpp_core::DirectVideoMediaType::Mp4
        );
        assert_eq!(decoded_video.url.as_str(), lookup_url.as_str());
        assert_eq!(decoded_video.size, Some(4096));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_encoded_token_returns_failed_without_preview() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let server = MockServer::start().await;
        let huge_path = format!("/{}", "a".repeat(8 * 1024));
        Mock::given(method("GET"))
            .and(path(huge_path.as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"<html><head>
                    <meta property="og:title" content="Example Article">
                  </head></html>"#,
                "text/html",
            ))
            .mount(&server)
            .await;
        let resolver_policy = LinkPreviewResolverPolicy {
            allow_http_loopback_for_tests: true,
            ..Default::default()
        };
        let lookup_url = format!("{}{}", server.uri(), huge_path);
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append(lookup_url.as_str())
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(&resolver_policy),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("failed"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverFailed),
            "token mint failure path must emit failed telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn non_https_lookup_returns_unsupported_without_token() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("http://example.com/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq(
            &iq,
            Some(&sender()),
            state.as_ref(),
            "muc.example.com",
            None,
            None,
            secret(),
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("unsupported"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverUnsupported),
            "unsupported lookup path must emit unsupported telemetry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disabled_policy_returns_blocked_without_token() {
        let _events_guard = recorded_events::async_lock().await;
        recorded_events::clear();
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://example.com/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(&LinkPreviewResolverPolicy {
                    enabled: false,
                    ..Default::default()
                }),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("blocked"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
        assert!(
            recorded_events::take().contains(&LinkPreviewTelemetryEvent::ResolverBlocked),
            "disabled lookup path must emit blocked telemetry"
        );
    }

    #[tokio::test]
    async fn blocked_host_policy_returns_blocked_lookup_without_token() {
        let payload = Element::builder("lookup", NS_WADDLE_LINK_PREVIEW)
            .append(
                Element::builder("url", NS_WADDLE_LINK_PREVIEW)
                    .append("https://blocked.example/a")
                    .build(),
            )
            .append(
                Element::builder("scope", NS_WADDLE_LINK_PREVIEW)
                    .append("alice@example.com")
                    .build(),
            )
            .build();
        let iq = iq_get_with_payload(payload);

        let state = create_test_websocket_state().await;
        let response = handle_link_preview_lookup_iq_with_policy(
            &iq,
            Some(&sender()),
            state.as_ref(),
            LinkPreviewLookupDeps {
                muc_domain: "muc.example.com",
                response_from: None,
                response_to: None,
                secret: secret(),
                resolver_policy: Some(&LinkPreviewResolverPolicy {
                    blocked_hosts: vec!["blocked.example".parse().expect("pattern")],
                    ..Default::default()
                }),
            },
        )
        .await;

        let elem: Element = response[0].parse().expect("iq result");
        let lookup = elem
            .get_child("lookup", NS_WADDLE_LINK_PREVIEW)
            .expect("lookup result");
        assert_eq!(lookup.attr("status"), Some("blocked"));
        assert!(lookup
            .get_child("preview", NS_WADDLE_LINK_PREVIEW)
            .is_none());
    }
}
