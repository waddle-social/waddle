use super::*;

/// Build an optional `ThreadRef` from a pair of `Option<String>` wasm
/// arguments. An empty/missing `thread_id` yields `None` so callers can
/// thread context through without conditionals at every site. A
/// `thread_parent` without a `thread_id` is ignored — XEP-0201 only
/// defines `parent` as a qualifier on a present `<thread/>`.
pub(crate) fn thread_ref_from_args(
    thread_id: Option<String>,
    thread_parent: Option<String>,
) -> Option<ThreadRef> {
    let id = thread_id.filter(|s| !s.is_empty())?;
    Some(ThreadRef {
        id,
        parent: thread_parent.filter(|s| !s.is_empty()),
    })
}

pub(crate) fn send_options_from_js(options: JsValue) -> Result<SendMessageOptions, JsValue> {
    let options = if options.is_null() || options.is_undefined() {
        WaddleSendOptions::default()
    } else {
        serde_wasm_bindgen::from_value(options).map_err(|err| js_error(err.to_string()))?
    };

    let reply = match options.reply {
        Some(target) => Some(ReplyMarker {
            to: target
                .author_jid
                .parse::<Jid>()
                .map_err(|err| js_error(format!("invalid reply author JID: {err}")))?,
            id: target.message_id,
        }),
        None => None,
    };

    let stanza_id = options
        .stanza_id
        .map(StanzaId::new)
        .transpose()
        .map_err(|err| js_error(err.to_string()))?;

    Ok(SendMessageOptions {
        stanza_id,
        subject: options.subject,
        // The wasm chat surface has no sticker picker yet; the field
        // exists so the shared builder compiles against one options
        // shape across FFI surfaces.
        sticker: None,
        reply,
        fallback: options.fallback.map(|range| FallbackRange {
            start: range.start,
            end: range.end,
        }),
        thread: options.thread.map(|thread| ThreadRef {
            id: thread.id,
            parent: thread.parent,
        }),
        link_preview_token: options
            .link_preview_token
            .map(LinkPreviewToken::new)
            .transpose()
            .map_err(|err| js_error(err.to_string()))?,
        request_displayed_marker: options.request_displayed_marker,
        muc_pm: options.muc_pm,
        markup_spans: options
            .markup_spans
            .into_iter()
            .map(|span| MarkupSpanData {
                span_type: span.span_type,
                start: span.start,
                end: span.end,
                uri: span.uri,
            })
            .collect(),
        references: options
            .references
            .into_iter()
            .map(|reference| ReferenceData {
                ref_type: reference.ref_type,
                uri: reference.uri,
                begin: reference.begin,
                end: reference.end,
                anchor: reference.anchor,
            })
            .collect(),
        shared_files: options
            .shared_files
            .into_iter()
            .map(|file| {
                let disposition = SharedFileDisposition::from_text_or_infer(
                    Some(file.disposition.as_str()),
                    file.media_type.as_deref(),
                );
                // Surface invalid encryption metadata as an explicit
                // error rather than silently sending a ciphertext URL
                // without the matching XEP-0448 envelope — that produces
                // hard-to-debug broken attachments for recipients.
                let encrypted = file
                    .encrypted
                    .map(encrypted_file_from_js)
                    .transpose()
                    .map_err(|err| js_error(err.to_string()))?;
                Ok(messaging::SharedFile {
                    url: file.url,
                    name: file.name,
                    media_type: file.media_type,
                    size: file.size,
                    width: file.width,
                    height: file.height,
                    desc: None,
                    hashes: Vec::new(),
                    disposition,
                    encrypted,
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?,
    })
}
