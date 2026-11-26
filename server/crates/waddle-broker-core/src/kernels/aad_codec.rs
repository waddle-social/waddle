//! Length-prefixed AAD encoder for broker envelope encryption.
//!
//! Formally verified for injectivity (`encode(a) == encode(b) ==> a == b`)
//! and overflow-free length encoding in `server/specs/verus/aad_codec.rs`.

use thiserror::Error;

/// Canonical fields bound into every credential envelope's AEAD Additional
/// Authenticated Data (AAD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AadFields<'a> {
    pub trust_domain: &'a [u8],
    pub tenant_id: &'a [u8],
    pub account_id: &'a [u8],
    pub provider: &'a [u8],
    pub credential_id: &'a [u8],
    pub version: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AadCodecError {
    #[error("AAD field length exceeds u32::MAX")]
    FieldTooLong,
    #[error("total encoded AAD size overflows usize")]
    TotalLengthOverflow,
}

fn push_length_prefixed(out: &mut Vec<u8>, field: &[u8]) -> Result<(), AadCodecError> {
    let len = u32::try_from(field.len()).map_err(|_| AadCodecError::FieldTooLong)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}

/// Deterministically encodes all 6 AAD fields with big-endian `u32` length
/// prefixes so no boundary ambiguity or cross-field concatenation collision
/// can exist.
pub fn encode_aad(fields: &AadFields<'_>) -> Result<Vec<u8>, AadCodecError> {
    let parts = [
        fields.trust_domain,
        fields.tenant_id,
        fields.account_id,
        fields.provider,
        fields.credential_id,
        fields.version,
    ];

    let mut total_len: usize = 0;
    for part in parts {
        let _ = u32::try_from(part.len()).map_err(|_| AadCodecError::FieldTooLong)?;
        total_len = total_len
            .checked_add(4)
            .and_then(|n| n.checked_add(part.len()))
            .ok_or(AadCodecError::TotalLengthOverflow)?;
    }

    let mut out = Vec::with_capacity(total_len);
    for part in parts {
        push_length_prefixed(&mut out, part)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::{any, prop_assert_eq, proptest};

    use super::*;

    #[test]
    fn concatenation_ambiguity_is_impossible() {
        let a = AadFields {
            trust_domain: b"ab",
            tenant_id: b"c",
            account_id: b"acct",
            provider: b"openai",
            credential_id: b"cred1",
            version: b"v1",
        };
        let b = AadFields {
            trust_domain: b"a",
            tenant_id: b"bc",
            account_id: b"acct",
            provider: b"openai",
            credential_id: b"cred1",
            version: b"v1",
        };
        let enc_a = encode_aad(&a).expect("encode a");
        let enc_b = encode_aad(&b).expect("encode b");
        assert_ne!(enc_a, enc_b);
    }

    proptest! {
        #[test]
        fn prop_encode_aad_is_injective(
            td1 in proptest::collection::vec(any::<u8>(), 0..16),
            t1 in proptest::collection::vec(any::<u8>(), 0..16),
            a1 in proptest::collection::vec(any::<u8>(), 0..16),
            p1 in proptest::collection::vec(any::<u8>(), 0..16),
            c1 in proptest::collection::vec(any::<u8>(), 0..16),
            v1 in proptest::collection::vec(any::<u8>(), 0..16),
            td2 in proptest::collection::vec(any::<u8>(), 0..16),
            t2 in proptest::collection::vec(any::<u8>(), 0..16),
            a2 in proptest::collection::vec(any::<u8>(), 0..16),
            p2 in proptest::collection::vec(any::<u8>(), 0..16),
            c2 in proptest::collection::vec(any::<u8>(), 0..16),
            v2 in proptest::collection::vec(any::<u8>(), 0..16),
        ) {
            let f1 = AadFields {
                trust_domain: &td1,
                tenant_id: &t1,
                account_id: &a1,
                provider: &p1,
                credential_id: &c1,
                version: &v1,
            };
            let f2 = AadFields {
                trust_domain: &td2,
                tenant_id: &t2,
                account_id: &a2,
                provider: &p2,
                credential_id: &c2,
                version: &v2,
            };
            let e1 = encode_aad(&f1).expect("encode f1");
            let e2 = encode_aad(&f2).expect("encode f2");
            if e1 == e2 {
                prop_assert_eq!(f1, f2);
            }
        }
    }
}
