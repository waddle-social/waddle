use minidom::Element;
use uuid::Uuid;

use crate::error::ClientResult;
use crate::request::StanzaId;
use crate::xep::encrypted_file as xep_encrypted_file;
use crate::xep::{reply as xep_reply, thread as xep_thread};
use waddle_xmpp_core::CoreError;

use super::namespaces::*;
use super::parsing::payloads::validate_chat_state;
use super::types::*;

pub fn build_chat_state_message(
    to: &str,
    state: &str,
    message_type: &str,
    thread: Option<&xep_thread::ThreadRef>,
) -> ClientResult<Element> {
    let state = validate_chat_state(state)?;
    let stanza_id = Uuid::new_v4().to_string();
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .append(Element::builder(state, NS_CHAT_STATES).build());
    if let Some(thread) = thread {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    Ok(builder.build())
}

pub fn build_displayed_message(
    to: &str,
    message_id: &str,
    message_type: &str,
    thread: Option<&xep_thread::ThreadRef>,
) -> Element {
    let stanza_id = Uuid::new_v4().to_string();
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .append(
            Element::builder("displayed", NS_CHAT_MARKERS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), message_id)
                .build(),
        )
        .append(Element::builder("no-store", NS_HINTS).build())
        .append(Element::builder("no-permanent-store", NS_HINTS).build());
    if let Some(thread) = thread {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    if should_stamp_origin_id(message_type) {
        builder = builder
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                stanza_id.as_str(),
            )
            .append(build_origin_id(&stanza_id));
    }
    builder.build()
}

pub fn build_reaction_message(
    to: &str,
    message_type: &str,
    target_id: &str,
    emojis: &[String],
    thread: Option<&xep_thread::ThreadRef>,
) -> Element {
    let stanza_id = Uuid::new_v4().to_string();
    let mut reactions = Element::builder("reactions", NS_REACTIONS)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), target_id)
        .build();
    for emoji in emojis {
        reactions.append_child(
            Element::builder("reaction", NS_REACTIONS)
                .append(emoji.as_str())
                .build(),
        );
    }

    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .append(build_origin_id(&stanza_id))
        .append(reactions);
    if let Some(thread) = thread {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    builder
        .append(Element::builder("store", NS_HINTS).build())
        .build()
}

pub fn build_in_call_reaction_message(
    recipient: &InCallReactionRecipient,
    sid: &InCallSessionId,
    emoji: &InCallReactionEmoji,
) -> Element {
    let to = recipient.to_jid_string();
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to.as_str())
        .attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            recipient.message_type(),
        )
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            Uuid::new_v4().to_string(),
        )
        .append(
            Element::builder("in-call", NS_WADDLE_IN_CALL)
                .attr(minidom::rxml::xml_ncname!("sid").to_owned(), sid.as_str())
                .append(
                    Element::builder("reaction", NS_WADDLE_IN_CALL)
                        .attr(
                            minidom::rxml::xml_ncname!("emoji").to_owned(),
                            emoji.as_str(),
                        )
                        .build(),
                )
                .build(),
        )
        .append(Element::builder("no-store", NS_HINTS).build())
        .append(Element::builder("no-copy", NS_HINTS).build())
        .build()
}

/// Build a `<message><pinned target='…'/></message>` request for #414.
/// `to` is the room bare JID; `target_stanza_id` is the XEP-0359
/// stanza-id (`by=room`) of the message being pinned.
pub fn build_pinned_message(to: &str, target_stanza_id: &str) -> Element {
    build_pin_message(to, PinMessageType::Groupchat, "pinned", target_stanza_id)
}

/// Build a 1:1 `<message type='chat'><pinned target='…'/></message>` request.
pub fn build_pinned_chat_message(to: &str, target_stanza_id: &str) -> Element {
    build_pin_message(to, PinMessageType::Chat, "pinned", target_stanza_id)
}

/// Build a `<message><unpinned target='…'/></message>` request for #414.
pub fn build_unpinned_message(to: &str, target_stanza_id: &str) -> Element {
    build_pin_message(to, PinMessageType::Groupchat, "unpinned", target_stanza_id)
}

