//! Classification of the origin of one retained SM frame.
//!
//! Provenance is a claim, not authority to settle or suppress a delivery. It is
//! separate from the receipt on the last frame of a completed response prefix.

use super::{SmIngressAppendKey, SmIngressReceiptKind};
use crate::ingress::MessageKey;
use serde::{de, ser::SerializeSeq, Deserialize, Deserializer, Serialize, Serializer};

/// Maximum encoded provenance and number of claims retained for a single frame.
pub const MAX_SM_PROVENANCE_BYTES: usize = 16 * 1024;
pub const MAX_SM_PROVENANCE_CLAIMS: usize = 32;
const HEADER: &[u8; 4] = b"SMP\x01";
const MAX_JID_BYTES: usize = 3071;

/// A protocol stanza and the provenance of that exact frame.
#[derive(Debug, Clone)]
pub struct SmProvenancedStanza {
    pub stanza: crate::Stanza,
    pub provenance: SmFrameProvenance,
}

impl std::ops::Deref for SmProvenancedStanza {
    type Target = crate::Stanza;

    fn deref(&self) -> &Self::Target {
        &self.stanza
    }
}

impl From<crate::Stanza> for SmProvenancedStanza {
    fn from(stanza: crate::Stanza) -> Self {
        Self {
            stanza,
            provenance: SmFrameProvenance::Unknown,
        }
    }
}

/// Uncertainty dominates an incomplete or invalid association; it never means
/// that the stanza has no ingress origin.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SmFrameProvenance {
    #[default]
    Unknown,
    /// Asserted only by a producer that knows this frame has no ingress origin.
    NonIngress,
    Ingress(SmFrameIngressClaims),
}

/// An all-or-nothing set of exact resource claims.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmFrameIngressClaims(Vec<SmIngressAppendKey>);

impl SmFrameProvenance {
    /// Create complete claims or preserve uncertainty; never truncate a set.
    pub fn ingress(claims: impl IntoIterator<Item = SmIngressAppendKey>) -> Self {
        let mut retained: Vec<SmIngressAppendKey> = Vec::new();
        let mut size = 6usize;
        for claim in claims {
            if retained.len() == MAX_SM_PROVENANCE_CLAIMS
                || retained
                    .first()
                    .is_some_and(|first| first.resource != claim.resource)
                || retained.contains(&claim)
            {
                return Self::Unknown;
            }
            let jid_len = claim.resource.to_string().len();
            if jid_len == 0 || jid_len > MAX_JID_BYTES {
                return Self::Unknown;
            }
            let Some(next_size) = size.checked_add(54).and_then(|n| n.checked_add(jid_len)) else {
                return Self::Unknown;
            };
            if next_size > MAX_SM_PROVENANCE_BYTES {
                return Self::Unknown;
            }
            size = next_size;
            retained.push(claim);
        }
        if retained.is_empty() {
            Self::Unknown
        } else {
            Self::Ingress(SmFrameIngressClaims(retained))
        }
    }

    pub fn claims(&self) -> Option<&[SmIngressAppendKey]> {
        match self {
            Self::Ingress(claims) => Some(&claims.0),
            Self::Unknown | Self::NonIngress => None,
        }
    }

