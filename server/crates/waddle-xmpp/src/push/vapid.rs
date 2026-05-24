//! RFC 8292 VAPID — ES256 JWT signing + per-`(Kid, Origin)` LRU cache.
//!
//! ## Invariants
//!
//! - JWT header is `{"typ":"JWT","alg":"ES256"}`; **no `kid` field** (VAPID forbids it).
//! - `aud` = scheme + host + optional non-default port; **no path** (RFC 8292 §2 + RFC 6454).
//! - `k=` in the `Authorization` header is base64url-no-pad of the
//!   uncompressed 65-byte P-256 point starting `0x04` — NOT the SPKI/DER form.
//! - `jti` is random per signed JWT (RFC 7519 §4.1.7). With JWT caching, the
//!   same `jti` is reused for cache-lifetime; **no replay-narrowing claim**
//!   attached — kept for spec conformance.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use lru::LruCache;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use url::Url;

use super::constants::{CACHE_EVICT_MARGIN, IAT_LAG, VAPID_JWT_CACHE_CAPACITY, VAPID_JWT_LIFETIME};
use super::types::{Kid, VapidJwt, VapidSignError, VapidSub, WebPushCryptoError};

/// Cross-crate boundary trait. Pure-crypto callers ask the trait for a
/// signed JWT; the implementation in `waddle-server/src/push_service/`
/// owns the durable key material.
pub trait VapidSigner: Send + Sync {
    fn sign(&self, aud: &url::Origin, sub: &VapidSub) -> Result<VapidJwt, VapidSignError>;

    /// Current public key (uncompressed P-256 point material) for the
    /// `Authorization: vapid k=…` header.
    fn current_public_key(&self) -> p256::PublicKey;

    /// Current `kid` (cache key component for the JWT cache + the disco-form
    /// `kid` field for chat-side rotation detection).
    fn current_kid(&self) -> Kid;
}

/// VAPID JWT claims body per RFC 8292 §2.
#[derive(Debug, Serialize, Deserialize)]
struct VapidClaims {
    aud: String,
    exp: u64,
    sub: String,
    iat: u64,
    jti: String,
}

/// Resolve the `aud` claim from a subscription endpoint per RFC 8292 §2 +
/// RFC 6454: scheme + host + optional non-default port. **No path.**
/// Rejects opaque origins and non-`https` schemes.
pub fn aud_for(endpoint: &Url) -> Result<url::Origin, WebPushCryptoError> {
    let origin = endpoint.origin();
    if !matches!(&origin, url::Origin::Tuple(scheme, _, _) if scheme == "https") {
        return Err(WebPushCryptoError::InvalidEndpoint(format!(
            "endpoint origin not https-tuple: {:?}",
            origin
        )));
    }
    Ok(origin)
}