/// Build a 1:1 `<message type='chat'><unpinned target='…'/></message>` request.
pub fn build_unpinned_chat_message(to: &str, target_stanza_id: &str) -> Element {
    build_pin_message(to, PinMessageType::Chat, "unpinned", target_stanza_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PinMessageType {
    Chat,
    Groupchat,
}

impl PinMessageType {
    fn as_attr(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Groupchat => "groupchat",
        }
    }
}

fn build_pin_message(
    to: &str,
    message_type: PinMessageType,
    element_name: &'static str,
    target_stanza_id: &str,
) -> Element {
    let stanza_id = Uuid::new_v4().to_string();
    Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(
            minidom::rxml::xml_ncname!("type").to_owned(),
            message_type.as_attr(),
        )
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .append(build_origin_id(&stanza_id))
        .append(
            Element::builder(element_name, NS_WADDLE_PIN_V0)
                .attr(
                    minidom::rxml::xml_ncname!("target").to_owned(),
                    target_stanza_id,
                )
                .build(),
        )
        .build()
}

pub fn build_retraction_message(
    to: &str,
    message_type: &str,
    retracts_id: &str,
    thread: Option<&xep_thread::ThreadRef>,
) -> Element {
    let stanza_id = Uuid::new_v4().to_string();
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .append(build_origin_id(&stanza_id))
        .append(
            Element::builder("retract", NS_MESSAGE_RETRACT)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), retracts_id)
                .build(),
        )
        .append(
            Element::builder("body", NS_CLIENT)
                .append("This person attempted to retract a previous message.")
                .build(),
        )
        // XEP-0424 §Use Case: the generic body is only a fallback for
        // non-supporting clients and SHOULD be marked with a XEP-0428
        // <fallback for='urn:xmpp:message-retract:1'/> so supporting
        // receivers never render it as real content (#1267 item 1).
        .append(
            Element::builder("fallback", crate::xep::fallback::NS_FALLBACK)
                .attr(
                    minidom::rxml::xml_ncname!("for").to_owned(),
                    NS_MESSAGE_RETRACT,
                )
                .build(),
        );
    if let Some(thread) = thread {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    builder
        .append(Element::builder("store", NS_HINTS).build())
        .build()
}

/// Build the XEP-0045 §7.2 room-join `<presence/>` for `room`/`nick`.
///
/// Always requests **no discussion history** (`<history maxstanzas='0'/>`,
/// XEP-0045 §7.2.15): Waddle clients hydrate room history exclusively via
/// MAM catch-up (XEP-0313), so accepting the service's default history on
/// join would deliver the same recent messages twice (#1255).
///
/// Typed-payloads rule: the occupant JID is built from a typed
/// [`jid::BareJid`] + resource (nick validation happens here, not via
/// string concatenation); an invalid nick surfaces as a bad-request error.
pub fn build_muc_join_presence(room: &jid::BareJid, nick: &str) -> ClientResult<Element> {
    let occupant = room
        .with_resource_str(nick)
        .map_err(|err| CoreError::bad_request(Some(format!("invalid MUC nick {nick:?}: {err}"))))?;
    Ok(Element::builder("presence", NS_CLIENT)
        .attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            occupant.to_string(),
        )
        .append(
            Element::builder("x", NS_MUC)
                .append(
                    Element::builder("history", NS_MUC)
                        .attr(minidom::rxml::xml_ncname!("maxstanzas").to_owned(), "0")
                        .build(),
                )
                .build(),
        )
        .build())
}

pub fn build_moderation_message(
    to: &str,
    _message_type: &str,
    target_id: &str,
    reason: Option<&str>,
) -> Element {
    let mut moderate = Element::builder("moderate", NS_MESSAGE_MODERATE)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), target_id)
        .append(Element::builder("retract", NS_MESSAGE_RETRACT).build());
    if let Some(reason) = reason {
        moderate = moderate.append(
            Element::builder("reason", NS_MESSAGE_MODERATE)
                .append(reason)
                .build(),
        );
    }

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            Uuid::new_v4().to_string(),
        )
        .append(moderate.build())
        .build()
}

pub fn build_correction_message(
    to: &str,
    message_type: &str,
    body: &str,
    replaces_id: &str,
    options: &SendMessageOptions,
) -> ClientResult<(StanzaId, Element)> {
    let (stanza_id, mut stanza) = build_outbound_message(to, message_type, body, options)?;
    stanza.append_child(
        Element::builder("replace", NS_MESSAGE_CORRECT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), replaces_id)
            .build(),
    );
    Ok((stanza_id, stanza))
}

