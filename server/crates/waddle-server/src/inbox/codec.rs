use super::*;
use chrono::{DateTime, TimeZone, Utc};
use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};

pub(super) fn decode_row(row: &crate::db::Row) -> Result<InboxEntry, InboxDecodeError> {
    let partner_raw: String = row.get(0)?;
    let partner: BareJid = partner_raw
        .parse()
        .map_err(|source| InboxDecodeError::PartnerJid { source })?;
    let thread_id_raw: String = row.get(1)?;
    let kind_raw: String = row.get(2)?;
    let last_stanza_id: String = row.get(3)?;
    let last_updated: i64 = row.get(4)?;
    let unread: i64 = row.get(5)?;
    let preview: Option<String> = row.get(6)?;
    let thread_title: Option<String> = row.get(7)?;
    let reply_count: i64 = row.get(8)?;
    let author: Option<String> = row.get(9)?;
    let call_thread_kind_raw: Option<String> = row.get(10)?;
    let call_thread_media_raw: Option<String> = row.get(11)?;
    let call_ended_at_raw: Option<i64> = row.get(12)?;
    let call_duration_raw: Option<String> = row.get(13)?;

    // Call-thread metadata is a display projection: a single corrupt row
    // must not brick the whole threads listing. A present-but-invalid
    // value degrades that field to `None` (the row decodes as a non-call
    // thread) rather than erroring out of `decode_row` → `list_all_threads`.
    let decoded_kind = call_thread_kind_raw
        .as_deref()
        .and_then(decode_call_thread_kind);
    let decoded_media = call_thread_media_raw
        .as_deref()
        .and_then(decode_call_thread_media);
    // Kind+media-together invariant on read: if either ends up `None`,
    // treat BOTH as `None` so a row never has kind-without-media or
    // media-without-kind after decode (matching the wire `<call>`
    // condition, which requires both).
    let (call_thread_kind, call_thread_media) = match (decoded_kind, decoded_media) {
        (Some(kind), Some(media)) => (Some(kind), Some(media)),
        _ => (None, None),
    };

    Ok(InboxEntry {
        partner,
        kind: decode_kind(&kind_raw)?,
        last_stanza_id,
        last_updated,
        unread: unread.max(0) as u32,
        preview,
        thread_id: if thread_id_raw.is_empty() {
            None
        } else {
            Some(thread_id_raw)
        },
        thread_title,
        reply_count: reply_count.max(0) as u32,
        author,
        call_thread_kind,
        call_thread_media,
        // Out-of-range epoch seconds degrade to `None` rather than failing
        // the whole row decode (display-projection resilience).
        call_ended_at: call_ended_at_raw.and_then(decode_call_ended_at),
        call_duration: call_duration_raw
            .as_deref()
            .and_then(|raw| CallThreadDuration::parse(raw).ok()),
    })
}

/// Encodes the four call-thread columns from an entry, in `SELECT_COLS`
/// order: `call_thread_kind, call_thread_media, call_ended_at,
/// call_duration`. Typed values serialize to `String`/`i64` only here at
/// the SQL boundary; `None` becomes a typed NULL.
///
/// Kind+media-together invariant on write: a genuine call row carries
/// both a kind and a non-empty media token set. `CallThreadMedia`
/// serializes the impossible `(audio:false, video:false)` state to `""`;
/// writing that empty string would later trip the decoder. So if the
/// media tokens are empty we treat the row as "not a call thread" and
/// write NULL for BOTH `call_thread_kind` AND `call_thread_media`. The
/// encoded pair is therefore always either `(Some kind, non-empty media)`
/// or `(NULL, NULL)`.
pub(super) fn encode_call_thread_columns(entry: &InboxEntry) -> [crate::db::Value; 4] {
    let media_tokens = entry
        .call_thread_media
        .map(|media| media.as_tokens())
        .filter(|tokens| !tokens.is_empty());
    let kind_token = media_tokens
        .as_ref()
        .and(entry.call_thread_kind)
        .map(|kind| kind.as_token().to_owned());
    // Either both kind and (non-empty) media survive, or both drop to
    // NULL — a row never carries kind-without-media or media-without-kind.
    let (kind_token, media_tokens) = match (kind_token, media_tokens) {
        (Some(kind), Some(media)) => (Some(kind), Some(media)),
        _ => (None, None),
    };
    [
        crate::db::Value::from(kind_token),
        crate::db::Value::from(media_tokens),
        crate::db::Value::from(entry.call_ended_at.map(|ended| ended.timestamp())),
        crate::db::Value::from(
            entry
                .call_duration
                .as_ref()
                .map(|duration| duration.as_str().to_owned()),
        ),
    ]
}