/// `k=` value for the `Authorization: vapid t=<jwt>, k=<...>` header.
/// Base64url-no-pad of the **uncompressed 65-byte P-256 point starting `0x04`**.
/// Asserts the length/prefix invariant before encoding.
pub fn vapid_k_header(public_key: &p256::PublicKey) -> String {
    let encoded = public_key.to_encoded_point(/* compress= */ false);
    let bytes = encoded.as_bytes();
    debug_assert_eq!(bytes.len(), 65, "P-256 uncompressed point must be 65 bytes");
    debug_assert_eq!(bytes[0], 0x04, "uncompressed-point prefix must be 0x04");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// In-process signer combining a `p256::SecretKey` private key + LRU JWT
/// cache keyed by `(Kid, Origin)`.
///
/// Wraps an `Arc<Mutex<LruCache>>` so concurrent senders share the cache.
/// `max_capacity = VAPID_JWT_CACHE_CAPACITY` (32) — comfortably exceeds
/// the legitimate browser-push origin set (~3), no origin allowlist.
pub struct InProcessVapidSigner {
    kid: Kid,
    public_key: p256::PublicKey,
    encoding_key: EncodingKey,
    cache: Arc<Mutex<LruCache<CacheKey, CachedJwt>>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    kid: Kid,
    /// Origin serialized once at key construction — `url::Origin` itself is
    /// `Hash`/`Eq`, but we cache the canonical ASCII form for cheap comparisons.
    origin_ascii: String,
    /// `sub` is part of the JWT claim set, so a cache hit MUST require an
    /// exact match — otherwise a caller varying `sub` for the same
    /// `(kid, origin)` would receive a stale-`sub` JWT.
    sub_claim: String,
}

#[derive(Clone)]
struct CachedJwt {
    jwt: VapidJwt,
    /// `exp` in seconds since UNIX_EPOCH. Cache hit only if
    /// `now() + CACHE_EVICT_MARGIN < exp`.
    exp_unix_seconds: u64,
}

impl InProcessVapidSigner {
    /// Constructs a signer from a P-256 keypair. The raw `secret_key`
    /// is consumed: its public point is captured and the scalar is
    /// dropped (`p256::SecretKey` zeroizes on drop), leaving only the
    /// `EncodingKey` (PKCS#8 PEM bytes) as the residual sign-capable
    /// material in heap.
    pub fn new(kid: Kid, secret_key: p256::SecretKey) -> Result<Self, VapidSignError> {
        use p256::pkcs8::EncodePrivateKey;
        let public_key = secret_key.public_key();
        let pem = secret_key
            .to_pkcs8_pem(Default::default())
            .map_err(|e| VapidSignError::Storage(format!("encode PKCS#8 PEM: {e}")))?;
        let encoding_key =
            EncodingKey::from_ec_pem(pem.as_bytes()).map_err(VapidSignError::Signing)?;
        // `secret_key` drops here (zeroized by p256). `pem` is `Zeroizing<String>`
        // and also zeroizes here. Only `encoding_key` retains key material.
        drop(secret_key);
        Ok(Self {
            kid,
            public_key,
            encoding_key,
            cache: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(VAPID_JWT_CACHE_CAPACITY).expect("non-zero capacity"),
            ))),
        })
    }

    /// Drop every cached JWT — used on VAPID key rotation so stale
    /// `(old_kid, *)` entries don't linger until LRU pressure evicts them.
    pub fn invalidate_all(&self) {
        let mut cache = self.lock_cache_recovering();
        cache.clear();
    }

    /// Lock the cache, recovering from a poisoned mutex by extracting the
    /// inner guard. The cache's invariants don't depend on prior writers
    /// having completed successfully — every entry is independent — so
    /// poison recovery is safe and matches what `parking_lot::Mutex` would
    /// do natively. Without this recovery, a single panic anywhere in the
    /// signer would silently bypass the cache for the rest of the process
    /// lifetime, forcing unbounded re-signing.
    fn lock_cache_recovering(&self) -> std::sync::MutexGuard<'_, LruCache<CacheKey, CachedJwt>> {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn cache_key(&self, aud: &url::Origin, sub: &VapidSub) -> CacheKey {
        CacheKey {
            kid: self.kid,
            origin_ascii: origin_ascii(aud),
            sub_claim: sub.as_claim(),
        }
    }
}

impl std::fmt::Debug for InProcessVapidSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InProcessVapidSigner")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

impl VapidSigner for InProcessVapidSigner {
    fn sign(&self, aud: &url::Origin, sub: &VapidSub) -> Result<VapidJwt, VapidSignError> {
        let key = self.cache_key(aud, sub);
        let now = now_unix_seconds();
        // Cache hit path — short critical section, then return.
        {
            let mut cache = self.lock_cache_recovering();
            if let Some(entry) = cache.get(&key) {
                if entry.exp_unix_seconds > now + CACHE_EVICT_MARGIN.as_secs() {
                    return Ok(entry.jwt.clone());
                }
                // expired/about-to-expire: fall through to re-sign
                cache.pop(&key);
            }
        }

        // Sign a fresh JWT. `saturating_sub` guards against a mis-set system
        // clock that returns `now == 0` (or `< IAT_LAG`) from
        // `now_unix_seconds()`; underflow would panic in debug and wrap to a
        // huge timestamp in release, producing JWTs push services reject.
        let iat = now.saturating_sub(IAT_LAG.as_secs());
        let exp = iat.saturating_add(VAPID_JWT_LIFETIME.as_secs());
        let claims = VapidClaims {
            aud: origin_ascii(aud),
            exp,
            sub: sub.as_claim(),
            iat,
            jti: fresh_jti(),
        };
        let header = Header::new(Algorithm::ES256);
        let token = jsonwebtoken::encode(&header, &claims, &self.encoding_key)
            .map_err(VapidSignError::Signing)?;
        let jwt = VapidJwt::new(token);

        let mut cache = self.lock_cache_recovering();
        cache.put(
            key,
            CachedJwt {
                jwt: jwt.clone(),
                exp_unix_seconds: exp,
            },
        );
        drop(cache);
        Ok(jwt)
    }

