mod files;
mod markup;
pub(super) mod payloads;

use chrono::{DateTime, Utc};
use minidom::Element;

use crate::xep::encrypted_file::{self as xep_encrypted_file, NS_ESFS as NS_ENCRYPTED_FILE};
use crate::xep::{reply as xep_reply, thread as xep_thread};

use self::files::{parse_file_sharing_element, parse_shared_file};
use self::markup::parse_markup_spans;
pub use self::payloads::{
    parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_moderation_payload, parse_reaction_payload, parse_retraction_payload,
};
use super::namespaces::*;
use super::presence::parse_presence;
use super::types::*;

/// Parse an XMPP element into a [`MessagingEvent`], or return `None` if the
/// element is not a `<message>` or `<presence>`.
pub fn parse(element: &Element) -> Option<MessagingEvent> {
    match element.name() {
        "message" => Some(MessagingEvent::Message(Box::new(parse_message(element)))),
        "presence" => Some(MessagingEvent::Presence(parse_presence(element))),
        _ => None,
    }
}

// ─── Message parsing ──────────────────────────────────────────────────────

fn parse_message(el: &Element) -> InboundMessage {
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

    let stanza_id = el
        .get_child("stanza-id", NS_STANZA_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let origin_id = el
        .get_child("origin-id", NS_ORIGIN_ID)
        .and_then(|e| e.attr("id"))
        .map(String::from);

    let correction = parse_correction_payload(el);
    let replaces_id = correction
        .as_ref()
        .map(|payload| payload.replaces_id.clone());

    let moderation = parse_moderation_payload(el);
    let moderation_target_id = moderation.as_ref().map(|payload| payload.target_id.clone());
    let moderated_by = moderation
        .as_ref()
        .map(|payload| payload.moderated_by.clone());
    let moderation_reason = moderation
        .as_ref()
        .and_then(|payload| payload.reason.clone());

    let retraction = parse_retraction_payload(el);
    let retracts_id = moderation_target_id
        .clone()
        .or_else(|| retraction.as_ref().map(|payload| payload.target_id.clone()));

    let reaction = parse_reaction_payload(el);
    let reaction_target_id = reaction.as_ref().map(|payload| payload.target_id.clone());
    let reaction_emojis = reaction.map(|payload| payload.emojis).unwrap_or_default();

    let pin_event = crate::pin::extract_pin_event_from_message(el);

    let reply_marker = xep_reply::parse_reply(el);
    let reply_to_id = reply_marker.as_ref().map(|m| m.id.clone());
    let reply_to_sender = reply_marker.as_ref().map(|m| m.to.to_string());
    let reply_fallback = xep_reply::parse_fallback(el).map(|r| (r.start, r.end));

    let markup_spans = el
        .get_child("markup", NS_MARKUP)
        .map(parse_markup_spans)
        .unwrap_or_default();

    let chat_state = parse_chat_state_payload(el).map(|payload| payload.state);
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
                        disposition: SharedFileDisposition::Attachment,
                        encrypted: None,
                    });
                }
            }
        }
    }

    // XEP-0448: top-level `<encrypted xmlns='urn:xmpp:esfs:0'/>` siblings
    // carry the cipher/key/iv metadata for the file-sharing entries
    // collected above (XEP-0447 file-sharing AND legacy XEP-0385 SIMS).
    // Match by URL so a `SharedFile` whose `url` already names the
    // ciphertext gains the metadata needed to decrypt it; if multiple
    // entries reference the same source URL (rare but legal — same blob
    // shared twice in one stanza) every one of them is annotated so none
    // render broken.
    for encrypted_el in el
        .children()
        .filter(|c| c.name() == "encrypted" && c.ns() == NS_ENCRYPTED_FILE)
    {
        let Some(encrypted) = xep_encrypted_file::parse_encrypted_element(encrypted_el) else {
            continue;
        };
        let mut matched = false;
        for file in shared_files.iter_mut() {
            if encrypted.sources.iter().any(|src| src == &file.url) {
                file.encrypted = Some(encrypted.clone());
                matched = true;
            }
        }
        // If no matching `<file-sharing/>` (or `<sims>`) sibling exists,
        // synthesise one from the encrypted envelope's first source so
        // the recipient still sees the attachment.
        if !matched {
            if let Some(url) = encrypted.sources.first().cloned() {
                shared_files.push(SharedFile {
                    url,
                    name: None,
                    media_type: None,
                    size: None,
                    width: None,
                    height: None,
                    disposition: SharedFileDisposition::Attachment,
                    encrypted: Some(encrypted),
                });
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

    InboundMessage {
        from,
        to,
        message_type,
        id,
        stanza_id,
        origin_id,
        body,
        subject,
        thread,
        timestamp,
        replaces_id,
        retracts_id,
        moderation_target_id,
        moderated_by,
        moderation_reason,
        reaction_target_id,
        reaction_emojis,
        reply_to_id,
        reply_to_sender,
        reply_fallback,
        markup_spans,
        chat_state,
        displayed_marker_id,
        shared_files,
        broadcast_mention,
        mention_uris,
        references,
        forum_post_kind,
        forum_title,
        thread_id,
        parent_thread_id,
        is_sticker,
        pin_event,
    }
}
