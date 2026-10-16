//! Data-only FCM HTTP v1 message.
//!
//! Waddle never sends an FCM `notification` block: Google would render
//! it, so it would have to carry plaintext. Instead the message carries a
//! single data field, [`FCM_DATA_KEY_PAYLOAD`], whose value is the
//! base64url (no padding) RFC 8291 `aes128gcm` ciphertext of the same v1
//! push envelope Web Push delivers. Only the device holding the
//! registration's `p256dh`/`auth` keys can read it, so Google sees no
//! conversation JID, class or item id.
//!
//! There is deliberately no `collapse_key`: FCM keeps at most four
//! collapsible messages per device, and a per-conversation key would let
//! Google correlate pushes by conversation.

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Serialize;
use thiserror::Error;

use super::identifiers::FcmRegistrationToken;
use crate::push::types::EncryptedPayload;

/// FCM's limit on the data payload of a message: the summed byte length
/// of every data key and value. Oversized messages are rejected by FCM
/// with `400 INVALID_ARGUMENT`, so the size is checked before sending.
pub const MAX_FCM_DATA_BYTES: usize = 4096;

/// The single data key carrying the encrypted envelope.
pub const FCM_DATA_KEY_PAYLOAD: &str = "payload";

/// FCM's maximum `android.ttl` (28 days).
const MAX_FCM_TTL: Duration = Duration::from_secs(28 * 24 * 60 * 60);

/// `android.priority`: `HIGH` wakes the device immediately, `NORMAL`
/// lets Android defer delivery under Doze.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FcmPriority {
    #[serde(rename = "HIGH")]
    High,
    #[serde(rename = "NORMAL")]
    Normal,
}

/// `android.ttl`: how long FCM stores the message for an offline device,
/// clamped to FCM's 28-day maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FcmTtl(Duration);

impl FcmTtl {
    pub fn new(ttl: Duration) -> Self {
        Self(ttl.min(MAX_FCM_TTL))
    }

    pub fn as_duration(self) -> Duration {
        self.0
    }
}

impl Serialize for FcmTtl {
    /// Protobuf JSON `Duration`: whole seconds with an `s` suffix.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{}s", self.0.as_secs()))
    }
}

/// One data-only message addressed to one registration token.
#[derive(Debug, Clone)]
pub struct FcmMessage {
    token: FcmRegistrationToken,
    encoded_payload: String,
    priority: FcmPriority,
    ttl: FcmTtl,
}

#[derive(Serialize)]
struct SendRequest<'a> {
    message: MessageBody<'a>,
}

#[derive(Serialize)]
struct MessageBody<'a> {
    token: &'a str,
    data: DataBody<'a>,
    android: AndroidConfig,
}

#[derive(Serialize)]
struct DataBody<'a> {
    payload: &'a str,
}

#[derive(Serialize)]
struct AndroidConfig {
    priority: FcmPriority,
    ttl: FcmTtl,
}

impl FcmMessage {
    /// `payload` is the RFC 8291 `aes128gcm` record built for the
    /// registration's own `p256dh`/`auth` keys.
    pub fn new(
        token: FcmRegistrationToken,
        payload: &EncryptedPayload,
        priority: FcmPriority,
        ttl: FcmTtl,
    ) -> Self {
        Self {
            token,
            encoded_payload: URL_SAFE_NO_PAD.encode(payload.as_slice()),
            priority,
            ttl,
        }
    }

    /// Byte size FCM counts against [`MAX_FCM_DATA_BYTES`].
    pub fn data_size(&self) -> usize {
        FCM_DATA_KEY_PAYLOAD.len() + self.encoded_payload.len()
    }

    /// Serialize the `messages:send` request body, rejecting a data
    /// payload over [`MAX_FCM_DATA_BYTES`] before it reaches FCM.
    pub fn encode(&self) -> Result<EncodedFcmMessage, FcmMessageError> {
        let size = self.data_size();
        if size > MAX_FCM_DATA_BYTES {
            return Err(FcmMessageError::TooLarge { size });
        }
        let body = SendRequest {
            message: MessageBody {
                token: self.token.as_str(),
                data: DataBody {
                    payload: &self.encoded_payload,
                },
                android: AndroidConfig {
                    priority: self.priority,
                    ttl: self.ttl,
                },
            },
        };
        serde_json::to_vec(&body)
            .map(EncodedFcmMessage)
            .map_err(FcmMessageError::Serialize)
    }
}

