use jid::{BareJid, Jid};
use serde::{Deserialize, Serialize};
use url::Url;

/// A binding request. Field names match the native and browser client facades.
#[derive(Debug, Deserialize)]
#[serde(tag = "method", content = "args", rename_all = "snake_case")]
pub enum TimelineCommand {
    TimelineReset {},
    TimelineSnapshot {
        conversation_jid: BareJid,
    },
    TimelineIngest(Ingest),
    TimelineLocalEcho(Local),
    TimelineDelivery {
        conversation_jid: BareJid,
        origin_id: String,
        delivery: DeliveryUpdate,
    },
    TimelineOwnReactions {
        conversation_jid: BareJid,
        target_id: String,
        emojis: Vec<String>,
        own_jid: BareJid,
        timestamp_ms: i64,
    },
}

#[derive(Debug, Deserialize)]
pub struct Ingest {
    pub conversation_jid: BareJid,
    pub group: bool,
    pub account_jid: BareJid,
    pub own_nick: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub complete: bool,
    pub cursor: Option<String>,
    #[serde(default)]
    pub received_at_ms: i64,
}

#[derive(Debug, Deserialize)]
pub struct Local {
    pub conversation_jid: BareJid,
    pub group: bool,
    pub account_jid: BareJid,
    pub own_nick: Option<String>,
    pub origin_id: String,
    pub body: String,
    pub timestamp_ms: i64,
    pub reply_to: Option<Reply>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryUpdate {
    Sending,
    Sent,
    Failed,
    Rejected,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: Option<String>,
    pub from: Option<Jid>,
    pub to: Option<Jid>,
    pub body: Option<String>,
    pub message_type: String,
    pub timestamp: Option<String>,
    pub mam_id: Option<String>,
    pub stanza_id: Option<String>,
    pub stanza_id_by: Option<Jid>,
    pub stanza_ids: Vec<AssignedId>,
    pub origin_id: Option<String>,
    pub replaces_id: Option<String>,
    pub retracts_id: Option<String>,
    pub moderation_target_id: Option<String>,
    pub is_retracted: bool,
    pub reaction_target_id: Option<String>,
    pub reaction_emojis: Vec<String>,
    pub is_muc: bool,
    pub muc_user: bool,
    pub muc_pm: bool,
    pub author_real_jid: Option<Jid>,
    /// Immutable application attribution from the occupant presence active when
    /// this live message arrived; never inferred from a later nickname owner.
    #[serde(rename = "_author_stamp")]
    pub author_stamp: Option<Jid>,
    pub carbon: Option<CarbonDirection>,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
    pub reply_to_id: Option<String>,
    pub reply_to_sender: Option<Jid>,
    pub reply_fallback_start: Option<u32>,
    pub reply_fallback_end: Option<u32>,
    pub displayed_marker_requested: bool,
    pub shared_files: Vec<SharedFile>,
    pub safety_scores: Option<ScoresFastening>,
    pub call_thread: Option<CallThread>,
    pub call_thread_ended: Option<CallThreadEnded>,
    pub call_event: Option<CallEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallEvent {
    pub from: Jid,
    pub to: Option<Jid>,
    pub sid: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallThread {
    pub kind: String,
    pub sid: String,
    pub media: Vec<String>,
    pub initiator: Jid,
    pub started: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CallThreadEnded {
    pub anchor_id: String,
    pub ended: String,
    pub duration: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignedId {
    pub id: String,
    pub by: Jid,
}

#[derive(Debug, Clone)]
pub enum CarbonDirection {
    Sent,
    Received,
}

impl<'de> Deserialize<'de> for CarbonDirection {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Direction {
            #[serde(alias = "Sent")]
            Sent,
            #[serde(alias = "Received")]
            Received,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Boundary {
            Native(Direction),
            Browser { sent: bool, received: bool },
        }
        match Boundary::deserialize(deserializer)? {
            Boundary::Native(Direction::Sent)
            | Boundary::Browser {
                sent: true,
                received: false,
            } => Ok(Self::Sent),
            Boundary::Native(Direction::Received)
            | Boundary::Browser {
                sent: false,
                received: true,
            } => Ok(Self::Received),
            Boundary::Browser { .. } => Err(serde::de::Error::custom(
                "carbon direction must have exactly one direction",
            )),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SharedFile {
    pub url: Url,
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    #[serde(default)]
    pub disposition: String,
    pub encrypted: Option<EncryptedFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EncryptedFile {
    pub cipher: Url,
    pub key_b64: String,
    pub iv_b64: String,
    pub hashes: Vec<FileHash>,
    pub sources: Vec<Url>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileHash {
    pub algo: String,
    pub value_b64: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScoresFastening {
    pub target_origin_id: String,
    pub target_stanza_id: String,
    pub target_stanza_by: BareJid,
    pub source_revision_id: String,
    pub scores: Scores,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scores {
    pub model_version: String,
    pub scores: Vec<Score>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    pub category: Category,
    pub probability: f64,
    pub taxonomy_version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    #[serde(alias = "is_question", alias = "IsQuestion")]
    IsQuestion,
    #[serde(alias = "hate_speech", alias = "HateSpeech")]
    HateSpeech,
    #[serde(alias = "Explicit")]
    Explicit,
    #[serde(alias = "Harassment")]
    Harassment,
    #[serde(alias = "Violence")]
    Violence,
    #[serde(alias = "self_harm", alias = "SelfHarm")]
    SelfHarm,
    #[serde(alias = "Spam")]
    Spam,
    #[serde(alias = "Scam")]
    Scam,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: String,
    pub sender: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub url: String,
    pub name: Option<String>,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    #[serde(default)]
    pub inline: bool,
    pub encryption: Option<Encryption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Encryption {
    pub cipher: String,
    pub key_b64: String,
    pub iv_b64: String,
    pub sha256_b64: Vec<String>,
    pub sources: Vec<String>,
}

/// A full canonical snapshot. Sensitive file keys stay in app-runtime memory;
/// callers must redact them before sending a presentation snapshot to widgets.
#[derive(Debug, Default, Serialize)]
pub struct TimelineSnapshot {
    pub items: Vec<Item>,
    pub history_complete: bool,
    pub older_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub identity_ids: Vec<String>,
    pub sender: String,
    pub author_id: Option<String>,
    pub own: bool,
    pub body: String,
    pub timestamp_ms: i64,
    pub delivery: &'static str,
    pub edited: bool,
    pub retracted: bool,
    pub reference_id: Option<String>,
    pub correction_id: Option<String>,
    pub reply: Option<ReplyPreview>,
    pub reactions: Vec<Reaction>,
    pub attachments: Vec<Attachment>,
    pub scores: Option<Scores>,
    pub origin_id: Option<String>,
    pub message_id: Option<String>,
    pub archive_id: Option<String>,
    pub archive_by: Option<BareJid>,
    pub marker_requested: bool,
    pub thread: Option<String>,
    pub parent_thread_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReplyPreview {
    pub target_id: String,
    pub sender: Option<String>,
    pub snippet: Option<String>,
    pub target_retracted: bool,
}

#[derive(Debug, Serialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub own: bool,
}
