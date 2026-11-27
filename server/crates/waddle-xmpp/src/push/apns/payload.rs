//! APNs alert payload.
//!
//! XEP-0357 keeps message content away from the push service, and #529
//! makes the Apple payload minimal by default: a localized generic
//! banner chosen by notification class, the unread badge, a thread id
//! for grouping, and the same routing context the Web Push envelope
//! carries. By default there is no sender and no body, so nothing
//! downstream of APNs can learn who wrote what.
//!
//! The alert shows message content only when the publisher supplied a
//! sender in the XEP-0357 summary ([`ApnsAlertContent`]). For an opted-in
//! conversation (`urn:waddle:push:rich:0`), the user server can also
//! provide display names alongside the summary. There is never a
//! `mutable-content` flag.
//!
//! The payload is built from serde structs and serialized by
//! [`ApnsPayload::encode`], which keeps it within Apple's 4096-byte
//! limit: content that does not fit is shortened, then dropped in favour
//! of the generic banner.

use jid::{BareJid, Jid};
use serde::{Serialize, Serializer};
use thiserror::Error;
use xmpp_parsers::jingle::SessionId;

use crate::push::envelope::NotificationClass;
use crate::push::names::DisplayName;

/// Apple's maximum payload size for regular remote notifications.
pub const MAX_APNS_PAYLOAD_BYTES: usize = 4096;

/// Schema version of the `waddle` routing object. Matches the Web Push
/// envelope's `"v": 1`.
const WADDLE_ROUTING_VERSION: u8 = 1;

/// Longest alert body sent, in UTF-8 bytes before JSON escaping. A
/// banner shows a few lines; the rest of the message is in the app.
const MAX_ALERT_BODY_BYTES: usize = 1024;

/// Longest alert title sent, in UTF-8 bytes before JSON escaping.
const MAX_ALERT_TITLE_BYTES: usize = 256;

/// Longest alert subtitle sent, in UTF-8 bytes before JSON escaping.
const MAX_ALERT_SUBTITLE_BYTES: usize = 256;

const ELLIPSIS: char = '…';

/// `aps.sound`: the system default alert sound.
const DEFAULT_SOUND: &str = "default";

/// `aps.alert.loc-key` per notification class. The Apple app ships a
/// localized string for each key, so the banner is generic ("New
/// message", "You were mentioned") and never carries message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsAlertLocKey {
    MissedCall,
    Dm,
    DmMention,
    PersonalMention,
    ChannelMention,
    ActiveChannelMention,
    NotifyAll,
}

impl ApnsAlertLocKey {
    pub fn for_class(class: NotificationClass) -> Result<Self, ApnsPayloadError> {
        Ok(match class {
            NotificationClass::Call => return Err(ApnsPayloadError::InvalidAlertClass),
            NotificationClass::MissedCall => Self::MissedCall,
            NotificationClass::Dm => Self::Dm,
            NotificationClass::DmMention => Self::DmMention,
            NotificationClass::PersonalMention => Self::PersonalMention,
            NotificationClass::ChannelMention => Self::ChannelMention,
            NotificationClass::ActiveChannelMention => Self::ActiveChannelMention,
            NotificationClass::NotifyAll => Self::NotifyAll,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissedCall => "WADDLE_PUSH_MISSED_CALL",
            Self::Dm => "WADDLE_PUSH_DM",
            Self::DmMention => "WADDLE_PUSH_DM_MENTION",
            Self::PersonalMention => "WADDLE_PUSH_PERSONAL_MENTION",
            Self::ChannelMention => "WADDLE_PUSH_CHANNEL_MENTION",
            Self::ActiveChannelMention => "WADDLE_PUSH_ACTIVE_CHANNEL_MENTION",
            Self::NotifyAll => "WADDLE_PUSH_NOTIFY_ALL",
        }
    }
}

/// Top-level APNs JSON document.
#[derive(Clone, Serialize)]
pub struct ApnsPayload<'a> {
    aps: Aps<'a>,
    waddle: WaddleRouting<'a>,
    /// The banner used when the content does not fit.
    #[serde(skip)]
    generic_loc_key: ApnsAlertLocKey,
}

