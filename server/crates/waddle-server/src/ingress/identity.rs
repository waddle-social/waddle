//! Typed admission and canonical identities for the authority transaction.
use jid::BareJid;
use serde::{Deserialize, Serialize};
use waddle_xmpp::{
    auth::AuthenticatedPrincipalRef,
    ingress::{MessageKey, SmIngressId, WireHandledCount},
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
