mod files;
mod markup;
pub(super) mod payloads;

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use minidom::Element;

use crate::xep::fallback::{body_fallbacks_for, strip_fallback_ranges, BodyFallback};
use crate::xep::{call_thread as xep_call_thread, reply as xep_reply, thread as xep_thread};
use waddle_xmpp_core::{
    first_eligible_https_url_text, xep0359::StanzaId as StableStanzaId, PreviewImageMediaType,
};

use self::files::{parse_file_sharing_element, parse_shared_file};
use self::markup::parse_markup_spans;
pub use self::payloads::{
    parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_extension_envelope, parse_in_call_reaction_payload, parse_markable_marker_payload,
    parse_moderation_payload, parse_reaction_payload, parse_retraction_payload,
    parse_retraction_tombstone_payload,
};
use super::namespaces::*;
use super::presence::parse_presence;
use super::types::*;
use url::Url;

/// Parse an XMPP element into a [`MessagingEvent`], or return `None` if the
/// element is not a `<message>` or `<presence>`.
pub fn parse(element: &Element) -> Option<MessagingEvent> {
    match element.name() {
        "message" => {
            parse_message(element).map(|message| MessagingEvent::Message(Box::new(message)))
        }
        "presence" => Some(MessagingEvent::Presence(Box::new(parse_presence(element)))),
        _ => None,
    }
}

// ─── Message parsing ──────────────────────────────────────────────────────