#[derive(Clone, Serialize)]
struct Aps<'a> {
    alert: Alert,
    #[serde(skip_serializing_if = "Option::is_none")]
    badge: Option<u64>,
    sound: &'static str,
    #[serde(rename = "thread-id")]
    thread_id: &'a str,
}

/// `aps.alert`. `body` and `loc-key` are alternatives: Apple shows the
/// localized string only when there is no literal body.
#[derive(Clone, Serialize)]
struct Alert {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subtitle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<String>,
    #[serde(rename = "loc-key", skip_serializing_if = "Option::is_none")]
    loc_key: Option<&'static str>,
}

impl Alert {
    fn generic(loc_key: ApnsAlertLocKey) -> Self {
        Self {
            title: None,
            subtitle: None,
            body: None,
            loc_key: Some(loc_key.as_str()),
        }
    }

    fn with_content(
        loc_key: ApnsAlertLocKey,
        class: NotificationClass,
        content: ApnsAlertContent<'_>,
    ) -> Self {
        // A message is never shown without its sender.
        let is_room = class.is_room();
        let fallback = if is_room {
            content.sender.resource().map(|nick| nick.as_str())
        } else {
            content.sender.node().map(|node| node.as_str())
        };
        let Some(sender) = content.sender_name.map(DisplayName::as_str).or(fallback) else {
            return Self::generic(loc_key);
        };
        let (title, subtitle) = match content.room_name.filter(|_| is_room) {
            Some(room) => (
                truncated(&format!("#{}", room.as_str()), MAX_ALERT_TITLE_BYTES),
                Some(truncated(sender, MAX_ALERT_SUBTITLE_BYTES)),
            ),
            None => (truncated(sender, MAX_ALERT_TITLE_BYTES), None),
        };
        let body = content
            .body
            .filter(|body| !body.is_empty())
            .map(|body| truncated(body, MAX_ALERT_BODY_BYTES));
        Self {
            title: Some(title),
            subtitle,
            loc_key: body.is_none().then_some(loc_key.as_str()),
            body,
        }
    }
}

/// `text` cut to at most `max_bytes` on a character boundary, ending in
/// an ellipsis when something was cut.
fn truncated(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes.saturating_sub(ELLIPSIS.len_utf8());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut cut = text[..end].trim_end().to_owned();
    cut.push(ELLIPSIS);
    cut
}

/// The last message from the XEP-0357 summary and optional display names
/// carried alongside it.
#[derive(Clone, Copy)]
pub struct ApnsAlertContent<'a> {
    /// Full JID: the sending resource in a 1:1 conversation, the
    /// occupant JID in a room.
    pub sender: &'a Jid,
    pub body: Option<&'a str>,
    pub sender_name: Option<&'a DisplayName>,
    pub room_name: Option<&'a DisplayName>,
}

/// Message content stays out of logs.
impl std::fmt::Debug for ApnsAlertContent<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApnsAlertContent")
            .field("has_body", &self.body.is_some())
            .finish_non_exhaustive()
    }
}

/// Routing context for the notification tap, mirroring the Web Push
/// envelope (`urn:waddle:push:context:0` model) plus the push node id.
#[derive(Debug, Clone, Serialize)]
struct WaddleRouting<'a> {
    v: u8,
    class: &'static str,
    conversation: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread: Option<&'a str>,
    item: &'a str,
    node: &'a str,
}

/// Inputs for [`ApnsPayload::new`]. Borrowed so the worker renders the
/// typed JID once and reuses it across the device fan-out.
#[derive(Debug, Clone, Copy)]
pub struct ApnsPayloadFields<'a> {
    pub class: NotificationClass,
    /// Conversation bare JID (DM peer or MUC room), already rendered.
    pub conversation: &'a str,
    /// XEP-0201 thread id, if any.
    pub thread: Option<&'a str>,
    /// Dedup key: the XEP-0359 stanza id, else the pubsub item id.
    pub item: &'a str,
    /// XEP-0357 push node id.
    pub node: &'a str,
    /// Waddle account-wide unread snapshot for `aps.badge`, if present.
    /// This is deliberately independent of XEP-0357's per-conversation
    /// summary `message-count`.
    pub badge_count: Option<u64>,
    /// Sender and body to show. `None` keeps the generic banner.
    pub content: Option<ApnsAlertContent<'a>>,
}

