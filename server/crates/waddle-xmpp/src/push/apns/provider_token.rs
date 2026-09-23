//! APNs token-based provider authentication.
//!
//! Apple authenticates providers with an ES256 JWT signed by the `.p8`
//! key downloaded from the developer portal: header `{alg: ES256, kid}`
//! and claims `{iss: <team id>, iat}`. Apple rejects tokens older than
//! one hour (`ExpiredProviderToken`) and refreshes more often than once
//! every 20 minutes (`TooManyProviderTokenUpdates`), so the signer mints
//! one token and reuses it for [`APNS_PROVIDER_TOKEN_REUSE`] (50
//! minutes), which sits inside that window.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use p256::pkcs8::DecodePrivateKey;
use serde::Serialize;
use thiserror::Error;

use super::identifiers::{ApnsKeyId, ApnsTeamId};

/// How long a minted provider token is reused before a fresh one is
/// signed.
pub const APNS_PROVIDER_TOKEN_REUSE: Duration = Duration::from_secs(50 * 60);

/// Apple refuses provider tokens refreshed more often than this
/// (`429 TooManyProviderTokenUpdates`), so a `403` for a younger token
/// keeps it: a fresh token would not fix whatever Apple rejected.
pub const APNS_PROVIDER_TOKEN_MIN_REFRESH: Duration = Duration::from_secs(20 * 60);

/// Wall-clock source for `iat` and cache age. Injected so tests can
/// move time without sleeping.
pub trait ApnsClock: Send + Sync + 'static {
    fn now_unix_seconds(&self) -> u64;
}

/// Production clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemApnsClock;

impl ApnsClock for SystemApnsClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}

/// A signed provider token, carried as `authorization: bearer <jwt>`.
#[derive(Clone, PartialEq, Eq)]
pub struct ApnsProviderJwt(String);

impl ApnsProviderJwt {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The token authenticates the whole provider: never print it.
impl std::fmt::Debug for ApnsProviderJwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApnsProviderJwt(<redacted>)")
    }
}

/// Why the `.p8` key could not be loaded.
#[derive(Debug, Error)]
pub enum ApnsKeyError {
    #[error("APNs key is not a PKCS#8 PEM P-256 private key")]
    NotP256Pkcs8,
    #[error("APNs key could not be loaded for ES256 signing: {0}")]
    Encoding(#[source] jsonwebtoken::errors::Error),
}

/// Why a provider token could not be signed.
#[derive(Debug, Error)]
pub enum ApnsSignError {
    #[error("APNs provider token signing failed: {0}")]
    Signing(#[source] jsonwebtoken::errors::Error),
}

/// Source of provider tokens for the worker. Object-safe so the Push
/// Service store holds an `Arc<dyn ApnsProviderTokenSource>`.
pub trait ApnsProviderTokenSource: Send + Sync + 'static {
    /// The current token, minting a fresh one when the cached token is
    /// older than [`APNS_PROVIDER_TOKEN_REUSE`].
    fn current(&self) -> Result<ApnsProviderJwt, ApnsSignError>;

    /// Drop the cached token if it is still `rejected` and at least
    /// [`APNS_PROVIDER_TOKEN_MIN_REFRESH`] old. Keyed on the rejected
    /// token so a burst of concurrent `403`s for the same token causes
    /// exactly one refresh, not one per device. Returns whether the next
    /// [`Self::current`] can differ from `rejected`, i.e. whether a
    /// retry is worth sending.
    fn invalidate(&self, rejected: &ApnsProviderJwt) -> bool;
}

#[derive(Serialize)]
struct ProviderClaims<'a> {
    iss: &'a str,
    iat: u64,
}

struct CachedToken {
    jwt: ApnsProviderJwt,
    issued_at: u64,
}

/// In-process ES256 signer with a single cached token.
pub struct CachingApnsTokenSigner {
    team_id: ApnsTeamId,
    key_id: ApnsKeyId,
    encoding_key: EncodingKey,
    clock: Arc<dyn ApnsClock>,
    cache: Mutex<Option<CachedToken>>,
}

