//! Extension invocation, webhook, launch, and PubSub event payloads.

use super::{
    BodyRange, CommandNode, CommandSessionId, DataForm, DataFormValue, Deserialize, DetectedLink,
    DisplayText, FrameworkTypeError, FullJidValue, LaunchContext, LaunchId, ProviderDeliveryId,
    ProviderEventType, ProviderFieldName, ProviderFieldNumber, ProviderFieldText, ProviderId,
    RoomJid, RoomMessageObserve, Serialize, StanzaId, ThreadId, UiActionId, Url, WaddleId,
};

/// Typed lifecycle inputs dispatched to an extension's event handler.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtensionEvent {
    /// Observe a canonical room message revision through the durable subscription path.
    RoomMessageObserve(RoomMessageObserve),
    /// Inspect message content and detected links for enrichment or observation.
    MessageHook(MessageHook),
    /// Handle an ad-hoc command action and submitted form state.
    Command(CommandInvocation),
    /// Handle an authorized launch descriptor invocation.
    Launch(LaunchInvocation),
    /// Handle a normalized external provider event.
    ProviderWebhook(ProviderWebhook),
}

/// Message body and detected links delivered to an enrichment or observation hook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageHook {
    /// Community, source stanza, sender, thread, and reply metadata for this hook.
    pub context: MessageContext,
    /// Text body of the message.
    pub body: DisplayText,
    /// Detected links and their ranges within the supplied message body.
    pub links: Vec<LinkTarget>,
}

/// Community and message identity supplied to a message hook.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageContext {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Source message stanza ID used to correlate enrichment output, when available.
    pub stanza_id: Option<StanzaId>,
    /// Bare JID of the room in scope.
    pub room: Option<RoomJid>,
    /// Full sender JID of the source message, when available.
    pub sender: Option<FullJidValue>,
    /// Conversation thread identifier.
    pub thread_id: Option<ThreadId>,
    /// Message and optional author targeted by the source message reply.
    pub reply_to: Option<ReplyTarget>,
}

/// Detected link URI paired with its UTF-8 byte range in a message body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkTarget {
    /// Resource URL exposed to the host or user.
    pub url: Url,
    /// Half-open UTF-8 byte range occupied by the link in the message body.
    pub range: BodyRange,
}

/// Original message identity and optional author JID carried by a reply marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyTarget {
    /// Identifier of the original message named by the reply marker.
    pub id: StanzaId,
    /// Full JID of the original author, when known.
    pub to: Option<FullJidValue>,
}

impl TryFrom<DetectedLink> for LinkTarget {
    type Error = FrameworkTypeError;

    fn try_from(value: DetectedLink) -> Result<Self, Self::Error> {
        Ok(Self {
            url: Url::try_new(value.url)?,
            range: BodyRange::try_new(value.start_offset, value.end_offset)?,
        })
    }
}

/// Ad-hoc command state, submitted fields, and full-JID requester identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandInvocation {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Bare JID of the room in scope.
    pub room: Option<RoomJid>,
    /// Full JID of the authenticated client issuing this ad-hoc command.
    pub requester: FullJidValue,
    /// Ad-hoc command node used to dispatch the requested extension action.
    pub command_node: CommandNode,
    /// Identifier of the session associated with this value.
    pub session_id: Option<CommandSessionId>,
    /// Action requested by the client or exposed by the UI.
    pub action: Option<CommandAction>,
    /// Optional structured form supplied with the command action.
    pub form: Option<DataForm>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<FormFieldValue>,
}

/// Submitted values for one named ad-hoc command form field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldValue {
    /// Machine-readable form field name submitted by the client.
    pub name: UiActionId,
    /// Values associated with this field.
    pub values: Vec<DataFormValue>,
}

/// XEP-0050 action selecting execution, navigation, completion, or cancellation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CommandAction {
    /// Start or execute the command.
    Execute,
    /// Advance to the next stage of a multi-stage command.
    Next,
    /// Return to the previous stage of a multi-stage command.
    Prev,
    /// Submit the final stage and finish the command.
    Complete,
    /// Cancel the current command session.
    Cancel,
}

impl CommandAction {
    /// Return the canonical wire token used by extension XML and WIT.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
        }
    }
}

/// Authorized launch submission delivered to the owning extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchInvocation {
    /// Authorized community, room, and source message for the selected launch.
    pub context: LaunchContext,
    /// Full JID of the authenticated client invoking the launch.
    pub requester: FullJidValue,
    /// Identifier of the launch associated with this value.
    pub launch_id: LaunchId,
    /// Identifier of the session associated with this value.
    pub session_id: Option<CommandSessionId>,
    /// Action requested by the client or exposed by the UI.
    pub action: Option<CommandAction>,
    /// Optional structured form supplied with the command action.
    pub form: Option<DataForm>,
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<FormFieldValue>,
}

/// Normalized provider event with community, provider, and delivery identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderWebhook {
    /// Identifier of the Waddle community providing invocation context.
    pub waddle_id: WaddleId,
    /// Identifier of the external provider producing this event or usage record.
    pub provider: ProviderId,
    /// Provider-specific event type used for webhook dispatch.
    pub event_type: ProviderEventType,
    /// Identifier of the delivery associated with this value.
    pub delivery_id: ProviderDeliveryId,
    /// Optional typed payload carried by this item.
    pub payload: ProviderPayload,
}

/// Flattened scalar provider values addressed by typed field paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderPayload {
    /// Fields carried by the data form or provider payload.
    pub fields: Vec<ProviderField>,
}

/// Scalar provider value paired with its path through the original payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderField {
    /// Ordered field-name path locating this scalar value in the original provider payload.
    pub path: Vec<ProviderFieldName>,
    /// Normalized scalar value found at the specified provider field path.
    pub value: ProviderFieldValue,
}

/// Scalar values admitted by the normalized webhook payload model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderFieldValue {
    /// Explicit null value from the provider payload.
    Null,
    /// Boolean scalar from the provider payload.
    Boolean(bool),
    /// Provider numeric text retained without selecting a Rust numeric representation.
    Number(ProviderFieldNumber),
    /// Provider string value, including empty text.
    Text(ProviderFieldText),
}