impl<'a> ApnsPayload<'a> {
    pub fn new(fields: ApnsPayloadFields<'a>) -> Result<Self, ApnsPayloadError> {
        let loc_key = ApnsAlertLocKey::for_class(fields.class)?;
        Ok(Self {
            generic_loc_key: loc_key,
            aps: Aps {
                alert: match fields.content {
                    Some(content) => Alert::with_content(loc_key, fields.class, content),
                    None => Alert::generic(loc_key),
                },
                badge: fields.badge_count,
                sound: DEFAULT_SOUND,
                thread_id: fields.conversation,
            },
            waddle: WaddleRouting {
                v: WADDLE_ROUTING_VERSION,
                class: fields.class.as_db_value(),
                conversation: fields.conversation,
                thread: fields.thread,
                item: fields.item,
                node: fields.node,
            },
        })
    }

    /// Serialize to the request body, rejecting anything over
    /// [`MAX_APNS_PAYLOAD_BYTES`] before it reaches Apple.
    ///
    /// Content never costs the notification: JSON escaping can grow a
    /// body several times over, so an oversized payload is retried with
    /// half the body, and at last with the generic banner.
    pub fn encode(&self) -> Result<EncodedApnsPayload, ApnsPayloadError> {
        let bytes = serde_json::to_vec(self).map_err(ApnsPayloadError::Serialize)?;
        if bytes.len() <= MAX_APNS_PAYLOAD_BYTES {
            return Ok(EncodedApnsPayload(bytes));
        }
        let alert = &self.aps.alert;
        let smaller = match alert.body.as_deref() {
            Some(body) if body.len() > 2 * ELLIPSIS.len_utf8() => Alert {
                body: Some(truncated(body, body.len() / 2)),
                ..alert.clone()
            },
            _ if alert.title.is_some() || alert.subtitle.is_some() || alert.body.is_some() => {
                Alert::generic(self.generic_loc_key)
            }
            _ => return Err(ApnsPayloadError::TooLarge { size: bytes.len() }),
        };
        Self {
            aps: Aps {
                alert: smaller,
                ..self.aps.clone()
            },
            waddle: self.waddle.clone(),
            generic_loc_key: self.generic_loc_key,
        }
        .encode()
    }
}

/// Immutable call metadata carried from the publisher to PushKit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApnsCall {
    #[serde(serialize_with = "serialize_session_id")]
    pub sid: SessionId,
    pub video: bool,
    pub expires: u64,
}

fn serialize_session_id<S: Serializer>(sid: &SessionId, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&sid.0)
}

/// A PushKit payload has only routing and call metadata, without `aps`.
#[derive(Debug, Clone, Serialize)]
pub struct ApnsVoipPayload<'a> {
    waddle: VoipRouting<'a>,
}

#[derive(Debug, Clone, Serialize)]
struct VoipRouting<'a> {
    v: u8,
    class: &'static str,
    conversation: &'a BareJid,
    item: &'a str,
    node: &'a str,
    call: &'a ApnsCall,
}

#[derive(Debug, Clone, Copy)]
pub struct ApnsVoipPayloadFields<'a> {
    pub conversation: &'a BareJid,
    pub item: &'a str,
    pub node: &'a str,
    pub call: &'a ApnsCall,
}

impl<'a> ApnsVoipPayload<'a> {
    pub fn new(fields: ApnsVoipPayloadFields<'a>) -> Self {
        Self {
            waddle: VoipRouting {
                v: WADDLE_ROUTING_VERSION,
                class: NotificationClass::Call.as_db_value(),
                conversation: fields.conversation,
                item: fields.item,
                node: fields.node,
                call: fields.call,
            },
        }
    }

