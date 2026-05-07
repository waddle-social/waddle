use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ExtensionEvent {
    MessageHook(MessageHook),
    Command(CommandInvocation),
    Launch(LaunchInvocation),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageHook {
    pub context: MessageContext,
    pub body: DisplayText,
    pub links: Vec<LinkTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageContext {
    pub waddle_id: WaddleId,
    pub stanza_id: Option<StanzaId>,
    pub room: Option<RoomJid>,
    pub sender: Option<FullJidValue>,
    pub thread_id: Option<ThreadId>,
    pub reply_to: Option<ReplyTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LinkTarget {
    pub url: Url,
    pub range: BodyRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyTarget {
    pub id: StanzaId,
    pub to: Option<FullJidValue>,
}

impl TryFrom<DetectedLink> for LinkTarget {
    type Error = FrameworkTypeError;

    fn try_from(value: DetectedLink) -> Result<Self, Self::Error> {
        Ok(Self {
            url: Url::new(value.url)?,
            range: BodyRange::new(value.start_offset, value.end_offset)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandInvocation {
    pub waddle_id: WaddleId,
    pub room: Option<RoomJid>,
    pub requester: FullJidValue,
    pub command_node: CommandNode,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub form: Option<DataForm>,
    pub fields: Vec<FormFieldValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FormFieldValue {
    pub name: UiActionId,
    pub values: Vec<DataFormValue>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum CommandAction {
    Execute,
    Next,
    Prev,
    Complete,
    Cancel,
}

impl CommandAction {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaunchInvocation {
    pub context: LaunchContext,
    pub requester: FullJidValue,
    pub launch_id: LaunchId,
    pub session_id: Option<CommandSessionId>,
    pub action: Option<CommandAction>,
    pub form: Option<DataForm>,
    pub fields: Vec<FormFieldValue>,
}