/// Build a `<message/>` stanza carrying the body plus any XEP payloads from
/// `options`. All XML construction goes through typed `minidom::Element`
/// builders — never `format!` — per the project XML hard rule.
pub fn build_outbound_message(
    to: &str,
    message_type: &str,
    body: &str,
    options: &SendMessageOptions,
) -> ClientResult<(StanzaId, Element)> {
    let stanza_id = match options.stanza_id.clone() {
        Some(stanza_id) => stanza_id,
        None => StanzaId::new(Uuid::new_v4().to_string())?,
    };
    let mut builder = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), message_type)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            stanza_id.as_str(),
        )
        .append(Element::builder("body", NS_CLIENT).append(body).build());

    if matches!(message_type, "chat" | "groupchat") {
        builder = builder.append(build_origin_id(stanza_id.as_str()));
    }

    if options.request_displayed_marker && matches!(message_type, "chat" | "groupchat") {
        builder = builder.append(Element::builder("markable", NS_CHAT_MARKERS).build());
    }

    // XEP-0045 §7.5: mark MUC private messages with an empty
    // `<x xmlns='http://jabber.org/protocol/muc#user'/>` so the sender's
    // other clients can classify the XEP-0280 sent-carbon copy as a MUC
    // PM (receivers MUST NOT rely on it; it is a sender-side SHOULD).
    if options.muc_pm && message_type == "chat" {
        builder = builder.append(Element::builder("x", NS_MUC_USER).build());
    }

    if let Some(subject) = options.subject.as_deref() {
        builder = builder.append(
            Element::builder("subject", NS_CLIENT)
                .append(subject)
                .build(),
        );
    }
    if let Some(marker) = options.reply.as_ref() {
        builder = builder.append(xep_reply::build_reply_element(marker));
    }
    if let Some(range) = options.fallback.as_ref() {
        builder = builder.append(xep_reply::build_fallback_element(range));
    }
    if let Some(thread) = options.thread.as_ref() {
        builder = builder.append(xep_thread::build_thread_element(thread));
    }
    if !options.markup_spans.is_empty() {
        let mut markups = Element::builder("markup", NS_MARKUP).build();
        for span in &options.markup_spans {
            // XEP-0394: inline spans use <span start="..." end="..."><child/></span>;
            // block-level elements (bcode, bquote) are siblings of span, not wrapped in it;
            // links use <span start="..." end="..." uri="..."/> with no child element.
            let el = match span.span_type.as_str() {
                "bold" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            span.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            span.end.to_string(),
                        )
                        .build();
                    el.append_child(Element::builder("strong", NS_MARKUP).build());
                    el
                }
                "italic" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            span.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            span.end.to_string(),
                        )
                        .build();
                    el.append_child(Element::builder("emphasis", NS_MARKUP).build());
                    el
                }
                "strikethrough" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            span.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            span.end.to_string(),
                        )
                        .build();
                    el.append_child(Element::builder("deleted", NS_MARKUP).build());
                    el
                }
                "code" => {
                    let mut el = Element::builder("span", NS_MARKUP)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            span.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            span.end.to_string(),
                        )
                        .build();
                    el.append_child(Element::builder("code", NS_MARKUP).build());
                    el
                }
                // Block-level: <bcode start="..." end="..."/> — sibling of <span>, never wrapped
                "code_block" => Element::builder("bcode", NS_MARKUP)
                    .attr(
                        minidom::rxml::xml_ncname!("start").to_owned(),
                        span.start.to_string(),
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("end").to_owned(),
                        span.end.to_string(),
                    )
                    .build(),
                // Block-level: <bquote start="..." end="..."/> — sibling of <span>, never wrapped
                "blockquote" => Element::builder("bquote", NS_MARKUP)
                    .attr(
                        minidom::rxml::xml_ncname!("start").to_owned(),
                        span.start.to_string(),
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("end").to_owned(),
                        span.end.to_string(),
                    )
                    .build(),
                // Link: <span start="..." end="..." uri="..."/> in urn:waddle:markup:0 —
                // XEP-0394 does not define a link span; use custom namespace to avoid
                // polluting the official markup namespace.
                "link" => {
                    let mut b = Element::builder("span", NS_WADDLE_MARKUP)
                        .attr(
                            minidom::rxml::xml_ncname!("start").to_owned(),
                            span.start.to_string(),
                        )
                        .attr(
                            minidom::rxml::xml_ncname!("end").to_owned(),
                            span.end.to_string(),
                        );
                    if let Some(uri) = &span.uri {
                        b = b.attr(minidom::rxml::xml_ncname!("uri").to_owned(), uri);
                    }
                    b.build()
                }
                _ => continue,
            };
            markups.append_child(el);
        }
        builder = builder.append(markups);
    }
    for reference in &options.references {
        // XEP-0372 §2.1: `begin` and `end` are OPTIONAL. They MUST be present
        // when the reference points at a substring of the body, and MUST be
        // absent when no body position applies (e.g. anchor-only references
        // to a previous message). Treat (0, 0) as the "no position" sentinel
        // so anchor-only / future use cases can travel cleanly on the wire
        // without emitting `begin="0" end="0"` (which a conformant receiver
        // would interpret as a 0-length annotation at body offset 0).
        let mut ref_builder = Element::builder("reference", NS_REFERENCES)
            .attr(
                minidom::rxml::xml_ncname!("type").to_owned(),
                reference.ref_type.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("uri").to_owned(),
                reference.uri.as_str(),
            );
        if reference.begin != 0 || reference.end != 0 {
            ref_builder = ref_builder
                .attr(
                    minidom::rxml::xml_ncname!("begin").to_owned(),
                    reference.begin.to_string(),
                )
                .attr(
                    minidom::rxml::xml_ncname!("end").to_owned(),
                    reference.end.to_string(),
                );
        }
        if let Some(anchor) = reference.anchor.as_deref() {
            ref_builder = ref_builder.attr(minidom::rxml::xml_ncname!("anchor").to_owned(), anchor);
        }
        builder = builder.append(ref_builder.build());
    }
    for file in &options.shared_files {
        builder = builder.append(build_file_sharing_element(file)?);
    }
    // XEP-0449: mark the message as a sticker. The marker travels
    // alongside the XEP-0447 file-sharing payload built above.
    if let Some(sticker) = options.sticker.as_ref() {
        builder = builder.append(crate::stickers::build_sticker_marker_element(sticker));
    }
    if let Some(token) = options.link_preview_token.as_ref() {
        builder = builder.append(
            Element::builder("preview-request", NS_WADDLE_LINK_PREVIEW)
                .attr(
                    minidom::rxml::xml_ncname!("token").to_owned(),
                    token.as_str(),
                )
                .build(),
        );
    }
    Ok((stanza_id, builder.build()))
}