impl CachingApnsTokenSigner {
    /// Load the `.p8` key (PKCS#8 PEM, P-256) once. The PEM is checked
    /// to be a P-256 key before it is handed to the ES256 encoder.
    pub fn from_pkcs8_pem(
        team_id: ApnsTeamId,
        key_id: ApnsKeyId,
        pem: &str,
        clock: Arc<dyn ApnsClock>,
    ) -> Result<Self, ApnsKeyError> {
        p256::SecretKey::from_pkcs8_pem(pem).map_err(|_| ApnsKeyError::NotP256Pkcs8)?;
        let encoding_key =
            EncodingKey::from_ec_pem(pem.as_bytes()).map_err(ApnsKeyError::Encoding)?;
        Ok(Self {
            team_id,
            key_id,
            encoding_key,
            clock,
            cache: Mutex::new(None),
        })
    }

    /// Every cached entry is independent of any panicking writer, so a
    /// poisoned lock is recovered rather than disabling the cache.
    fn lock_cache(&self) -> MutexGuard<'_, Option<CachedToken>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn sign(&self, issued_at: u64) -> Result<ApnsProviderJwt, ApnsSignError> {
        let mut header = Header::new(Algorithm::ES256);
        header.typ = None;
        header.kid = Some(self.key_id.as_str().to_owned());
        let claims = ProviderClaims {
            iss: self.team_id.as_str(),
            iat: issued_at,
        };
        jsonwebtoken::encode(&header, &claims, &self.encoding_key)
            .map(ApnsProviderJwt)
            .map_err(ApnsSignError::Signing)
    }
}

