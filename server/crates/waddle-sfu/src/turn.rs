//! TURN time-limited credential issuance.
//!
//! Implements the [TURN REST credential] mechanism that LiveKit's
//! managed coturn deployment expects: the username is
//! `<unix_expiry>:<identity>` and the password is the base64-encoded
//! HMAC-SHA1 of the username under the shared TURN secret.
//!
//! Advertised over the wire through XEP-0215 with an `expires`
//! attribute so the client can refresh before the credential dies.
//!
//! [TURN REST credential]: https://datatracker.ietf.org/doc/html/draft-uberti-behave-turn-rest-00

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::call::Identity;
use crate::config::TurnSharedSecret;
use crate::error::SfuError;

type HmacSha1 = Hmac<Sha1>;

/// Hostname the client should reach for TURN. Bare DNS name; the
/// scheme is implied (`turns://` for the TLS port, `turn://` for
/// UDP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnHost(String);

impl TurnHost {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// TURN REST username, of the form `<unix_expiry>[:identity]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnUsername(String);

impl TurnUsername {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Base64-encoded HMAC-SHA1 password. Treated as a one-time secret;
/// [`std::fmt::Debug`] redacts it.
#[derive(Clone, PartialEq, Eq)]
pub struct TurnPassword(String);

impl TurnPassword {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for TurnPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnPassword")
            .field("len", &self.0.len())
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Issued TURN credential pair plus the absolute expiry the username
/// carries.
#[derive(Debug, Clone)]
pub struct TurnCredential {
    pub username: TurnUsername,
    pub password: TurnPassword,
    pub expires_at: DateTime<Utc>,
}

pub(crate) fn mint_turn_credential(
    secret: &TurnSharedSecret,
    identity: &Identity,
    ttl: Duration,
) -> Result<TurnCredential, SfuError> {
    let expires_at = Utc::now() + ttl;
    let username_str = format!(
        "{}:{}",
        expires_at.timestamp(),
        identity.as_livekit_identity()
    );

    let mut mac = HmacSha1::new_from_slice(secret.as_bytes()).map_err(|_| SfuError::HmacInit)?;
    mac.update(username_str.as_bytes());
    let digest = mac.finalize().into_bytes();
    let password = BASE64.encode(digest);

    Ok(TurnCredential {
        username: TurnUsername(username_str),
        password: TurnPassword(password),
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jid::FullJid;

    fn fixture_identity() -> Identity {
        let jid: FullJid = "alice@waddle.social/desktop".parse().expect("valid JID");
        Identity::from_jid(jid)
    }

    #[test]
    fn credential_username_encodes_expiry_and_identity() {
        let secret = TurnSharedSecret::from_text("turn-shared-secret-value");
        let identity = fixture_identity();
        let cred =
            mint_turn_credential(&secret, &identity, Duration::seconds(60)).expect("mint succeeds");

        let username = cred.username.as_str();
        let (timestamp_part, identity_part) = username
            .split_once(':')
            .expect("username has expiry:identity shape");

        assert_eq!(identity_part, "alice@waddle.social/desktop");
        let parsed_ts: i64 = timestamp_part.parse().expect("expiry parses as i64");
        assert_eq!(parsed_ts, cred.expires_at.timestamp());
    }

    #[test]
    fn credential_password_is_deterministic_under_same_secret_and_username() {
        let secret = TurnSharedSecret::from_text("turn-shared-secret-value");
        let identity = fixture_identity();

        let cred =
            mint_turn_credential(&secret, &identity, Duration::seconds(60)).expect("first mint");

        let mut mac = HmacSha1::new_from_slice(secret.as_bytes()).expect("hmac key");
        mac.update(cred.username.as_str().as_bytes());
        let expected = BASE64.encode(mac.finalize().into_bytes());

        assert_eq!(cred.password.as_str(), expected);
    }

    #[test]
    fn different_secret_yields_different_password() {
        let identity = fixture_identity();
        let username = "0:alice@waddle.social/desktop";

        let mut mac_a = HmacSha1::new_from_slice(b"shared-secret-a").expect("hmac key");
        mac_a.update(username.as_bytes());
        let pw_a = BASE64.encode(mac_a.finalize().into_bytes());

        let mut mac_b = HmacSha1::new_from_slice(b"shared-secret-b").expect("hmac key");
        mac_b.update(username.as_bytes());
        let pw_b = BASE64.encode(mac_b.finalize().into_bytes());

        assert_ne!(pw_a, pw_b);
        // Avoid unused-variable lint on `identity` while preserving
        // documentation of the username layout the credential follows.
        let _ = identity;
    }
}
