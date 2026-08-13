//! Immutable, typed descriptions of effects selected during ingress.

use std::cmp::Ordering;

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use waddle_xmpp_core::xep0359::StanzaId;

use crate::{
    error::StanzaErrorCondition, ingress::EntityGeneration, pending_delivery::SmSessionId,
};

/// Largest accepted version-one storage payload, matching the database check.
pub const MAX_EFFECT_INTENT_PAYLOAD_BYTES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelayTargetIdentity {
    pub node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_epoch: Option<String>,
}

impl RelayTargetIdentity {
    pub fn owner_node(node_id: impl Into<String>, node_epoch: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            node_epoch: Some(node_epoch.into()),
        }
    }

    pub fn relay_node(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            node_epoch: None,
        }
    }
}

/// A frozen effect decision; it carries no executable callback or mutable
/// lookup and can therefore be durably replayed without re-deriving policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressEffectIntent {
    ArchiveAuthoritative {
        archive: BareJid,
        stanza_id: StanzaId,
        by: BareJid,
    },
    RouteDirect {
        recipient: BareJid,
        fanout: Vec<FullJid>,
    },
    RouteMucGroupchat {
        room: BareJid,
        occupants: Vec<FullJid>,
        reflection: FullJid,
        room_generation: EntityGeneration,
    },
    RouteOccupantPm {
        recipient: FullJid,
        sender: FullJid,
    },
    DispatchToRoomRemote {
        room: BareJid,
        relay_target: RelayTargetIdentity,
    },
    RecipientSmAppend {
        stream: SmSessionId,
    },
    Carbons {
        carbon_recipients: Vec<FullJid>,
        excluded_source: FullJid,
    },
    InboxProject {
        owner: BareJid,
        increment_unread: bool,
    },
    NotificationActivityPreview {
        owner: BareJid,
    },
    CallSignal {
        recipient: FullJid,
        stanza_id: StanzaId,
    },
    Pin {
        room: BareJid,
        stanza_id: StanzaId,
    },
    Extension {
        recipient: BareJid,
        stanza_id: StanzaId,
    },
    ErrorReply {
        recipient: FullJid,
        condition: StanzaErrorCondition,
    },
}

/// Closed semantic identity used to deduplicate a stanza's frozen effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressEffectKey {
    ArchiveAuthoritative(BareJid),
    RouteDirect(BareJid),
    RouteMucGroupchat(BareJid),
    RouteOccupantPm(FullJid),
    DispatchToRoomRemote(BareJid, RelayTargetIdentity),
    RecipientSmAppend(SmSessionId),
    Carbons(FullJid),
    InboxProject(BareJid),
    NotificationActivityPreview(BareJid),
    CallSignal(FullJid),
    Pin(BareJid),
    Extension(BareJid),
    ErrorReply(FullJid),
}

impl IngressEffectKey {
    fn ordering_key(&self) -> (u8, String) {
        match self {
            Self::ArchiveAuthoritative(value) => (0, value.to_string()),
            Self::RouteDirect(value) => (1, value.to_string()),
            Self::RouteMucGroupchat(value) => (2, value.to_string()),
            Self::RouteOccupantPm(value) => (3, value.to_string()),
            Self::DispatchToRoomRemote(room, relay_target) => (
                4,
                format!(
                    "{}|{}|{}",
                    room,
                    relay_target.node_id,
                    relay_target.node_epoch.as_deref().unwrap_or("")
                ),
            ),
            Self::RecipientSmAppend(value) => (5, value.as_str().to_string()),
            Self::Carbons(value) => (6, value.to_string()),
            Self::InboxProject(value) => (7, value.to_string()),
            Self::NotificationActivityPreview(value) => (8, value.to_string()),
            Self::CallSignal(value) => (9, value.to_string()),
            Self::Pin(value) => (10, value.to_string()),
            Self::Extension(value) => (11, value.to_string()),
            Self::ErrorReply(value) => (12, value.to_string()),
        }
    }
}