    /// Encode the independent versioned format used by storage and relay.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(HEADER);
        match self {
            Self::Unknown => bytes.push(0),
            Self::NonIngress => bytes.push(1),
            Self::Ingress(claims) => {
                bytes.push(2);
                bytes.push(claims.0.len() as u8);
                for claim in &claims.0 {
                    bytes.extend_from_slice(claim.message_key.to_storage().as_bytes());
                    bytes.extend_from_slice(&claim.kind.to_storage().to_be_bytes());
                    bytes.extend_from_slice(&claim.semantic_identity_hash);
                    let resource = claim.resource.to_string();
                    bytes.extend_from_slice(&(resource.len() as u16).to_be_bytes());
                    bytes.extend_from_slice(resource.as_bytes());
                }
            }
        }
        bytes
    }

    /// A missing, malformed, unsupported or excessive value retains the frame
    /// as unknown; it must never make ordinary SM resumption fail.
    pub fn decode(bytes: &[u8]) -> Self {
        if bytes.len() > MAX_SM_PROVENANCE_BYTES || !bytes.starts_with(HEADER) {
            return Self::Unknown;
        }
        let Some((&tag, mut rest)) = bytes.get(4..).and_then(|v| v.split_first()) else {
            return Self::Unknown;
        };
        match tag {
            0 if rest.is_empty() => Self::Unknown,
            1 if rest.is_empty() => Self::NonIngress,
            2 => {
                let Some((&count, tail)) = rest.split_first() else {
                    return Self::Unknown;
                };
                if count == 0 || usize::from(count) > MAX_SM_PROVENANCE_CLAIMS {
                    return Self::Unknown;
                }
                rest = tail;
                let mut claims = Vec::with_capacity(usize::from(count));
                for _ in 0..count {
                    if rest.len() < 54 {
                        return Self::Unknown;
                    }
                    let Ok(message) = uuid::Uuid::from_slice(&rest[..16]) else {
                        return Self::Unknown;
                    };
                    let kind = i32::from_be_bytes([rest[16], rest[17], rest[18], rest[19]]);
                    let mut hash = [0; 32];
                    hash.copy_from_slice(&rest[20..52]);
                    let len = usize::from(u16::from_be_bytes([rest[52], rest[53]]));
                    rest = &rest[54..];
                    if len == 0 || len > MAX_JID_BYTES || rest.len() < len {
                        return Self::Unknown;
                    }
                    let Ok(resource) = std::str::from_utf8(&rest[..len])
                        .ok()
                        .and_then(|jid| jid.parse().ok())
                        .ok_or(())
                    else {
                        return Self::Unknown;
                    };
                    rest = &rest[len..];
                    claims.push(SmIngressAppendKey {
                        message_key: MessageKey::from_storage(message),
                        kind: SmIngressReceiptKind::from_storage(kind),
                        semantic_identity_hash: hash,
                        resource,
                    });
                }
                if rest.is_empty() {
                    Self::ingress(claims)
                } else {
                    Self::Unknown
                }
            }
            _ => Self::Unknown,
        }
    }
}