    pub fn encode(&self) -> Result<EncodedApnsPayload, ApnsPayloadError> {
        let bytes = serde_json::to_vec(self).map_err(ApnsPayloadError::Serialize)?;
        if bytes.len() > MAX_APNS_PAYLOAD_BYTES {
            return Err(ApnsPayloadError::TooLarge { size: bytes.len() });
        }
        Ok(EncodedApnsPayload(bytes))
    }
}

/// Why a payload could not be encoded.
#[derive(Debug, Error)]
pub enum ApnsPayloadError {
    #[error("call notifications require an APNs VoIP payload")]
    InvalidAlertClass,
    #[error("APNs payload is {size} bytes, over the {MAX_APNS_PAYLOAD_BYTES}-byte limit")]
    TooLarge { size: usize },
    #[error("APNs payload serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// A serialized payload known to fit Apple's size limit.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedApnsPayload(Vec<u8>);

/// The bytes may hold message content, which stays out of logs.
impl std::fmt::Debug for EncodedApnsPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedApnsPayload")
            .field("len", &self.0.len())
            .finish()
    }
}

impl EncodedApnsPayload {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(class: NotificationClass, badge_count: Option<u64>) -> ApnsPayloadFields<'static> {
        ApnsPayloadFields {
            class,
            conversation: "alice@example.com",
            thread: None,
            item: "stanza-1",
            node: "node-1",
            badge_count,
            content: None,
        }
    }

    fn encode_to_string(fields: ApnsPayloadFields<'_>) -> String {
        let encoded = ApnsPayload::new(fields)
            .expect("alert class")
            .encode()
            .expect("encodes");
        String::from_utf8(encoded.as_slice().to_vec()).expect("utf-8 JSON")
    }

    fn alert_of(fields: ApnsPayloadFields<'_>) -> serde_json::Value {
        let json = encode_to_string(fields);
        assert!(json.len() <= MAX_APNS_PAYLOAD_BYTES);
        serde_json::from_str::<serde_json::Value>(&json).expect("JSON")["aps"]["alert"].clone()
    }

    fn content<'a>(sender: &'a Jid, body: Option<&'a str>) -> Option<ApnsAlertContent<'a>> {
        Some(ApnsAlertContent {
            sender,
            body,
            sender_name: None,
            room_name: None,
        })
    }

    #[test]
    fn direct_message_content_shows_the_senders_localpart_and_the_body() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        assert_eq!(
            alert_of(ApnsPayloadFields {
                content: content(&sender, Some("see you at 5")),
                ..fields(NotificationClass::Dm, None)
            }),
            serde_json::json!({ "title": "mallory", "body": "see you at 5" })
        );
    }

    #[test]
    fn room_content_shows_the_occupants_nick() {
        let sender: Jid = "general@rooms.example.com/Mallory M".parse().expect("jid");
        assert_eq!(
            alert_of(ApnsPayloadFields {
                content: content(&sender, Some("@alice ping")),
                ..fields(NotificationClass::PersonalMention, None)
            }),
            serde_json::json!({ "title": "Mallory M", "body": "@alice ping" })
        );
    }

    #[test]
    fn display_names_title_direct_alerts_and_add_room_subtitles() {
        let room_sender: Jid = "general@rooms.example.com/Wonderland".parse().expect("jid");
        let dm_sender: Jid = "alice@example.com/phone".parse().expect("jid");
        let sender_name = DisplayName::parse("Alice Liddell").expect("sender name");
        let room_name = DisplayName::parse("General").expect("room name");
        for (class, sender, sender_name, room_name, title, subtitle) in [
            (
                NotificationClass::Dm,
                &dm_sender,
                Some(&sender_name),
                Some(&room_name),
                "Alice Liddell",
                None,
            ),
            (NotificationClass::Dm, &dm_sender, None, None, "alice", None),
            (
                NotificationClass::PersonalMention,
                &room_sender,
                Some(&sender_name),
                Some(&room_name),
                "#General",
                Some("Alice Liddell"),
            ),
            (
                NotificationClass::PersonalMention,
                &room_sender,
                None,
                Some(&room_name),
                "#General",
                Some("Wonderland"),
            ),
            (
                NotificationClass::PersonalMention,
                &room_sender,
                Some(&sender_name),
                None,
                "Alice Liddell",
                None,
            ),
            (
                NotificationClass::PersonalMention,
                &room_sender,
                None,
                None,
                "Wonderland",
                None,
            ),
        ] {
            let json = encode_to_string(ApnsPayloadFields {
                content: Some(ApnsAlertContent {
                    sender,
                    body: Some("hello"),
                    sender_name,
                    room_name,
                }),
                ..fields(class, None)
            });
            let payload: serde_json::Value = serde_json::from_str(&json).expect("JSON");
            let alert = &payload["aps"]["alert"];
            assert_eq!(alert["title"], title);
            assert_eq!(
                alert.get("subtitle").and_then(serde_json::Value::as_str),
                subtitle
            );
            assert_eq!(alert["body"], "hello");
            assert!(payload["aps"].get("mutable-content").is_none());
        }
    }

    #[test]
    fn hostile_names_are_cleaned_and_capped_before_encoding() {
        let raw = format!(
            "  Alice\n\u{202e}\u{2069}\0  Liddell\u{2003} {}  ",
            "🦋".repeat(10_000)
        );
        let name = DisplayName::parse(&raw).expect("name");
        assert_eq!(name.as_str(), format!("Alice Liddell {}", "🦋".repeat(50)));
        assert!(DisplayName::parse(" \n\0\u{202a}\u{2066}\u{fffe}\u{ffff} ").is_none());

        let sender = "general@rooms.example.com/Wonderland".parse().expect("jid");
        let long_name = DisplayName::parse(&"🦋".repeat(10_000)).expect("name");
        let alert = alert_of(ApnsPayloadFields {
            content: Some(ApnsAlertContent {
                sender: &sender,
                body: Some("hello"),
                sender_name: Some(&long_name),
                room_name: Some(&long_name),
            }),
            ..fields(NotificationClass::PersonalMention, None)
        });
        assert!(alert["title"].as_str().expect("title").len() <= 256);
        assert!(alert["subtitle"].as_str().expect("subtitle").len() <= 256);
    }

    #[test]
    fn oversized_room_alert_falls_back_to_generic_banner_without_subtitle() {
        let class = NotificationClass::PersonalMention;
        let sender = "general@rooms.example.com/Wonderland".parse().expect("jid");
        let name = DisplayName::parse(&"🦋".repeat(64)).expect("name");
        let overhead = encode_to_string(ApnsPayloadFields {
            conversation: "",
            ..fields(class, None)
        })
        .len();
        let conversation = "a".repeat((MAX_APNS_PAYLOAD_BYTES - overhead) / 2);
        assert_eq!(
            alert_of(ApnsPayloadFields {
                conversation: &conversation,
                content: Some(ApnsAlertContent {
                    sender: &sender,
                    body: Some("hello"),
                    sender_name: Some(&name),
                    room_name: Some(&name),
                }),
                ..fields(class, None)
            }),
            serde_json::json!({ "loc-key": "WADDLE_PUSH_PERSONAL_MENTION" })
        );
    }

    #[test]
    fn a_sender_without_a_body_keeps_the_generic_banner_under_their_name() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        for body in [None, Some("")] {
            assert_eq!(
                alert_of(ApnsPayloadFields {
                    content: content(&sender, body),
                    ..fields(NotificationClass::MissedCall, None)
                }),
                serde_json::json!({ "title": "mallory", "loc-key": "WADDLE_PUSH_MISSED_CALL" })
            );
        }
    }

    #[test]
    fn a_long_body_is_cut_on_a_character_boundary() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        let body = "é".repeat(2000);
        let alert = alert_of(ApnsPayloadFields {
            content: content(&sender, Some(&body)),
            ..fields(NotificationClass::Dm, None)
        });
        let sent = alert["body"].as_str().expect("body");
        assert!(sent.len() <= MAX_ALERT_BODY_BYTES);
        assert_eq!(sent, format!("{}…", "é".repeat(510)));
    }

    #[test]
    fn a_body_that_escapes_past_the_limit_is_shortened_not_dropped() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        // Each control character is six bytes of JSON.
        let body = "\u{1}".repeat(1000);
        let alert = alert_of(ApnsPayloadFields {
            content: content(&sender, Some(&body)),
            ..fields(NotificationClass::Dm, None)
        });
        let sent = alert["body"].as_str().expect("body");
        assert!(sent.starts_with('\u{1}') && sent.ends_with('…'));
        assert_eq!(alert["title"], "mallory");
    }

    #[test]
    fn content_that_can_never_fit_falls_back_to_the_generic_banner() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        // The longest conversation whose generic payload still fits.
        let overhead = encode_to_string(ApnsPayloadFields {
            conversation: "",
            ..fields(NotificationClass::Dm, None)
        })
        .len();
        let conversation = "a".repeat((MAX_APNS_PAYLOAD_BYTES - overhead) / 2);
        assert_eq!(
            alert_of(ApnsPayloadFields {
                conversation: &conversation,
                content: content(&sender, Some("hello")),
                ..fields(NotificationClass::Dm, None)
            }),
            serde_json::json!({ "loc-key": "WADDLE_PUSH_DM" })
        );
    }

    #[test]
    fn a_body_without_a_nameable_sender_keeps_the_generic_banner() {
        // A room message needs the occupant's nick, a 1:1 message a localpart.
        for (class, sender) in [
            (
                NotificationClass::PersonalMention,
                "general@rooms.example.com",
            ),
            (NotificationClass::Dm, "example.com/phone"),
        ] {
            let sender: Jid = sender.parse().expect("jid");
            let alert = alert_of(ApnsPayloadFields {
                content: content(&sender, Some("hello")),
                ..fields(class, None)
            });
            assert_eq!(alert.as_object().expect("alert").len(), 1, "{alert}");
            assert!(alert["loc-key"].is_string());
        }
    }

    #[test]
    fn debug_output_omits_the_body() {
        let sender: Jid = "mallory@example.com/phone".parse().expect("jid");
        let content = content(&sender, Some("the launch code"));
        assert!(!format!("{content:?}").contains("launch"));
        let encoded = ApnsPayload::new(ApnsPayloadFields {
            content,
            ..fields(NotificationClass::Dm, None)
        })
        .expect("alert class")
        .encode()
        .expect("encodes");
        assert!(!format!("{encoded:?}").contains("launch"));
    }

    #[test]
    fn voip_payload_has_exact_routing_shape_and_no_aps() {
        let conversation = "alice@example.com".parse().expect("bare jid");
        let call = ApnsCall {
            sid: xmpp_parsers::jingle::SessionId("call-1".into()),
            video: false,
            expires: 1_700_000_000,
        };
        let payload = ApnsVoipPayload::new(ApnsVoipPayloadFields {
            conversation: &conversation,
            item: "stanza-1",
            node: "node-1",
            call: &call,
        })
        .encode()
        .expect("voip payload");
        assert_eq!(
            std::str::from_utf8(payload.as_slice()).expect("json"),
            r#"{"waddle":{"v":1,"class":"call","conversation":"alice@example.com","item":"stanza-1","node":"node-1","call":{"sid":"call-1","video":false,"expires":1700000000}}}"#
        );
    }

    #[test]
    fn call_cannot_render_as_an_alert() {
        assert!(matches!(
            ApnsPayload::new(fields(NotificationClass::Call, None)),
            Err(ApnsPayloadError::InvalidAlertClass)
        ));
    }

    #[test]
    fn exact_json_shape_with_badge_and_thread() {
        let json = encode_to_string(ApnsPayloadFields {
            thread: Some("thread-9"),
            ..fields(NotificationClass::PersonalMention, Some(3))
        });
        assert_eq!(
            json,
            concat!(
                r#"{"aps":{"alert":{"loc-key":"WADDLE_PUSH_PERSONAL_MENTION"},"badge":3,"#,
                r#""sound":"default","thread-id":"alice@example.com"},"#,
                r#""waddle":{"v":1,"class":"personal_mention","conversation":"alice@example.com","#,
                r#""thread":"thread-9","item":"stanza-1","node":"node-1"}}"#
            )
        );
    }

    #[test]
    fn missed_call_has_exact_alert_shape_and_real_badge() {
        let json = encode_to_string(fields(NotificationClass::MissedCall, Some(7)));
        assert_eq!(
            json,
            r#"{"aps":{"alert":{"loc-key":"WADDLE_PUSH_MISSED_CALL"},"badge":7,"sound":"default","thread-id":"alice@example.com"},"waddle":{"v":1,"class":"missed_call","conversation":"alice@example.com","item":"stanza-1","node":"node-1"}}"#
        );
    }

    #[test]
    fn badge_and_thread_are_omitted_when_absent() {
        let json = encode_to_string(fields(NotificationClass::Dm, None));
        assert_eq!(
            json,
            concat!(
                r#"{"aps":{"alert":{"loc-key":"WADDLE_PUSH_DM"},"sound":"default","#,
                r#""thread-id":"alice@example.com"},"#,
                r#""waddle":{"v":1,"class":"dm","conversation":"alice@example.com","#,
                r#""item":"stanza-1","node":"node-1"}}"#
            )
        );
    }

    #[test]
    fn payload_without_content_carries_no_sender_body_or_mutable_content() {
        let json = encode_to_string(fields(NotificationClass::Dm, Some(1)));
        let value: serde_json::Value = serde_json::from_str(&json).expect("JSON");
        let aps = value["aps"].as_object().expect("aps object");
        let mut aps_keys = aps.keys().map(String::as_str).collect::<Vec<_>>();
        aps_keys.sort_unstable();
        assert_eq!(aps_keys, ["alert", "badge", "sound", "thread-id"]);
        let alert = aps["alert"].as_object().expect("alert object");
        assert_eq!(alert.keys().collect::<Vec<_>>(), ["loc-key"]);
        for forbidden in ["body", "sender", "from", "title", "mutable-content"] {
            assert!(!json.contains(forbidden), "{forbidden} leaked into {json}");
        }
    }

    #[test]
    fn loc_key_per_class() {
        for (class, key) in [
            (NotificationClass::MissedCall, "WADDLE_PUSH_MISSED_CALL"),
            (NotificationClass::Dm, "WADDLE_PUSH_DM"),
            (NotificationClass::DmMention, "WADDLE_PUSH_DM_MENTION"),
            (
                NotificationClass::PersonalMention,
                "WADDLE_PUSH_PERSONAL_MENTION",
            ),
            (
                NotificationClass::ChannelMention,
                "WADDLE_PUSH_CHANNEL_MENTION",
            ),
            (
                NotificationClass::ActiveChannelMention,
                "WADDLE_PUSH_ACTIVE_CHANNEL_MENTION",
            ),
            (NotificationClass::NotifyAll, "WADDLE_PUSH_NOTIFY_ALL"),
        ] {
            let json = encode_to_string(fields(class, None));
            let value: serde_json::Value = serde_json::from_str(&json).expect("JSON");
            assert_eq!(value["aps"]["alert"]["loc-key"], key);
            assert_eq!(value["waddle"]["class"], class.as_db_value());
        }
    }

    #[test]
    fn oversized_payload_is_a_typed_error() {
        let huge_thread = "t".repeat(MAX_APNS_PAYLOAD_BYTES);
        let err = ApnsPayload::new(ApnsPayloadFields {
            thread: Some(&huge_thread),
            ..fields(NotificationClass::Dm, None)
        })
        .expect("alert class")
        .encode()
        .expect_err("over the limit");
        assert!(
            matches!(err, ApnsPayloadError::TooLarge { size } if size > MAX_APNS_PAYLOAD_BYTES)
        );
    }
}