/// Why a message could not be encoded.
#[derive(Debug, Error)]
pub enum FcmMessageError {
    #[error("FCM data payload is {size} bytes, over the {MAX_FCM_DATA_BYTES}-byte limit")]
    TooLarge { size: usize },
    #[error("FCM message serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// A serialized `messages:send` body known to fit FCM's data limit. It
/// contains the registration token, so its `Debug` is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedFcmMessage(Vec<u8>);

impl EncodedFcmMessage {
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for EncodedFcmMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EncodedFcmMessage(<{} bytes>)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token() -> FcmRegistrationToken {
        FcmRegistrationToken::parse("device-token-1").expect("token")
    }

    #[test]
    fn exact_json_shape_is_data_only() {
        let payload = EncryptedPayload::new(vec![0xfb, 0xff, 0x00, 0x01]);
        let encoded = FcmMessage::new(
            token(),
            &payload,
            FcmPriority::High,
            FcmTtl::new(Duration::from_secs(604_800)),
        )
        .encode()
        .expect("encodes");
        assert_eq!(
            std::str::from_utf8(encoded.as_slice()).expect("utf-8"),
            concat!(
                r#"{"message":{"token":"device-token-1","data":{"payload":"-_8AAQ"},"#,
                r#""android":{"priority":"HIGH","ttl":"604800s"}}}"#
            )
        );
    }

    #[test]
    fn message_never_carries_a_notification_or_collapse_key() {
        let encoded = FcmMessage::new(
            token(),
            &EncryptedPayload::new(vec![1; 32]),
            FcmPriority::Normal,
            FcmTtl::new(Duration::from_secs(60)),
        )
        .encode()
        .expect("encodes");
        let value: serde_json::Value =
            serde_json::from_slice(encoded.as_slice()).expect("JSON body");
        let message = value["message"].as_object().expect("message object");
        let mut keys = message.keys().map(String::as_str).collect::<Vec<_>>();
        keys.sort_unstable();
        assert_eq!(keys, ["android", "data", "token"]);
        let android = message["android"].as_object().expect("android object");
        let mut android_keys = android.keys().map(String::as_str).collect::<Vec<_>>();
        android_keys.sort_unstable();
        assert_eq!(android_keys, ["priority", "ttl"]);
        assert_eq!(android["priority"], "NORMAL");
        let data = message["data"].as_object().expect("data object");
        assert_eq!(data.keys().collect::<Vec<_>>(), [FCM_DATA_KEY_PAYLOAD]);
    }

    #[test]
    fn ttl_is_clamped_to_twenty_eight_days() {
        assert_eq!(
            FcmTtl::new(Duration::from_secs(60 * 24 * 60 * 60)).as_duration(),
            MAX_FCM_TTL
        );
    }

    #[test]
    fn oversized_data_is_a_typed_error_before_sending() {
        // 3069 bytes of ciphertext encode to 4092 base64url characters;
        // with the 7-byte key the data payload is 4099 bytes.
        let err = FcmMessage::new(
            token(),
            &EncryptedPayload::new(vec![0; 3069]),
            FcmPriority::High,
            FcmTtl::new(Duration::from_secs(60)),
        )
        .encode()
        .expect_err("over the limit");
        assert!(matches!(err, FcmMessageError::TooLarge { size: 4099 }));
    }

    #[test]
    fn data_at_the_limit_is_accepted() {
        // 3066 bytes → 4088 characters + 7-byte key = 4095 bytes.
        let message = FcmMessage::new(
            token(),
            &EncryptedPayload::new(vec![0; 3066]),
            FcmPriority::High,
            FcmTtl::new(Duration::from_secs(60)),
        );
        assert!(message.data_size() <= MAX_FCM_DATA_BYTES);
        assert!(message.encode().is_ok());
    }

    #[test]
    fn encoded_message_debug_hides_the_token() {
        let encoded = FcmMessage::new(
            token(),
            &EncryptedPayload::new(vec![1]),
            FcmPriority::High,
            FcmTtl::new(Duration::from_secs(60)),
        )
        .encode()
        .expect("encodes");
        assert!(!format!("{encoded:?}").contains("device-token-1"));
    }
}
