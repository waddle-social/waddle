//! Immutable, typed descriptions of effects selected during ingress.

use std::{cmp::Ordering, collections::BTreeMap, ops::Deref};

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use waddle_xmpp_core::xep0359::StanzaId;
use xmpp_parsers::{
    message::Lang,
    stanza_error::{DefinedCondition, ErrorType as XmppStanzaErrorType, StanzaError},
};

use crate::{
    error::StanzaErrorCondition, ingress::EntityGeneration, muc::SubjectState,
    pending_delivery::SmSessionId,
};

/// Largest accepted version-one storage payload, matching the database check.
pub const MAX_EFFECT_INTENT_PAYLOAD_BYTES: usize = 65_536;

/// Typed relay target node identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelayNodeId(String);

impl RelayNodeId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RelayNodeId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl std::fmt::Display for RelayNodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Typed relay target node incarnation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelayNodeEpoch(String);

impl RelayNodeEpoch {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Deref for RelayNodeEpoch {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.as_str()
    }
}

impl std::fmt::Display for RelayNodeEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelayTargetIdentity {
    pub node_id: RelayNodeId,
    pub node_epoch: Option<RelayNodeEpoch>,
}

impl RelayTargetIdentity {
    pub fn owner_node(node_id: impl Into<String>, node_epoch: impl Into<String>) -> Self {
        Self {
            node_id: RelayNodeId::new(node_id),
            node_epoch: Some(RelayNodeEpoch::new(node_epoch)),
        }
    }

    pub fn relay_node(node_id: impl Into<String>) -> Self {
        Self {
            node_id: RelayNodeId::new(node_id),
            node_epoch: None,
        }
    }

    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}",
            self.node_id,
            self.node_epoch.as_deref().unwrap_or("")
        )
    }
}

/// Typed identity distinguishing repeated replay-buffer appends to one SM
/// session within the same canonical message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecipientSmAppendIdentity(u64);

impl RecipientSmAppendIdentity {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn storage_identity(self) -> String {
        format!("{:020}", self.0)
    }
}

impl std::fmt::Display for RecipientSmAppendIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Typed stanza-error type preserved by frozen ingress error-reply intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrozenStanzaErrorType {
    Auth,
    Cancel,
    Continue,
    Modify,
    Wait,
}

impl FrozenStanzaErrorType {
    fn from_xmpp(value: XmppStanzaErrorType) -> Self {
        match value {
            XmppStanzaErrorType::Auth => Self::Auth,
            XmppStanzaErrorType::Cancel => Self::Cancel,
            XmppStanzaErrorType::Continue => Self::Continue,
            XmppStanzaErrorType::Modify => Self::Modify,
            XmppStanzaErrorType::Wait => Self::Wait,
        }
    }

