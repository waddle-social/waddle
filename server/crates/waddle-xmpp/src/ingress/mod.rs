//! Typed ingress identity domain's dark substrate; see issues #1650 and #1635
//! for the broader identity-domain design.

mod alias;
pub mod digest;
mod effect_intent;
mod epoch;
mod error;
mod generation;
mod keys;
mod ordinal;
mod stream;
mod target;

/// Maximum UTF-8 byte length of an XEP-0359 origin-id accepted by ingress.
///
/// This is shared by the ingress validator and the Postgres schema contract.
pub const MAX_ORIGIN_ID_BYTES: usize = 1024;

pub use alias::{resolve_alias, AliasConflict, AliasOutcome, AliasResolution, StoredAlias};
pub use digest::{DigestContext, DigestInput, DigestInputError, DigestVersion, SemanticDigest};
pub use effect_intent::{
    EffectIntentCodecError, EncodedEffectIntent, FrozenStanzaError, FrozenStanzaErrorAddress,
    FrozenStanzaErrorConditionPayload, FrozenStanzaErrorText, FrozenStanzaErrorTexts,
    FrozenStanzaErrorType, IngressEffectIntent, IngressEffectKey, RecipientSmAppendIdentity,
    RelayNodeEpoch, RelayNodeId, RelayTargetIdentity, MAX_EFFECT_INTENT_PAYLOAD_BYTES,
};
pub use epoch::ProtocolEpoch;
pub use error::IngressTypeError;
pub use generation::{ConnectionGeneration, EntityGeneration, RowRevision};
pub use keys::{DeliveryKey, MessageKey};
pub use ordinal::IngressOrdinal;
pub use stream::{IngressStreamId, SmIngressId};
pub use target::{NormalizedTarget, NormalizedTargetStorage};