impl std::fmt::Debug for CachingApnsTokenSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachingApnsTokenSigner")
            .field("team_id", &self.team_id)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl ApnsProviderTokenSource for CachingApnsTokenSigner {
    fn current(&self) -> Result<ApnsProviderJwt, ApnsSignError> {
        let now = self.clock.now_unix_seconds();
        // Mint under the lock so concurrent callers share one refresh.
        let mut cache = self.lock_cache();
        if let Some(cached) = cache.as_ref() {
            if now.saturating_sub(cached.issued_at) < APNS_PROVIDER_TOKEN_REUSE.as_secs() {
                return Ok(cached.jwt.clone());
            }
        }
        let jwt = self.sign(now)?;
        *cache = Some(CachedToken {
            jwt: jwt.clone(),
            issued_at: now,
        });
        Ok(jwt)
    }

    fn invalidate(&self, rejected: &ApnsProviderJwt) -> bool {
        let now = self.clock.now_unix_seconds();
        let mut cache = self.lock_cache();
        let Some(cached) = cache.as_ref() else {
            return true;
        };
        if &cached.jwt != rejected {
            // Already replaced by a concurrent refresh.
            return true;
        }
        if now.saturating_sub(cached.issued_at) < APNS_PROVIDER_TOKEN_MIN_REFRESH.as_secs() {
            return false;
        }
        *cache = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use jsonwebtoken::{DecodingKey, Validation};
    use p256::elliptic_curve::rand_core::OsRng;
    use p256::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use serde::Deserialize;

    use super::*;

    const START: u64 = 1_700_000_000;

    struct ManualClock(AtomicU64);

    impl ManualClock {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_secs(), Ordering::SeqCst);
        }
    }

    impl ApnsClock for ManualClock {
        fn now_unix_seconds(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[derive(Deserialize)]
    struct DecodedClaims {
        iss: String,
        iat: u64,
    }

    fn signer() -> (CachingApnsTokenSigner, Arc<ManualClock>, DecodingKey) {
        let secret = p256::SecretKey::random(&mut OsRng);
        let pem = secret.to_pkcs8_pem(Default::default()).expect("pkcs8 pem");
        let public_pem = secret
            .public_key()
            .to_public_key_pem(Default::default())
            .expect("public pem");
        let clock = Arc::new(ManualClock(AtomicU64::new(START)));
        let signer = CachingApnsTokenSigner::from_pkcs8_pem(
            ApnsTeamId::parse("TEAM123456").expect("team"),
            ApnsKeyId::parse("KEY1234567").expect("key"),
            &pem,
            clock.clone(),
        )
        .expect("signer");
        let decoding = DecodingKey::from_ec_pem(public_pem.as_bytes()).expect("decoding key");
        (signer, clock, decoding)
    }

    fn decode(jwt: &ApnsProviderJwt, key: &DecodingKey) -> DecodedClaims {
        let mut validation = Validation::new(Algorithm::ES256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        jsonwebtoken::decode::<DecodedClaims>(jwt.as_str(), key, &validation)
            .expect("valid ES256 signature")
            .claims
    }

    #[test]
    fn token_has_apple_header_and_claims() {
        let (signer, _clock, key) = signer();
        let jwt = signer.current().expect("token");
        let header = jsonwebtoken::decode_header(jwt.as_str()).expect("header");
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("KEY1234567"));
        assert_eq!(header.typ, None);
        let claims = decode(&jwt, &key);
        assert_eq!(claims.iss, "TEAM123456");
        assert_eq!(claims.iat, START);
    }

    #[test]
    fn token_is_reused_for_just_under_fifty_minutes() {
        let (signer, clock, _key) = signer();
        let first = signer.current().expect("first");
        clock.advance(APNS_PROVIDER_TOKEN_REUSE - Duration::from_secs(1));
        assert_eq!(signer.current().expect("cached"), first);
    }

    #[test]
    fn token_is_refreshed_after_fifty_minutes() {
        let (signer, clock, key) = signer();
        let first = signer.current().expect("first");
        clock.advance(APNS_PROVIDER_TOKEN_REUSE);
        let second = signer.current().expect("refreshed");
        assert_ne!(second, first);
        assert_eq!(
            decode(&second, &key).iat,
            START + APNS_PROVIDER_TOKEN_REUSE.as_secs()
        );
    }

    #[test]
    fn invalidate_forces_a_fresh_token() {
        let (signer, clock, key) = signer();
        let first = signer.current().expect("first");
        clock.advance(APNS_PROVIDER_TOKEN_MIN_REFRESH);
        assert!(signer.invalidate(&first));
        let second = signer.current().expect("fresh");
        assert_ne!(second, first);
        assert_eq!(
            decode(&second, &key).iat,
            START + APNS_PROVIDER_TOKEN_MIN_REFRESH.as_secs()
        );
    }

    #[test]
    fn a_rejected_young_token_is_kept() {
        let (signer, clock, _key) = signer();
        let first = signer.current().expect("first");
        clock.advance(Duration::from_secs(60));
        // Apple throttles refreshes inside 20 minutes: re-minting would
        // only earn TooManyProviderTokenUpdates.
        assert!(!signer.invalidate(&first));
        assert_eq!(signer.current().expect("same"), first);
    }

    #[test]
    fn invalidating_a_stale_token_keeps_the_newer_one() {
        let (signer, clock, _key) = signer();
        let first = signer.current().expect("first");
        clock.advance(APNS_PROVIDER_TOKEN_MIN_REFRESH);
        assert!(signer.invalidate(&first));
        let second = signer.current().expect("second");
        // A late 403 for the already-replaced token must not throw
        // away the fresh one (Apple throttles token refreshes).
        assert!(signer.invalidate(&first));
        assert_eq!(signer.current().expect("still second"), second);
    }

    #[test]
    fn rejects_a_non_p256_key() {
        let clock: Arc<dyn ApnsClock> = Arc::new(SystemApnsClock);
        let err = CachingApnsTokenSigner::from_pkcs8_pem(
            ApnsTeamId::parse("TEAM123456").expect("team"),
            ApnsKeyId::parse("KEY1234567").expect("key"),
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
            clock,
        )
        .expect_err("garbage key");
        assert!(matches!(err, ApnsKeyError::NotP256Pkcs8));
    }
}