impl Serialize for SmFrameProvenance {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bytes = self.encode();
        let mut sequence = serializer.serialize_seq(Some(bytes.len()))?;
        for byte in bytes {
            sequence.serialize_element(&byte)?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for SmFrameProvenance {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Preserve well-formed but invalid metadata as Unknown while consuming
        // it, rather than failing a containing frame's deserialization.
        struct Byte(Option<u8>);
        impl<'de> Deserialize<'de> for Byte {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct ByteVisitor;
                impl<'de> de::Visitor<'de> for ByteVisitor {
                    type Value = Byte;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a provenance byte")
                    }
                    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Byte, E> {
                        Ok(Byte(u8::try_from(value).ok()))
                    }
                    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Byte, E> {
                        Ok(Byte(u8::try_from(value).ok()))
                    }
                    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_unit<E: de::Error>(self) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_none<E: de::Error>(self) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_str<E: de::Error>(self, _: &str) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Byte, E> {
                        Ok(Byte(None))
                    }
                    fn visit_seq<A: de::SeqAccess<'de>>(
                        self,
                        mut seq: A,
                    ) -> Result<Byte, A::Error> {
                        while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                        Ok(Byte(None))
                    }
                    fn visit_map<A: de::MapAccess<'de>>(
                        self,
                        mut map: A,
                    ) -> Result<Byte, A::Error> {
                        while map
                            .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
                            .is_some()
                        {}
                        Ok(Byte(None))
                    }
                }
                deserializer.deserialize_any(ByteVisitor)
            }
        }
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = SmFrameProvenance;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("bounded SM frame provenance bytes")
            }
            fn visit_bytes<E: de::Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::decode(bytes))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map
                    .next_entry::<de::IgnoredAny, de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(SmFrameProvenance::Unknown)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut bytes = Vec::new();
                let mut invalid = false;
                // Continue consuming an oversized sequence without allocating it;
                // hostile input can still consume CPU until its input ends.
                while let Some(Byte(byte)) = seq.next_element::<Byte>()? {
                    match byte {
                        Some(byte) if bytes.len() < MAX_SM_PROVENANCE_BYTES => bytes.push(byte),
                        _ => invalid = true,
                    }
                }
                Ok(if invalid {
                    SmFrameProvenance::Unknown
                } else {
                    SmFrameProvenance::decode(&bytes)
                })
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(resource: &str) -> SmIngressAppendKey {
        SmIngressAppendKey {
            message_key: MessageKey::new(),
            kind: SmIngressReceiptKind::from_storage(3),
            semantic_identity_hash: [7; 32],
            resource: resource.parse().expect("full JID"),
        }
    }

    #[test]
    fn exact_resources_round_trip_without_becoming_receipts() {
        let first = claim("user@example.org/one");
        let mut second = first.clone();
        second.resource = "user@example.org/two".parse().expect("full JID");
        let one = SmFrameProvenance::ingress([first.clone()]);
        let two = SmFrameProvenance::ingress([second.clone()]);
        assert_ne!(one, two);
        for expected in [&one, &two] {
            assert_eq!(&SmFrameProvenance::decode(&expected.encode()), expected);
            let encoded = serde_json::to_vec(expected).expect("serialize");
            assert_eq!(
                &serde_json::from_slice::<SmFrameProvenance>(&encoded).expect("deserialize"),
                expected
            );
        }
        let mut distinct = first.clone();
        distinct.semantic_identity_hash = [8; 32];
        let same_resource = SmFrameProvenance::ingress([first.clone(), distinct]);
        assert_eq!(
            SmFrameProvenance::decode(&same_resource.encode()),
            same_resource
        );
        assert_eq!(
            SmFrameProvenance::ingress([first.clone(), first.clone()]),
            SmFrameProvenance::Unknown
        );
        assert_eq!(
            SmFrameProvenance::ingress([first, second]),
            SmFrameProvenance::Unknown
        );
        let mut mixed = one.encode();
        mixed[5] = 2;
        mixed.extend_from_slice(&two.encode()[6..]);
        assert_eq!(
            SmFrameProvenance::decode(&mixed),
            SmFrameProvenance::Unknown
        );
        let mut duplicate = one.encode();
        duplicate[5] = 2;
        duplicate.extend_from_slice(&one.encode()[6..]);
        assert_eq!(
            SmFrameProvenance::decode(&duplicate),
            SmFrameProvenance::Unknown
        );
        assert_eq!(
            SmFrameProvenance::decode(&SmFrameProvenance::NonIngress.encode()),
            SmFrameProvenance::NonIngress
        );
    }

    #[test]
    fn incomplete_excessive_or_invalid_claims_are_unknown() {
        let encoded = SmFrameProvenance::ingress([claim("user@example.org/one")]).encode();
        for end in 0..encoded.len() {
            assert_eq!(
                SmFrameProvenance::decode(&encoded[..end]),
                SmFrameProvenance::Unknown
            );
        }
        let mut invalid = encoded.clone();
        invalid[3] = 9;
        assert_eq!(
            SmFrameProvenance::decode(&invalid),
            SmFrameProvenance::Unknown
        );
        invalid = encoded.clone();
        invalid[5] = 2;
        assert_eq!(
            SmFrameProvenance::decode(&invalid),
            SmFrameProvenance::Unknown
        );
        invalid = encoded;
        let last = invalid.len() - 1;
        invalid[last] = 0xff;
        assert_eq!(
            SmFrameProvenance::decode(&invalid),
            SmFrameProvenance::Unknown
        );
        assert_eq!(
            SmFrameProvenance::decode(&vec![0; MAX_SM_PROVENANCE_BYTES + 1]),
            SmFrameProvenance::Unknown
        );
        assert_eq!(
            SmFrameProvenance::ingress(std::iter::repeat_n(
                claim("user@example.org/one"),
                MAX_SM_PROVENANCE_CLAIMS + 1
            )),
            SmFrameProvenance::Unknown
        );
        assert_eq!(SmFrameProvenance::ingress([]), SmFrameProvenance::Unknown);
        let oversized =
            serde_json::to_vec(&vec![0u8; MAX_SM_PROVENANCE_BYTES + 1]).expect("serialize");
        assert_eq!(
            serde_json::from_slice::<SmFrameProvenance>(&oversized).expect("deserialize"),
            SmFrameProvenance::Unknown
        );
        for invalid in ["null", "{}", "true", "[83,77,80,1,0,\"bad\"]"] {
            assert_eq!(
                serde_json::from_str::<SmFrameProvenance>(invalid).expect("invalid value"),
                SmFrameProvenance::Unknown
            );
        }
    }
}