fn parse_message(el: &Element) -> Option<InboundMessage> {
    let id = el.attr("id").map(String::from);
    let from = el.attr("from").map(String::from);
    let to = el.attr("to").map(String::from);
    let message_type = el.attr("type").unwrap_or("normal").to_string();

    let body = el.get_child("body", NS_CLIENT).map(|e| e.text());
    let subject = el.get_child("subject", NS_CLIENT).map(|e| e.text());
    let thread = el.get_child("thread", NS_CLIENT).map(|e| e.text());

    let timestamp = el
        .get_child("delay", NS_DELAY)
        .and_then(|d| d.attr("stamp"))
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc));

    let stanza_ids: Vec<StableStanzaId> = el
        .children()
        .filter(|child| child.name() == "stanza-id" && child.ns() == NS_STANZA_ID)
        .filter_map(|child| {
            let id = child.attr("id").filter(|id| !id.is_empty())?;
            let by = child.attr("by")?.parse::<jid::Jid>().ok()?;
            Some(StableStanzaId::new(id, by))
        })
        .collect();
    let stanza_id = stanza_ids.first().cloned();

    let origin_id = el
        .get_child("origin-id", NS_ORIGIN_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let correction = parse_correction_payload(el);
    let replaces_id = correction
        .as_ref()
        .map(|payload| payload.replaces_id.clone());

    let moderation = parse_moderation_payload(el);
    if has_moderation_retract(el) && moderation.is_none() {
        return None;
    }
    let moderation_target_id = moderation.as_ref().map(|payload| payload.target_id.clone());
    let tombstone = parse_retraction_tombstone_payload(el);
    let moderated_by = moderation
        .as_ref()
        .and_then(|payload| payload.moderated_by.clone())
        .or_else(|| {
            tombstone
                .as_ref()
                .and_then(|payload| payload.moderated_by.clone())
        });
    let moderation_reason = moderation
        .as_ref()
        .and_then(|payload| payload.reason.clone())
        .or_else(|| {
            tombstone
                .as_ref()
                .and_then(|payload| payload.reason.clone())
        });

    let retraction = parse_retraction_payload(el);
    let retracts_id = moderation_target_id
        .clone()
        .or_else(|| retraction.as_ref().map(|payload| payload.target_id.clone()));
    let retraction_id = tombstone
        .as_ref()
        .and_then(|payload| payload.retraction_id.clone());
    let is_retracted = tombstone.is_some();

    let reaction = parse_reaction_payload(el);
    let reaction_target_id = reaction.as_ref().map(|payload| payload.target_id.clone());
    let reaction_emojis = reaction.map(|payload| payload.emojis).unwrap_or_default();
    let in_call = parse_in_call_reaction_payload(el).map(InCallSignal::Reaction);

    let pin_event = crate::pin::extract_pin_event_from_message(el);
    let extension_envelope = parse_extension_envelope(el);
    let extension_body_fallback = extension_envelope.is_some()
        && crate::xep::fallback::has_whole_body_fallback_for(el, NS_WADDLE_EXTENSION);
    let mds_displayed = crate::mds::parse_mds_event(el).map(|entries| {
        entries
            .into_iter()
            .map(|entry| MdsDisplayedEntry {
                chat_id: entry.chat_id,
                stanza_id: entry.stanza_id,
                stanza_id_by: entry.stanza_id_by,
            })
            .collect::<Vec<_>>()
    });
    let pubsub_events = crate::pubsub_event::parse_pubsub_events(el);
    let call_thread = xep_call_thread::parse_call_thread_anchor_child(el);
    let call_thread_ended = xep_call_thread::parse_call_thread_ended_child(el);

    let reply_marker = xep_reply::parse_reply(el);
    let reply_to_id = reply_marker.as_ref().map(|m| m.id.clone());
    let reply_to_sender = reply_marker.as_ref().map(|m| m.to.to_string());
    let reply_fallback = xep_reply::parse_fallback(el).map(|r| (r.start, r.end));

    let markup_spans = el
        .get_child("markup", NS_MARKUP)
        .map(parse_markup_spans)
        .unwrap_or_default();

    let chat_state = parse_chat_state_payload(el).map(|payload| payload.state);
    let displayed_marker_requested = parse_markable_marker_payload(el, id.as_deref()).is_some();
    let displayed_marker_id = parse_displayed_marker_payload(el).map(|payload| payload.id);

    // XEP-0372: References (mentions and data)
    //
    // Every `<reference xmlns="urn:xmpp:reference:0"/>` child with the required
    // `type` and `uri` attributes is captured as a typed `ReferenceData` and
    // also fans out into the flat helper views (`mention_uris`,
    // `broadcast_mention`, `shared_files`) the rest of the codebase already
    // consumes. The flat views are derived projections; `references` is the
    // structural source of truth. Reading `begin`/`end` is the only string→u32
    // parse on the inbound path; per the typed-payloads hard rule, no other
    // boundary may stringify protocol values.
    let mut references: Vec<ReferenceData> = Vec::new();
    let mut mention_uris: Vec<String> = Vec::new();
    let mut broadcast_mention: Option<String> = None;
    let mut shared_files: Vec<SharedFile> = Vec::new();

    for child in el
        .children()
        .filter(|c| c.name() == "reference" && c.ns() == NS_REFERENCES)
    {
        let ref_type = match child.attr("type") {
            Some(t) => t,
            None => continue,
        };
        let uri = match child.attr("uri") {
            Some(u) => u,
            None => continue,
        };
        // begin/end are optional per XEP-0372, but they form an all-or-nothing
        // pair: a reference either points at a body substring (both present
        // and numeric) or it is anchor-only (both absent → represented as the
        // (0, 0) sentinel). A half-specified pair like `begin="3"` with no
        // `end` is meaningless — drop it. Same for malformed values like
        // `begin="abc"`, which would otherwise mis-position the span.
        let begin_attr = child.attr("begin");
        let end_attr = child.attr("end");
        let (begin, end) = match (begin_attr, end_attr) {
            (Some(b), Some(e)) => match (b.parse::<u32>(), e.parse::<u32>()) {
                (Ok(b), Ok(e)) if e >= b => (b, e),
                _ => continue,
            },
            (None, None) => (0, 0),
            _ => continue,
        };
        let anchor = child.attr("anchor").map(String::from);

        references.push(ReferenceData {
            ref_type: ref_type.to_string(),
            uri: uri.to_string(),
            begin,
            end,
            anchor: anchor.clone(),
        });

        match ref_type {
            "mention" => {
                let uri_str = uri.to_string();
                if uri_str.starts_with("xmpp:")
                    && (uri_str.contains("@everyone") || uri_str.contains("@here"))
                {
                    broadcast_mention = Some(uri_str.clone());
                }
                mention_uris.push(uri_str);
            }
            "data" => {
                if let Some(file) = parse_shared_file(child) {
                    shared_files.push(file);
                }
            }
            _ => {}
        }
    }

    for file_sharing_el in el
        .children()
        .filter(|c| c.name() == "file-sharing" && c.ns() == NS_SFS)
    {
        if let Some(file) = parse_file_sharing_element(file_sharing_el) {
            shared_files.push(file);
        }
    }
    let link_preview_body = body
        .as_deref()
        .map(|body| body_without_reply_fallback(el, body));
    let link_previews = parse_link_previews(el, link_preview_body.as_deref());

    // XEP-0447 / XEP-0363: also check <sims> children for file sharing
    for sims_el in el.children().filter(|c| c.ns() == NS_SIMS) {
        for source_el in sims_el.children() {
            for url_data_el in source_el.children() {
                let url = url_data_el.attr("url").map(String::from);
                if let Some(url) = url {
                    shared_files.push(SharedFile {
                        url,
                        name: sims_el
                            .get_child("name", sims_el.ns().as_str())
                            .map(|e| e.text()),
                        media_type: sims_el
                            .get_child("media-type", sims_el.ns().as_str())
                            .map(|e| e.text()),
                        size: sims_el
                            .get_child("size", sims_el.ns().as_str())
                            .and_then(|e| e.text().parse().ok()),
                        width: None,
                        height: None,
                        desc: None,
                        hashes: Vec::new(),
                        disposition: SharedFileDisposition::Attachment,
                        encrypted: None,
                    });
                }
            }
        }
    }

    // XEP-0201 thread + nested thread parent
    let thread_ref = xep_thread::parse_thread(el);
    let thread_id = thread_ref
        .as_ref()
        .map(|t| t.id.clone())
        .or_else(|| thread.clone());
    let parent_thread_id = thread_ref.as_ref().and_then(|t| t.parent.clone());
    let (forum_post_kind, forum_title) =
        if thread_id.is_some() && body.is_some() && subject.is_some() {
            (Some("topic".to_string()), subject.clone())
        } else if thread_id.is_some() && body.is_some() {
            (Some("reply".to_string()), None)
        } else {
            (None, None)
        };

    // XEP-0449: Stickers
    let is_sticker = el.get_child("sticker", NS_STICKERS).is_some();

    Some(InboundMessage {
        from,
        to,
        message_type,
        id,
        stanza_id,
        stanza_ids,
        origin_id,
        body,
        subject,
        thread,
        timestamp,
        replaces_id,
        retracts_id,
        retraction_id,
        is_retracted,
        moderation_target_id,
        moderated_by,
        moderation_reason,
        reaction_target_id,
        reaction_emojis,
        in_call,
        reply_to_id,
        reply_to_sender,
        reply_fallback,
        markup_spans,
        chat_state,
        displayed_marker_requested,
        displayed_marker_id,
        shared_files,
        link_previews,
        broadcast_mention,
        mention_uris,
        references,
        forum_post_kind,
        forum_title,
        thread_id,
        parent_thread_id,
        call_thread,
        call_thread_ended,
        is_sticker,
        pin_event,
        extension_envelope,
        extension_body_fallback,
        mds_displayed,
        pubsub_events,
        // XEP-0280 direction is a property of the wrapping envelope, not
        // of the message element itself; the runtime stamps it after the
        // §11 own-bare-JID verification.
        carbon: None,
    })
}