impl Ord for IngressEffectKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordering_key().cmp(&other.ordering_key())
    }
}
impl PartialOrd for IngressEffectKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl IngressEffectIntent {
    pub fn semantic_key(&self) -> IngressEffectKey {
        match self {
            Self::ArchiveAuthoritative { archive, .. } => {
                IngressEffectKey::ArchiveAuthoritative(archive.clone())
            }
            Self::RouteDirect { recipient, .. } => IngressEffectKey::RouteDirect(recipient.clone()),
            Self::RouteMucGroupchat { room, .. } => {
                IngressEffectKey::RouteMucGroupchat(room.clone())
            }
            Self::RouteOccupantPm { recipient, .. } => {
                IngressEffectKey::RouteOccupantPm(recipient.clone())
            }
            Self::DispatchToRoomRemote { room, relay_target } => {
                IngressEffectKey::DispatchToRoomRemote(room.clone(), relay_target.clone())
            }
            Self::RecipientSmAppend { stream } => {
                IngressEffectKey::RecipientSmAppend(stream.clone())
            }
            Self::Carbons {
                excluded_source, ..
            } => IngressEffectKey::Carbons(excluded_source.clone()),
            Self::InboxProject { owner, .. } => IngressEffectKey::InboxProject(owner.clone()),
            Self::NotificationActivityPreview { owner, .. } => {
                IngressEffectKey::NotificationActivityPreview(owner.clone())
            }
            Self::CallSignal { recipient, .. } => IngressEffectKey::CallSignal(recipient.clone()),
            Self::Pin { room, .. } => IngressEffectKey::Pin(room.clone()),
            Self::Extension { recipient, .. } => IngressEffectKey::Extension(recipient.clone()),
            Self::ErrorReply { recipient, .. } => IngressEffectKey::ErrorReply(recipient.clone()),
        }
    }

    /// Encode the canonical V1 storage representation at the persistence edge.
    pub fn encode_v1(&self) -> Result<EncodedEffectIntent, EffectIntentCodecError> {
        let intent = StoredEffectIntent::from_domain(self.clone());
        let kind = intent.kind();
        let payload = serde_json::to_vec(&StoredPayload { version: 1, intent })
            .map_err(|_| EffectIntentCodecError::MalformedPayload)?;
        if payload.len() > MAX_EFFECT_INTENT_PAYLOAD_BYTES {
            return Err(EffectIntentCodecError::PayloadTooLarge);
        }
        Ok(EncodedEffectIntent { kind, payload })
    }

    /// Decode a canonical V1 storage representation and reject unknown tags.
    pub fn decode_v1(kind: i32, payload: &[u8]) -> Result<Self, EffectIntentCodecError> {
        if payload.len() > MAX_EFFECT_INTENT_PAYLOAD_BYTES {
            return Err(EffectIntentCodecError::PayloadTooLarge);
        }
        let stored: StoredPayload = serde_json::from_slice(payload)
            .map_err(|_| EffectIntentCodecError::MalformedPayload)?;
        if stored.version != 1 {
            return Err(EffectIntentCodecError::UnknownPayloadVersion(
                stored.version,
            ));
        }
        if stored.intent.kind() != kind {
            return Err(EffectIntentCodecError::UnknownKind(kind));
        }
        stored.intent.into_domain()
    }
}

/// Database-ready version-one payload and its closed table kind tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedEffectIntent {
    pub kind: i32,
    pub payload: Vec<u8>,
}

/// Codec failures intentionally exclude client values and payload bytes.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum EffectIntentCodecError {
    #[error("effect-intent payload exceeds its storage limit")]
    PayloadTooLarge,
    #[error("effect-intent payload is malformed")]
    MalformedPayload,
    #[error("effect-intent payload version is unsupported")]
    UnknownPayloadVersion(u8),
    #[error("effect-intent kind is unsupported")]
    UnknownKind(i32),
}

#[derive(Serialize, Deserialize)]
struct StoredPayload {
    version: u8,
    intent: StoredEffectIntent,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredEffectIntent {
    ArchiveAuthoritative {
        archive: BareJid,
        stanza_id: StanzaId,
        by: BareJid,
    },
    RouteDirect {
        recipient: BareJid,
        fanout: Vec<FullJid>,
    },
    RouteMucGroupchat {
        room: BareJid,
        occupants: Vec<FullJid>,
        reflection: FullJid,
        room_generation: u64,
    },
    RouteOccupantPm {
        recipient: FullJid,
        sender: FullJid,
    },
    DispatchToRoomRemote {
        room: BareJid,
        relay_target: RelayTargetIdentity,
    },
    RecipientSmAppend {
        stream: SmSessionId,
    },
    Carbons {
        carbon_recipients: Vec<FullJid>,
        excluded_source: FullJid,
    },
    InboxProject {
        owner: BareJid,
        increment_unread: bool,
    },
    NotificationActivityPreview {
        owner: BareJid,
    },
    CallSignal {
        recipient: FullJid,
        stanza_id: StanzaId,
    },
    Pin {
        room: BareJid,
        stanza_id: StanzaId,
    },
    Extension {
        recipient: BareJid,
        stanza_id: StanzaId,
    },
    ErrorReply {
        recipient: FullJid,
        condition: u8,
    },
}

impl StoredEffectIntent {
    fn kind(&self) -> i32 {
        match self {
            Self::ArchiveAuthoritative { .. } => 0,
            Self::RouteDirect { .. } => 1,
            Self::RouteMucGroupchat { .. } => 2,
            Self::RouteOccupantPm { .. } => 3,
            Self::DispatchToRoomRemote { .. } => 12,
            Self::RecipientSmAppend { .. } => 4,
            Self::Carbons { .. } => 5,
            Self::InboxProject { .. } => 6,
            Self::NotificationActivityPreview { .. } => 7,
            Self::CallSignal { .. } => 8,
            Self::Pin { .. } => 9,
            Self::Extension { .. } => 10,
            Self::ErrorReply { .. } => 11,
        }
    }

