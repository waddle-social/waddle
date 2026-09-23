//! Minimal APNs alert payload.
//!
//! XEP-0357 keeps message content away from the push service, and #529
//! makes the Apple payload minimal by default: a localized generic
//! banner chosen by notification class, the unread badge, a thread id
//! for grouping, and the same routing context the Web Push envelope
//! carries. There is deliberately no sender and no body field, and no
//! `mutable-content` flag, so nothing downstream of APNs can learn who
//! wrote what.
//!
//! The payload is built from serde structs and serialized once by
//! [`ApnsPayload::encode`], which also enforces Apple's 4096-byte limit.

use serde::Serialize;
use thiserror::Error;

use crate::push::envelope::NotificationClass;

/// Apple's maximum payload size for regular remote notifications.
pub const MAX_APNS_PAYLOAD_BYTES: usize = 4096;

/// Schema version of the `waddle` routing object. Matches the Web Push
/// envelope's `"v": 1`.
const WADDLE_ROUTING_VERSION: u8 = 1;

/// `aps.sound`: the system default alert sound.
const DEFAULT_SOUND: &str = "default";

/// `aps.alert.loc-key` per notification class. The Apple app ships a
/// localized string for each key, so the banner is generic ("New
/// message", "You were mentioned") and never carries message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsAlertLocKey {
    Dm,
    DmMention,
    PersonalMention,
    ChannelMention,
    ActiveChannelMention,
    NotifyAll,
}

impl ApnsAlertLocKey {
    pub fn for_class(class: NotificationClass) -> Self {
        match class {
            NotificationClass::Dm => Self::Dm,
            NotificationClass::DmMention => Self::DmMention,
            NotificationClass::PersonalMention => Self::PersonalMention,
            NotificationClass::ChannelMention => Self::ChannelMention,
            NotificationClass::ActiveChannelMention => Self::ActiveChannelMention,
            NotificationClass::NotifyAll => Self::NotifyAll,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
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
#[derive(Debug, Clone, Serialize)]
pub struct ApnsPayload<'a> {
    aps: Aps<'a>,
    waddle: WaddleRouting<'a>,
}

#[derive(Debug, Clone, Serialize)]
struct Aps<'a> {
    alert: Alert,
    #[serde(skip_serializing_if = "Option::is_none")]
    badge: Option<u64>,
    sound: &'static str,
    #[serde(rename = "thread-id")]
    thread_id: &'a str,
}

#[derive(Debug, Clone, Serialize)]
struct Alert {
    #[serde(rename = "loc-key")]
    loc_key: &'static str,
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
    /// XEP-0357 summary `message-count`, if the publisher sent one.
    pub message_count: Option<u64>,
}

impl<'a> ApnsPayload<'a> {
    pub fn new(fields: ApnsPayloadFields<'a>) -> Self {
        Self {
            aps: Aps {
                alert: Alert {
                    loc_key: ApnsAlertLocKey::for_class(fields.class).as_str(),
                },
                badge: fields.message_count,
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
        }
    }

    /// Serialize to the request body, rejecting anything over
    /// [`MAX_APNS_PAYLOAD_BYTES`] before it reaches Apple.
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
    #[error("APNs payload is {size} bytes, over the {MAX_APNS_PAYLOAD_BYTES}-byte limit")]
    TooLarge { size: usize },
    #[error("APNs payload serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// A serialized payload known to fit Apple's size limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedApnsPayload(Vec<u8>);

impl EncodedApnsPayload {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(class: NotificationClass, message_count: Option<u64>) -> ApnsPayloadFields<'static> {
        ApnsPayloadFields {
            class,
            conversation: "alice@example.com",
            thread: None,
            item: "stanza-1",
            node: "node-1",
            message_count,
        }
    }

    fn encode_to_string(fields: ApnsPayloadFields<'_>) -> String {
        let encoded = ApnsPayload::new(fields).encode().expect("encodes");
        String::from_utf8(encoded.as_slice().to_vec()).expect("utf-8 JSON")
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
    fn payload_never_carries_sender_body_or_mutable_content() {
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
        .encode()
        .expect_err("over the limit");
        assert!(
            matches!(err, ApnsPayloadError::TooLarge { size } if size > MAX_APNS_PAYLOAD_BYTES)
        );
    }
}