    fn current_public_key(&self) -> p256::PublicKey {
        self.public_key
    }

    fn current_kid(&self) -> Kid {
        self.kid
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn fresh_jti() -> String {
    let bytes: [u8; 16] = {
        let mut b = [0u8; 16];
        rand::rng().fill(&mut b[..]);
        b
    };
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Canonical ASCII serialization of an `url::Origin`. Used in both the
/// cache key and as the `aud` claim string.
fn origin_ascii(origin: &url::Origin) -> String {
    origin.ascii_serialization()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::elliptic_curve::rand_core::OsRng;

    fn fresh_signer() -> InProcessVapidSigner {
        let secret = p256::SecretKey::random(&mut OsRng);
        InProcessVapidSigner::new(Kid::new(), secret).expect("signer init")
    }

    fn parse_origin(url: &str) -> url::Origin {
        Url::parse(url).expect("valid").origin()
    }

    #[test]
    fn aud_for_fcm_strips_path() {
        let endpoint = Url::parse("https://fcm.googleapis.com/fcm/send/abc-def").unwrap();
        let aud = aud_for(&endpoint).expect("valid");
        assert_eq!(origin_ascii(&aud), "https://fcm.googleapis.com");
    }

    #[test]
    fn aud_for_mozilla() {
        let endpoint =
            Url::parse("https://updates.push.services.mozilla.com/wpush/v2/abc").unwrap();
        let aud = aud_for(&endpoint).expect("valid");
        assert_eq!(
            origin_ascii(&aud),
            "https://updates.push.services.mozilla.com"
        );
    }

    #[test]
    fn aud_for_apple() {
        let endpoint = Url::parse("https://web.push.apple.com/abc").unwrap();
        let aud = aud_for(&endpoint).expect("valid");
        assert_eq!(origin_ascii(&aud), "https://web.push.apple.com");
    }

    #[test]
    fn aud_for_non_default_port() {
        let endpoint = Url::parse("https://example.com:8443/push/abc").unwrap();
        let aud = aud_for(&endpoint).expect("valid");
        assert_eq!(origin_ascii(&aud), "https://example.com:8443");
    }

    #[test]
    fn aud_for_rejects_http() {
        let endpoint = Url::parse("http://insecure.example.com/abc").unwrap();
        let err = aud_for(&endpoint).unwrap_err();
        assert!(matches!(err, WebPushCryptoError::InvalidEndpoint(_)));
    }

    #[test]
    fn aud_for_rejects_data_scheme() {
        let endpoint = Url::parse("data:text/plain,hello").unwrap();
        let err = aud_for(&endpoint).unwrap_err();
        assert!(matches!(err, WebPushCryptoError::InvalidEndpoint(_)));
    }

    #[test]
    fn vapid_k_header_is_65_byte_uncompressed_base64url() {
        let secret = p256::SecretKey::random(&mut OsRng);
        let pk = secret.public_key();
        let k = vapid_k_header(&pk);
        let decoded = URL_SAFE_NO_PAD.decode(&k).expect("base64url");
        assert_eq!(decoded.len(), 65);
        assert_eq!(decoded[0], 0x04);
    }

    #[test]
    fn vapid_k_header_no_padding() {
        let secret = p256::SecretKey::random(&mut OsRng);
        let k = vapid_k_header(&secret.public_key());
        assert!(!k.contains('='));
        assert!(!k.contains('+'));
        assert!(!k.contains('/'));
    }

    #[test]
    fn signer_signs_jwt_with_correct_claims() {
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt = signer.sign(&aud, &sub).expect("sign");
        // Parse the JWT (skip signature verify here; we have a separate
        // verification test below).
        let mut parts = jwt.as_str().split('.');
        let _header = parts.next().expect("header");
        let claims_b64 = parts.next().expect("claims");
        let claims_bytes = URL_SAFE_NO_PAD
            .decode(claims_b64)
            .expect("claims base64url");
        let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).expect("claims json");
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "mailto:postmaster@example.com");
        assert!(claims["jti"].is_string());
        assert!(claims["iat"].is_number());
        assert!(claims["exp"].is_number());
        let iat = claims["iat"].as_u64().unwrap();
        let exp = claims["exp"].as_u64().unwrap();
        assert_eq!(exp - iat, VAPID_JWT_LIFETIME.as_secs());
    }

    #[test]
    fn signer_jwt_header_has_no_kid_field() {
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt = signer.sign(&aud, &sub).expect("sign");
        let header_b64 = jwt.as_str().split('.').next().unwrap();
        let header_bytes = URL_SAFE_NO_PAD.decode(header_b64).expect("header");
        let header: serde_json::Value = serde_json::from_slice(&header_bytes).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["typ"], "JWT");
        assert!(
            header.get("kid").is_none(),
            "VAPID forbids `kid` in JWT header"
        );
    }