    fn from_domain(intent: IngressEffectIntent) -> Self {
        match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
            } => Self::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
            },
            IngressEffectIntent::RouteDirect {
                recipient,
                mut fanout,
            } => {
                canonicalize(&mut fanout);
                Self::RouteDirect { recipient, fanout }
            }
            IngressEffectIntent::RouteMucGroupchat {
                room,
                mut occupants,
                reflection,
                room_generation,
            } => {
                canonicalize(&mut occupants);
                Self::RouteMucGroupchat {
                    room,
                    occupants,
                    reflection,
                    room_generation: room_generation.to_storage(),
                }
            }
            IngressEffectIntent::RouteOccupantPm { recipient, sender } => {
                Self::RouteOccupantPm { recipient, sender }
            }
            IngressEffectIntent::DispatchToRoomRemote { room, relay_target } => {
                Self::DispatchToRoomRemote { room, relay_target }
            }
            IngressEffectIntent::RecipientSmAppend { stream } => Self::RecipientSmAppend { stream },
            IngressEffectIntent::Carbons {
                mut carbon_recipients,
                excluded_source,
            } => {
                canonicalize(&mut carbon_recipients);
                Self::Carbons {
                    carbon_recipients,
                    excluded_source,
                }
            }
            IngressEffectIntent::InboxProject {
                owner,
                increment_unread,
            } => Self::InboxProject {
                owner,
                increment_unread,
            },
            IngressEffectIntent::NotificationActivityPreview { owner } => {
                Self::NotificationActivityPreview { owner }
            }
            IngressEffectIntent::CallSignal {
                recipient,
                stanza_id,
            } => Self::CallSignal {
                recipient,
                stanza_id,
            },
            IngressEffectIntent::Pin { room, stanza_id } => Self::Pin { room, stanza_id },
            IngressEffectIntent::Extension {
                recipient,
                stanza_id,
            } => Self::Extension {
                recipient,
                stanza_id,
            },
            IngressEffectIntent::ErrorReply {
                recipient,
                condition,
            } => Self::ErrorReply {
                recipient,
                condition: condition_tag(condition),
            },
        }
    }

    fn into_domain(self) -> Result<IngressEffectIntent, EffectIntentCodecError> {
        Ok(match self {
            Self::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
            } => IngressEffectIntent::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
            },
            Self::RouteDirect { recipient, fanout } => {
                IngressEffectIntent::RouteDirect { recipient, fanout }
            }
            Self::RouteMucGroupchat {
                room,
                occupants,
                reflection,
                room_generation,
            } => IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants,
                reflection,
                room_generation: EntityGeneration::from_storage(room_generation),
            },
            Self::RouteOccupantPm { recipient, sender } => {
                IngressEffectIntent::RouteOccupantPm { recipient, sender }
            }
            Self::DispatchToRoomRemote { room, relay_target } => {
                IngressEffectIntent::DispatchToRoomRemote { room, relay_target }
            }
            Self::RecipientSmAppend { stream } => IngressEffectIntent::RecipientSmAppend { stream },
            Self::Carbons {
                carbon_recipients,
                excluded_source,
            } => IngressEffectIntent::Carbons {
                carbon_recipients,
                excluded_source,
            },
            Self::InboxProject {
                owner,
                increment_unread,
            } => IngressEffectIntent::InboxProject {
                owner,
                increment_unread,
            },
            Self::NotificationActivityPreview { owner } => {
                IngressEffectIntent::NotificationActivityPreview { owner }
            }
            Self::CallSignal {
                recipient,
                stanza_id,
            } => IngressEffectIntent::CallSignal {
                recipient,
                stanza_id,
            },
            Self::Pin { room, stanza_id } => IngressEffectIntent::Pin { room, stanza_id },
            Self::Extension {
                recipient,
                stanza_id,
            } => IngressEffectIntent::Extension {
                recipient,
                stanza_id,
            },
            Self::ErrorReply {
                recipient,
                condition,
            } => IngressEffectIntent::ErrorReply {
                recipient,
                condition: condition_from_tag(condition)?,
            },
        })
    }
}