fn parse_link_previews(el: &Element, body: Option<&str>) -> Vec<LinkPreviewData> {
    let first_url = body.and_then(first_eligible_https_url);
    if body.is_some() && first_url.is_none() {
        return Vec::new();
    }
    el.children()
        .filter(|child| child.name() == "Description" && child.ns() == NS_RDF_SYNTAX)
        .filter_map(|child| parse_link_preview(child, first_url.as_ref()))
        .collect()
}

fn parse_link_preview(el: &Element, first_url: Option<&Url>) -> Option<LinkPreviewData> {
    let original_url =
        parse_web_url(el.attr_ns(&minidom::rxml::Namespace::from(NS_RDF_SYNTAX), "about")?)?;
    if first_url.is_some() && first_url != Some(&original_url) {
        return None;
    }
    let preview_image_url = link_preview_image_url(el);
    let has_preview_image = preview_image_url.is_some();
    let image = parse_link_preview_image(el, preview_image_url.as_deref());
    let player_embed = parse_link_preview_player(el);
    let video = parse_link_preview_native_video(el);
    Some(LinkPreviewData {
        original_url,
        normalized_url: og_text(el, "url").and_then(|value| parse_web_url(&value)),
        title: og_text(el, "title"),
        description: og_text(el, "description"),
        remote_media_unavailable: has_preview_image && image.is_none(),
        image,
        video,
        player_embed,
    })
}

/// The MIME essence of an `og:video:type`, stripping any media-type parameters
/// (`video/mp4; codecs="…"`, `text/html; charset=utf-8`) so a parameterised type
/// from a conformant (e.g. federated) sender is matched, mirroring the resolver.
fn og_video_type_essence(value: &str) -> &str {
    value.split(';').next().unwrap_or_default().trim()
}

