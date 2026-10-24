//! Deserialization and validation of JavaScript message and client options.

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

/// Typed mention directory from the JS options. A target whose JID does
/// not parse as a bare JID is dropped: it can never be a conformant
/// `xmpp:<barejid>` mention, and one bad roster entry must not fail the send.
fn mention_directory_from_js(directory: WaddleMentionDirectory) -> MentionDirectory {
    MentionDirectory {
        targets: directory
            .targets
            .into_iter()
            .filter_map(|target| {
                let jid = target.jid.parse::<BareJid>().ok()?;
                Some(MentionTarget {
                    label: target.label,
                    jid,
                })
            })
            .collect(),
        broadcasts: directory.broadcasts,
    }
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
        mention_directory: options.mention_directory.map(mention_directory_from_js),
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
                // Plaintext file-level hashes (XEP-0448 requires ≥1 for
                // encrypted sends). Unknown algorithms are refused at
                // this boundary rather than smuggled through as strings.
                let hashes = file_hashes_from_js(file.hashes).map_err(js_error)?;
                Ok(messaging::SharedFile {
                    url: file.url,
                    name: file.name,
                    media_type: file.media_type,
                    size: file.size,
                    width: file.width,
                    height: file.height,
                    desc: None,
                    hashes,
                    disposition,
                    encrypted,
                })
            })
            .collect::<Result<Vec<_>, JsValue>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mention_directory_drops_targets_without_a_bare_jid() {
        let directory = mention_directory_from_js(WaddleMentionDirectory {
            targets: vec![
                WaddleMentionTarget {
                    label: "alice".to_string(),
                    jid: "alice@example.com".to_string(),
                },
                WaddleMentionTarget {
                    label: "ghost".to_string(),
                    jid: "not a jid@".to_string(),
                },
            ],
            broadcasts: true,
        });

        assert!(directory.broadcasts);
        assert_eq!(
            directory.targets,
            vec![MentionTarget {
                label: "alice".to_string(),
                jid: "alice@example.com".parse().expect("bare JID"),
            }]
        );
    }
}