/// Decodes a stored call-thread kind token. Resilient: an
/// unrecognized value degrades to `None` (the row is treated as a
/// non-call thread) instead of failing the whole row decode. NULL
/// (absent) is handled by the caller before this is reached.
fn decode_call_thread_kind(raw: &str) -> Option<CallThreadKind> {
    match raw {
        "dm" => Some(CallThreadKind::Dm),
        "muc" => Some(CallThreadKind::Muc),
        _ => None,
    }
}

/// Decodes a stored call-thread media token set. Resilient: an
/// unrecognized token, or an empty/token-less value (the impossible
/// `audio:false, video:false` state that `media_attr` panics on),
/// degrades the field to `None` instead of failing the whole row
/// decode. NULL (absent) is handled by the caller before this is
/// reached.
fn decode_call_thread_media(raw: &str) -> Option<CallThreadMedia> {
    let mut audio = false;
    let mut video = false;
    let mut saw_token = false;
    for token in raw.split_ascii_whitespace() {
        saw_token = true;
        match token {
            "audio" => audio = true,
            "video" => video = true,
            // Unknown token → not a media value we can project; degrade.
            _ => return None,
        }
    }
    // A non-NULL media value that yields no recognized tokens is the
    // impossible `audio:false, video:false` state. Degrade to `None`.
    if !saw_token {
        return None;
    }
    Some(CallThreadMedia { audio, video })
}

/// Decodes stored `call_ended_at` epoch seconds. Resilient:
/// out-of-range seconds degrade to `None` rather than failing the whole
/// row decode.
fn decode_call_ended_at(secs: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_opt(secs, 0).single()
}

pub(super) fn encode_kind(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::MucRoom => "muc",
    }
}

fn decode_kind(raw: &str) -> Result<ConversationKind, InboxDecodeError> {
    match raw {
        "direct" => Ok(ConversationKind::Direct),
        "muc" => Ok(ConversationKind::MucRoom),
        other => Err(InboxDecodeError::UnknownConversationKind {
            value: other.to_owned(),
        }),
    }
}

pub(super) const SELECT_COLS: &str = "partner_jid, thread_id, kind, last_stanza_id, last_updated, unread, preview, thread_title, reply_count, author, call_thread_kind, call_thread_media, call_ended_at, call_duration";

#[cfg(test)]
mod codec_tests {
    use super::*;

    #[test]
    fn decode_call_thread_media_parses_both_tokens() {
        let media = decode_call_thread_media("audio video").expect("both tokens decode");
        assert_eq!(
            media,
            CallThreadMedia {
                audio: true,
                video: true,
            }
        );
    }

    #[test]
    fn decode_call_thread_media_parses_audio_only() {
        let media = decode_call_thread_media("audio").expect("audio-only decodes");
        assert_eq!(
            media,
            CallThreadMedia {
                audio: true,
                video: false,
            }
        );
    }

    #[test]
    fn decode_call_thread_media_degrades_present_but_token_less_value() {
        // A present-but-empty media string is the impossible
        // `audio:false, video:false` state. Decode is a display
        // projection: it degrades to `None` (graceful) rather than
        // erroring and bricking the whole threads listing. NULL stays
        // `None` upstream before this is reached.
        assert_eq!(decode_call_thread_media(""), None);
        assert_eq!(decode_call_thread_media("   "), None);
    }

    #[test]
    fn decode_call_thread_media_degrades_unknown_token() {
        // An unknown media token degrades the field to `None` instead of
        // erroring (resilience over fail-fast for this projection).
        assert_eq!(decode_call_thread_media("audio screenshare"), None);
    }

    #[test]
    fn decode_call_thread_kind_degrades_unknown_value() {
        // A garbage kind yields `None`, not an error; the row still
        // decodes (as a non-call thread).
        assert_eq!(decode_call_thread_kind("muc"), Some(CallThreadKind::Muc));
        assert_eq!(decode_call_thread_kind("dm"), Some(CallThreadKind::Dm));
        assert_eq!(decode_call_thread_kind("garbage"), None);
        assert_eq!(decode_call_thread_kind(""), None);
    }
}
