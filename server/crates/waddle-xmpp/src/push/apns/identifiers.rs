//! Validated APNs identifiers.
//!
//! Every value that ends up in an APNs request path or header is parsed
//! into one of these newtypes exactly once, so the sender never has to
//! re-validate (or escape) a raw string.

use thiserror::Error;

/// Upper bound on the hex length of a device token. Apple's tokens are
/// 32 bytes today but Apple explicitly reserves the right to change the
/// length, so the bound is generous rather than exact.
const MAX_DEVICE_TOKEN_HEX_LEN: usize = 200;
/// Upper bound on a bundle identifier / topic.
const MAX_TOPIC_LEN: usize = 255;
/// Apple team ids and key ids are both 10-character alphanumerics.
const APPLE_TEN_CHAR_ID_LEN: usize = 10;
/// Apple rejects `apns-collapse-id` values longer than 64 bytes.
const MAX_COLLAPSE_ID_LEN: usize = 64;

/// Why an APNs identifier failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ApnsIdentifierError {
    #[error("APNs device token must be a non-empty, even-length hex string of at most {MAX_DEVICE_TOKEN_HEX_LEN} characters")]
    DeviceToken,
    #[error("APNs topic must be 1..={MAX_TOPIC_LEN} characters of [A-Za-z0-9.-_]")]
    Topic,
    #[error("Apple team id must be exactly {APPLE_TEN_CHAR_ID_LEN} ASCII alphanumeric characters")]
    TeamId,
    #[error("APNs key id must be exactly {APPLE_TEN_CHAR_ID_LEN} ASCII alphanumeric characters")]
    KeyId,
}

/// Hex-encoded APNs device token, normalized to lowercase. It is the
/// last path segment of `POST /3/device/<token>`, so the hex-only
/// invariant is also what keeps the request path well-formed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ApnsDeviceToken(String);

impl ApnsDeviceToken {
    pub fn parse(value: &str) -> Result<Self, ApnsIdentifierError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_DEVICE_TOKEN_HEX_LEN
            && value.len().is_multiple_of(2)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if valid {
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(ApnsIdentifierError::DeviceToken)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A device token is a per-device bearer identifier: never print it.
impl std::fmt::Debug for ApnsDeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApnsDeviceToken(<redacted>)")
    }
}

/// `apns-topic`: the app's bundle identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApnsTopic(String);

impl ApnsTopic {
    pub fn parse(value: &str) -> Result<Self, ApnsIdentifierError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_TOPIC_LEN
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(ApnsIdentifierError::Topic)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_apple_ten_char_id(value: &str) -> bool {
    value.len() == APPLE_TEN_CHAR_ID_LEN && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Apple Developer team id: the `iss` claim of the provider token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApnsTeamId(String);

impl ApnsTeamId {
    pub fn parse(value: &str) -> Result<Self, ApnsIdentifierError> {
        if is_apple_ten_char_id(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ApnsIdentifierError::TeamId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// APNs signing key id: the `kid` header of the provider token.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApnsKeyId(String);

impl ApnsKeyId {
    pub fn parse(value: &str) -> Result<Self, ApnsIdentifierError> {
        if is_apple_ten_char_id(value) {
            Ok(Self(value.to_owned()))
        } else {
            Err(ApnsIdentifierError::KeyId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `apns-collapse-id`: at most 64 bytes of visible ASCII. Optional on
/// the wire, so construction returns `None` for a value Apple would
/// reject rather than an error; the notification is still delivered,
/// just without coalescing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApnsCollapseId(String);

impl ApnsCollapseId {
    pub fn new(value: &str) -> Option<Self> {
        let valid = !value.is_empty()
            && value.len() <= MAX_COLLAPSE_ID_LEN
            && value.bytes().all(|byte| byte.is_ascii_graphic());
        valid.then(|| Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_token_accepts_hex_and_normalizes_case() {
        let token = ApnsDeviceToken::parse("ABCDEF0123456789").expect("hex token");
        assert_eq!(token.as_str(), "abcdef0123456789");
    }

    #[test]
    fn device_token_rejects_non_hex_odd_empty_and_oversized() {
        for bad in ["", "abc", "zz", "ab/cd", &"a".repeat(202)] {
            assert_eq!(
                ApnsDeviceToken::parse(bad),
                Err(ApnsIdentifierError::DeviceToken),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn device_token_debug_is_redacted() {
        let token = ApnsDeviceToken::parse("abcd").expect("hex token");
        assert!(!format!("{token:?}").contains("abcd"));
    }

    #[test]
    fn topic_accepts_bundle_ids_and_rejects_header_breaking_values() {
        assert!(ApnsTopic::parse("social.waddle.app").is_ok());
        assert!(ApnsTopic::parse("p4x.waddle.social").is_ok());
        for bad in ["", "bad topic", "evil\r\nx: y", &"a".repeat(256)] {
            assert_eq!(ApnsTopic::parse(bad), Err(ApnsIdentifierError::Topic));
        }
    }

    #[test]
    fn team_and_key_ids_must_be_ten_alphanumerics() {
        assert!(ApnsTeamId::parse("ABCDE12345").is_ok());
        assert!(ApnsKeyId::parse("KEY1234567").is_ok());
        assert_eq!(ApnsTeamId::parse("SHORT"), Err(ApnsIdentifierError::TeamId));
        assert_eq!(
            ApnsKeyId::parse("ABCDE-1234"),
            Err(ApnsIdentifierError::KeyId)
        );
    }

    #[test]
    fn collapse_id_is_dropped_past_64_bytes() {
        assert!(ApnsCollapseId::new(&"a".repeat(64)).is_some());
        assert!(ApnsCollapseId::new(&"a".repeat(65)).is_none());
        assert!(ApnsCollapseId::new("").is_none());
        assert!(ApnsCollapseId::new("has space").is_none());
    }
}
