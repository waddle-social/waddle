//! Typed admission and canonical identities for the authority transaction.
use jid::BareJid;
use serde::{Deserialize, Serialize};
use waddle_xmpp::{
    auth::AuthenticatedPrincipalRef,
    ingress::{IngressEffectKind, MessageKey, SmIngressId, WireHandledCount},
    pending_delivery::SmSessionId,
};
#[cfg(feature = "clustering")]
use waddle_xmpp::{
    muc::durable::RoomClaimFenceContext,
    ownership::{ClaimEpoch, NodeIdentity},
};
use waddle_xmpp_core::xep0359::OriginId;

#[derive(Clone, Debug)]
pub enum IngressStreamIdentity {
    Extension {
        plugin: waddle_extensions::PluginId,
        requester: Option<BareJid>,
    },
    Resumable {
        stream_id: SmSessionId,
        sm_ingress_id: SmIngressId,
        #[cfg(feature = "clustering")]
        owner: NodeIdentity,
        #[cfg(feature = "clustering")]
        claim_epoch: ClaimEpoch,
        reserved_wire_position: WireHandledCount,
        checkpoint_h: WireHandledCount,
    },
    Ephemeral {
        principal: AuthenticatedPrincipalRef,
    },
    Relayed {
        canonical: IngressCanonicalRef,
        room: BareJid,
        #[cfg(feature = "clustering")]
        room_fence: RoomClaimFenceContext,
    },
}

/// Identity carried to the room owner; serialization is confined to the relay boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressCanonicalRef {
    #[serde(with = "message_key_serde")]
    pub message_key: MessageKey,
    pub sender_bare: BareJid,
    pub origin_id: Option<OriginId>,
}

/// Recorded ingress obligation an append on another node discharges.
/// Serialization is confined to the relay boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressAppendObligationRef {
    pub message_key: MessageKey,
    /// Binds the obligation to an entity whose claim the relaying node must own.
    pub sender_bare: BareJid,
    pub receipt: super::EffectReceiptKey,
    pub received_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl IngressAppendObligationRef {
    pub fn from_context(
        context: &crate::server::routes::interpret::SmIngressAppendContext,
        sender_bare: BareJid,
    ) -> Self {
        Self {
            message_key: context.message_key,
            sender_bare,
            receipt: context.receipt.clone(),
            received_at: context.received_at,
        }
    }

    pub fn into_context(self) -> crate::server::routes::interpret::SmIngressAppendContext {
        crate::server::routes::interpret::SmIngressAppendContext {
            message_key: self.message_key,
            receipt: self.receipt,
            received_at: self.received_at,
        }
    }

    /// Only recorded direct and MUC groupchat routes allocate keyed SM appends.
    pub fn kind_is_append_eligible(&self) -> bool {
        [
            IngressEffectKind::RouteDirect,
            IngressEffectKind::RouteMucGroupchat,
        ]
        .into_iter()
        .any(|kind| self.receipt.kind.to_storage() == kind.storage_tag())
    }
}

/// Authenticated origin context propagated with a committed room proxy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressRelayAdmission {
    pub canonical: IngressCanonicalRef,
    pub principal: AuthenticatedPrincipalRef,
    /// Original top-level xml:lang, which the parsed Message does not retain.
    #[serde(with = "stanza_lang_serde")]
    pub stanza_lang: Option<xmpp_parsers::message::Lang>,
}

impl IngressRelayAdmission {
    pub fn from_parts(
        canonical: Option<IngressCanonicalRef>,
        principal: Option<AuthenticatedPrincipalRef>,
        stanza_lang: Option<xmpp_parsers::message::Lang>,
    ) -> Option<Self> {
        Some(Self {
            canonical: canonical?,
            principal: principal?,
            stanza_lang,
        })
    }
}

/// Language remains typed until encoding the relay envelope.
pub(crate) mod stanza_lang_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use xmpp_parsers::message::Lang;

    pub fn serialize<S: Serializer>(
        language: &Option<Lang>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        language
            .as_ref()
            .map(|language| language.0.as_str())
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Lang>, D::Error> {
        Option::<String>::deserialize(deserializer).map(|language| language.map(Lang))
    }
}

mod message_key_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use waddle_xmpp::ingress::MessageKey;
    pub fn serialize<S: Serializer>(key: &MessageKey, serializer: S) -> Result<S::Ok, S::Error> {
        key.to_storage().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<MessageKey, D::Error> {
        uuid::Uuid::deserialize(deserializer).map(MessageKey::from_storage)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_ref_round_trips_relay_identity() {
        let identity = IngressCanonicalRef {
            message_key: MessageKey::new(),
            sender_bare: "sender@example.test".parse().expect("sender"),
            origin_id: Some(OriginId::new("origin")),
        };
        let encoded = serde_json::to_vec(&identity).expect("encode");
        let decoded: IngressCanonicalRef = serde_json::from_slice(&encoded).expect("decode");
        assert_eq!(decoded, identity);
    }
}