fn canonicalize(values: &mut Vec<FullJid>) {
    values.sort_by_key(ToString::to_string);
    values.dedup();
}

fn condition_tag(condition: StanzaErrorCondition) -> u8 {
    use StanzaErrorCondition::*;
    match condition {
        BadRequest => 0,
        Conflict => 1,
        FeatureNotImplemented => 2,
        Forbidden => 3,
        Gone => 4,
        InternalServerError => 5,
        ItemNotFound => 6,
        JidMalformed => 7,
        NotAcceptable => 8,
        NotAllowed => 9,
        NotAuthorized => 10,
        PolicyViolation => 11,
        RecipientUnavailable => 12,
        Redirect => 13,
        RegistrationRequired => 14,
        RemoteServerNotFound => 15,
        RemoteServerTimeout => 16,
        ResourceConstraint => 17,
        ServiceUnavailable => 18,
        SubscriptionRequired => 19,
        UndefinedCondition => 20,
        UnexpectedRequest => 21,
    }
}
fn condition_from_tag(tag: u8) -> Result<StanzaErrorCondition, EffectIntentCodecError> {
    use StanzaErrorCondition::*;
    Ok(match tag {
        0 => BadRequest,
        1 => Conflict,
        2 => FeatureNotImplemented,
        3 => Forbidden,
        4 => Gone,
        5 => InternalServerError,
        6 => ItemNotFound,
        7 => JidMalformed,
        8 => NotAcceptable,
        9 => NotAllowed,
        10 => NotAuthorized,
        11 => PolicyViolation,
        12 => RecipientUnavailable,
        13 => Redirect,
        14 => RegistrationRequired,
        15 => RemoteServerNotFound,
        16 => RemoteServerTimeout,
        17 => ResourceConstraint,
        18 => ServiceUnavailable,
        19 => SubscriptionRequired,
        20 => UndefinedCondition,
        21 => UnexpectedRequest,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

#[cfg(test)]
mod tests {
    use jid::Jid;
    use waddle_xmpp_core::xep0359::StanzaId;

    use super::*;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare JID")
    }
    fn full(value: &str) -> FullJid {
        value.parse().expect("valid full JID")
    }
    fn stanza_id() -> StanzaId {
        StanzaId::new(
            "stable-1",
            "archive@example.test".parse::<Jid>().expect("valid JID"),
        )
    }

    fn samples() -> Vec<IngressEffectIntent> {
        vec![
            IngressEffectIntent::ArchiveAuthoritative {
                archive: bare("archive@example.test"),
                stanza_id: stanza_id(),
                by: bare("archive@example.test"),
            },
            IngressEffectIntent::RouteDirect {
                recipient: bare("romeo@example.test"),
                fanout: vec![full("romeo@example.test/phone")],
            },
            IngressEffectIntent::RouteMucGroupchat {
                room: bare("room@conference.example.test"),
                occupants: vec![full("juliet@example.test/laptop")],
                reflection: full("romeo@example.test/phone"),
                room_generation: EntityGeneration::from_storage(7),
            },
            IngressEffectIntent::RouteOccupantPm {
                recipient: full("juliet@example.test/laptop"),
                sender: full("romeo@example.test/phone"),
            },
            IngressEffectIntent::DispatchToRoomRemote {
                room: bare("room@conference.example.test"),
                relay_target: RelayTargetIdentity::owner_node("relay-node", "relay-epoch"),
            },
            IngressEffectIntent::RecipientSmAppend {
                stream: SmSessionId::new("stream-1"),
            },
            IngressEffectIntent::Carbons {
                carbon_recipients: vec![full("romeo@example.test/phone")],
                excluded_source: full("romeo@example.test/laptop"),
            },
            IngressEffectIntent::InboxProject {
                owner: bare("romeo@example.test"),
                increment_unread: true,
            },
            IngressEffectIntent::NotificationActivityPreview {
                owner: bare("romeo@example.test"),
            },
            IngressEffectIntent::CallSignal {
                recipient: full("romeo@example.test/phone"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::Pin {
                room: bare("room@conference.example.test"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::Extension {
                recipient: bare("romeo@example.test"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::ErrorReply {
                recipient: full("romeo@example.test/phone"),
                condition: StanzaErrorCondition::Forbidden,
            },
        ]
    }

    #[test]
    fn every_kind_round_trips_through_its_fixed_golden_vector() {
        let golden = [
            r#"{"version":1,"intent":{"type":"archive_authoritative","archive":"archive@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"},"by":"archive@example.test"}}"#,
            r#"{"version":1,"intent":{"type":"route_direct","recipient":"romeo@example.test","fanout":["romeo@example.test/phone"]}}"#,
            r#"{"version":1,"intent":{"type":"route_muc_groupchat","room":"room@conference.example.test","occupants":["juliet@example.test/laptop"],"reflection":"romeo@example.test/phone","room_generation":7}}"#,
            r#"{"version":1,"intent":{"type":"route_occupant_pm","recipient":"juliet@example.test/laptop","sender":"romeo@example.test/phone"}}"#,
            r#"{"version":1,"intent":{"type":"dispatch_to_room_remote","room":"room@conference.example.test","relay_target":{"node_id":"relay-node","node_epoch":"relay-epoch"}}}"#,
            r#"{"version":1,"intent":{"type":"recipient_sm_append","stream":"stream-1"}}"#,
            r#"{"version":1,"intent":{"type":"carbons","carbon_recipients":["romeo@example.test/phone"],"excluded_source":"romeo@example.test/laptop"}}"#,
            r#"{"version":1,"intent":{"type":"inbox_project","owner":"romeo@example.test","increment_unread":true}}"#,
            r#"{"version":1,"intent":{"type":"notification_activity_preview","owner":"romeo@example.test"}}"#,
            r#"{"version":1,"intent":{"type":"call_signal","recipient":"romeo@example.test/phone","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"pin","room":"room@conference.example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"extension","recipient":"romeo@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"error_reply","recipient":"romeo@example.test/phone","condition":3}}"#,
        ];
        for (intent, expected) in samples().into_iter().zip(golden) {
            let encoded = intent.encode_v1().expect("encode sample");
            assert_eq!(encoded.payload, expected.as_bytes());
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind, &encoded.payload)
                    .expect("decode sample"),
                intent
            );
        }
    }

    #[test]
    fn canonicalizes_unordered_fanout_audiences() {
        let first = IngressEffectIntent::RouteDirect {
            recipient: bare("romeo@example.test"),
            fanout: vec![
                full("romeo@example.test/phone"),
                full("romeo@example.test/laptop"),
                full("romeo@example.test/phone"),
            ],
        };
        let second = IngressEffectIntent::RouteDirect {
            recipient: bare("romeo@example.test"),
            fanout: vec![
                full("romeo@example.test/laptop"),
                full("romeo@example.test/phone"),
            ],
        };
        assert_eq!(
            first.encode_v1().expect("encode first"),
            second.encode_v1().expect("encode second")
        );
    }

    #[test]
    fn relay_target_without_epoch_round_trips() {
        let intent = IngressEffectIntent::DispatchToRoomRemote {
            room: bare("room@conference.example.test"),
            relay_target: RelayTargetIdentity::relay_node("relay-node"),
        };
        let encoded = intent.encode_v1().expect("encode sample");
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind, &encoded.payload).expect("decode sample"),
            intent
        );
        assert_eq!(
            encoded.payload,
            br#"{"version":1,"intent":{"type":"dispatch_to_room_remote","room":"room@conference.example.test","relay_target":{"node_id":"relay-node"}}}"#
        );
    }

    #[test]
    fn rejects_invalid_versions_kinds_and_oversized_payloads() {
        let encoded = samples().remove(0).encode_v1().expect("encode sample");
        let unknown_version = encoded
            .payload
            .windows(11)
            .position(|part| part == b"\"version\":1")
            .expect("version marker");
        let mut version_payload = encoded.payload.clone();
        version_payload[unknown_version + 10] = b'2';
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind, &version_payload),
            Err(EffectIntentCodecError::UnknownPayloadVersion(2))
        );
        assert_eq!(
            IngressEffectIntent::decode_v1(99, &encoded.payload),
            Err(EffectIntentCodecError::UnknownKind(99))
        );
        let oversized = IngressEffectIntent::Extension {
            recipient: bare("romeo@example.test"),
            stanza_id: StanzaId::new(
                "x".repeat(MAX_EFFECT_INTENT_PAYLOAD_BYTES),
                "archive@example.test".parse::<Jid>().expect("valid JID"),
            ),
        };
        assert_eq!(
            oversized.encode_v1(),
            Err(EffectIntentCodecError::PayloadTooLarge)
        );
        assert_eq!(
            IngressEffectIntent::decode_v1(
                encoded.kind,
                &vec![b'x'; MAX_EFFECT_INTENT_PAYLOAD_BYTES + 1],
            ),
            Err(EffectIntentCodecError::PayloadTooLarge)
        );
    }
}