    fn to_xmpp(self) -> XmppStanzaErrorType {
        match self {
            Self::Auth => XmppStanzaErrorType::Auth,
            Self::Cancel => XmppStanzaErrorType::Cancel,
            Self::Continue => XmppStanzaErrorType::Continue,
            Self::Modify => XmppStanzaErrorType::Modify,
            Self::Wait => XmppStanzaErrorType::Wait,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenStanzaErrorText {
    pub lang: Lang,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrozenStanzaErrorTexts(BTreeMap<Lang, String>);

impl FrozenStanzaErrorTexts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, lang: Lang, text: impl Into<String>) {
        self.0.insert(lang, text.into());
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Lang, &String)> {
        self.0.iter()
    }

    pub fn get(&self, lang: &str) -> Option<&str> {
        self.0.get(&Lang(lang.to_string())).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenStanzaErrorAddress(Url);

impl FrozenStanzaErrorAddress {
    pub fn new(url: Url) -> Self {
        Self(url)
    }

    pub fn parse(value: &str) -> Result<Self, url::ParseError> {
        Url::parse(value).map(Self)
    }

    pub fn as_url(&self) -> &Url {
        &self.0
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for FrozenStanzaErrorAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrozenStanzaErrorConditionPayload {
    Gone {
        new_address: Option<FrozenStanzaErrorAddress>,
    },
    Redirect {
        new_address: Option<FrozenStanzaErrorAddress>,
    },
}

/// Complete typed stanza-error semantics frozen into an ingress effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenStanzaError {
    pub error_type: FrozenStanzaErrorType,
    pub condition: StanzaErrorCondition,
    pub texts: FrozenStanzaErrorTexts,
    pub condition_payload: Option<FrozenStanzaErrorConditionPayload>,
}

impl FrozenStanzaError {
    pub fn new(error_type: FrozenStanzaErrorType, condition: StanzaErrorCondition) -> Self {
        Self {
            error_type,
            condition,
            texts: FrozenStanzaErrorTexts::new(),
            condition_payload: None,
        }
    }

    pub fn with_text(mut self, lang: impl Into<String>, text: impl Into<String>) -> Self {
        self.texts.insert(Lang(lang.into()), text.into());
        self
    }

    pub fn with_condition_payload(mut self, payload: FrozenStanzaErrorConditionPayload) -> Self {
        self.condition_payload = Some(payload);
        self
    }

    pub fn from_xmpp(error: &StanzaError) -> Result<Self, EffectIntentCodecError> {
        let mut texts = FrozenStanzaErrorTexts::new();
        for (lang, text) in &error.texts {
            texts.insert(Lang(lang.clone()), text.clone());
        }
        let condition_payload = match &error.defined_condition {
            DefinedCondition::Gone { new_address } => {
                Some(FrozenStanzaErrorConditionPayload::Gone {
                    new_address: new_address
                        .as_deref()
                        .map(FrozenStanzaErrorAddress::parse)
                        .transpose()
                        .map_err(|_| EffectIntentCodecError::MalformedPayload)?,
                })
            }
            DefinedCondition::Redirect { new_address } => {
                Some(FrozenStanzaErrorConditionPayload::Redirect {
                    new_address: new_address
                        .as_deref()
                        .map(FrozenStanzaErrorAddress::parse)
                        .transpose()
                        .map_err(|_| EffectIntentCodecError::MalformedPayload)?,
                })
            }
            _ => None,
        };
        Ok(Self {
            error_type: FrozenStanzaErrorType::from_xmpp(error.type_.clone()),
            condition: StanzaErrorCondition::from_xmpp(&error.defined_condition),
            texts,
            condition_payload,
        })
    }

    pub fn to_xmpp(&self) -> StanzaError {
        let mut texts = BTreeMap::new();
        for (lang, text) in self.texts.iter() {
            texts.insert(lang.to_string(), text.clone());
        }
        StanzaError {
            type_: self.error_type.to_xmpp(),
            by: None,
            defined_condition: self.to_xmpp_condition(),
            texts,
            other: None,
        }
    }

    fn to_xmpp_condition(&self) -> DefinedCondition {
        match (&self.condition, self.condition_payload.as_ref()) {
            (
                StanzaErrorCondition::Gone,
                Some(FrozenStanzaErrorConditionPayload::Gone { new_address }),
            ) => DefinedCondition::Gone {
                new_address: new_address.as_ref().map(|address| address.to_string()),
            },
            (
                StanzaErrorCondition::Redirect,
                Some(FrozenStanzaErrorConditionPayload::Redirect { new_address }),
            ) => DefinedCondition::Redirect {
                new_address: new_address.as_ref().map(|address| address.to_string()),
            },
            (StanzaErrorCondition::Gone, _) => DefinedCondition::Gone { new_address: None },
            (StanzaErrorCondition::Redirect, _) => DefinedCondition::Redirect { new_address: None },
            (condition, _) => condition.to_xmpp(),
        }
    }
}

impl From<StanzaErrorCondition> for FrozenStanzaError {
    fn from(condition: StanzaErrorCondition) -> Self {
        Self::new(default_error_type(condition), condition)
    }
}

fn default_error_type(condition: StanzaErrorCondition) -> FrozenStanzaErrorType {
    use FrozenStanzaErrorType::{Auth, Cancel, Modify, Wait};
    use StanzaErrorCondition::*;

    match condition {
        BadRequest => Modify,
        Conflict => Cancel,
        FeatureNotImplemented => Cancel,
        Forbidden => Auth,
        Gone => Cancel,
        InternalServerError => Wait,
        ItemNotFound => Cancel,
        JidMalformed => Modify,
        NotAcceptable => Modify,
        NotAllowed => Cancel,
        NotAuthorized => Auth,
        PolicyViolation => Modify,
        RecipientUnavailable => Wait,
        Redirect => Modify,
        RegistrationRequired => Auth,
        RemoteServerNotFound => Cancel,
        RemoteServerTimeout => Wait,
        ResourceConstraint => Wait,
        ServiceUnavailable => Cancel,
        SubscriptionRequired => Auth,
        UndefinedCondition => Cancel,
        UnexpectedRequest => Wait,
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
        append_identity: RecipientSmAppendIdentity,
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
    RoomSubjectMutation {
        room: BareJid,
        state: SubjectState,
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
        error: FrozenStanzaError,
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
    RecipientSmAppend(SmSessionId, RecipientSmAppendIdentity),
    Carbons(FullJid),
    InboxProject(BareJid),
    NotificationActivityPreview(BareJid),
    RoomSubjectMutation(BareJid),
    CallSignal(FullJid),
    Pin(BareJid),
    Extension(BareJid),
    ErrorReply(FullJid),
}

impl IngressEffectKey {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::ArchiveAuthoritative(value) => value.to_string(),
            Self::RouteDirect(value) => value.to_string(),
            Self::RouteMucGroupchat(value) => value.to_string(),
            Self::RouteOccupantPm(value) => value.to_string(),
            Self::DispatchToRoomRemote(room, relay_target) => {
                format!("{}|{}", room, relay_target.storage_identity())
            }
            Self::RecipientSmAppend(stream, append_identity) => {
                format!("{}|{}", stream.as_str(), append_identity.storage_identity())
            }
            Self::Carbons(value) => value.to_string(),
            Self::InboxProject(value) => value.to_string(),
            Self::NotificationActivityPreview(value) => value.to_string(),
            Self::RoomSubjectMutation(value) => value.to_string(),
            Self::CallSignal(value) => value.to_string(),
            Self::Pin(value) => value.to_string(),
            Self::Extension(value) => value.to_string(),
            Self::ErrorReply(value) => value.to_string(),
        }
    }

    fn ordering_key(&self) -> (u8, String) {
        let class = match self {
            Self::ArchiveAuthoritative(..) => 0,
            Self::RouteDirect(..) => 1,
            Self::RouteMucGroupchat(..) => 2,
            Self::RouteOccupantPm(..) => 3,
            Self::DispatchToRoomRemote(..) => 4,
            Self::RecipientSmAppend(..) => 5,
            Self::Carbons(..) => 6,
            Self::InboxProject(..) => 7,
            Self::NotificationActivityPreview(..) => 8,
            Self::RoomSubjectMutation(..) => 9,
            Self::CallSignal(..) => 10,
            Self::Pin(..) => 11,
            Self::Extension(..) => 12,
            Self::ErrorReply(..) => 13,
        };
        (class, self.storage_identity())
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
            Self::RecipientSmAppend {
                stream,
                append_identity,
            } => IngressEffectKey::RecipientSmAppend(stream.clone(), *append_identity),
            Self::Carbons {
                excluded_source, ..
            } => IngressEffectKey::Carbons(excluded_source.clone()),
            Self::InboxProject { owner, .. } => IngressEffectKey::InboxProject(owner.clone()),
            Self::NotificationActivityPreview { owner } => {
                IngressEffectKey::NotificationActivityPreview(owner.clone())
            }
            Self::RoomSubjectMutation { room, .. } => {
                IngressEffectKey::RoomSubjectMutation(room.clone())
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
struct StoredRelayTargetIdentity {
    node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_epoch: Option<String>,
}

impl From<RelayTargetIdentity> for StoredRelayTargetIdentity {
    fn from(value: RelayTargetIdentity) -> Self {
        Self {
            node_id: value.node_id.as_str().to_string(),
            node_epoch: value.node_epoch.map(|epoch| epoch.as_str().to_string()),
        }
    }
}

impl From<StoredRelayTargetIdentity> for RelayTargetIdentity {
    fn from(value: StoredRelayTargetIdentity) -> Self {
        Self {
            node_id: RelayNodeId::new(value.node_id),
            node_epoch: value.node_epoch.map(RelayNodeEpoch::new),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredFrozenStanzaErrorText {
    lang: String,
    text: String,
}

impl From<FrozenStanzaErrorText> for StoredFrozenStanzaErrorText {
    fn from(value: FrozenStanzaErrorText) -> Self {
        Self {
            lang: value.lang.to_string(),
            text: value.text,
        }
    }
}

impl From<StoredFrozenStanzaErrorText> for FrozenStanzaErrorText {
    fn from(value: StoredFrozenStanzaErrorText) -> Self {
        Self {
            lang: Lang(value.lang),
            text: value.text,
        }
    }
}

impl FrozenStanzaErrorTexts {
    fn from_stored(values: Vec<StoredFrozenStanzaErrorText>) -> Self {
        let mut texts = Self::new();
        for value in values {
            texts.insert(Lang(value.lang), value.text);
        }
        texts
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredFrozenStanzaErrorConditionPayload {
    Gone {
        #[serde(skip_serializing_if = "Option::is_none")]
        new_address: Option<String>,
    },
    Redirect {
        #[serde(skip_serializing_if = "Option::is_none")]
        new_address: Option<String>,
    },
}

impl From<FrozenStanzaErrorConditionPayload> for StoredFrozenStanzaErrorConditionPayload {
    fn from(value: FrozenStanzaErrorConditionPayload) -> Self {
        match value {
            FrozenStanzaErrorConditionPayload::Gone { new_address } => Self::Gone {
                new_address: new_address.map(|address| address.to_string()),
            },
            FrozenStanzaErrorConditionPayload::Redirect { new_address } => Self::Redirect {
                new_address: new_address.map(|address| address.to_string()),
            },
        }
    }
}

impl StoredFrozenStanzaErrorConditionPayload {
    fn into_domain(self) -> Result<FrozenStanzaErrorConditionPayload, EffectIntentCodecError> {
        Ok(match self {
            StoredFrozenStanzaErrorConditionPayload::Gone { new_address } => {
                FrozenStanzaErrorConditionPayload::Gone {
                    new_address: new_address
                        .as_deref()
                        .map(FrozenStanzaErrorAddress::parse)
                        .transpose()
                        .map_err(|_| EffectIntentCodecError::MalformedPayload)?,
                }
            }
            StoredFrozenStanzaErrorConditionPayload::Redirect { new_address } => {
                FrozenStanzaErrorConditionPayload::Redirect {
                    new_address: new_address
                        .as_deref()
                        .map(FrozenStanzaErrorAddress::parse)
                        .transpose()
                        .map_err(|_| EffectIntentCodecError::MalformedPayload)?,
                }
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredFrozenStanzaError {
    error_type: u8,
    condition: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    texts: Vec<StoredFrozenStanzaErrorText>,
    #[serde(skip_serializing_if = "Option::is_none")]
    condition_payload: Option<StoredFrozenStanzaErrorConditionPayload>,
}

impl StoredFrozenStanzaError {
    fn from_domain(value: FrozenStanzaError) -> Self {
        Self {
            error_type: frozen_error_type_tag(value.error_type),
            condition: condition_tag(value.condition),
            texts: value
                .texts
                .iter()
                .map(|(lang, text)| StoredFrozenStanzaErrorText {
                    lang: lang.to_string(),
                    text: text.clone(),
                })
                .collect(),
            condition_payload: value.condition_payload.map(Into::into),
        }
    }

    fn into_domain(self) -> Result<FrozenStanzaError, EffectIntentCodecError> {
        Ok(FrozenStanzaError {
            error_type: frozen_error_type_from_tag(self.error_type)?,
            condition: condition_from_tag(self.condition)?,
            texts: FrozenStanzaErrorTexts::from_stored(self.texts),
            condition_payload: self
                .condition_payload
                .map(StoredFrozenStanzaErrorConditionPayload::into_domain)
                .transpose()?,
        })
    }
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
        relay_target: StoredRelayTargetIdentity,
    },
    RecipientSmAppend {
        stream: SmSessionId,
        append_identity: u64,
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
    RoomSubjectMutation {
        room: BareJid,
        state: SubjectState,
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
        error: StoredFrozenStanzaError,
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
            Self::RoomSubjectMutation { .. } => 13,
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
                Self::DispatchToRoomRemote {
                    room,
                    relay_target: relay_target.into(),
                }
            }
            IngressEffectIntent::RecipientSmAppend {
                stream,
                append_identity,
            } => Self::RecipientSmAppend {
                stream,
                append_identity: append_identity.as_u64(),
            },
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
            IngressEffectIntent::RoomSubjectMutation { room, state } => {
                Self::RoomSubjectMutation { room, state }
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
            IngressEffectIntent::ErrorReply { recipient, error } => Self::ErrorReply {
                recipient,
                error: StoredFrozenStanzaError::from_domain(error),
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
                IngressEffectIntent::DispatchToRoomRemote {
                    room,
                    relay_target: relay_target.into(),
                }
            }
            Self::RecipientSmAppend {
                stream,
                append_identity,
            } => IngressEffectIntent::RecipientSmAppend {
                stream,
                append_identity: RecipientSmAppendIdentity::new(append_identity),
            },
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
            Self::RoomSubjectMutation { room, state } => {
                IngressEffectIntent::RoomSubjectMutation { room, state }
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
            Self::ErrorReply { recipient, error } => IngressEffectIntent::ErrorReply {
                recipient,
                error: error.into_domain()?,
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

fn frozen_error_type_tag(error_type: FrozenStanzaErrorType) -> u8 {
    match error_type {
        FrozenStanzaErrorType::Auth => 0,
        FrozenStanzaErrorType::Cancel => 1,
        FrozenStanzaErrorType::Continue => 2,
        FrozenStanzaErrorType::Modify => 3,
        FrozenStanzaErrorType::Wait => 4,
    }
}

fn frozen_error_type_from_tag(tag: u8) -> Result<FrozenStanzaErrorType, EffectIntentCodecError> {
    Ok(match tag {
        0 => FrozenStanzaErrorType::Auth,
        1 => FrozenStanzaErrorType::Cancel,
        2 => FrozenStanzaErrorType::Continue,
        3 => FrozenStanzaErrorType::Modify,
        4 => FrozenStanzaErrorType::Wait,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

#[cfg(test)]
mod tests {
    use jid::Jid;
    use waddle_xmpp_core::xep0359::StanzaId;
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType as XmppStanzaErrorType};

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
                append_identity: RecipientSmAppendIdentity::new(0),
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
                error: FrozenStanzaError::new(
                    FrozenStanzaErrorType::Modify,
                    StanzaErrorCondition::Redirect,
                )
                .with_text("nb", "moved")
                .with_condition_payload(
                    FrozenStanzaErrorConditionPayload::Redirect {
                        new_address: Some(
                            FrozenStanzaErrorAddress::parse("xmpp:romeo@example.test/mobile")
                                .expect("valid redirect URI"),
                        ),
                    },
                ),
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
            r#"{"version":1,"intent":{"type":"recipient_sm_append","stream":"stream-1","append_identity":0}}"#,
            r#"{"version":1,"intent":{"type":"carbons","carbon_recipients":["romeo@example.test/phone"],"excluded_source":"romeo@example.test/laptop"}}"#,
            r#"{"version":1,"intent":{"type":"inbox_project","owner":"romeo@example.test","increment_unread":true}}"#,
            r#"{"version":1,"intent":{"type":"notification_activity_preview","owner":"romeo@example.test"}}"#,
            r#"{"version":1,"intent":{"type":"call_signal","recipient":"romeo@example.test/phone","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"pin","room":"room@conference.example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"extension","recipient":"romeo@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"error_reply","recipient":"romeo@example.test/phone","error":{"error_type":3,"condition":13,"texts":[{"lang":"nb","text":"moved"}],"condition_payload":{"type":"redirect","new_address":"xmpp:romeo@example.test/mobile"}}}}"#,
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
    fn recipient_sm_append_key_distinguishes_repeated_appends_and_preserves_order() {
        let first = IngressEffectIntent::RecipientSmAppend {
            stream: SmSessionId::new("stream-1"),
            append_identity: RecipientSmAppendIdentity::new(1),
        };
        let second = IngressEffectIntent::RecipientSmAppend {
            stream: SmSessionId::new("stream-1"),
            append_identity: RecipientSmAppendIdentity::new(2),
        };

        assert_ne!(first.semantic_key(), second.semantic_key());
        assert!(first.semantic_key() < second.semantic_key());
        assert_eq!(
            first.semantic_key().storage_identity(),
            "stream-1|00000000000000000001"
        );
    }

    #[test]
    fn error_reply_gone_payload_round_trips() {
        let intent = IngressEffectIntent::ErrorReply {
            recipient: full("romeo@example.test/phone"),
            error: FrozenStanzaError::new(
                FrozenStanzaErrorType::Cancel,
                StanzaErrorCondition::Gone,
            )
            .with_text("en", "moved permanently")
            .with_condition_payload(FrozenStanzaErrorConditionPayload::Gone {
                new_address: Some(
                    FrozenStanzaErrorAddress::parse("xmpp:romeo@example.test/desktop")
                        .expect("valid gone URI"),
                ),
            }),
        };

        let encoded = intent.encode_v1().expect("encode sample");
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind, &encoded.payload).expect("decode sample"),
            intent
        );
        assert_eq!(
            encoded.payload,
            br#"{"version":1,"intent":{"type":"error_reply","recipient":"romeo@example.test/phone","error":{"error_type":1,"condition":4,"texts":[{"lang":"en","text":"moved permanently"}],"condition_payload":{"type":"gone","new_address":"xmpp:romeo@example.test/desktop"}}}}"#
        );
    }

    #[test]
    fn frozen_stanza_error_round_trips_through_xmpp_type() {
        let mut xmpp = StanzaError::new(
            XmppStanzaErrorType::Continue,
            DefinedCondition::Redirect {
                new_address: Some("xmpp:romeo@example.test/mobile".to_string()),
            },
            "fr",
            "redirige",
        );
        xmpp.texts
            .insert("en".to_string(), "redirected".to_string());

        let frozen = FrozenStanzaError::from_xmpp(&xmpp).expect("typed stanza error");
        assert_eq!(frozen.error_type, FrozenStanzaErrorType::Continue);
        assert_eq!(frozen.condition, StanzaErrorCondition::Redirect);
        assert_eq!(frozen.texts.get("en"), Some("redirected"));
        assert_eq!(frozen.texts.get("fr"), Some("redirige"));
        assert_eq!(
            frozen.condition_payload,
            Some(FrozenStanzaErrorConditionPayload::Redirect {
                new_address: Some(
                    FrozenStanzaErrorAddress::parse("xmpp:romeo@example.test/mobile")
                        .expect("valid redirect URI"),
                ),
            })
        );

        let round_trip = frozen.to_xmpp();
        assert_eq!(round_trip.type_, XmppStanzaErrorType::Continue);
        assert_eq!(
            round_trip.defined_condition,
            DefinedCondition::Redirect {
                new_address: Some("xmpp:romeo@example.test/mobile".to_string()),
            }
        );
        assert_eq!(round_trip.texts.get("en"), Some(&"redirected".to_string()));
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