fn parse_link_preview_player(el: &Element) -> Option<LinkPreviewPlayer> {
    let mut url = None;
    let mut secure_url = None;
    let mut is_html = false;
    let mut width = None;
    let mut height = None;
    for child in el.children() {
        if child.ns() == NS_OPENGRAPH && child.name() == "video" {
            url = parse_web_url(child.text().trim());
            continue;
        }
        if child.ns() != NS_OPENGRAPH_VIDEO {
            continue;
        }
        let value = child.text().trim().to_string();
        match child.name() {
            "secure_url" => secure_url = parse_web_url(&value),
            "type" => is_html = og_video_type_essence(&value).eq_ignore_ascii_case("text/html"),
            "width" => width = value.parse().ok(),
            "height" => height = value.parse().ok(),
            _ => {}
        }
    }
    let url = secure_url.or(url)?;
    is_html.then_some(LinkPreviewPlayer { url, width, height })
}

fn parse_link_preview_native_video(el: &Element) -> Option<LinkPreviewVideoData> {
    let mut url = None;
    let mut secure_url = None;
    let mut media_type = None;
    for child in el.children() {
        if child.ns() == NS_OPENGRAPH && child.name() == "video" {
            url = parse_web_url(child.text().trim());
            continue;
        }
        if child.ns() != NS_OPENGRAPH_VIDEO {
            continue;
        }
        let value = child.text().trim().to_string();
        match child.name() {
            "secure_url" => secure_url = parse_web_url(&value),
            // A supported direct (non-iframe) media type marks this as a native
            // `<video>` stream; `text/html` (the iframe player) parses to `Err`
            // and is handled by `parse_link_preview_player` instead.
            "type" => {
                media_type = og_video_type_essence(&value)
                    .parse::<waddle_xmpp_core::DirectVideoMediaType>()
                    .ok()
            }
            _ => {}
        }
    }
    let url = secure_url.or(url)?;
    let media_type = media_type?;
    Some(LinkPreviewVideoData { url, media_type })
}

fn parse_link_preview_image(el: &Element, image_url: Option<&str>) -> Option<LinkPreviewImageData> {
    let url = image_url.and_then(parse_cached_preview_image_url)?;
    let media_type =
        og_image_text(el, "type").and_then(|value| value.parse::<PreviewImageMediaType>().ok())?;
    Some(LinkPreviewImageData {
        url,
        media_type,
        width: og_image_text(el, "width").and_then(|value| value.parse().ok()),
        height: og_image_text(el, "height").and_then(|value| value.parse().ok()),
        alt: og_image_text(el, "alt"),
    })
}

fn link_preview_image_url(el: &Element) -> Option<String> {
    el.children()
        .find(|child| child.name() == "image" && child.ns() == NS_OPENGRAPH)
        .map(|child| child.text().trim().to_string())
        .filter(|url| !url.is_empty())
}

pub(super) fn parse_cached_preview_image_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    if !cached_preview_media_origin_allowed(&url) {
        return None;
    }
    let path = url.path();
    is_link_preview_xep0363_file_path(path).then_some(url)
}

fn cached_preview_media_origin_allowed(url: &Url) -> bool {
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => url.host().is_some_and(|host| match host {
            url::Host::Domain(domain) => domain == "localhost",
            url::Host::Ipv4(addr) => addr.is_loopback(),
            url::Host::Ipv6(addr) => addr.is_loopback(),
        }),
        _ => false,
    }
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

fn parse_web_url(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    (url.scheme() == "https" && url.host_str().is_some_and(|host| host.contains('.')))
        .then_some(url)
}

fn first_eligible_https_url(body: &str) -> Option<Url> {
    first_eligible_https_url_text(body).and_then(|candidate| Url::parse(candidate).ok())
}

fn body_without_reply_fallback<'a>(message: &Element, body: &'a str) -> Cow<'a, str> {
    let mut ranges = Vec::new();
    for fallback in body_fallbacks_for(message, xep_reply::NS_REPLY) {
        match fallback {
            BodyFallback::Whole => return Cow::Borrowed(""),
            BodyFallback::Ranges(body_ranges) => ranges.extend(body_ranges),
        }
    }
    if ranges.is_empty() {
        Cow::Borrowed(body)
    } else {
        Cow::Owned(strip_fallback_ranges(body, &ranges))
    }
}

fn og_text(el: &Element, name: &str) -> Option<String> {
    el.children()
        .find(|child| child.name() == name && child.ns() == NS_OPENGRAPH)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn og_image_text(el: &Element, name: &str) -> Option<String> {
    el.children()
        .find(|child| child.name() == name && child.ns() == NS_OPENGRAPH_IMAGE)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn has_moderation_retract(element: &Element) -> bool {
    element
        .get_child("retract", NS_MESSAGE_RETRACT)
        .and_then(|retract| retract.get_child("moderated", NS_MESSAGE_MODERATE))
        .is_some()
}
