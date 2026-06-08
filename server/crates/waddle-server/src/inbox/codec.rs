use super::*;
use chrono::{DateTime, TimeZone, Utc};
use waddle_xmpp::xep::{CallThreadDuration, CallThreadKind, CallThreadMedia};

pub(super) fn decode_row(row: &crate::db::Row) -> Result<InboxEntry, InboxStorageError> {
    let partner_raw: String = row
        .get(0)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let partner: BareJid = partner_raw
        .parse()
        .map_err(|error| InboxStorageError::Other(format!("invalid partner JID: {error}")))?;
    let thread_id_raw: String = row
        .get(1)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let kind_raw: String = row
        .get(2)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let last_stanza_id: String = row
        .get(3)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let last_updated: i64 = row
        .get(4)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let unread: i64 = row
        .get(5)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let preview: Option<String> = row
        .get(6)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let thread_title: Option<String> = row
        .get(7)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let reply_count: i64 = row
        .get(8)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let author: Option<String> = row
        .get(9)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let call_thread_kind_raw: Option<String> = row
        .get(10)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let call_thread_media_raw: Option<String> = row
        .get(11)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let call_ended_at_raw: Option<i64> = row
        .get(12)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;
    let call_duration_raw: Option<String> = row
        .get(13)
        .map_err(|error| InboxStorageError::Other(error.to_string()))?;

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
        call_thread_kind: call_thread_kind_raw
            .as_deref()
            .map(decode_call_thread_kind)
            .transpose()?,
        call_thread_media: call_thread_media_raw
            .as_deref()
            .map(decode_call_thread_media)
            .transpose()?,
        call_ended_at: call_ended_at_raw.map(decode_call_ended_at).transpose()?,
        call_duration: call_duration_raw
            .as_deref()
            .and_then(|raw| CallThreadDuration::parse(raw).ok()),
    })
}

/// Encodes the four call-thread columns from an entry, in `SELECT_COLS`
/// order: `call_thread_kind, call_thread_media, call_ended_at,
/// call_duration`. Typed values serialize to `String`/`i64` only here at
/// the SQL boundary; `None` becomes a typed NULL.
pub(super) fn encode_call_thread_columns(entry: &InboxEntry) -> [crate::db::Value; 4] {
    [
        crate::db::Value::from(
            entry
                .call_thread_kind
                .map(|kind| kind.as_token().to_owned()),
        ),
        crate::db::Value::from(entry.call_thread_media.map(|media| media.as_tokens())),
        crate::db::Value::from(entry.call_ended_at.map(|ended| ended.timestamp())),
        crate::db::Value::from(
            entry
                .call_duration
                .as_ref()
                .map(|duration| duration.as_str().to_owned()),
        ),
    ]
}

fn decode_call_thread_kind(raw: &str) -> Result<CallThreadKind, InboxStorageError> {
    match raw {
        "dm" => Ok(CallThreadKind::Dm),
        "muc" => Ok(CallThreadKind::Muc),
        other => Err(InboxStorageError::Other(format!(
            "unknown call-thread kind '{other}'"
        ))),
    }
}

fn decode_call_thread_media(raw: &str) -> Result<CallThreadMedia, InboxStorageError> {
    let mut audio = false;
    let mut video = false;
    let mut saw_token = false;
    for token in raw.split_ascii_whitespace() {
        saw_token = true;
        match token {
            "audio" => audio = true,
            "video" => video = true,
            other => {
                return Err(InboxStorageError::Other(format!(
                    "unknown call-thread media token '{other}'"
                )))
            }
        }
    }
    // A non-NULL media value that yields no recognized tokens is the
    // impossible `audio:false, video:false` state (the XEP `media_attr`
    // panics on it). Reject it so decode is a true inverse of encode;
    // NULL (absent) decodes to `None` upstream before this is called.
    if !saw_token {
        return Err(InboxStorageError::Other(
            "empty call-thread media value has no audio/video token".to_owned(),
        ));
    }
    Ok(CallThreadMedia { audio, video })
}

fn decode_call_ended_at(secs: i64) -> Result<DateTime<Utc>, InboxStorageError> {
    Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
        InboxStorageError::Other(format!("invalid call_ended_at epoch seconds: {secs}"))
    })
}

pub(super) fn encode_kind(kind: ConversationKind) -> &'static str {
    match kind {
        ConversationKind::Direct => "direct",
        ConversationKind::MucRoom => "muc",
    }
}

fn decode_kind(raw: &str) -> Result<ConversationKind, InboxStorageError> {
    match raw {
        "direct" => Ok(ConversationKind::Direct),
        "muc" => Ok(ConversationKind::MucRoom),
        other => Err(InboxStorageError::Other(format!(
            "unknown inbox conversation kind '{other}'"
        ))),
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
    fn decode_call_thread_media_rejects_present_but_token_less_value() {
        // A present-but-empty media string would otherwise decode to the
        // impossible `audio:false, video:false` (which `media_attr` panics
        // on). Decode must reject it instead. NULL stays `None` upstream.
        assert!(decode_call_thread_media("").is_err());
        assert!(decode_call_thread_media("   ").is_err());
    }

    #[test]
    fn decode_call_thread_media_rejects_unknown_token() {
        assert!(decode_call_thread_media("audio screenshare").is_err());
    }
}