fn should_stamp_origin_id(message_type: &str) -> bool {
    matches!(message_type, "chat" | "groupchat")
}

fn build_origin_id(stanza_id: &str) -> Element {
    Element::builder("origin-id", NS_ORIGIN_ID)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), stanza_id)
        .build()
}

pub fn build_file_sharing_element(file: &SharedFile) -> ClientResult<Element> {
    let mut file_sharing = Element::builder("file-sharing", NS_SFS)
        .attr(
            minidom::rxml::xml_ncname!("disposition").to_owned(),
            file.disposition.as_str(),
        )
        .build();

    let mut metadata = Element::builder("file", NS_FILE_METADATA).build();
    if let Some(media_type) = file.media_type.as_deref() {
        metadata.append_child(
            Element::builder("media-type", NS_FILE_METADATA)
                .append(media_type)
                .build(),
        );
    }
    if let Some(name) = file.name.as_deref() {
        metadata.append_child(
            Element::builder("name", NS_FILE_METADATA)
                .append(name)
                .build(),
        );
    }
    if let Some(size) = file.size {
        metadata.append_child(
            Element::builder("size", NS_FILE_METADATA)
                .append(size.to_string())
                .build(),
        );
    }
    if let Some(width) = file.width {
        metadata.append_child(
            Element::builder("width", NS_FILE_METADATA)
                .append(width.to_string())
                .build(),
        );
    }
    if let Some(height) = file.height {
        metadata.append_child(
            Element::builder("height", NS_FILE_METADATA)
                .append(height.to_string())
                .build(),
        );
    }
    // XEP-0446 lang-less textual fallback — mandatory for XEP-0449
    // sticker files so non-supporting clients render the emoji.
    if let Some(desc) = file.desc.as_deref() {
        metadata.append_child(
            Element::builder("desc", NS_FILE_METADATA)
                .append(desc)
                .build(),
        );
    }
    // XEP-0300 content hashes of the plaintext file (XEP-0446 §hash).
    for hash in &file.hashes {
        metadata.append_child(crate::stickers::build_hash_element(hash));
    }
    file_sharing.append_child(metadata);

    let mut sources = Element::builder("sources", NS_SFS).build();
    if let Some(encrypted) = file.encrypted.as_ref() {
        let encrypted_with_source;
        let mut sources_for_encrypted: Vec<String> = encrypted
            .sources
            .iter()
            .map(|source| source.trim())
            .filter(|source| !source.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        let primary_source = file.url.trim();
        if !primary_source.is_empty() {
            sources_for_encrypted.retain(|source| source != primary_source);
            sources_for_encrypted.insert(0, primary_source.to_owned());
        }
        if sources_for_encrypted.is_empty() {
            return Err(CoreError::bad_request(Some(
                "encrypted shared file requires a ciphertext source URL".to_string(),
            ))
            .into());
        }
        let encrypted = if sources_for_encrypted == encrypted.sources {
            encrypted
        } else {
            encrypted_with_source = xep_encrypted_file::EncryptedFile {
                sources: sources_for_encrypted,
                ..encrypted.clone()
            };
            &encrypted_with_source
        };
        if let Some(encrypted_el) = xep_encrypted_file::build_encrypted_element(encrypted) {
            sources.append_child(encrypted_el);
        }
    } else {
        sources.append_child(
            Element::builder("url-data", NS_URL_DATA)
                .attr(
                    minidom::rxml::xml_ncname!("target").to_owned(),
                    file.url.as_str(),
                )
                .build(),
        );
    }
    file_sharing.append_child(sources);
    Ok(file_sharing)
}