    #[test]
    fn signer_caches_per_origin() {
        let signer = fresh_signer();
        let aud_a = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let aud_b = parse_origin("https://fcm.googleapis.com/fcm/send/different");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt_1 = signer.sign(&aud_a, &sub).unwrap();
        let jwt_2 = signer.sign(&aud_b, &sub).unwrap();
        // Same origin (just different paths) → same cache key → identical JWT.
        assert_eq!(jwt_1, jwt_2);
    }

    #[test]
    fn signer_distinguishes_subs_within_same_origin() {
        // The JWT claim set embeds `sub`, so a caller that varies `sub` for
        // the same (kid, origin) MUST get a JWT with the new `sub` — not a
        // cached one bound to a previous `sub` value.
        use std::str::FromStr as _;
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub_a = VapidSub::default_for_domain("example.com").unwrap();
        let sub_b = VapidSub::from_str("mailto:ops@example.com").unwrap();
        let jwt_a = signer.sign(&aud, &sub_a).unwrap();
        let jwt_b = signer.sign(&aud, &sub_b).unwrap();
        assert_ne!(jwt_a, jwt_b, "cache key must include `sub`");
        // Each `sub` round-trips under itself.
        let jwt_a2 = signer.sign(&aud, &sub_a).unwrap();
        assert_eq!(jwt_a, jwt_a2);
    }

    #[test]
    fn signer_distinguishes_origins() {
        let signer = fresh_signer();
        let aud_fcm = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let aud_moz = parse_origin("https://updates.push.services.mozilla.com/wpush/v2/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt_fcm = signer.sign(&aud_fcm, &sub).unwrap();
        let jwt_moz = signer.sign(&aud_moz, &sub).unwrap();
        assert_ne!(jwt_fcm, jwt_moz);
    }

    #[test]
    fn invalidate_all_clears_cache() {
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt_a = signer.sign(&aud, &sub).unwrap();
        signer.invalidate_all();
        let jwt_b = signer.sign(&aud, &sub).unwrap();
        // After invalidate_all, a fresh signing produces a new jti (random)
        // — so the JWTs differ even though the cache key, claims (modulo jti),
        // and key are identical.
        assert_ne!(jwt_a, jwt_b);
    }

    #[test]
    fn signer_verifies_with_own_public_key() {
        use p256::pkcs8::EncodePublicKey;
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt = signer.sign(&aud, &sub).expect("sign");
        let pk_pem = signer
            .current_public_key()
            .to_public_key_pem(Default::default())
            .expect("public-key PEM");
        let decoding_key =
            jsonwebtoken::DecodingKey::from_ec_pem(pk_pem.as_bytes()).expect("decode key");
        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.set_audience(&["https://fcm.googleapis.com"]);
        let _ = jsonwebtoken::decode::<VapidClaims>(jwt.as_str(), &decoding_key, &validation)
            .expect("verifies under our own public key");
    }

    #[test]
    fn jti_is_unique_across_signed_jwts() {
        // jti within a cached JWT lifetime is reused (documented). To verify
        // RFC 7519 conformance (random 128-bit), invalidate the cache
        // between signs.
        let signer = fresh_signer();
        let aud = parse_origin("https://fcm.googleapis.com/fcm/send/abc");
        let sub = VapidSub::default_for_domain("example.com").unwrap();
        let jwt_a = signer.sign(&aud, &sub).unwrap();
        signer.invalidate_all();
        let jwt_b = signer.sign(&aud, &sub).unwrap();
        let extract_jti = |jwt: &VapidJwt| -> String {
            let parts: Vec<&str> = jwt.as_str().split('.').collect();
            let claims_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
            let claims: serde_json::Value = serde_json::from_slice(&claims_bytes).unwrap();
            claims["jti"].as_str().unwrap().to_string()
        };
        assert_ne!(extract_jti(&jwt_a), extract_jti(&jwt_b));
    }
}
