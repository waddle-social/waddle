//! Immutable, typed descriptions of effects selected during ingress.

use std::{cmp::Ordering, collections::BTreeMap, ops::Deref};

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use waddle_xmpp_core::mam::{RichMessageId, ThreadId};
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};
use xmpp_parsers::{
    message::Lang,
    stanza_error::{DefinedCondition, ErrorType as XmppStanzaErrorType, StanzaError},
};

use crate::{
    error::StanzaErrorCondition,
    inbox::InboxEntry,
    ingress::EntityGeneration,
    muc::{pin::PinnedEntry, SubjectState},
    pending_delivery::{PendingRowId, SmSessionId},
    protocol::CarbonKind,
    xep::{xep0085::ChatState, CallThreadDuration, CallThreadMedia},
};

/// Largest accepted version-one storage payload, matching the database check.
pub const MAX_EFFECT_INTENT_PAYLOAD_BYTES: usize = 65_536;

/// XEP-0191 application-condition namespace preserved in frozen errors.
pub const NS_XEP0191_BLOCKING_ERRORS: &str = "urn:xmpp:blocking:errors";

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
pub enum NotificationCandidateOutcome {
    Inserted,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationActivityMutation {
    ChatState {
        conversation: BareJid,
        state: ChatState,
        committed_at_ms: i64,
    },
    ChatStateGone {
        conversation: BareJid,
        committed_at_ms: i64,
    },
    ReadMarker {
        conversation: BareJid,
        committed_at_ms: i64,
    },
    OutboundMessage {
        conversation: BareJid,
        committed_at_ms: i64,
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
            Self::ChatState {
                conversation,
                state,
                committed_at_ms,
            } => format!(
                "chat_state|{}|{}|{}",
                conversation,
                chat_state_storage_identity(*state),
                committed_at_ms,
            ),
            Self::ChatStateGone {
                conversation,
                committed_at_ms,
            } => {
                format!("chat_state_gone|{}|{}", conversation, committed_at_ms)
            }
            Self::ReadMarker {
                conversation,
                committed_at_ms,
            } => format!("read_marker|{}|{}", conversation, committed_at_ms),
            Self::OutboundMessage {
                conversation,
                committed_at_ms,
            } => {
                format!("outbound_message|{}|{}", conversation, committed_at_ms)
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
        entry: InboxEntry,
        increment_unread: bool,
    },
    GroupchatChannel {
        room: BareJid,
        increment_unread: bool,
    },
    GroupchatThread {
        room: BareJid,
        thread_id: ThreadId,
    },
    GroupchatChannelRead {
        room: BareJid,
    },
    GroupchatThreadRead {
        room: BareJid,
        thread_id: ThreadId,
    },
    GroupchatChannelAndThread {
        room: BareJid,
        thread_id: ThreadId,
        increment_unread: bool,
    },
    DirectCallThreadAnchor {
        peer: BareJid,
        thread_id: ThreadId,
        archive_stanza_id: StanzaId,
        media: CallThreadMedia,
        last_updated: i64,
    },
    DirectCallThreadEnded {
        peer: BareJid,
        thread_id: ThreadId,
        ended: chrono::DateTime<chrono::Utc>,
        duration: CallThreadDuration,
    },
}

impl InboxProjectionMutation {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Direct {
                entry,
                increment_unread,
            } => format!(
                "direct|{}|{}",
                inbox_entry_storage_identity(entry),
                increment_unread
            ),
            Self::GroupchatChannel {
                room,
                increment_unread,
            } => format!("groupchat_channel|{}|{}", room, increment_unread),
            Self::GroupchatThread { room, thread_id } => {
                format!("groupchat_thread|{}|{}", room, thread_id.as_str())
            }
            Self::GroupchatChannelRead { room } => {
                format!("groupchat_channel_read|{}", room)
            }
            Self::GroupchatThreadRead { room, thread_id } => {
                format!("groupchat_thread_read|{}|{}", room, thread_id.as_str())
            }
            Self::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => format!(
                "groupchat_channel_and_thread|{}|{}|{}",
                room,
                thread_id.as_str(),
                increment_unread
            ),
            Self::DirectCallThreadAnchor {
                peer,
                thread_id,
                archive_stanza_id,
                media,
                last_updated,
            } => format!(
                "direct_call_thread_anchor|{}|{}|{}|{}|{}|{}",
                peer,
                thread_id.as_str(),
                stanza_storage_identity(archive_stanza_id),
                media.audio,
                media.video,
                last_updated,
            ),
            Self::DirectCallThreadEnded {
                peer,
                thread_id,
                ended,
                duration,
            } => format!(
                "direct_call_thread_ended|{}|{}|{}|{}",
                peer,
                thread_id.as_str(),
                ended.to_rfc3339(),
                duration.as_str(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupchatNotificationRecoveryAction {
    Recorded,
    Completed,
}

impl GroupchatNotificationRecoveryAction {
    pub fn storage_identity(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupchatNotificationRecoveryMutation {
    pub recipient: BareJid,
    pub room: BareJid,
    pub thread_id: Option<ThreadId>,
    pub archive_stanza_id: StanzaId,
    pub sender: Jid,
    pub is_live_occupant: bool,
    pub room_members_only: bool,
    pub sender_can_broadcast_channel_mention: bool,
    pub created_at_ms: i64,
    pub action: GroupchatNotificationRecoveryAction,
}

impl GroupchatNotificationRecoveryMutation {
    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.action.storage_identity(),
            self.recipient,
            self.room,
            self.thread_id.as_ref().map_or("", ThreadId::as_str),
            stanza_storage_identity(&self.archive_stanza_id),
            self.sender,
            self.is_live_occupant,
            self.room_members_only,
            self.sender_can_broadcast_channel_mention,
            self.created_at_ms
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingDeliveryMutation {
    Archived {
        recipient: BareJid,
        row_id: PendingRowId,
        archive_stanza_id: StanzaId,
    },
    Transient {
        recipient: BareJid,
        row_id: PendingRowId,
    },
}

impl PendingDeliveryMutation {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Archived {
                recipient,
                row_id,
                archive_stanza_id,
            } => format!(
                "archived|{}|{}|{}",
                recipient,
                row_id.as_str(),
                stanza_storage_identity(archive_stanza_id)
            ),
            Self::Transient { recipient, row_id } => {
                format!("transient|{}|{}", recipient, row_id.as_str())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TombstoneReplayTarget {
    Groupchat {
        stanza_id: String,
        room: BareJid,
    },
    Direct {
        wire_id: String,
        author: BareJid,
        archive: BareJid,
    },
}

impl TombstoneReplayTarget {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Groupchat { stanza_id, room } => {
                format!("groupchat|{}|{}", room, stanza_id)
            }
            Self::Direct {
                wire_id,
                author,
                archive,
            } => format!("direct|{}|{}|{}", archive, author, wire_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneReplaySmEntry {
    pub stream: SmSessionId,
    pub sequence: u32,
}

impl TombstoneReplaySmEntry {
    pub fn storage_identity(&self) -> String {
        format!("{}|{:010}", self.stream.as_str(), self.sequence)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPreviewMediaRefState {
    Current,
    Unreferenced,
}

impl LinkPreviewMediaRefState {
    pub fn storage_identity(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unreferenced => "unreferenced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewMediaRefMutation {
    pub upload_slot_id: Uuid,
    pub archive: BareJid,
    pub message_id: RichMessageId,
    pub current_archive_stanza_id: StanzaId,
    pub state: LinkPreviewMediaRefState,
}

impl LinkPreviewMediaRefMutation {
    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.upload_slot_id,
            self.archive,
            self.message_id.as_str(),
            stanza_storage_identity(&self.current_archive_stanza_id),
            self.state.storage_identity()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmPinMutationAction {
    Pin { entry: PinnedEntry },
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
    pub history_visibility: GroupDmHistoryVisibility,
}

impl GroupDmMembershipGrant {
    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.room,
            self.invitee,
            self.inviter,
            self.history_visibility.storage_identity()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupDmHistoryVisibility {
    Full,
    FromJoin {
        visible_after: chrono::DateTime<chrono::Utc>,
    },
}

impl GroupDmHistoryVisibility {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Full => "full".to_string(),
            Self::FromJoin { visible_after } => {
                format!("from_join|{}", visible_after.to_rfc3339())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MucInviteMembershipGrant {
    pub room: BareJid,
    pub invitee: BareJid,
    pub inviter: BareJid,
}

impl MucInviteMembershipGrant {
    pub fn storage_identity(&self) -> String {
        format!("{}|{}|{}", self.room, self.invitee, self.inviter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MucInviteLedgerAction {
    Recorded,
    Claimed,
}

impl MucInviteLedgerAction {
    pub fn storage_identity(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::Claimed => "claimed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MucInviteLedgerMutation {
    pub room: BareJid,
    pub invitee: BareJid,
    pub inviter: BareJid,
    pub action: MucInviteLedgerAction,
    pub recorded_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl MucInviteLedgerMutation {
    pub fn storage_identity(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            self.room,
            self.invitee,
            self.inviter,
            self.action.storage_identity(),
            self.recorded_at
                .as_ref()
                .map(chrono::DateTime::to_rfc3339)
                .unwrap_or_default()
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomPinMutation {
    Pin { entry: PinnedEntry },
    Unpin { target_stanza_id: StanzaId },
}

impl RoomPinMutation {
    pub fn storage_identity(&self) -> String {
        match self {
            Self::Pin { entry } => {
                let preview = &entry.preview;
                format!(
                    "pin|{}|{}|{}|{}|{}|{}|{}",
                    stanza_storage_identity(&entry.target_stanza_id),
                    entry.pinner_jid,
                    entry.pinned_at.to_rfc3339(),
                    preview.author_jid,
                    preview.author_nick.as_deref().unwrap_or(""),
                    preview.text,
                    preview.message_timestamp.to_rfc3339(),
                )
            }
            Self::Unpin { target_stanza_id } => {
                format!("unpin|{}", stanza_storage_identity(target_stanza_id))
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredDmPinMutationAction {
    Pin { entry: PinnedEntry },
    Unpin,
    RetractionCascadeUnpin,
}

impl From<DmPinMutationAction> for StoredDmPinMutationAction {
    fn from(value: DmPinMutationAction) -> Self {
        match value {
            DmPinMutationAction::Pin { entry } => Self::Pin { entry },
            DmPinMutationAction::Unpin => Self::Unpin,
            DmPinMutationAction::RetractionCascadeUnpin => Self::RetractionCascadeUnpin,
        }
    }
}

impl From<StoredDmPinMutationAction> for DmPinMutationAction {
    fn from(value: StoredDmPinMutationAction) -> Self {
        match value {
            StoredDmPinMutationAction::Pin { entry } => Self::Pin { entry },
            StoredDmPinMutationAction::Unpin => Self::Unpin,
            StoredDmPinMutationAction::RetractionCascadeUnpin => Self::RetractionCascadeUnpin,
        }
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
    /// XEP-0191 §3.2 application condition. This has no fields, but it is
    /// semantically distinct from an otherwise-identical not-acceptable.
    Blocked,
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
            _ if error
                .other
                .as_ref()
                .is_some_and(|condition| condition.is("blocked", NS_XEP0191_BLOCKING_ERRORS)) =>
            {
                Some(FrozenStanzaErrorConditionPayload::Blocked)
            }
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
        let other = match self.condition_payload.as_ref() {
            Some(FrozenStanzaErrorConditionPayload::Blocked) => {
                Some(Element::builder("blocked", NS_XEP0191_BLOCKING_ERRORS).build())
            }
            _ => None,
        };
        StanzaError {
            type_: self.error_type.to_xmpp(),
            by: None,
            defined_condition: self.to_xmpp_condition(),
            texts,
            other,
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
            Some(FrozenStanzaErrorConditionPayload::Blocked) => "blocked".to_string(),
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

fn chat_state_storage_identity(state: ChatState) -> &'static str {
    match state {
        ChatState::Active => "active",
        ChatState::Composing => "composing",
        ChatState::Paused => "paused",
        ChatState::Inactive => "inactive",
        ChatState::Gone => "gone",
    }
}

fn inbox_entry_storage_identity(entry: &InboxEntry) -> String {
    serde_json::to_string(entry).expect("InboxEntry serialization is infallible")
}

/// A frozen effect decision; it carries no executable callback or mutable
/// lookup and can therefore be durably replayed without re-deriving policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressEffectIntent {
    ArchiveAuthoritative {
        archive: BareJid,
        stanza_id: StanzaId,
        by: BareJid,
        archived_at: chrono::DateTime<chrono::Utc>,
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
        route_identity: EffectMessageIdentity,
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
    GroupchatNotificationRecovery {
        mutation: GroupchatNotificationRecoveryMutation,
    },
    PendingDelivery {
        mutation: PendingDeliveryMutation,
    },
    LinkPreviewMediaRef {
        mutation: LinkPreviewMediaRefMutation,
    },
    RetractionTombstone {
        mutation: RetractionTombstoneMutation,
    },
    DmPinMutation {
        pair: (BareJid, BareJid),
        target_stanza_id: StanzaId,
        action: DmPinMutationAction,
    },
    MucInviteMembershipGrant {
        grant: MucInviteMembershipGrant,
    },
    MucInviteLedger {
        mutation: MucInviteLedgerMutation,
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
        mutation: RoomPinMutation,
    },
    Extension {
        recipient: BareJid,
        stanza_id: StanzaId,
    },
    TombstoneReplayDeletion {
        target: TombstoneReplayTarget,
        sm_entries: Vec<TombstoneReplaySmEntry>,
        pending_rows: Vec<PendingRowId>,
    },
    ErrorReply {
        recipient: FullJid,
        error: FrozenStanzaError,
    },
}

/// Closed classification of an ingress effect, independent of its storage tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IngressEffectKind {
    ArchiveAuthoritative,
    RouteDirect,
    RouteMucGroupchat,
    RouteOccupantPm,
    DispatchToRoomRemote,
    RecipientSmAppend,
    Carbons,
    InboxProject,
    NotificationActivityPreview,
    GroupchatNotificationRecovery,
    PendingDelivery,
    LinkPreviewMediaRef,
    RetractionTombstone,
    DmPinMutation,
    MucInviteMembershipGrant,
    MucInviteLedger,
    GroupDmMembershipGrant,
    GroupDmInviteLedger,
    RoomSubjectMutation,
    CallSignal,
    Pin,
    Extension,
    TombstoneReplayDeletion,
    ErrorReply,
}

/// The entity assigning an effect's identity. Audience and mutable policy are
/// deliberately excluded so reconciliation can preserve the recorded decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectAuthorityKey {
    Archive {
        by: BareJid,
        archive: BareJid,
    },
    Route {
        recipient: Jid,
    },
    Inbox {
        owner: BareJid,
        partner: BareJid,
        thread: Option<ThreadId>,
    },
    Room {
        kind: IngressEffectKind,
        room: BareJid,
    },
    Recipient {
        kind: IngressEffectKind,
        recipient: Jid,
    },
    Stream {
        stream: SmSessionId,
        append: RecipientSmAppendIdentity,
    },
    Carbons {
        source: FullJid,
        kind: CarbonKind,
    },
    Conversation {
        owner: BareJid,
        conversation: BareJid,
    },
    Recovery {
        recipient: BareJid,
        room: BareJid,
        thread: Option<ThreadId>,
    },
    Media {
        archive: BareJid,
        slot: Uuid,
    },
    Retraction {
        archive: BareJid,
        target: StanzaId,
    },
    DirectPin {
        pair: (BareJid, BareJid),
        target: StanzaId,
    },
    Membership {
        kind: IngressEffectKind,
        room: BareJid,
        invitee: BareJid,
    },
    Tombstone {
        target: TombstoneReplayTarget,
    },
    ErrorReply {
        recipient: FullJid,
    },
}

/// Closed semantic identity used to deduplicate a stanza's frozen effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressEffectKey {
    ArchiveAuthoritative(BareJid, String),
    RouteDirect(BareJid, String),
    RouteMucGroupchat(BareJid, String),
    RouteOccupantPm(FullJid),
    DispatchToRoomRemote(BareJid, RelayTargetIdentity),
    RecipientSmAppend(SmSessionId, RecipientSmAppendIdentity),
    Carbons(FullJid, CarbonKind),
    InboxProject(BareJid, String),
    NotificationActivityPreview(BareJid, String),
    GroupchatNotificationRecovery(String),
    PendingDelivery(String),
    LinkPreviewMediaRef(String),
    RetractionTombstone(String),
    DmPinMutation(String),
    MucInviteMembershipGrant(String),
    MucInviteLedger(String),
    GroupDmMembershipGrant(String),
    GroupDmInviteLedger(String),
    RoomSubjectMutation(BareJid),
    CallSignal(FullJid),
    Pin(BareJid, String),
    Extension(BareJid),
    TombstoneReplayDeletion(String),
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
            Self::RouteMucGroupchat(room, route_identity) => {
                format!("{}|{}", room, route_identity)
            }
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
            Self::GroupchatNotificationRecovery(identity) => identity.clone(),
            Self::PendingDelivery(identity) => identity.clone(),
            Self::LinkPreviewMediaRef(identity) => identity.clone(),
            Self::RetractionTombstone(identity) => identity.clone(),
            Self::DmPinMutation(identity) => identity.clone(),
            Self::MucInviteMembershipGrant(identity) => identity.clone(),
            Self::MucInviteLedger(identity) => identity.clone(),
            Self::GroupDmMembershipGrant(identity) => identity.clone(),
            Self::GroupDmInviteLedger(identity) => identity.clone(),
            Self::RoomSubjectMutation(value) => value.to_string(),
            Self::CallSignal(value) => value.to_string(),
            Self::Pin(room, pin_identity) => format!("{}|{}", room, pin_identity),
            Self::Extension(value) => value.to_string(),
            Self::TombstoneReplayDeletion(identity) => identity.clone(),
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
            Self::GroupchatNotificationRecovery(..) => 9,
            Self::PendingDelivery(..) => 10,
            Self::LinkPreviewMediaRef(..) => 11,
            Self::RetractionTombstone(..) => 12,
            Self::DmPinMutation(..) => 13,
            Self::MucInviteMembershipGrant(..) => 14,
            Self::MucInviteLedger(..) => 15,
            Self::GroupDmMembershipGrant(..) => 16,
            Self::GroupDmInviteLedger(..) => 17,
            Self::RoomSubjectMutation(..) => 18,
            Self::CallSignal(..) => 19,
            Self::Pin(..) => 20,
            Self::Extension(..) => 21,
            Self::TombstoneReplayDeletion(..) => 22,
            Self::ErrorReply(..) => 23,
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
    /// Representative values for every stable V1 kind, used by storage
    /// integration tests to keep the codec and ledger contract in sync.
    #[doc(hidden)]
    pub fn storage_round_trip_samples() -> Vec<Self> {
        let bare = |value: &str| value.parse::<BareJid>().expect("valid fixture bare JID");
        let full = |value: &str| value.parse::<FullJid>().expect("valid fixture full JID");
        let stanza = || {
            StanzaId::new(
                "stable-1",
                "archive@example.test"
                    .parse::<Jid>()
                    .expect("valid fixture JID"),
            )
        };
        let direct_entry = || {
            InboxEntry::new(
                bare("juliet@example.test"),
                crate::inbox::ConversationKind::Direct,
                "stable-1",
                1_752_768_000,
            )
            .with_unread(3)
            .with_preview("important hello")
        };
        let pinned_entry = || PinnedEntry {
            target_stanza_id: stanza(),
            pinner_jid: bare("romeo@example.test"),
            pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                .expect("timestamp")
                .with_timezone(&chrono::Utc),
            preview: crate::muc::pin::PinPreview::new(
                bare("juliet@example.test"),
                Some("Juliet".to_string()),
                "important",
                chrono::DateTime::parse_from_rfc3339("2025-07-27T11:59:00Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc),
            ),
        };
        vec![
            Self::ArchiveAuthoritative {
                archive: bare("archive@example.test"),
                stanza_id: stanza(),
                by: bare("archive@example.test"),
                archived_at: chrono::DateTime::from_timestamp(1_753_617_600, 0)
                    .expect("fixture timestamp"),
            },
            Self::RouteDirect {
                recipient: bare("romeo@example.test"),
                fanout: vec![full("romeo@example.test/phone")],
                route_identity: EffectMessageIdentity::stanza(stanza()),
            },
            Self::RouteMucGroupchat {
                room: bare("room@conference.example.test"),
                occupants: vec![full("juliet@example.test/laptop")],
                reflection: full("romeo@example.test/phone"),
                room_generation: EntityGeneration::from_storage(7),
                route_identity: EffectMessageIdentity::stanza(stanza()),
            },
            Self::RouteOccupantPm {
                recipient: full("juliet@example.test/laptop"),
                sender: full("romeo@example.test/phone"),
            },
            Self::DispatchToRoomRemote {
                room: bare("room@conference.example.test"),
                relay_target: RelayTargetIdentity::owner_node("relay-node", "relay-epoch"),
            },
            Self::RecipientSmAppend {
                stream: SmSessionId::new("stream-1"),
                append_identity: RecipientSmAppendIdentity::new(0),
            },
            Self::Carbons {
                carbon_recipients: vec![full("romeo@example.test/phone")],
                excluded_source: full("romeo@example.test/laptop"),
                kind: CarbonKind::Sent,
            },
            Self::InboxProject {
                owner: bare("romeo@example.test"),
                mutation: InboxProjectionMutation::Direct {
                    entry: direct_entry(),
                    increment_unread: true,
                },
            },
            Self::NotificationActivityPreview {
                owner: bare("romeo@example.test"),
                mutation: NotificationActivityMutation::NotificationCandidate {
                    conversation: bare("room@conference.example.test"),
                    archive_stanza_id: stanza(),
                    outcome: NotificationCandidateOutcome::Inserted,
                },
            },
            Self::GroupchatNotificationRecovery {
                mutation: GroupchatNotificationRecoveryMutation {
                    recipient: bare("romeo@example.test"),
                    room: bare("room@conference.example.test"),
                    thread_id: Some(ThreadId::new("thread-1").expect("fixture thread id")),
                    archive_stanza_id: stanza(),
                    sender: "juliet@example.test/balcony"
                        .parse::<Jid>()
                        .expect("valid fixture JID"),
                    is_live_occupant: true,
                    room_members_only: false,
                    sender_can_broadcast_channel_mention: true,
                    created_at_ms: 1_753_620_000_000,
                    action: GroupchatNotificationRecoveryAction::Recorded,
                },
            },
            Self::PendingDelivery {
                mutation: PendingDeliveryMutation::Archived {
                    recipient: bare("romeo@example.test"),
                    row_id: PendingRowId::new("pending-row-1"),
                    archive_stanza_id: stanza(),
                },
            },
            Self::LinkPreviewMediaRef {
                mutation: LinkPreviewMediaRefMutation {
                    upload_slot_id: Uuid::parse_str("d5c7a44f-7c8c-4587-b0fb-f0e68444d36a")
                        .expect("fixture UUID"),
                    archive: bare("room@conference.example.test"),
                    message_id: RichMessageId::new("client-msg-1").expect("fixture message id"),
                    current_archive_stanza_id: stanza(),
                    state: LinkPreviewMediaRefState::Current,
                },
            },
            Self::RetractionTombstone {
                mutation: RetractionTombstoneMutation {
                    archive: bare("archive@example.test"),
                    target_stanza_id: StanzaId::new(
                        "target-1",
                        "archive@example.test"
                            .parse::<Jid>()
                            .expect("valid fixture JID"),
                    ),
                    retraction_stanza_id: stanza(),
                },
            },
            Self::DmPinMutation {
                pair: (bare("juliet@example.test"), bare("romeo@example.test")),
                target_stanza_id: stanza(),
                action: DmPinMutationAction::Pin {
                    entry: pinned_entry(),
                },
            },
            Self::MucInviteMembershipGrant {
                grant: MucInviteMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                },
            },
            Self::MucInviteLedger {
                mutation: MucInviteLedgerMutation {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    action: MucInviteLedgerAction::Recorded,
                    recorded_at: Some(
                        chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                    ),
                },
            },
            Self::GroupDmMembershipGrant {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    history_visibility: GroupDmHistoryVisibility::FromJoin {
                        visible_after: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("fixture timestamp")
                            .with_timezone(&chrono::Utc),
                    },
                },
            },
            Self::GroupDmInviteLedger {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    history_visibility: GroupDmHistoryVisibility::FromJoin {
                        visible_after: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("fixture timestamp")
                            .with_timezone(&chrono::Utc),
                    },
                },
            },
            Self::RoomSubjectMutation {
                room: bare("room@conference.example.test"),
                state: SubjectState {
                    texts: crate::muc::RoomSubjectTexts::new(),
                    setter: bare("romeo@example.test"),
                    setter_nick: "romeo".to_owned(),
                    set_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                        .expect("fixture timestamp")
                        .with_timezone(&chrono::Utc),
                },
            },
            Self::CallSignal {
                recipient: full("romeo@example.test/phone"),
                stanza_id: stanza(),
            },
            Self::Pin {
                room: bare("room@conference.example.test"),
                mutation: RoomPinMutation::Pin {
                    entry: PinnedEntry {
                        target_stanza_id: stanza(),
                        pinner_jid: bare("romeo@example.test"),
                        pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("fixture timestamp")
                            .with_timezone(&chrono::Utc),
                        preview: crate::muc::pin::PinPreview::new(
                            bare("juliet@example.test"),
                            Some("Juliet".to_string()),
                            "important",
                            chrono::DateTime::parse_from_rfc3339("2025-07-27T11:59:00Z")
                                .expect("fixture timestamp")
                                .with_timezone(&chrono::Utc),
                        ),
                    },
                },
            },
            Self::Extension {
                recipient: bare("romeo@example.test"),
                stanza_id: stanza(),
            },
            Self::TombstoneReplayDeletion {
                target: TombstoneReplayTarget::Direct {
                    wire_id: "wire-1".to_owned(),
                    author: bare("juliet@example.test"),
                    archive: bare("romeo@example.test"),
                },
                sm_entries: vec![TombstoneReplaySmEntry {
                    stream: SmSessionId::new("stream-1"),
                    sequence: 42,
                }],
                pending_rows: vec![PendingRowId::new("pending-row-1")],
            },
            Self::ErrorReply {
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
                                .expect("valid fixture URI"),
                        ),
                    },
                ),
            },
        ]
    }

    pub fn kind(&self) -> IngressEffectKind {
        match self {
            Self::ArchiveAuthoritative { .. } => IngressEffectKind::ArchiveAuthoritative,
            Self::RouteDirect { .. } => IngressEffectKind::RouteDirect,
            Self::RouteMucGroupchat { .. } => IngressEffectKind::RouteMucGroupchat,
            Self::RouteOccupantPm { .. } => IngressEffectKind::RouteOccupantPm,
            Self::DispatchToRoomRemote { .. } => IngressEffectKind::DispatchToRoomRemote,
            Self::RecipientSmAppend { .. } => IngressEffectKind::RecipientSmAppend,
            Self::Carbons { .. } => IngressEffectKind::Carbons,
            Self::InboxProject { .. } => IngressEffectKind::InboxProject,
            Self::NotificationActivityPreview { .. } => {
                IngressEffectKind::NotificationActivityPreview
            }
            Self::GroupchatNotificationRecovery { .. } => {
                IngressEffectKind::GroupchatNotificationRecovery
            }
            Self::PendingDelivery { .. } => IngressEffectKind::PendingDelivery,
            Self::LinkPreviewMediaRef { .. } => IngressEffectKind::LinkPreviewMediaRef,
            Self::RetractionTombstone { .. } => IngressEffectKind::RetractionTombstone,
            Self::DmPinMutation { .. } => IngressEffectKind::DmPinMutation,
            Self::MucInviteMembershipGrant { .. } => IngressEffectKind::MucInviteMembershipGrant,
            Self::MucInviteLedger { .. } => IngressEffectKind::MucInviteLedger,
            Self::GroupDmMembershipGrant { .. } => IngressEffectKind::GroupDmMembershipGrant,
            Self::GroupDmInviteLedger { .. } => IngressEffectKind::GroupDmInviteLedger,
            Self::RoomSubjectMutation { .. } => IngressEffectKind::RoomSubjectMutation,
            Self::CallSignal { .. } => IngressEffectKind::CallSignal,
            Self::Pin { .. } => IngressEffectKind::Pin,
            Self::Extension { .. } => IngressEffectKind::Extension,
            Self::TombstoneReplayDeletion { .. } => IngressEffectKind::TombstoneReplayDeletion,
            Self::ErrorReply { .. } => IngressEffectKind::ErrorReply,
        }
    }

    pub fn authority_key(&self) -> EffectAuthorityKey {
        let kind = self.kind();
        match self {
            Self::ArchiveAuthoritative { archive, by, .. } => EffectAuthorityKey::Archive {
                by: by.clone(),
                archive: archive.clone(),
            },
            Self::RouteDirect { recipient, .. } => EffectAuthorityKey::Route {
                recipient: recipient.clone().into(),
            },
            Self::RouteOccupantPm { recipient, .. } => EffectAuthorityKey::Route {
                recipient: recipient.clone().into(),
            },
            Self::RouteMucGroupchat { room, .. }
            | Self::DispatchToRoomRemote { room, .. }
            | Self::RoomSubjectMutation { room, .. }
            | Self::Pin { room, .. } => EffectAuthorityKey::Room {
                kind,
                room: room.clone(),
            },
            Self::RecipientSmAppend {
                stream,
                append_identity,
            } => EffectAuthorityKey::Stream {
                stream: stream.clone(),
                append: *append_identity,
            },
            Self::Carbons {
                excluded_source,
                kind,
                ..
            } => EffectAuthorityKey::Carbons {
                source: excluded_source.clone(),
                kind: *kind,
            },
            Self::InboxProject { owner, mutation } => {
                let (partner, thread) = match mutation {
                    InboxProjectionMutation::Direct { entry, .. } => (
                        entry.partner.clone(),
                        entry.thread_id.clone().and_then(ThreadId::new),
                    ),
                    InboxProjectionMutation::GroupchatChannel { room, .. }
                    | InboxProjectionMutation::GroupchatChannelRead { room } => {
                        (room.clone(), None)
                    }
                    InboxProjectionMutation::GroupchatThread { room, thread_id }
                    | InboxProjectionMutation::GroupchatThreadRead { room, thread_id }
                    | InboxProjectionMutation::GroupchatChannelAndThread {
                        room, thread_id, ..
                    } => (room.clone(), Some(thread_id.clone())),
                    InboxProjectionMutation::DirectCallThreadAnchor {
                        peer, thread_id, ..
                    }
                    | InboxProjectionMutation::DirectCallThreadEnded {
                        peer, thread_id, ..
                    } => (peer.clone(), Some(thread_id.clone())),
                };
                EffectAuthorityKey::Inbox {
                    owner: owner.clone(),
                    partner,
                    thread,
                }
            }
            Self::NotificationActivityPreview { owner, mutation } => {
                let conversation = match mutation {
                    NotificationActivityMutation::ChatState { conversation, .. }
                    | NotificationActivityMutation::ChatStateGone { conversation, .. }
                    | NotificationActivityMutation::ReadMarker { conversation, .. }
                    | NotificationActivityMutation::OutboundMessage { conversation, .. }
                    | NotificationActivityMutation::OfflineDelivery { conversation, .. }
                    | NotificationActivityMutation::NotificationCandidate {
                        conversation, ..
                    } => conversation,
                };
                EffectAuthorityKey::Conversation {
                    owner: owner.clone(),
                    conversation: conversation.clone(),
                }
            }
            Self::GroupchatNotificationRecovery { mutation } => EffectAuthorityKey::Recovery {
                recipient: mutation.recipient.clone(),
                room: mutation.room.clone(),
                thread: mutation.thread_id.clone(),
            },
            Self::PendingDelivery { mutation } => {
                let (PendingDeliveryMutation::Archived { recipient, .. }
                | PendingDeliveryMutation::Transient { recipient, .. }) = mutation;
                EffectAuthorityKey::Recipient {
                    kind,
                    recipient: recipient.clone().into(),
                }
            }
            Self::LinkPreviewMediaRef { mutation } => EffectAuthorityKey::Media {
                archive: mutation.archive.clone(),
                slot: mutation.upload_slot_id,
            },
            Self::RetractionTombstone { mutation } => EffectAuthorityKey::Retraction {
                archive: mutation.archive.clone(),
                target: mutation.target_stanza_id.clone(),
            },
            Self::DmPinMutation {
                pair,
                target_stanza_id,
                ..
            } => EffectAuthorityKey::DirectPin {
                pair: pair.clone(),
                target: target_stanza_id.clone(),
            },
            Self::MucInviteMembershipGrant { grant } => EffectAuthorityKey::Membership {
                kind,
                room: grant.room.clone(),
                invitee: grant.invitee.clone(),
            },
            Self::MucInviteLedger { mutation } => EffectAuthorityKey::Membership {
                kind,
                room: mutation.room.clone(),
                invitee: mutation.invitee.clone(),
            },
            Self::GroupDmMembershipGrant { grant } | Self::GroupDmInviteLedger { grant } => {
                EffectAuthorityKey::Membership {
                    kind,
                    room: grant.room.clone(),
                    invitee: grant.invitee.clone(),
                }
            }
            Self::CallSignal { recipient, .. } => EffectAuthorityKey::Recipient {
                kind,
                recipient: recipient.clone().into(),
            },
            Self::Extension { recipient, .. } => EffectAuthorityKey::Recipient {
                kind,
                recipient: recipient.clone().into(),
            },
            Self::TombstoneReplayDeletion { target, .. } => EffectAuthorityKey::Tombstone {
                target: target.clone(),
            },
            Self::ErrorReply { recipient, .. } => EffectAuthorityKey::ErrorReply {
                recipient: recipient.clone(),
            },
        }
    }

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
            Self::RouteMucGroupchat {
                room,
                route_identity,
                ..
            } => {
                IngressEffectKey::RouteMucGroupchat(room.clone(), route_identity.storage_identity())
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
            Self::GroupchatNotificationRecovery { mutation } => {
                IngressEffectKey::GroupchatNotificationRecovery(mutation.storage_identity())
            }
            Self::PendingDelivery { mutation } => {
                IngressEffectKey::PendingDelivery(mutation.storage_identity())
            }
            Self::LinkPreviewMediaRef { mutation } => {
                IngressEffectKey::LinkPreviewMediaRef(mutation.storage_identity())
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
                dm_pin_action_storage_identity(action)
            )),
            Self::MucInviteMembershipGrant { grant } => {
                IngressEffectKey::MucInviteMembershipGrant(grant.storage_identity())
            }
            Self::MucInviteLedger { mutation } => {
                IngressEffectKey::MucInviteLedger(mutation.storage_identity())
            }
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
            Self::Pin { room, mutation } => {
                IngressEffectKey::Pin(room.clone(), mutation.storage_identity())
            }
            Self::Extension { recipient, .. } => IngressEffectKey::Extension(recipient.clone()),
            Self::TombstoneReplayDeletion {
                target,
                sm_entries,
                pending_rows,
            } => IngressEffectKey::TombstoneReplayDeletion(format!(
                "{}|sm:{}|pending:{}",
                target.storage_identity(),
                canonicalized_tombstone_replay_sm_entries(sm_entries)
                    .iter()
                    .map(TombstoneReplaySmEntry::storage_identity)
                    .collect::<Vec<_>>()
                    .join(","),
                canonicalized_pending_row_ids(pending_rows)
                    .iter()
                    .map(|row_id| row_id.as_str().to_owned())
                    .collect::<Vec<_>>()
                    .join(",")
            )),
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
    Blocked,
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
            FrozenStanzaErrorConditionPayload::Blocked => Self::Blocked,
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
            StoredFrozenStanzaErrorConditionPayload::Blocked => {
                FrozenStanzaErrorConditionPayload::Blocked
            }
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
        entry: InboxEntry,
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
    GroupchatChannelRead {
        room: BareJid,
    },
    GroupchatThreadRead {
        room: BareJid,
        thread_id: String,
    },
    GroupchatChannelAndThread {
        room: BareJid,
        thread_id: String,
        increment_unread: bool,
    },
    DirectCallThreadAnchor {
        peer: BareJid,
        thread_id: String,
        archive_stanza_id: StanzaId,
        media: CallThreadMedia,
        last_updated: i64,
    },
    DirectCallThreadEnded {
        peer: BareJid,
        thread_id: String,
        ended: chrono::DateTime<chrono::Utc>,
        duration: CallThreadDuration,
    },
}

impl From<InboxProjectionMutation> for StoredInboxProjectionMutation {
    fn from(value: InboxProjectionMutation) -> Self {
        match value {
            InboxProjectionMutation::Direct {
                entry,
                increment_unread,
            } => Self::Direct {
                entry,
                increment_unread,
            },
            InboxProjectionMutation::GroupchatChannel {
                room,
                increment_unread,
            } => Self::GroupchatChannel {
                room,
                increment_unread,
            },
            InboxProjectionMutation::GroupchatThread { room, thread_id } => Self::GroupchatThread {
                room,
                thread_id: thread_id.as_str().to_owned(),
            },
            InboxProjectionMutation::GroupchatChannelRead { room } => {
                Self::GroupchatChannelRead { room }
            }
            InboxProjectionMutation::GroupchatThreadRead { room, thread_id } => {
                Self::GroupchatThreadRead {
                    room,
                    thread_id: thread_id.as_str().to_owned(),
                }
            }
            InboxProjectionMutation::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => Self::GroupchatChannelAndThread {
                room,
                thread_id: thread_id.as_str().to_owned(),
                increment_unread,
            },
            InboxProjectionMutation::DirectCallThreadAnchor {
                peer,
                thread_id,
                archive_stanza_id,
                media,
                last_updated,
            } => Self::DirectCallThreadAnchor {
                peer,
                thread_id: thread_id.as_str().to_owned(),
                archive_stanza_id,
                media,
                last_updated,
            },
            InboxProjectionMutation::DirectCallThreadEnded {
                peer,
                thread_id,
                ended,
                duration,
            } => Self::DirectCallThreadEnded {
                peer,
                thread_id: thread_id.as_str().to_owned(),
                ended,
                duration,
            },
        }
    }
}

impl StoredInboxProjectionMutation {
    fn into_domain(self) -> Result<InboxProjectionMutation, EffectIntentCodecError> {
        Ok(match self {
            Self::Direct {
                entry,
                increment_unread,
            } => InboxProjectionMutation::Direct {
                entry,
                increment_unread,
            },
            Self::GroupchatChannel {
                room,
                increment_unread,
            } => InboxProjectionMutation::GroupchatChannel {
                room,
                increment_unread,
            },
            Self::GroupchatThread { room, thread_id } => InboxProjectionMutation::GroupchatThread {
                room,
                thread_id: ThreadId::new(thread_id)
                    .ok_or(EffectIntentCodecError::MalformedPayload)?,
            },
            Self::GroupchatChannelRead { room } => {
                InboxProjectionMutation::GroupchatChannelRead { room }
            }
            Self::GroupchatThreadRead { room, thread_id } => {
                InboxProjectionMutation::GroupchatThreadRead {
                    room,
                    thread_id: ThreadId::new(thread_id)
                        .ok_or(EffectIntentCodecError::MalformedPayload)?,
                }
            }
            Self::GroupchatChannelAndThread {
                room,
                thread_id,
                increment_unread,
            } => InboxProjectionMutation::GroupchatChannelAndThread {
                room,
                thread_id: ThreadId::new(thread_id)
                    .ok_or(EffectIntentCodecError::MalformedPayload)?,
                increment_unread,
            },
            Self::DirectCallThreadAnchor {
                peer,
                thread_id,
                archive_stanza_id,
                media,
                last_updated,
            } => InboxProjectionMutation::DirectCallThreadAnchor {
                peer,
                thread_id: ThreadId::new(thread_id)
                    .ok_or(EffectIntentCodecError::MalformedPayload)?,
                archive_stanza_id,
                media,
                last_updated,
            },
            Self::DirectCallThreadEnded {
                peer,
                thread_id,
                ended,
                duration,
            } => InboxProjectionMutation::DirectCallThreadEnded {
                peer,
                thread_id: ThreadId::new(thread_id)
                    .ok_or(EffectIntentCodecError::MalformedPayload)?,
                ended,
                duration,
            },
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredNotificationActivityMutation {
    ChatState {
        conversation: BareJid,
        state: u8,
        committed_at_ms: i64,
    },
    ChatStateGone {
        conversation: BareJid,
        committed_at_ms: i64,
    },
    ReadMarker {
        conversation: BareJid,
        committed_at_ms: i64,
    },
    OutboundMessage {
        conversation: BareJid,
        committed_at_ms: i64,
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
            NotificationActivityMutation::ChatState {
                conversation,
                state,
                committed_at_ms,
            } => Self::ChatState {
                conversation,
                state: chat_state_tag(state),
                committed_at_ms,
            },
            NotificationActivityMutation::ChatStateGone {
                conversation,
                committed_at_ms,
            } => Self::ChatStateGone {
                conversation,
                committed_at_ms,
            },
            NotificationActivityMutation::ReadMarker {
                conversation,
                committed_at_ms,
            } => Self::ReadMarker {
                conversation,
                committed_at_ms,
            },
            NotificationActivityMutation::OutboundMessage {
                conversation,
                committed_at_ms,
            } => Self::OutboundMessage {
                conversation,
                committed_at_ms,
            },
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
            Self::ChatState {
                conversation,
                state,
                committed_at_ms,
            } => NotificationActivityMutation::ChatState {
                conversation,
                state: chat_state_from_tag(state)?,
                committed_at_ms,
            },
            Self::ChatStateGone {
                conversation,
                committed_at_ms,
            } => NotificationActivityMutation::ChatStateGone {
                conversation,
                committed_at_ms,
            },
            Self::ReadMarker {
                conversation,
                committed_at_ms,
            } => NotificationActivityMutation::ReadMarker {
                conversation,
                committed_at_ms,
            },
            Self::OutboundMessage {
                conversation,
                committed_at_ms,
            } => NotificationActivityMutation::OutboundMessage {
                conversation,
                committed_at_ms,
            },
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
struct StoredGroupchatNotificationRecoveryMutation {
    recipient: BareJid,
    room: BareJid,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
    archive_stanza_id: StanzaId,
    sender: Jid,
    is_live_occupant: bool,
    room_members_only: bool,
    sender_can_broadcast_channel_mention: bool,
    created_at_ms: i64,
    action: u8,
}

impl From<GroupchatNotificationRecoveryMutation> for StoredGroupchatNotificationRecoveryMutation {
    fn from(value: GroupchatNotificationRecoveryMutation) -> Self {
        Self {
            recipient: value.recipient,
            room: value.room,
            thread_id: value
                .thread_id
                .map(|thread_id| thread_id.as_str().to_owned()),
            archive_stanza_id: value.archive_stanza_id,
            sender: value.sender,
            is_live_occupant: value.is_live_occupant,
            room_members_only: value.room_members_only,
            sender_can_broadcast_channel_mention: value.sender_can_broadcast_channel_mention,
            created_at_ms: value.created_at_ms,
            action: groupchat_notification_recovery_action_tag(value.action),
        }
    }
}

impl StoredGroupchatNotificationRecoveryMutation {
    fn into_domain(self) -> Result<GroupchatNotificationRecoveryMutation, EffectIntentCodecError> {
        Ok(GroupchatNotificationRecoveryMutation {
            recipient: self.recipient,
            room: self.room,
            thread_id: self
                .thread_id
                .map(|thread_id| {
                    ThreadId::new(thread_id).ok_or(EffectIntentCodecError::MalformedPayload)
                })
                .transpose()?,
            archive_stanza_id: self.archive_stanza_id,
            sender: self.sender,
            is_live_occupant: self.is_live_occupant,
            room_members_only: self.room_members_only,
            sender_can_broadcast_channel_mention: self.sender_can_broadcast_channel_mention,
            created_at_ms: self.created_at_ms,
            action: groupchat_notification_recovery_action_from_tag(self.action)?,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredPendingDeliveryMutation {
    Archived {
        recipient: BareJid,
        row_id: String,
        archive_stanza_id: StanzaId,
    },
    Transient {
        recipient: BareJid,
        row_id: String,
    },
}

impl From<PendingDeliveryMutation> for StoredPendingDeliveryMutation {
    fn from(value: PendingDeliveryMutation) -> Self {
        match value {
            PendingDeliveryMutation::Archived {
                recipient,
                row_id,
                archive_stanza_id,
            } => Self::Archived {
                recipient,
                row_id: row_id.as_str().to_owned(),
                archive_stanza_id,
            },
            PendingDeliveryMutation::Transient { recipient, row_id } => Self::Transient {
                recipient,
                row_id: row_id.as_str().to_owned(),
            },
        }
    }
}

impl From<StoredPendingDeliveryMutation> for PendingDeliveryMutation {
    fn from(value: StoredPendingDeliveryMutation) -> Self {
        match value {
            StoredPendingDeliveryMutation::Archived {
                recipient,
                row_id,
                archive_stanza_id,
            } => Self::Archived {
                recipient,
                row_id: PendingRowId::new(row_id),
                archive_stanza_id,
            },
            StoredPendingDeliveryMutation::Transient { recipient, row_id } => Self::Transient {
                recipient,
                row_id: PendingRowId::new(row_id),
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredLinkPreviewMediaRefMutation {
    upload_slot_id: Uuid,
    archive: BareJid,
    message_id: RichMessageId,
    current_archive_stanza_id: StanzaId,
    state: u8,
}

impl From<LinkPreviewMediaRefMutation> for StoredLinkPreviewMediaRefMutation {
    fn from(value: LinkPreviewMediaRefMutation) -> Self {
        Self {
            upload_slot_id: value.upload_slot_id,
            archive: value.archive,
            message_id: value.message_id,
            current_archive_stanza_id: value.current_archive_stanza_id,
            state: link_preview_media_ref_state_tag(value.state),
        }
    }
}

impl StoredLinkPreviewMediaRefMutation {
    fn into_domain(self) -> Result<LinkPreviewMediaRefMutation, EffectIntentCodecError> {
        Ok(LinkPreviewMediaRefMutation {
            upload_slot_id: self.upload_slot_id,
            archive: self.archive,
            message_id: self.message_id,
            current_archive_stanza_id: self.current_archive_stanza_id,
            state: link_preview_media_ref_state_from_tag(self.state)?,
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
    visible_after: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<GroupDmMembershipGrant> for StoredGroupDmMembershipGrant {
    fn from(value: GroupDmMembershipGrant) -> Self {
        let visible_after = match value.history_visibility {
            GroupDmHistoryVisibility::Full => None,
            GroupDmHistoryVisibility::FromJoin { visible_after } => Some(visible_after),
        };
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
            visible_after,
        }
    }
}

impl From<StoredGroupDmMembershipGrant> for GroupDmMembershipGrant {
    fn from(value: StoredGroupDmMembershipGrant) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
            history_visibility: match value.visible_after {
                Some(visible_after) => GroupDmHistoryVisibility::FromJoin { visible_after },
                None => GroupDmHistoryVisibility::Full,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredMucInviteMembershipGrant {
    room: BareJid,
    invitee: BareJid,
    inviter: BareJid,
}

impl From<MucInviteMembershipGrant> for StoredMucInviteMembershipGrant {
    fn from(value: MucInviteMembershipGrant) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
        }
    }
}

impl From<StoredMucInviteMembershipGrant> for MucInviteMembershipGrant {
    fn from(value: StoredMucInviteMembershipGrant) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredMucInviteLedgerMutation {
    room: BareJid,
    invitee: BareJid,
    inviter: BareJid,
    action: u8,
    recorded_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<MucInviteLedgerMutation> for StoredMucInviteLedgerMutation {
    fn from(value: MucInviteLedgerMutation) -> Self {
        Self {
            room: value.room,
            invitee: value.invitee,
            inviter: value.inviter,
            action: muc_invite_ledger_action_tag(value.action),
            recorded_at: value.recorded_at,
        }
    }
}

impl StoredMucInviteLedgerMutation {
    fn into_domain(self) -> Result<MucInviteLedgerMutation, EffectIntentCodecError> {
        Ok(MucInviteLedgerMutation {
            room: self.room,
            invitee: self.invitee,
            inviter: self.inviter,
            action: muc_invite_ledger_action_from_tag(self.action)?,
            recorded_at: self.recorded_at,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredRoomPinMutation {
    Pin { entry: PinnedEntry },
    Unpin { target_stanza_id: StanzaId },
}

impl From<RoomPinMutation> for StoredRoomPinMutation {
    fn from(value: RoomPinMutation) -> Self {
        match value {
            RoomPinMutation::Pin { entry } => Self::Pin { entry },
            RoomPinMutation::Unpin { target_stanza_id } => Self::Unpin { target_stanza_id },
        }
    }
}

impl From<StoredRoomPinMutation> for RoomPinMutation {
    fn from(value: StoredRoomPinMutation) -> Self {
        match value {
            StoredRoomPinMutation::Pin { entry } => Self::Pin { entry },
            StoredRoomPinMutation::Unpin { target_stanza_id } => Self::Unpin { target_stanza_id },
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StoredTombstoneReplayTarget {
    Groupchat {
        stanza_id: String,
        room: BareJid,
    },
    Direct {
        wire_id: String,
        author: BareJid,
        archive: BareJid,
    },
}

impl From<TombstoneReplayTarget> for StoredTombstoneReplayTarget {
    fn from(value: TombstoneReplayTarget) -> Self {
        match value {
            TombstoneReplayTarget::Groupchat { stanza_id, room } => {
                Self::Groupchat { stanza_id, room }
            }
            TombstoneReplayTarget::Direct {
                wire_id,
                author,
                archive,
            } => Self::Direct {
                wire_id,
                author,
                archive,
            },
        }
    }
}

impl From<StoredTombstoneReplayTarget> for TombstoneReplayTarget {
    fn from(value: StoredTombstoneReplayTarget) -> Self {
        match value {
            StoredTombstoneReplayTarget::Groupchat { stanza_id, room } => {
                Self::Groupchat { stanza_id, room }
            }
            StoredTombstoneReplayTarget::Direct {
                wire_id,
                author,
                archive,
            } => Self::Direct {
                wire_id,
                author,
                archive,
            },
        }
    }
}

#[derive(Serialize, Deserialize)]
struct StoredTombstoneReplaySmEntry {
    stream: SmSessionId,
    sequence: u32,
}

impl From<TombstoneReplaySmEntry> for StoredTombstoneReplaySmEntry {
    fn from(value: TombstoneReplaySmEntry) -> Self {
        Self {
            stream: value.stream,
            sequence: value.sequence,
        }
    }
}

impl From<StoredTombstoneReplaySmEntry> for TombstoneReplaySmEntry {
    fn from(value: StoredTombstoneReplaySmEntry) -> Self {
        Self {
            stream: value.stream,
            sequence: value.sequence,
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
        archived_at: chrono::DateTime<chrono::Utc>,
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
        route_identity: StoredEffectMessageIdentity,
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
    GroupchatNotificationRecovery {
        mutation: StoredGroupchatNotificationRecoveryMutation,
    },
    PendingDelivery {
        mutation: StoredPendingDeliveryMutation,
    },
    LinkPreviewMediaRef {
        mutation: StoredLinkPreviewMediaRefMutation,
    },
    RetractionTombstone {
        mutation: StoredRetractionTombstoneMutation,
    },
    DmPinMutation {
        first_peer: BareJid,
        second_peer: BareJid,
        target_stanza_id: StanzaId,
        action: StoredDmPinMutationAction,
    },
    MucInviteMembershipGrant {
        grant: StoredMucInviteMembershipGrant,
    },
    MucInviteLedger {
        mutation: StoredMucInviteLedgerMutation,
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
        mutation: StoredRoomPinMutation,
    },
    Extension {
        recipient: BareJid,
        stanza_id: StanzaId,
    },
    TombstoneReplayDeletion {
        target: StoredTombstoneReplayTarget,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        sm_entries: Vec<StoredTombstoneReplaySmEntry>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_rows: Vec<String>,
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
            Self::GroupchatNotificationRecovery { .. } => 21,
            Self::PendingDelivery { .. } => 22,
            Self::LinkPreviewMediaRef { .. } => 18,
            Self::RetractionTombstone { .. } => 14,
            Self::DmPinMutation { .. } => 15,
            Self::MucInviteMembershipGrant { .. } => 19,
            Self::MucInviteLedger { .. } => 20,
            Self::GroupDmMembershipGrant { .. } => 16,
            Self::GroupDmInviteLedger { .. } => 17,
            Self::RoomSubjectMutation { .. } => 13,
            Self::CallSignal { .. } => 8,
            Self::Pin { .. } => 9,
            Self::Extension { .. } => 10,
            Self::TombstoneReplayDeletion { .. } => 23,
            Self::ErrorReply { .. } => 11,
        }
    }

    fn from_domain(intent: IngressEffectIntent) -> Self {
        match intent {
            IngressEffectIntent::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
                archived_at,
            } => Self::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
                archived_at,
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
                route_identity,
            } => {
                canonicalize(&mut occupants);
                Self::RouteMucGroupchat {
                    room,
                    occupants,
                    reflection,
                    room_generation: room_generation.to_storage(),
                    route_identity: route_identity.into(),
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
            IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                Self::GroupchatNotificationRecovery {
                    mutation: mutation.into(),
                }
            }
            IngressEffectIntent::PendingDelivery { mutation } => Self::PendingDelivery {
                mutation: mutation.into(),
            },
            IngressEffectIntent::LinkPreviewMediaRef { mutation } => Self::LinkPreviewMediaRef {
                mutation: mutation.into(),
            },
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
                action: action.into(),
            },
            IngressEffectIntent::MucInviteMembershipGrant { grant } => {
                Self::MucInviteMembershipGrant {
                    grant: grant.into(),
                }
            }
            IngressEffectIntent::MucInviteLedger { mutation } => Self::MucInviteLedger {
                mutation: mutation.into(),
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
            IngressEffectIntent::Pin { room, mutation } => Self::Pin {
                room,
                mutation: mutation.into(),
            },
            IngressEffectIntent::Extension {
                recipient,
                stanza_id,
            } => Self::Extension {
                recipient,
                stanza_id,
            },
            IngressEffectIntent::TombstoneReplayDeletion {
                target,
                mut sm_entries,
                mut pending_rows,
            } => {
                canonicalize_tombstone_replay_sm_entries(&mut sm_entries);
                canonicalize_pending_row_ids(&mut pending_rows);
                Self::TombstoneReplayDeletion {
                    target: target.into(),
                    sm_entries: sm_entries.into_iter().map(Into::into).collect(),
                    pending_rows: pending_rows
                        .into_iter()
                        .map(|row_id| row_id.as_str().to_owned())
                        .collect(),
                }
            }
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
                archived_at,
            } => IngressEffectIntent::ArchiveAuthoritative {
                archive,
                stanza_id,
                by,
                archived_at,
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
                route_identity,
            } => IngressEffectIntent::RouteMucGroupchat {
                room,
                occupants,
                reflection,
                room_generation: EntityGeneration::from_storage(room_generation),
                route_identity: route_identity.into_domain(),
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
                mutation: mutation.into_domain()?,
            },
            Self::NotificationActivityPreview { owner, mutation } => {
                IngressEffectIntent::NotificationActivityPreview {
                    owner,
                    mutation: mutation.into_domain()?,
                }
            }
            Self::GroupchatNotificationRecovery { mutation } => {
                IngressEffectIntent::GroupchatNotificationRecovery {
                    mutation: mutation.into_domain()?,
                }
            }
            Self::PendingDelivery { mutation } => IngressEffectIntent::PendingDelivery {
                mutation: mutation.into(),
            },
            Self::LinkPreviewMediaRef { mutation } => IngressEffectIntent::LinkPreviewMediaRef {
                mutation: mutation.into_domain()?,
            },
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
                action: action.into(),
            },
            Self::MucInviteMembershipGrant { grant } => {
                IngressEffectIntent::MucInviteMembershipGrant {
                    grant: grant.into(),
                }
            }
            Self::MucInviteLedger { mutation } => IngressEffectIntent::MucInviteLedger {
                mutation: mutation.into_domain()?,
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
            Self::Pin { room, mutation } => IngressEffectIntent::Pin {
                room,
                mutation: mutation.into(),
            },
            Self::Extension {
                recipient,
                stanza_id,
            } => IngressEffectIntent::Extension {
                recipient,
                stanza_id,
            },
            Self::TombstoneReplayDeletion {
                target,
                sm_entries,
                pending_rows,
            } => IngressEffectIntent::TombstoneReplayDeletion {
                target: target.into(),
                sm_entries: sm_entries.into_iter().map(Into::into).collect(),
                pending_rows: pending_rows.into_iter().map(PendingRowId::new).collect(),
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

fn canonicalize_tombstone_replay_sm_entries(entries: &mut Vec<TombstoneReplaySmEntry>) {
    entries.sort_by_key(TombstoneReplaySmEntry::storage_identity);
    entries.dedup_by(|left, right| left.storage_identity() == right.storage_identity());
}

fn canonicalized_tombstone_replay_sm_entries(
    entries: &[TombstoneReplaySmEntry],
) -> Vec<TombstoneReplaySmEntry> {
    let mut entries = entries.to_vec();
    canonicalize_tombstone_replay_sm_entries(&mut entries);
    entries
}

fn canonicalize_pending_row_ids(row_ids: &mut Vec<PendingRowId>) {
    row_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    row_ids.dedup_by(|left, right| left.as_str() == right.as_str());
}

fn canonicalized_pending_row_ids(row_ids: &[PendingRowId]) -> Vec<PendingRowId> {
    let mut row_ids = row_ids.to_vec();
    canonicalize_pending_row_ids(&mut row_ids);
    row_ids
}

fn carbon_kind_tag(kind: CarbonKind) -> u8 {
    match kind {
        CarbonKind::Sent => 0,
        CarbonKind::Received => 1,
    }
}

fn groupchat_notification_recovery_action_tag(action: GroupchatNotificationRecoveryAction) -> u8 {
    match action {
        GroupchatNotificationRecoveryAction::Recorded => 0,
        GroupchatNotificationRecoveryAction::Completed => 1,
    }
}

fn groupchat_notification_recovery_action_from_tag(
    tag: u8,
) -> Result<GroupchatNotificationRecoveryAction, EffectIntentCodecError> {
    Ok(match tag {
        0 => GroupchatNotificationRecoveryAction::Recorded,
        1 => GroupchatNotificationRecoveryAction::Completed,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn carbon_kind_from_tag(tag: u8) -> Result<CarbonKind, EffectIntentCodecError> {
    Ok(match tag {
        0 => CarbonKind::Sent,
        1 => CarbonKind::Received,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn muc_invite_ledger_action_tag(action: MucInviteLedgerAction) -> u8 {
    match action {
        MucInviteLedgerAction::Recorded => 0,
        MucInviteLedgerAction::Claimed => 1,
    }
}

fn muc_invite_ledger_action_from_tag(
    tag: u8,
) -> Result<MucInviteLedgerAction, EffectIntentCodecError> {
    Ok(match tag {
        0 => MucInviteLedgerAction::Recorded,
        1 => MucInviteLedgerAction::Claimed,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn dm_pin_action_storage_identity(action: &DmPinMutationAction) -> String {
    match action {
        DmPinMutationAction::Pin { entry } => {
            serde_json::to_string(entry).expect("PinnedEntry serialization is infallible")
        }
        DmPinMutationAction::Unpin => "unpin".to_string(),
        DmPinMutationAction::RetractionCascadeUnpin => "retraction_cascade_unpin".to_string(),
    }
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

fn link_preview_media_ref_state_tag(state: LinkPreviewMediaRefState) -> u8 {
    match state {
        LinkPreviewMediaRefState::Current => 0,
        LinkPreviewMediaRefState::Unreferenced => 1,
    }
}

fn link_preview_media_ref_state_from_tag(
    tag: u8,
) -> Result<LinkPreviewMediaRefState, EffectIntentCodecError> {
    Ok(match tag {
        0 => LinkPreviewMediaRefState::Current,
        1 => LinkPreviewMediaRefState::Unreferenced,
        _ => return Err(EffectIntentCodecError::MalformedPayload),
    })
}

fn chat_state_tag(state: ChatState) -> u8 {
    match state {
        ChatState::Active => 0,
        ChatState::Composing => 1,
        ChatState::Paused => 2,
        ChatState::Inactive => 3,
        ChatState::Gone => 4,
    }
}

fn chat_state_from_tag(tag: u8) -> Result<ChatState, EffectIntentCodecError> {
    Ok(match tag {
        0 => ChatState::Active,
        1 => ChatState::Composing,
        2 => ChatState::Paused,
        3 => ChatState::Inactive,
        4 => ChatState::Gone,
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
    use crate::inbox::ConversationKind;
    use jid::Jid;
    use waddle_xmpp_core::mam::ThreadId;
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

    fn thread_id(value: &str) -> ThreadId {
        ThreadId::new(value).expect("valid thread id")
    }

    fn rich_message_id(value: &str) -> RichMessageId {
        RichMessageId::new(value).expect("valid message id")
    }

    fn direct_entry() -> InboxEntry {
        InboxEntry::new(
            bare("juliet@example.test"),
            ConversationKind::Direct,
            "stable-1",
            1_752_768_000,
        )
        .with_unread(3)
        .with_preview("important hello")
    }

    fn pinned_entry() -> PinnedEntry {
        PinnedEntry {
            target_stanza_id: stanza_id(),
            pinner_jid: bare("romeo@example.test"),
            pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                .expect("timestamp")
                .with_timezone(&chrono::Utc),
            preview: crate::muc::pin::PinPreview::new(
                bare("juliet@example.test"),
                Some("Juliet".to_string()),
                "important",
                chrono::DateTime::parse_from_rfc3339("2025-07-27T11:59:00Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc),
            ),
        }
    }

    fn samples() -> Vec<IngressEffectIntent> {
        vec![
            IngressEffectIntent::ArchiveAuthoritative {
                archive: bare("archive@example.test"),
                stanza_id: stanza_id(),
                by: bare("archive@example.test"),
                archived_at: chrono::DateTime::from_timestamp(1_753_617_600, 0)
                    .expect("fixture timestamp"),
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
                route_identity: EffectMessageIdentity::stanza(stanza_id()),
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
                    entry: direct_entry(),
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
            IngressEffectIntent::GroupchatNotificationRecovery {
                mutation: GroupchatNotificationRecoveryMutation {
                    recipient: bare("romeo@example.test"),
                    room: bare("room@conference.example.test"),
                    thread_id: Some(thread_id("thread-1")),
                    archive_stanza_id: stanza_id(),
                    sender: "juliet@example.test/balcony"
                        .parse::<Jid>()
                        .expect("valid JID"),
                    is_live_occupant: true,
                    room_members_only: false,
                    sender_can_broadcast_channel_mention: true,
                    created_at_ms: 1_753_620_000_000,
                    action: GroupchatNotificationRecoveryAction::Recorded,
                },
            },
            IngressEffectIntent::PendingDelivery {
                mutation: PendingDeliveryMutation::Archived {
                    recipient: bare("romeo@example.test"),
                    row_id: PendingRowId::new("pending-row-1"),
                    archive_stanza_id: stanza_id(),
                },
            },
            IngressEffectIntent::LinkPreviewMediaRef {
                mutation: LinkPreviewMediaRefMutation {
                    upload_slot_id: Uuid::parse_str("d5c7a44f-7c8c-4587-b0fb-f0e68444d36a")
                        .expect("uuid"),
                    archive: bare("room@conference.example.test"),
                    message_id: rich_message_id("client-msg-1"),
                    current_archive_stanza_id: stanza_id(),
                    state: LinkPreviewMediaRefState::Current,
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
                action: DmPinMutationAction::Pin {
                    entry: pinned_entry(),
                },
            },
            IngressEffectIntent::MucInviteMembershipGrant {
                grant: MucInviteMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                },
            },
            IngressEffectIntent::MucInviteLedger {
                mutation: MucInviteLedgerMutation {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    action: MucInviteLedgerAction::Recorded,
                    recorded_at: Some(
                        chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                    ),
                },
            },
            IngressEffectIntent::GroupDmMembershipGrant {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    history_visibility: GroupDmHistoryVisibility::FromJoin {
                        visible_after: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                    },
                },
            },
            IngressEffectIntent::GroupDmInviteLedger {
                grant: GroupDmMembershipGrant {
                    room: bare("room@conference.example.test"),
                    invitee: bare("mercutio@example.test"),
                    inviter: bare("romeo@example.test"),
                    history_visibility: GroupDmHistoryVisibility::FromJoin {
                        visible_after: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                    },
                },
            },
            IngressEffectIntent::RoomSubjectMutation {
                room: bare("room@conference.example.test"),
                state: SubjectState {
                    texts: crate::muc::RoomSubjectTexts::new(),
                    setter: bare("romeo@example.test"),
                    setter_nick: "romeo".to_owned(),
                    set_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                },
            },
            IngressEffectIntent::CallSignal {
                recipient: full("romeo@example.test/phone"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::Pin {
                room: bare("room@conference.example.test"),
                mutation: RoomPinMutation::Pin {
                    entry: PinnedEntry {
                        target_stanza_id: stanza_id(),
                        pinner_jid: bare("romeo@example.test"),
                        pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                        preview: crate::muc::pin::PinPreview::new(
                            bare("juliet@example.test"),
                            Some("Juliet".to_string()),
                            "important",
                            chrono::DateTime::parse_from_rfc3339("2025-07-27T11:59:00Z")
                                .expect("timestamp")
                                .with_timezone(&chrono::Utc),
                        ),
                    },
                },
            },
            IngressEffectIntent::Extension {
                recipient: bare("romeo@example.test"),
                stanza_id: stanza_id(),
            },
            IngressEffectIntent::TombstoneReplayDeletion {
                target: TombstoneReplayTarget::Direct {
                    wire_id: "wire-1".to_owned(),
                    author: bare("juliet@example.test"),
                    archive: bare("romeo@example.test"),
                },
                sm_entries: vec![TombstoneReplaySmEntry {
                    stream: SmSessionId::new("stream-1"),
                    sequence: 42,
                }],
                pending_rows: vec![PendingRowId::new("pending-row-1")],
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
            r#"{"version":1,"intent":{"type":"archive_authoritative","archive":"archive@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"},"by":"archive@example.test","archived_at":"2025-07-27T12:00:00Z"}}"#,
            r#"{"version":1,"intent":{"type":"route_direct","recipient":"romeo@example.test","fanout":["romeo@example.test/phone"],"route_identity":{"type":"stanza_id","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"route_muc_groupchat","room":"room@conference.example.test","occupants":["juliet@example.test/laptop"],"reflection":"romeo@example.test/phone","room_generation":7,"route_identity":{"type":"stanza_id","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"route_occupant_pm","recipient":"juliet@example.test/laptop","sender":"romeo@example.test/phone"}}"#,
            r#"{"version":1,"intent":{"type":"dispatch_to_room_remote","room":"room@conference.example.test","relay_target":{"node_id":"relay-node","node_epoch":"relay-epoch"}}}"#,
            r#"{"version":1,"intent":{"type":"recipient_sm_append","stream":"stream-1","append_identity":0}}"#,
            r#"{"version":1,"intent":{"type":"carbons","carbon_recipients":["romeo@example.test/phone"],"excluded_source":"romeo@example.test/laptop","kind":0}}"#,
            r#"{"version":1,"intent":{"type":"inbox_project","owner":"romeo@example.test","mutation":{"type":"direct","entry":{"partner":"juliet@example.test","kind":"Direct","last_stanza_id":"stable-1","last_updated":1752768000,"unread":3,"preview":"important hello","thread_id":null,"thread_title":null,"reply_count":0,"author":null,"call_thread_kind":null,"call_thread_media":null,"call_ended_at":null,"call_duration":null},"increment_unread":true}}}"#,
            r#"{"version":1,"intent":{"type":"notification_activity_preview","owner":"romeo@example.test","mutation":{"type":"notification_candidate","conversation":"room@conference.example.test","archive_stanza_id":{"id":"stable-1","by":"archive@example.test"},"outcome":0}}}"#,
            r#"{"version":1,"intent":{"type":"groupchat_notification_recovery","mutation":{"recipient":"romeo@example.test","room":"room@conference.example.test","thread_id":"thread-1","archive_stanza_id":{"id":"stable-1","by":"archive@example.test"},"sender":"juliet@example.test/balcony","is_live_occupant":true,"room_members_only":false,"sender_can_broadcast_channel_mention":true,"created_at_ms":1753620000000,"action":0}}}"#,
            r#"{"version":1,"intent":{"type":"pending_delivery","mutation":{"type":"archived","recipient":"romeo@example.test","row_id":"pending-row-1","archive_stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"link_preview_media_ref","mutation":{"upload_slot_id":"d5c7a44f-7c8c-4587-b0fb-f0e68444d36a","archive":"room@conference.example.test","message_id":"client-msg-1","current_archive_stanza_id":{"id":"stable-1","by":"archive@example.test"},"state":0}}}"#,
            r#"{"version":1,"intent":{"type":"retraction_tombstone","mutation":{"archive":"archive@example.test","target_stanza_id":{"id":"target-1","by":"archive@example.test"},"retraction_stanza_id":{"id":"stable-1","by":"archive@example.test"}}}}"#,
            r#"{"version":1,"intent":{"type":"dm_pin_mutation","first_peer":"juliet@example.test","second_peer":"romeo@example.test","target_stanza_id":{"id":"stable-1","by":"archive@example.test"},"action":{"type":"pin","entry":{"target_stanza_id":{"id":"stable-1","by":"archive@example.test"},"pinner_jid":"romeo@example.test","pinned_at":"2025-07-27T12:00:00Z","preview":{"author_jid":"juliet@example.test","author_nick":"Juliet","text":"important","message_timestamp":"2025-07-27T11:59:00Z"}}}}}"#,
            r#"{"version":1,"intent":{"type":"muc_invite_membership_grant","grant":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"muc_invite_ledger","mutation":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test","action":0,"recorded_at":"2025-07-27T12:00:00Z"}}}"#,
            r#"{"version":1,"intent":{"type":"group_dm_membership_grant","grant":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test","visible_after":"2025-07-27T12:00:00Z"}}}"#,
            r#"{"version":1,"intent":{"type":"group_dm_invite_ledger","grant":{"room":"room@conference.example.test","invitee":"mercutio@example.test","inviter":"romeo@example.test","visible_after":"2025-07-27T12:00:00Z"}}}"#,
            r#"{"version":1,"intent":{"type":"room_subject_mutation","room":"room@conference.example.test","state":{"texts":{},"setter":"romeo@example.test","setter_nick":"romeo","set_at":"2025-07-27T12:00:00Z"}}}"#,
            r#"{"version":1,"intent":{"type":"call_signal","recipient":"romeo@example.test/phone","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"pin","room":"room@conference.example.test","mutation":{"type":"pin","entry":{"target_stanza_id":{"id":"stable-1","by":"archive@example.test"},"pinner_jid":"romeo@example.test","pinned_at":"2025-07-27T12:00:00Z","preview":{"author_jid":"juliet@example.test","author_nick":"Juliet","text":"important","message_timestamp":"2025-07-27T11:59:00Z"}}}}}"#,
            r#"{"version":1,"intent":{"type":"extension","recipient":"romeo@example.test","stanza_id":{"id":"stable-1","by":"archive@example.test"}}}"#,
            r#"{"version":1,"intent":{"type":"tombstone_replay_deletion","target":{"type":"direct","wire_id":"wire-1","author":"juliet@example.test","archive":"romeo@example.test"},"sm_entries":[{"stream":"stream-1","sequence":42}],"pending_rows":["pending-row-1"]}}"#,
            r#"{"version":1,"intent":{"type":"error_reply","recipient":"romeo@example.test/phone","error":{"error_type":3,"condition":13,"texts":[{"lang":"nb","text":"moved"}],"condition_payload":{"type":"redirect","new_address":"xmpp:romeo@example.test/mobile"}}}}"#,
        ];
        assert_eq!(
            samples().len(),
            golden.len(),
            "every stable kind has a golden vector"
        );
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
    fn direct_call_thread_inbox_mutations_round_trip_with_distinct_identities() {
        let anchor = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::DirectCallThreadAnchor {
                peer: bare("juliet@example.test"),
                thread_id: thread_id("call-thread-1"),
                archive_stanza_id: stanza_id(),
                media: CallThreadMedia::audio_video(),
                last_updated: 1_752_768_000,
            },
        };
        let ended = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::DirectCallThreadEnded {
                peer: bare("juliet@example.test"),
                thread_id: thread_id("call-thread-1"),
                ended: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                    .expect("timestamp")
                    .with_timezone(&chrono::Utc),
                duration: CallThreadDuration::parse("PT1M").expect("duration"),
            },
        };

        for intent in [&anchor, &ended] {
            let encoded = intent.encode_v1().expect("encode typed mutation");
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                    .expect("decode typed mutation"),
                *intent
            );
        }
        assert_ne!(anchor.semantic_key(), ended.semantic_key());
    }

    #[test]
    fn room_routes_preserve_distinct_server_authored_stanza_identities() {
        let first = IngressEffectIntent::RouteMucGroupchat {
            room: bare("room@conference.example.test"),
            occupants: vec![full("juliet@example.test/laptop")],
            reflection: full("room@conference.example.test/__system__"),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(stanza_id()),
        };
        let second = IngressEffectIntent::RouteMucGroupchat {
            room: bare("room@conference.example.test"),
            occupants: vec![full("juliet@example.test/laptop")],
            reflection: full("room@conference.example.test/__system__"),
            room_generation: EntityGeneration::INITIAL,
            route_identity: EffectMessageIdentity::stanza(StanzaId::new(
                "stable-2",
                "archive@example.test".parse::<Jid>().expect("valid JID"),
            )),
        };
        assert_ne!(first.semantic_key(), second.semantic_key());
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
            archived_at: chrono::DateTime::from_timestamp(1_753_617_600, 0)
                .expect("fixture timestamp"),
        };
        let archive_two = IngressEffectIntent::ArchiveAuthoritative {
            archive: bare("archive@example.test"),
            stanza_id: StanzaId::new(
                "stable-2",
                "archive@example.test".parse::<Jid>().expect("valid JID"),
            ),
            by: bare("archive@example.test"),
            archived_at: chrono::DateTime::from_timestamp(1_753_617_600, 0)
                .expect("fixture timestamp"),
        };
        assert_ne!(archive_one.semantic_key(), archive_two.semantic_key());
        assert_eq!(archive_one.authority_key(), archive_two.authority_key());

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
            mutation: RoomPinMutation::Pin {
                entry: PinnedEntry {
                    target_stanza_id: stanza_id(),
                    pinner_jid: bare("romeo@example.test"),
                    pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                    preview: crate::muc::pin::PinPreview::new(
                        bare("juliet@example.test"),
                        Some("Juliet".to_string()),
                        "important",
                        chrono::DateTime::parse_from_rfc3339("2025-07-27T11:59:00Z")
                            .expect("timestamp")
                            .with_timezone(&chrono::Utc),
                    ),
                },
            },
        };
        let unpin = IngressEffectIntent::Pin {
            room: bare("room@conference.example.test"),
            mutation: RoomPinMutation::Unpin {
                target_stanza_id: stanza_id(),
            },
        };
        assert_ne!(pin.semantic_key(), unpin.semantic_key());

        let recorded = IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: bare("room@conference.example.test"),
                invitee: bare("mercutio@example.test"),
                inviter: bare("romeo@example.test"),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(
                    chrono::DateTime::parse_from_rfc3339("2025-07-27T12:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                ),
            },
        };
        let recorded_later = IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: bare("room@conference.example.test"),
                invitee: bare("mercutio@example.test"),
                inviter: bare("romeo@example.test"),
                action: MucInviteLedgerAction::Recorded,
                recorded_at: Some(
                    chrono::DateTime::parse_from_rfc3339("2025-07-28T12:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                ),
            },
        };
        let claimed = IngressEffectIntent::MucInviteLedger {
            mutation: MucInviteLedgerMutation {
                room: bare("room@conference.example.test"),
                invitee: bare("mercutio@example.test"),
                inviter: bare("romeo@example.test"),
                action: MucInviteLedgerAction::Claimed,
                recorded_at: None,
            },
        };
        assert_ne!(recorded.semantic_key(), recorded_later.semantic_key());
        assert_ne!(recorded.semantic_key(), claimed.semantic_key());

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
                thread_id: thread_id("thread-1"),
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
    fn inbox_groupchat_read_mutations_round_trip_with_distinct_keys() {
        let channel_read = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::GroupchatChannelRead {
                room: bare("room@conference.example.test"),
            },
        };
        let thread_read = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::GroupchatThreadRead {
                room: bare("room@conference.example.test"),
                thread_id: thread_id("thread-1"),
            },
        };
        let unread = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::GroupchatChannel {
                room: bare("room@conference.example.test"),
                increment_unread: true,
            },
        };

        for intent in [&channel_read, &thread_read] {
            let encoded = intent.encode_v1().expect("encode read mutation");
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                    .expect("decode read mutation"),
                *intent
            );
        }
        assert_ne!(channel_read.semantic_key(), thread_read.semantic_key());
        assert_ne!(channel_read.semantic_key(), unread.semantic_key());
    }

    #[test]
    fn recovery_pending_and_tombstone_keys_distinguish_actions_and_identities() {
        let recovery_recorded = IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: GroupchatNotificationRecoveryMutation {
                recipient: bare("romeo@example.test"),
                room: bare("room@conference.example.test"),
                thread_id: Some(thread_id("thread-1")),
                archive_stanza_id: stanza_id(),
                sender: "juliet@example.test/balcony"
                    .parse::<Jid>()
                    .expect("valid JID"),
                is_live_occupant: true,
                room_members_only: false,
                sender_can_broadcast_channel_mention: true,
                created_at_ms: 1_753_620_000_000,
                action: GroupchatNotificationRecoveryAction::Recorded,
            },
        };
        let recovery_completed = IngressEffectIntent::GroupchatNotificationRecovery {
            mutation: GroupchatNotificationRecoveryMutation {
                action: GroupchatNotificationRecoveryAction::Completed,
                ..match &recovery_recorded {
                    IngressEffectIntent::GroupchatNotificationRecovery { mutation } => {
                        mutation.clone()
                    }
                    _ => unreachable!("fixture shape"),
                }
            },
        };
        let pending_archived = IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Archived {
                recipient: bare("romeo@example.test"),
                row_id: PendingRowId::new("pending-row-1"),
                archive_stanza_id: stanza_id(),
            },
        };
        let pending_transient = IngressEffectIntent::PendingDelivery {
            mutation: PendingDeliveryMutation::Transient {
                recipient: bare("romeo@example.test"),
                row_id: PendingRowId::new("pending-row-1"),
            },
        };
        let tombstone_one = IngressEffectIntent::TombstoneReplayDeletion {
            target: TombstoneReplayTarget::Direct {
                wire_id: "wire-1".to_owned(),
                author: bare("juliet@example.test"),
                archive: bare("romeo@example.test"),
            },
            sm_entries: vec![TombstoneReplaySmEntry {
                stream: SmSessionId::new("stream-1"),
                sequence: 42,
            }],
            pending_rows: vec![PendingRowId::new("pending-row-1")],
        };
        let tombstone_two = IngressEffectIntent::TombstoneReplayDeletion {
            target: TombstoneReplayTarget::Direct {
                wire_id: "wire-1".to_owned(),
                author: bare("juliet@example.test"),
                archive: bare("romeo@example.test"),
            },
            sm_entries: vec![TombstoneReplaySmEntry {
                stream: SmSessionId::new("stream-1"),
                sequence: 43,
            }],
            pending_rows: vec![PendingRowId::new("pending-row-1")],
        };

        assert_ne!(
            recovery_recorded.semantic_key(),
            recovery_completed.semantic_key()
        );
        assert_ne!(
            pending_archived.semantic_key(),
            pending_transient.semantic_key()
        );
        assert_ne!(tombstone_one.semantic_key(), tombstone_two.semantic_key());

        for intent in [
            recovery_recorded,
            recovery_completed,
            pending_archived,
            pending_transient,
            tombstone_one,
        ] {
            let encoded = intent.encode_v1().expect("encode typed effect");
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                    .expect("decode typed effect"),
                intent
            );
        }
    }

    #[test]
    fn inbox_thread_decode_rejects_empty_thread_ids() {
        let payload = br#"{"version":1,"intent":{"type":"inbox_project","owner":"romeo@example.test","mutation":{"type":"groupchat_thread","room":"room@conference.example.test","thread_id":"   "}}}"#;
        assert_eq!(
            IngressEffectIntent::decode_v1(6, payload),
            Err(EffectIntentCodecError::MalformedPayload)
        );
    }

    #[test]
    fn chat_state_effect_round_trips_with_distinct_semantic_identity() {
        let active = IngressEffectIntent::NotificationActivityPreview {
            owner: bare("romeo@example.test"),
            mutation: NotificationActivityMutation::ChatState {
                conversation: bare("juliet@example.test"),
                state: ChatState::Active,
                committed_at_ms: 1_752_768_000_000,
            },
        };
        let paused = IngressEffectIntent::NotificationActivityPreview {
            owner: bare("romeo@example.test"),
            mutation: NotificationActivityMutation::ChatState {
                conversation: bare("juliet@example.test"),
                state: ChatState::Paused,
                committed_at_ms: 1_752_768_001_000,
            },
        };

        let encoded = active.encode_v1().expect("encode active chat state");
        assert_eq!(
            encoded.payload(),
            br#"{"version":1,"intent":{"type":"notification_activity_preview","owner":"romeo@example.test","mutation":{"type":"chat_state","conversation":"juliet@example.test","state":0,"committed_at_ms":1752768000000}}}"#
        );
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                .expect("decode active chat state"),
            active
        );
        assert_ne!(active.semantic_key(), paused.semantic_key());
    }

    #[test]
    fn direct_and_dm_pin_effects_preserve_committed_entries_in_semantic_identity() {
        let direct_one = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::Direct {
                entry: direct_entry(),
                increment_unread: true,
            },
        };
        let direct_two = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::Direct {
                entry: InboxEntry {
                    unread: 4,
                    ..direct_entry()
                },
                increment_unread: true,
            },
        };
        let pin_one = IngressEffectIntent::DmPinMutation {
            pair: (bare("juliet@example.test"), bare("romeo@example.test")),
            target_stanza_id: stanza_id(),
            action: DmPinMutationAction::Pin {
                entry: pinned_entry(),
            },
        };
        let pin_two = IngressEffectIntent::DmPinMutation {
            pair: (bare("juliet@example.test"), bare("romeo@example.test")),
            target_stanza_id: stanza_id(),
            action: DmPinMutationAction::Pin {
                entry: PinnedEntry {
                    pinned_at: chrono::DateTime::parse_from_rfc3339("2025-07-27T12:01:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                    ..pinned_entry()
                },
            },
        };

        for intent in [&direct_one, &pin_one] {
            let encoded = intent.encode_v1().expect("encode widened effect");
            assert_eq!(
                IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                    .expect("decode widened effect"),
                *intent
            );
        }
        assert_ne!(direct_one.semantic_key(), direct_two.semantic_key());
        assert_ne!(pin_one.semantic_key(), pin_two.semantic_key());

        let direct_delimiter_one = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::Direct {
                entry: InboxEntry {
                    preview: Some("a|b".to_string()),
                    thread_id: None,
                    ..direct_entry()
                },
                increment_unread: true,
            },
        };
        let direct_delimiter_two = IngressEffectIntent::InboxProject {
            owner: bare("romeo@example.test"),
            mutation: InboxProjectionMutation::Direct {
                entry: InboxEntry {
                    preview: Some("a".to_string()),
                    thread_id: Some("b".to_string()),
                    ..direct_entry()
                },
                increment_unread: true,
            },
        };
        let pin_delimiter_one = IngressEffectIntent::DmPinMutation {
            pair: (bare("juliet@example.test"), bare("romeo@example.test")),
            target_stanza_id: stanza_id(),
            action: DmPinMutationAction::Pin {
                entry: PinnedEntry {
                    preview: crate::muc::pin::PinPreview::new(
                        bare("juliet@example.test"),
                        Some("A|B".to_string()),
                        "",
                        pinned_entry().preview.message_timestamp,
                    ),
                    ..pinned_entry()
                },
            },
        };
        let pin_delimiter_two = IngressEffectIntent::DmPinMutation {
            pair: (bare("juliet@example.test"), bare("romeo@example.test")),
            target_stanza_id: stanza_id(),
            action: DmPinMutationAction::Pin {
                entry: PinnedEntry {
                    preview: crate::muc::pin::PinPreview::new(
                        bare("juliet@example.test"),
                        Some("A".to_string()),
                        "B",
                        pinned_entry().preview.message_timestamp,
                    ),
                    ..pinned_entry()
                },
            },
        };
        assert_ne!(
            direct_delimiter_one.semantic_key(),
            direct_delimiter_two.semantic_key()
        );
        assert_ne!(
            pin_delimiter_one.semantic_key(),
            pin_delimiter_two.semantic_key()
        );
    }

    #[test]
    fn link_preview_media_ref_identity_distinguishes_state_and_archive() {
        let slot = Uuid::parse_str("d5c7a44f-7c8c-4587-b0fb-f0e68444d36a").expect("uuid");
        let current = IngressEffectIntent::LinkPreviewMediaRef {
            mutation: LinkPreviewMediaRefMutation {
                upload_slot_id: slot,
                archive: bare("room@conference.example.test"),
                message_id: rich_message_id("client-msg-1"),
                current_archive_stanza_id: stanza_id(),
                state: LinkPreviewMediaRefState::Current,
            },
        };
        let stale = IngressEffectIntent::LinkPreviewMediaRef {
            mutation: LinkPreviewMediaRefMutation {
                upload_slot_id: slot,
                archive: bare("room@conference.example.test"),
                message_id: rich_message_id("client-msg-1"),
                current_archive_stanza_id: StanzaId::new(
                    "stable-2",
                    "archive@example.test".parse::<Jid>().expect("valid JID"),
                ),
                state: LinkPreviewMediaRefState::Unreferenced,
            },
        };

        let encoded = current.encode_v1().expect("encode current ref");
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                .expect("decode current ref"),
            current
        );
        assert_ne!(current.semantic_key(), stale.semantic_key());
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
    fn frozen_stanza_error_preserves_xep_0191_blocked_condition() {
        let mut xmpp = StanzaError::new(
            XmppStanzaErrorType::Cancel,
            DefinedCondition::NotAcceptable,
            "en",
            "Recipient is on your blocklist.",
        );
        xmpp.other = Some(Element::builder("blocked", NS_XEP0191_BLOCKING_ERRORS).build());

        let frozen = FrozenStanzaError::from_xmpp(&xmpp).expect("typed stanza error");
        assert_eq!(
            frozen.condition_payload,
            Some(FrozenStanzaErrorConditionPayload::Blocked)
        );

        let intent = IngressEffectIntent::ErrorReply {
            recipient: full("romeo@example.test/phone"),
            error: frozen.clone(),
        };
        let encoded = intent.encode_v1().expect("encode blocked error");
        assert_eq!(
            IngressEffectIntent::decode_v1(encoded.kind(), encoded.payload())
                .expect("decode blocked error"),
            intent
        );
        assert!(matches!(
            frozen.to_xmpp().other,
            Some(condition) if condition.is("blocked", NS_XEP0191_BLOCKING_ERRORS)
        ));
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
