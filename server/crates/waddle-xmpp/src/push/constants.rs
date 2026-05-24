//! Wire constants for RFC 8030, RFC 8188, RFC 8291, RFC 8292.
//!
//! Centralized so the `rs`-field round-trip test asserts against named
//! values instead of inline literals.

use std::time::Duration;

/// RFC 8188 §2.1 salt length: 16 bytes.
pub const AES128GCM_SALT_LEN: usize = 16;

/// RFC 8188 §2.1 `rs` field length: u32 in network byte order.
pub const AES128GCM_RS_LEN: usize = 4;

/// RFC 8188 §2.1 `idlen` field length: u8 indicating keyid byte length.
pub const AES128GCM_IDLEN_FIELD_LEN: usize = 1;

/// Uncompressed P-256 SEC1 point length: `0x04` prefix + 32-byte X + 32-byte Y.
pub const P256_UNCOMPRESSED_POINT_LEN: usize = 65;

/// SEC1 uncompressed-point prefix byte.
pub const P256_UNCOMPRESSED_POINT_PREFIX: u8 = 0x04;

/// RFC 8188 §2.1 last-record padding delimiter byte.
pub const AES128GCM_LAST_RECORD_DELIM: u8 = 0x02;

/// RFC 8188 §2.1 header length for a Web Push payload:
/// salt + rs + idlen + uncompressed P-256 keyid.
pub const AES128GCM_HEADER_LEN: usize =
    AES128GCM_SALT_LEN + AES128GCM_RS_LEN + AES128GCM_IDLEN_FIELD_LEN + P256_UNCOMPRESSED_POINT_LEN;

/// AES-GCM authentication tag length (RFC 5116 §5.1).
pub const AES128GCM_TAG_LEN: usize = 16;

/// RFC 8188 §2.1 padding delimiter byte (`0x02` for the last record).
pub const AES128GCM_PAD_DELIMITER_LEN: usize = 1;

/// Conservative cap on the encrypted body sent to the browser push service.
/// FCM documents a 4KB Web Push message limit; Mozilla autopush and Apple
/// web push are equal or more permissive. We cap conservatively at 4096 and
/// surface any vendor's stricter rejection as `WebPushOutcome::PayloadTooLarge`.
pub const WEB_PUSH_MAX_BODY_LEN: usize = 4096;

/// RFC 8188 `rs` field — **per-record max size** = plaintext + padding + delimiter + AES-GCM tag.
/// NOT the body length. `body = AES128GCM_HEADER_LEN + actual_record_len ≤ WEB_PUSH_MAX_BODY_LEN`.
pub const WEB_PUSH_MAX_RS: u32 = (WEB_PUSH_MAX_BODY_LEN - AES128GCM_HEADER_LEN) as u32;

/// Maximum plaintext + zero-padding (RFC 8188 §2.1): `rs − tag − delimiter`.
pub const WEB_PUSH_MAX_PLAINTEXT: usize =
    WEB_PUSH_MAX_RS as usize - AES128GCM_TAG_LEN - AES128GCM_PAD_DELIMITER_LEN;

/// HKDF PRK length (SHA-256 output, RFC 5869 §2.3).
pub const HKDF_PRK_LEN: usize = 32;

/// AES-128 key length (RFC 8291 §3.4 CEK).
pub const HKDF_CEK_LEN: usize = 16;

/// AES-GCM nonce length (RFC 8291 §3.4 / RFC 5116 §5.1).
pub const HKDF_NONCE_LEN: usize = 12;

/// VAPID JWT clock-skew tolerance for `ClockSkew` classification: 5 minutes.
/// On 401/403, if `now() − jwt.iat() < CLOCK_SKEW_THRESHOLD` we classify as
/// `WebPushOutcome::ClockSkew` rather than `SubscriptionGone`.
pub const CLOCK_SKEW_THRESHOLD: Duration = Duration::from_secs(5 * 60);

/// VAPID JWT `iat` lag — sign with `iat = now − IAT_LAG` to absorb minor
/// clock-skew without rejection.
pub const IAT_LAG: Duration = Duration::from_secs(30);

/// VAPID JWT lifetime — `exp = iat + VAPID_JWT_LIFETIME`.
/// RFC 8292 §2 mandates ≤ 24h; we use 12h to leave headroom for the
/// 11h cache reuse window.
pub const VAPID_JWT_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);

/// Eviction-before-expiry margin for the JWT cache: drop cached JWTs
/// when `now() + CACHE_EVICT_MARGIN >= exp`.
pub const CACHE_EVICT_MARGIN: Duration = Duration::from_secs(60 * 60);

/// LRU JWT cache max capacity. The legitimate browser-push origin set is
/// small (~3 — FCM, Mozilla autopush, Apple); 32 accommodates multi-`kid`
/// rotation windows and unknown origins without growing unbounded.
pub const VAPID_JWT_CACHE_CAPACITY: usize = 32;

/// HKDF `info` string for the IKM combine step (RFC 8291 §3.4).
/// Format: `"WebPush: info\0" || ua_public || as_public`.
pub const WEBPUSH_INFO_PREFIX: &[u8] = b"WebPush: info\0";

/// HKDF `info` string for the CEK derivation (RFC 8188 §2.2).
pub const AES128GCM_KEY_INFO: &[u8] = b"Content-Encoding: aes128gcm\0";

/// HKDF `info` string for the nonce derivation (RFC 8188 §2.2).
pub const AES128GCM_NONCE_INFO: &[u8] = b"Content-Encoding: nonce\0";

/// Padding bucket for `DirectMessage` / `DirectMessageMention` classes.
/// Larger bucket covers >P95 of empirical chat-message body lengths while
/// keeping ciphertext well under 1200 bytes — reduces cadence leakage.
pub const DM_PLAINTEXT_BUCKET: usize = 1024;

/// Padding bucket for non-DM classes.
pub const DEFAULT_PLAINTEXT_BUCKET: usize = 256;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_size_matches_rfc_8188_serialization() {
        // 16 salt + 4 rs + 1 idlen + 65 raw P-256 keyid = 86
        assert_eq!(AES128GCM_HEADER_LEN, 16 + 4 + 1 + 65);
    }

    #[test]
    fn rs_plus_header_equals_body_cap() {
        assert_eq!(
            AES128GCM_HEADER_LEN + WEB_PUSH_MAX_RS as usize,
            WEB_PUSH_MAX_BODY_LEN
        );
    }

    #[test]
    fn plaintext_cap_accounts_for_tag_and_delimiter() {
        assert_eq!(
            WEB_PUSH_MAX_PLAINTEXT,
            WEB_PUSH_MAX_RS as usize - AES128GCM_TAG_LEN - AES128GCM_PAD_DELIMITER_LEN
        );
        assert_eq!(WEB_PUSH_MAX_PLAINTEXT, 3993);
    }

    #[test]
    fn cache_window_invariant_holds() {
        // Cache must evict before exp, and exp must be within RFC 8292 §2's
        // 24h cap.
        assert!(CACHE_EVICT_MARGIN < VAPID_JWT_LIFETIME);
        assert!(VAPID_JWT_LIFETIME + IAT_LAG <= Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn hkdf_info_strings_end_in_nul() {
        assert_eq!(*WEBPUSH_INFO_PREFIX.last().unwrap(), 0u8);
        assert_eq!(*AES128GCM_KEY_INFO.last().unwrap(), 0u8);
        assert_eq!(*AES128GCM_NONCE_INFO.last().unwrap(), 0u8);
    }
}
