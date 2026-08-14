//! Immutable, typed descriptions of effects selected during ingress.

use std::{cmp::Ordering, collections::BTreeMap, ops::Deref};

use jid::{BareJid, FullJid};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
use xmpp_parsers::{
    message::Lang,
    stanza_error::{DefinedCondition, ErrorType as XmppStanzaErrorType, StanzaError},
};

use crate::{
    error::StanzaErrorCondition, ingress::EntityGeneration, muc::SubjectState,
    pending_delivery::SmSessionId, protocol::CarbonKind,
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

/// Typed identity distinguishing multiple routing/archive effects for the same
/// bare/full JID within one ingress transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectMessageIdentity {
    StanzaId(StanzaId),
    OriginId(OriginId),
    CaptureOrdinal(u64),
}

impl EffectMessageIdentity {
    pub fn stanza(stanza_id: StanzaId) -> Self {
        Self::StanzaId(stanza_id)
    }

    pub fn origin(origin_id: OriginId) -> Self {
        Self::OriginId(origin_id)
    }

    pub fn capture_ordinal(ordinal: u64) -> Self {
        Self::CaptureOrdinal(ordinal)
    }

    pub fn storage_identity(&self) -> String {
        match self {
            Self::StanzaId(stanza_id) => {
                format!("stanza:{}|{}", stanza_id.by, stanza_id.id)
            }
            Self::OriginId(origin_id) => format!("origin:{}", origin_id.as_str()),
            Self::CaptureOrdinal(ordinal) => format!("capture:{ordinal:020}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinAction {
    Pin,
    Unpin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationCandidateOutcome {
    Inserted,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationActivityMutation {
    ChatState {
        conversation: BareJid,
    },
    ChatStateGone {
        conversation: BareJid,
    },
    ReadMarker {
        conversation: BareJid,
    },
    OutboundMessage {
        conversation: BareJid,
    },
    OfflineDelivery {
        conversation: BareJid,
        archive_stanza_id: StanzaId,
    },
    NotificationCandidate {
        conversation: BareJid,
        archive_stanza_id: StanzaId,
        outcome: NotificationCandidateOutcome,
    },
}

impl NotificationActivityMutation {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::ChatState { conversation } => format!("chat_state|{}", conversation),
            Self::ChatStateGone { conversation } => {
                format!("chat_state_gone|{}", conversation)
            }
            Self::ReadMarker { conversation } => format!("read_marker|{}", conversation),
            Self::OutboundMessage { conversation } => {
                format!("outbound_message|{}", conversation)
            }
            Self::OfflineDelivery {
                conversation,
                archive_stanza_id,
            } => format!(
                "offline_delivery|{}|{}|{}",
                conversation, archive_stanza_id.by, archive_stanza_id.id
            ),
            Self::NotificationCandidate {
                conversation,
                archive_stanza_id,
                outcome,
            } => format!(
                "notification_candidate|{}|{}|{}|{}",
                conversation,
                archive_stanza_id.by,
                archive_stanza_id.id,
                match outcome {
                    NotificationCandidateOutcome::Inserted => "inserted",
                    NotificationCandidateOutcome::Duplicate => "duplicate",
                }
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxProjectionMutation {
    Direct {
        peer: BareJid,
        increment_unread: bool,
    },
    GroupchatChannel {
        room: BareJid,
        increment_unread: bool,
    },
    GroupchatThread {
        room: BareJid,
        thread_id: String,
    },
    GroupchatChannelAndThread {
        room: BareJid,
        thread_id: String,
        increment_unread: bool,
    },
}

impl InboxProjectionMutation {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Direct {
                peer,
                increment_unread,
            } => format!("direct|{}|{}", peer, increment_unread),
            Self::GroupchatChannel {
                room,
                increment_unread,
            } => format!("groupchat_channel|{}|{}", room, increment_unread),
            Self::GroupchatThread { room, thread_id } => {
                format!("groupchat_thread|{}|{}", room, thread_id)
            }
            Self::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => format!(
                "groupchat_channel_and_thread|{}|{}|{}",
                room, thread_id, increment_unread
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmPinMutationAction {
    Pin,
    Unpin,
    RetractionCascadeUnpin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetractionTombstoneMutation {
    pub archive: BareJid,
    pub target_stanza_id: StanzaId,
    pub retraction_stanza_id: StanzaId,
}

impl RetractionTombstoneMutation {
    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}|{}",
            self.archive,
            stanza_storage_identity(&self.target_stanza_id),
            stanza_storage_identity(&self.retraction_stanza_id)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupDmMembershipGrant {
    pub room: BareJid,
    pub invitee: BareJid,
    pub inviter: BareJid,
}

impl GroupDmMembershipGrant {
    pub fn storage_identity(&self) -> String {
        format!("{}|{}|{}", self.room, self.invitee, self.inviter)
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

    fn semantic_identity(&self) -> String {
        let texts = self
            .texts
            .iter()
            .map(|(lang, text)| format!("{}={}", lang, text))
            .collect::<Vec<_>>()
            .join("|");
        let payload = match self.condition_payload.as_ref() {
            Some(FrozenStanzaErrorConditionPayload::Gone { new_address }) => format!(
                "gone:{}",
                new_address
                    .as_ref()
                    .map_or("", FrozenStanzaErrorAddress::as_str)
            ),
            Some(FrozenStanzaErrorConditionPayload::Redirect { new_address }) => format!(
                "redirect:{}",
                new_address
                    .as_ref()
                    .map_or("", FrozenStanzaErrorAddress::as_str)
            ),
            None => "none".to_string(),
        };
        format!(
            "{}|{}|{}|{}",
            frozen_error_type_tag(self.error_type),
            condition_tag(self.condition),
            texts,
            payload
        )
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

fn stanza_storage_identity(stanza_id: &StanzaId) -> String {
    format!("{}|{}", stanza_id.by, stanza_id.id)
}

fn carbon_kind_storage_identity(kind: CarbonKind) -> &'static str {
    match kind {
        CarbonKind::Sent => "sent",
        CarbonKind::Received => "received",
    }
}

fn pin_action_storage_identity(action: PinAction) -> &'static str {
    match action {
        PinAction::Pin => "pin",
        PinAction::Unpin => "unpin",
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
        route_identity: EffectMessageIdentity,
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
        kind: CarbonKind,
    },
    InboxProject {
        owner: BareJid,
        mutation: InboxProjectionMutation,
    },
    NotificationActivityPreview {
        owner: BareJid,
        mutation: NotificationActivityMutation,
    },
    RetractionTombstone {
        mutation: RetractionTombstoneMutation,
    },
    DmPinMutation {
        pair: (BareJid, BareJid),
        target_stanza_id: StanzaId,
        action: DmPinMutationAction,
    },
    GroupDmMembershipGrant {
        grant: GroupDmMembershipGrant,
    },
    GroupDmInviteLedger {
        grant: GroupDmMembershipGrant,
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
        action: PinAction,
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
    ArchiveAuthoritative(BareJid, String),
    RouteDirect(BareJid, String),
    RouteMucGroupchat(BareJid),
    RouteOccupantPm(FullJid),
    DispatchToRoomRemote(BareJid, RelayTargetIdentity),
    RecipientSmAppend(SmSessionId, RecipientSmAppendIdentity),
    Carbons(FullJid, CarbonKind),
    InboxProject(BareJid, String),
    NotificationActivityPreview(BareJid, String),
    RetractionTombstone(String),
    DmPinMutation(String),
    GroupDmMembershipGrant(String),
    GroupDmInviteLedger(String),
    RoomSubjectMutation(BareJid),
    CallSignal(FullJid),
    Pin(BareJid, String),
    Extension(BareJid),
    ErrorReply(FullJid, String),
}

impl IngressEffectKey {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::ArchiveAuthoritative(archive, stanza_identity) => {
                format!("{}|{}", archive, stanza_identity)
            }
            Self::RouteDirect(recipient, route_identity) => {
                format!("{}|{}", recipient, route_identity)
            }
            Self::RouteMucGroupchat(value) => value.to_string(),
            Self::RouteOccupantPm(value) => value.to_string(),
            Self::DispatchToRoomRemote(room, relay_target) => {
                format!("{}|{}", room, relay_target.storage_identity())
            }
            Self::RecipientSmAppend(stream, append_identity) => {
                format!("{}|{}", stream.as_str(), append_identity.storage_identity())
            }
            Self::Carbons(value, kind) => {
                format!("{}|{}", value, carbon_kind_storage_identity(*kind))
            }
            Self::InboxProject(owner, mutation) => format!("{}|{}", owner, mutation),
            Self::NotificationActivityPreview(owner, mutation) => {
                format!("{}|{}", owner, mutation)
            }
            Self::RetractionTombstone(identity) => identity.clone(),
            Self::DmPinMutation(identity) => identity.clone(),
            Self::GroupDmMembershipGrant(identity) => identity.clone(),
            Self::GroupDmInviteLedger(identity) => identity.clone(),
            Self::RoomSubjectMutation(value) => value.to_string(),
            Self::CallSignal(value) => value.to_string(),
            Self::Pin(room, pin_identity) => format!("{}|{}", room, pin_identity),
            Self::Extension(value) => value.to_string(),
            Self::ErrorReply(value, error_identity) => format!("{}|{}", value, error_identity),
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
            Self::RetractionTombstone(..) => 9,
            Self::DmPinMutation(..) => 10,
            Self::GroupDmMembershipGrant(..) => 11,
            Self::GroupDmInviteLedger(..) => 12,
            Self::RoomSubjectMutation(..) => 13,
            Self::CallSignal(..) => 14,
            Self::Pin(..) => 15,
            Self::Extension(..) => 16,
            Self::ErrorReply(..) => 17,
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
            Self::ArchiveAuthoritative {
                archive, stanza_id, ..
            } => IngressEffectKey::ArchiveAuthoritative(
                archive.clone(),
                stanza_storage_identity(stanza_id),
            ),
            Self::RouteDirect {
                recipient,
                route_identity,
                ..
            } => {
                IngressEffectKey::RouteDirect(recipient.clone(), route_identity.storage_identity())
            }
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
                excluded_source,
                kind,
                ..
            } => IngressEffectKey::Carbons(excluded_source.clone(), *kind),
            Self::InboxProject { owner, mutation } => {
                IngressEffectKey::InboxProject(owner.clone(), mutation.storage_identity())
            }
            Self::NotificationActivityPreview { owner, mutation } => {
                IngressEffectKey::NotificationActivityPreview(
                    owner.clone(),
                    mutation.storage_identity(),
                )
            }
            Self::RetractionTombstone { mutation } => {
                IngressEffectKey::RetractionTombstone(mutation.storage_identity())
            }
            Self::DmPinMutation {
                pair,
                target_stanza_id,
                action,
            } => IngressEffectKey::DmPinMutation(format!(
                "{}|{}|{}|{}",
                pair.0,
                pair.1,
                stanza_storage_identity(target_stanza_id),
                dm_pin_action_storage_identity(*action)
            )),
            Self::GroupDmMembershipGrant { grant } => {
                IngressEffectKey::GroupDmMembershipGrant(grant.storage_identity())
            }
            Self::GroupDmInviteLedger { grant } => {
                IngressEffectKey::GroupDmInviteLedger(grant.storage_identity())
            }
            Self::RoomSubjectMutation { room, .. } => {
                IngressEffectKey::RoomSubjectMutation(room.clone())
            }
            Self::CallSignal { recipient, .. } => IngressEffectKey::CallSignal(recipient.clone()),
            Self::Pin {
                room,
                stanza_id,
                action,
            } => IngressEffectKey::Pin(
                room.clone(),
                format!(
                    "{}|{}",
                    stanza_storage_identity(stanza_id),
                    pin_action_storage_identity(*action)
                ),
            ),
            Self::Extension { recipient, .. } => IngressEffectKey::Extension(recipient.clone()),
            Self::ErrorReply { recipient, error } => {
                IngressEffectKey::ErrorReply(recipient.clone(), error.semantic_identity())
            }
        }
    }

    /// Encode the canonical V1 storage representation at the persistence edge.
    fn encode_v1(&self) -> Result<EncodedEffectIntent, EffectIntentCodecError> {
        let intent = StoredEffectIntent::from_domain(self.clone());
        let kind = intent.kind();
        let payload = serde_json::to_vec(&StoredPayload { version: 1, intent })
            .map_err(|_| EffectIntentCodecError::MalformedPayload)?;
        if payload.len() > MAX_EFFECT_INTENT_PAYLOAD_BYTES {
            return Err(EffectIntentCodecError::PayloadTooLarge);
        }
        Ok(EncodedEffectIntent { kind, payload })
    }

    pub fn with_encoded_v1<T>(
        &self,
        f: impl FnOnce(i32, &[u8]) -> T,
    ) -> Result<T, EffectIntentCodecError> {
        let encoded = self.encode_v1()?;
        Ok(f(encoded.kind(), encoded.payload()))
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
struct EncodedEffectIntent {
    kind: i32,
    payload: Vec<u8>,
}

impl EncodedEffectIntent {
    pub(crate) fn kind(&self) -> i32 {
        self.kind
    }

    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload
    }
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
enum StoredEffectMessageIdentity {
    StanzaId { stanza_id: StanzaId },
    OriginId { origin_id: String },
    CaptureOrdinal { ordinal: u64 },
}

impl From<EffectMessageIdentity> for StoredEffectMessageIdentity {
    fn from(value: EffectMessageIdentity) -> Self {
        match value {
            EffectMessageIdentity::StanzaId(stanza_id) => Self::StanzaId { stanza_id },
            EffectMessageIdentity::OriginId(origin_id) => Self::OriginId {
                origin_id: origin_id.id,
            },
            EffectMessageIdentity::CaptureOrdinal(ordinal) => Self::CaptureOrdinal { ordinal },
        }
    }
}

impl StoredEffectMessageIdentity {
    fn into_domain(self) -> EffectMessageIdentity {
        match self {
            Self::StanzaId { stanza_id } => EffectMessageIdentity::StanzaId(stanza_id),
            Self::OriginId { origin_id } => {
                EffectMessageIdentity::OriginId(OriginId::new(origin_id))
            }
            Self::CaptureOrdinal { ordinal } => EffectMessageIdentity::CaptureOrdinal(ordinal),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredInboxProjectionMutation {
    Direct {
        peer: BareJid,
        increment_unread: bool,
    },
    GroupchatChannel {
        room: BareJid,
        increment_unread: bool,
    },
    GroupchatThread {
        room: BareJid,
        thread_id: String,
    },
    GroupchatChannelAndThread {
        room: BareJid,
        thread_id: String,
        increment_unread: bool,
    },
}

impl From<InboxProjectionMutation> for StoredInboxProjectionMutation {
    fn from(value: InboxProjectionMutation) -> Self {
        match value {
            InboxProjectionMutation::Direct {
                peer,
                increment_unread,
            } => Self::Direct {
                peer,
                increment_unread,
            },
            InboxProjectionMutation::GroupchatChannel {
                room,
                increment_unread,
            } => Self::GroupchatChannel {
                room,
                increment_unread,
            },
            InboxProjectionMutation::GroupchatThread { room, thread_id } => {
                Self::GroupchatThread { room, thread_id }
            }
            InboxProjectionMutation::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => Self::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            },
        }
    }
}

impl StoredInboxProjectionMutation {
    fn into_domain(self) -> InboxProjectionMutation {
        match self {
            Self::Direct {
                peer,
                increment_unread,
            } => InboxProjectionMutation::Direct {
                peer,
                increment_unread,
            },
            Self::GroupchatChannel {
                room,
                increment_unread,
            } => InboxProjectionMutation::GroupchatChannel {
                room,
                increment_unread,
            },
            Self::GroupchatThread { room, thread_id } => {
                InboxProjectionMutation::GroupchatThread { room, thread_id }
            }
            Self::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => InboxProjectionMutation::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredNotificationActivityMutation {
    ChatState {
        conversation: BareJid,
    },
    ChatStateGone {
        conversation: BareJid,
    },
    ReadMarker {
        conversation: BareJid,
    },
    OutboundMessage {
        conversation: BareJid,
    },
    OfflineDelivery {
        conversation: BareJid,
        archive_stanza_id: StanzaId,
    },
    NotificationCandidate {
        conversation: BareJid,
        archive_stanza_id: StanzaId,
        outcome: u8,
    },
}

impl From<NotificationActivityMutation> for StoredNotificationActivityMutation {
    fn from(value: NotificationActivityMutation) -> Self {
        match value {
            NotificationActivityMutation::ChatState { conversation } => {
                Self::ChatState { conversation }
            }
            NotificationActivityMutation::ChatStateGone { conversation } => {
                Self::ChatStateGone { conversation }
            }
            NotificationActivityMutation::ReadMarker { conversation } => {
                Self::ReadMarker { conversation }
            }
            NotificationActivityMutation::OutboundMessage { conversation } => {
                Self::OutboundMessage { conversation }
            }
            NotificationActivityMutation::OfflineDelivery {
                conversation,
                archive_stanza_id,
            } => Self::OfflineDelivery {
                conversation,
                archive_stanza_id,
            },
            NotificationActivityMutation::NotificationCandidate {
                conversation,
                archive_stanza_id,
                outcome,
            } => Self::NotificationCandidate {
                conversation,
                archive_stanza_id,
                outcome: notification_candidate_outcome_tag(outcome),
            },
        }
    }
}

impl StoredNotificationActivityMutation {
    fn into_domain(self) -> Result<NotificationActivityMutation, EffectIntentCodecError> {
        Ok(match self {
            Self::ChatState { conversation } => {
                NotificationActivityMutation::ChatState { conversation }
            }
            Self::ChatStateGone { conversation } => {
                NotificationActivityMutation::ChatStateGone { conversation }
            }
            Self::ReadMarker { conversation } => {
                NotificationActivityMutation::ReadMarker { conversation }
            }
            Self::OutboundMessage { conversation } => {
                NotificationActivityMutation::OutboundMessage { conversation }
            }
            Self::OfflineDelivery {
                conversation,
                archive_stanza_id,
            } => NotificationActivityMutation::OfflineDelivery {
                conversation,
                archive_stanza_id,
            },
            Self::NotificationCandidate {
                conversation,
                archive_stanza_id,
                outcome,
            } => NotificationActivityMutation::NotificationCandidate {
                conversation,
                archive_stanza_id,
                outcome: notification_candidate_outcome_from_tag(outcome)?,
            },
        })
    }
}

#[derive(Serialize, Deserialize)]
struct StoredRetractionTombstoneMutation {
    archive: BareJid,
    target_stanza_id: StanzaId,
    retraction_stanza_id: StanzaId,
}

impl From<RetractionTombstoneMutation> for StoredRetractionTombstoneMutation {
    fn from(value: RetractionTombstoneMutation) -> Self {
        Self {
            archive: value.archive,
            target_stanza_id: value.target_stanza_id,
            retraction_stanza_id: value.retraction_stanza_id,
        }
    }
}

impl From<StoredRetractionTombstoneMutation> for RetractionTombstoneMutation {
    fn from(value: StoredRetractionTombstoneMutation) -> Self {
        Self {
            archive: value.archive,
            target_stanza_id: value.target_stanza_id,
            retraction_stanza_id: value.retraction_stanza_id,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredGroupDmMembershipGrant {
    room: BareJid,
    invitee: BareJid,
    inviter: BareJid,
}

impl From<GroupDmMembershipGrant> for StoredGroupDmMembershipGrant {
    fn from(value: GroupDmMembershipGrant) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
        }
    }
}

impl From<StoredGroupDmMembershipGrant> for GroupDmMembershipGrant {
    fn from(value: StoredGroupDmMembershipGrant) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
        }
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
        route_identity: StoredEffectMessageIdentity,
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
        kind: u8,
    },
    InboxProject {
        owner: BareJid,
        mutation: StoredInboxProjectionMutation,
    },
    NotificationActivityPreview {
        owner: BareJid,
        mutation: StoredNotificationActivityMutation,
    },
    RetractionTombstone {
        mutation: StoredRetractionTombstoneMutation,
    },
    DmPinMutation {
        first_peer: BareJid,
        second_peer: BareJid,
        target_stanza_id: StanzaId,
        action: u8,
    },
    GroupDmMembershipGrant {
        grant: StoredGroupDmMembershipGrant,
    },
    GroupDmInviteLedger {
        grant: StoredGroupDmMembershipGrant,
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
        action: u8,
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
            Self::RetractionTombstone { .. } => 14,
            Self::DmPinMutation { .. } => 15,
            Self::GroupDmMembershipGrant { .. } => 16,
            Self::GroupDmInviteLedger { .. } => 17,
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
                route_identity,
            } => {
                canonicalize(&mut fanout);
                Self::RouteDirect {
                    recipient,
                    fanout,
                    route_identity: route_identity.into(),
                }
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
                kind,
            } => {
                canonicalize(&mut carbon_recipients);
                Self::Carbons {
                    carbon_recipients,
                    excluded_source,
                    kind: carbon_kind_tag(kind),
                }
            }
            IngressEffectIntent::InboxProject { owner, mutation } => Self::InboxProject {
                owner,
                mutation: mutation.into(),
            },
            IngressEffectIntent::NotificationActivityPreview { owner, mutation } => {
                Self::NotificationActivityPreview {
                    owner,
                    mutation: mutation.into(),
                }
            }
            IngressEffectIntent::RetractionTombstone { mutation } => Self::RetractionTombstone {
                mutation: mutation.into(),
            },
            IngressEffectIntent::DmPinMutation {
                pair,
                target_stanza_id,
                action,
            } => Self::DmPinMutation {
                first_peer: pair.0,
                second_peer: pair.1,
                target_stanza_id,
                action: dm_pin_action_tag(action),
            },
            IngressEffectIntent::GroupDmMembershipGrant { grant } => Self::GroupDmMembershipGrant {
                grant: grant.into(),
            },
            IngressEffectIntent::GroupDmInviteLedger { grant } => Self::GroupDmInviteLedger {
                grant: grant.into(),
            },
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
            IngressEffectIntent::Pin {
                room,
                stanza_id,
                action,
            } => Self::Pin {
                room,
                stanza_id,
                action: pin_action_tag(action),
            },
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
            Self::RouteDirect {
                recipient,
                fanout,
                route_identity,
            } => IngressEffectIntent::RouteDirect {
                recipient,
                fanout,
                route_identity: route_identity.into_domain(),
            },
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
                kind,
            } => IngressEffectIntent::Carbons {
                carbon_recipients,
                excluded_source,
                kind: carbon_kind_from_tag(kind)?,
            },
            Self::InboxProject { owner, mutation } => IngressEffectIntent::InboxProject {
                owner,
                mutation: mutation.into_domain(),
            },
            Self::NotificationActivityPreview { owner, mutation } => {
                IngressEffectIntent::NotificationActivityPreview {
                    owner,
                    mutation: mutation.into_domain()?,
                }
            }
            Self::RetractionTombstone { mutation } => IngressEffectIntent::RetractionTombstone {
                mutation: mutation.into(),
            },
            Self::DmPinMutation {
                first_peer,
                second_peer,
                target_stanza_id,
                action,
            } => IngressEffectIntent::DmPinMutation {
                pair: (first_peer, second_peer),
                target_stanza_id,
                action: dm_pin_action_from_tag(action)?,
            },
            Self::GroupDmMembershipGrant { grant } => IngressEffectIntent::GroupDmMembershipGrant {
                grant: grant.into(),
            },
            Self::GroupDmInviteLedger { grant } => IngressEffectIntent::GroupDmInviteLedger {
                grant: grant.into(),
            },
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
            Self::Pin {
                room,
                stanza_id,
                action,
            } => IngressEffectIntent::Pin {
                room,
                stanza_id,
                action: pin_action_from_tag(action)?,
            },
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

fn carbon_kind_tag(kind: CarbonKind) -> u8 {
    match kind {
        CarbonKind::Sent => 0,
        CarbonKind::Received => 1,
    }
}

fn carbon_kind_from_tag(tag: u8) -> Result<CarbonKind, EffectIntentCodecError> {
    Ok(match tag {
        0 => CarbonKind::Sent,
        1 => CarbonKind::Received,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn pin_action_tag(action: PinAction) -> u8 {
    match action {
        PinAction::Pin => 0,
        PinAction::Unpin => 1,
    }
}

fn pin_action_from_tag(tag: u8) -> Result<PinAction, EffectIntentCodecError> {
    Ok(match tag {
        0 => PinAction::Pin,
        1 => PinAction::Unpin,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn dm_pin_action_storage_identity(action: DmPinMutationAction) -> &'static str {
    match action {
        DmPinMutationAction::Pin => "pin",
        DmPinMutationAction::Unpin => "unpin",
        DmPinMutationAction::RetractionCascadeUnpin => "retraction_cascade_unpin",
    }
}

fn dm_pin_action_tag(action: DmPinMutationAction) -> u8 {
    match action {
        DmPinMutationAction::Pin => 0,
        DmPinMutationAction::Unpin => 1,
        DmPinMutationAction::RetractionCascadeUnpin => 2,
    }
}

fn dm_pin_action_from_tag(tag: u8) -> Result<DmPinMutationAction, EffectIntentCodecError> {
    Ok(match tag {
        0 => DmPinMutationAction::Pin,
        1 => DmPinMutationAction::Unpin,
        2 => DmPinMutationAction::RetractionCascadeUnpin,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn notification_candidate_outcome_tag(outcome: NotificationCandidateOutcome) -> u8 {
    match outcome {
        NotificationCandidateOutcome::Inserted => 0,
        NotificationCandidateOutcome::Duplicate => 1,
    }
}

fn notification_candidate_outcome_from_tag(
    tag: u8,
) -> Result<NotificationCandidateOutcome, EffectIntentCodecError> {
    Ok(match tag {
        0 => NotificationCandidateOutcome::Inserted,
        1 => NotificationCandidateOutcome::Duplicate,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
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
    use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
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
                route_identity: EffectMessageIdentity::stanza(stanza_id()),
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
                kind: CarbonKind::Sent,
            },
            IngressEffectIntent::InboxProject {
                owner: bare("romeo@example.test"),
                mutation: InboxProjectionMutation::Direct {
                    peer: bare("juliet@example.test"),
                    increment_unread: true,
                },
            },
            IngressEffectIntent::NotificationActivityPreview {
                owner: bare("romeo@example.test"),
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: bare("room@conference.example.test"),
                    archive_stanza_id: stanza_id(),
                    outcome: NotificationCandidateOutcome::Inserted,
                },
            },
            IngressEffectIntent::RetractionTombstone {
                mutation: RetractionTombstoneMutation {
                    archive: bare("archive@example.test"),
                    target_stanza_id: StanzaId::new(
                        "target-1",
                        "archive@example.test".parse::<Jid>().expect("valid JID"),
                    ),
                    retraction_stanza_id: stanza_id(),
                },
            },
            IngressEffectIntent::DmPinMutation {
                pair: (bare("juliet@example.test"), bare("romeo@example.test")),
                target_stanza_id: stanza_id(),
                action: DmPinMutationAction::Pin,
            },
            IngressEffectIntent::GroupDmMembershipGrant {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                },
            },
            IngressEffectIntent::GroupDmInviteLedger {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                },
            },
            IngressEffectIntent::CallSignal {
                recipient: full("romeo@example.test/phone"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::Pin {
                room: bare("room@conference.example.test"),
                stanza_id: stanza_id(),
                action: PinAction::Pin,
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
            r#"{"version":1,"intent":{"type":"route_direct","recipient":"romeo@example.test","fanout":["romeo@example.test/phone"],"route_identity":{"type":"stanza_id","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"route_muc_groupchat","room":"room@conference.example.test","occupants":["juliet@example.test/laptop"],"reflection":"romeo@example.test/phone","room_generation":7}}"#,
            r#"{"version":1,"intent":{"type":"route_occupant_pm","recipient":"juliet@example.test/laptop","sender":"romeo@example.test/phone"}}"#,
            r#"{"version":1,"intent":{"type":"dispatch_to_room_remote","room":"room@conference.example.test","relay_target":{"node_id":"relay-node","node_epoch":"relay-epoch"}}}"#,
            r#"{"version":1,"intent":{"type":"recipient_sm_append","stream":"stream-1","append_identity":0}}"#,
            r#"{"version":1,"intent":{"type":"carbons","carbon_recipients":["romeo@example.test/phone"],"excluded_source":"romeo@example.test/laptop","kind":0}}"#,
            r#"{"version":1,"intent":{"type":"inbox_project","owner":"romeo@example.test","mutation":{"type":"direct","peer":"juliet@example.test","increment_unread":true}}}"#,
            r#"{"version":1,"intent":{"type":"notification_activity_preview","owner":"romeo@example.test","mutation":{"type":"notification_candidate","conversation":"room@conference.example.test","archive_stanza_id":{"id":"stable-1","by":"archive@example.test"},"outcome":0}}}"#,
            r#"{"version":1,"intent":{"type":"retraction_tombstone","mutation":{"archive":"archive@example.test","target_stanza_id":{"id":"target-1","by":"archive@example.test"},"retraction_stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"dm_pin_mutation","first_peer":"juliet@example.test","second_peer":"romeo@example.test","target_stanza_id":{"id":"stable-1","by":"archive@example.test"},"action":0}}"#,
            r#"{"version":1,"intent":{"type":"group_dm_membership_grant","grant":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"group_dm_invite_ledger","grant":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"call_signal","recipient":"romeo@example.test/phone","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"pin","room":"room@conference.example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"},"action":0}}"#,
            r#"{"version":1,"intent":{"type":"extension","recipient":"romeo@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"error_reply","recipient":"romeo@example.test/phone","error":{"error_type":3,"condition":13,"texts":[{"lang":"nb","text":"moved"}],"condition_payload":{"type":"redirect","new_address":"xmpp:romeo@example.test/mobile"}}}}"#,
        ];
        for (intent, expected) in samples().into_iter().zip(golden) {
            let encoded = intent.encode_v1().expect("encode sample");
            assert_eq!(encoded.payload(), expected.as_bytes());
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
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
            route_identity: EffectMessageIdentity::origin(OriginId::new("client-origin")),
        };
        let second = IngressEffectIntent::RouteDirect {
            recipient: bare("romeo@example.test"),
            fanout: vec![
                full("romeo@example.test/laptop"),
                full("romeo@example.test/phone"),
            ],
            route_identity: EffectMessageIdentity::origin(OriginId::new("client-origin")),
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
            IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                .expect("decode sample"),
            intent
        );
        assert_eq!(
            encoded.payload(),
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
    fn archive_route_pin_and_error_keys_preserve_repeated_effect_identities() {
        let archive_one = IngressEffectIntent::ArchiveAuthoritative {
            archive: bare("archive@example.test"),
            stanza_id: stanza_id(),
            by: bare("archive@example.test"),
        };
        let archive_two = IngressEffectIntent::ArchiveAuthoritative {
            archive: bare("archive@example.test"),
            stanza_id: StanzaId::new(
                "stable-2",
                "archive@example.test".parse::<Jid>().expect("valid JID"),
            ),
            by: bare("archive@example.test"),
        };
        assert_ne!(archive_one.semantic_key(), archive_two.semantic_key());

        let route_one = IngressEffectIntent::RouteDirect {
            recipient: bare("romeo@example.test"),
            fanout: vec![full("romeo@example.test/phone")],
            route_identity: EffectMessageIdentity::origin(OriginId::new("origin-1")),
        };
        let route_two = IngressEffectIntent::RouteDirect {
            recipient: bare("romeo@example.test"),
            fanout: vec![full("romeo@example.test/phone")],
            route_identity: EffectMessageIdentity::origin(OriginId::new("origin-2")),
        };
        assert_ne!(route_one.semantic_key(), route_two.semantic_key());

        let pin = IngressEffectIntent::Pin {
            room: bare("room@conference.example.test"),
            stanza_id: stanza_id(),
            action: PinAction::Pin,
        };
        let unpin = IngressEffectIntent::Pin {
            room: bare("room@conference.example.test"),
            stanza_id: stanza_id(),
            action: PinAction::Unpin,
        };
        assert_ne!(pin.semantic_key(), unpin.semantic_key());

        let warning_one = IngressEffectIntent::ErrorReply {
            recipient: full("romeo@example.test/phone"),
            error: FrozenStanzaError::from(StanzaErrorCondition::PolicyViolation)
                .with_text("en", "warning one"),
        };
        let warning_two = IngressEffectIntent::ErrorReply {
            recipient: full("romeo@example.test/phone"),
            error: FrozenStanzaError::from(StanzaErrorCondition::PolicyViolation)
                .with_text("en", "warning two"),
        };
        assert_ne!(warning_one.semantic_key(), warning_two.semantic_key());
    }

    #[test]
    fn typed_inbox_notification_and_carbons_effects_round_trip() {
        let inbox = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::GroupchatChannelAndThread {
                room: bare("room@conference.example.test"),
                thread_id: "thread-1".to_string(),
                increment_unread: true,
            },
        };
        let notification = IngressEffectIntent::NotificationActivityPreview {
            owner: bare("romeo@example.test"),
            mutation: NotificationActivityMutation::OfflineDelivery {
                conversation: bare("juliet@example.test"),
                archive_stanza_id: stanza_id(),
            },
        };
        let carbons = IngressEffectIntent::Carbons {
            carbon_recipients: vec![full("romeo@example.test/phone")],
            excluded_source: full("romeo@example.test/laptop"),
            kind: CarbonKind::Received,
        };

        for intent in [inbox, notification, carbons] {
            let encoded = intent.encode_v1().expect("encode typed effect");
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                    .expect("decode typed effect"),
                intent
            );
        }
    }

    #[test]
    fn with_encoded_v1_exposes_only_storage_edge_fields() {
        let intent = samples().remove(0);
        let encoded = intent.encode_v1().expect("encode sample");
        let via_visitor = intent
            .with_encoded_v1(|kind, payload| (kind, payload.to_vec()))
            .expect("visitor encode");
        assert_eq!(via_visitor.0, encoded.kind());
        assert_eq!(via_visitor.1, encoded.payload());
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
            IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                .expect("decode sample"),
            intent
        );
        assert_eq!(
            encoded.payload(),
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
            .payload()
            .windows(11)
            .position(|part| part == b"\"version\":1")
            .expect("version marker");
        let mut version_payload = encoded.payload().to_vec();
        version_payload[unknown_version + 10] = b'2';
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind(), &version_payload),
            Err(EffectIntentCodecError::UnknownPayloadVersion(2))
        );
        assert_eq!(
            IngressEffectIntent::decode_v1(99, encoded.payload()),
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
                encoded.kind(),
                &vec![b'x'; MAX_EFFECT_INTENT_PAYLOAD_BYTES + 1],
            ),
            Err(EffectIntentCodecError::PayloadTooLarge)
        );
    }
}
