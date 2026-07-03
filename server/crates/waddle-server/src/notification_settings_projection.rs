//! Durable projection of XEP-0492 per-chat notification settings.
//!
//! The canonical setting remains the XMPP state that clients publish. This
//! module stores only the server-side indexed view used by notification policy
//! reads.

use jid::BareJid;
use minidom::Element;
use thiserror::Error;
use waddle_xmpp::xep::{
    is_notify_element, parse_notify_fallback_setting, validate_notify_element, NotificationLevel,
};

use crate::db::{Database, DatabaseError};

mod bookmark_derivation;

pub use bookmark_derivation::{
    derive_bookmark_projection_mutation, derive_dm_bookmark_projection_mutation,
    derive_validated_bookmark_projection_mutation, validate_dm_bookmark_publish,
    validate_xep0402_bookmark_publish,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Direct,
    PrivateGroup,
    PublicGroup,
}

impl ConversationKind {
    pub fn default_notification_setting(self) -> NotificationLevel {
        match self {
            Self::Direct | Self::PrivateGroup => NotificationLevel::Always,
            Self::PublicGroup => NotificationLevel::OnMention,
        }
    }

    pub(crate) fn as_db_value(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::PrivateGroup => "private_group",
            Self::PublicGroup => "public_group",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, NotificationSettingsProjectionError> {
        match value {
            "direct" => Ok(Self::Direct),
            "private_group" => Ok(Self::PrivateGroup),
            "public_group" => Ok(Self::PublicGroup),
            _ => {
                Err(NotificationSettingsProjectionError::InvalidConversationKind(value.to_string()))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSettingsSource {
    Xep0402Bookmarks,
    WaddleDmBookmarks,
}

impl NotificationSettingsSource {
    pub(crate) fn node(self) -> &'static str {
        match self {
            Self::Xep0402Bookmarks => waddle_xmpp::xep::xep0402::PEP_NODE,
            Self::WaddleDmBookmarks => {
                waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS
            }
        }
    }

    fn from_node(node: &str) -> Result<Self, NotificationSettingsProjectionError> {
        match node {
            waddle_xmpp::xep::xep0402::PEP_NODE => Ok(Self::Xep0402Bookmarks),
            waddle_xmpp::xep::xep_waddle_dm_bookmarks::PEP_NODE_WADDLE_DM_BOOKMARKS => {
                Ok(Self::WaddleDmBookmarks)
            }
            _ => Err(NotificationSettingsProjectionError::InvalidSourceNode(
                node.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSettingsProjection {
    pub owner_bare_jid: BareJid,
    pub conversation_jid: BareJid,
    pub conversation_kind: ConversationKind,
    pub mode: NotificationLevel,
    /// Waddle XEP-0492 `<advanced/>` rich-payload opt-in
    /// (see [`waddle_xmpp::xep::xep0492::NS_PUSH_RICH_PAYLOAD`]). When
    /// `true`, the T1 push evaluator may emit a rich XEP-0357 summary
    /// (`last-message-sender` + `last-message-body`) for this
    /// conversation, subject to XEP-0334 hint stripping. Defaults to
    /// `false` — the minimal summary payload.
    pub rich_payload_opt_in: bool,
    pub source_version: i64,
    pub updated_at_ms: i64,
    pub source: NotificationSettingsSource,
    pub source_item_jid: BareJid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationSettingsProjectionMutation {
    Upsert(NotificationSettingsProjection),
    Delete {
        owner_bare_jid: BareJid,
        conversation_jid: BareJid,
        /// The carrier whose ingestion derived this delete. The applied
        /// DELETE is scoped to this `source_node` so a publish that
        /// clears one carrier's override (an empty/absent `<notify>`)
        /// cannot clobber a row written by the OTHER carrier sharing the
        /// same `conversation_jid` — the DM vs XEP-0402 same-JID overlap
        /// the retract/eviction paths already guard against.
        source: NotificationSettingsSource,
    },
}

#[derive(Debug, Error)]
pub enum NotificationSettingsProjectionError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error("invalid owner bare JID in notification settings projection: {0}")]
    InvalidOwnerBareJid(String),
    #[error("invalid conversation JID in notification settings projection: {0}")]
    InvalidConversationJid(String),
    #[error("invalid source item JID in notification settings projection: {0}")]
    InvalidSourceItemJid(String),
    #[error("invalid notification setting mode in projection: {0}")]
    InvalidMode(String),
    #[error("invalid conversation kind in notification settings projection: {0}")]
    InvalidConversationKind(String),
    #[error("invalid notification settings source node: {0}")]
    InvalidSourceNode(String),
    #[error("invalid XEP-0402 bookmark payload: {0}")]
    InvalidBookmarkPayload(String),
    #[error("multiple XEP-0492 notify elements in XEP-0402 bookmark extensions")]
    MultipleNotifyElements,
    #[error("invalid XEP-0402 bookmark payload: {0}")]
    InvalidBookmark(#[from] waddle_xmpp::xep::xep0402::BookmarkError),
    #[error("invalid Waddle DM-bookmark payload: {0}")]
    InvalidDmBookmark(#[from] waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmarkError),
    #[error("invalid XEP-0492 notify payload: {0}")]
    InvalidNotify(#[from] waddle_xmpp::xep::NotificationSettingsError),
}

#[derive(Clone)]
pub struct NotificationSettingsProjectionStore {
    db: Database,
}

impl NotificationSettingsProjectionStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub async fn upsert(
        &self,
        projection: &NotificationSettingsProjection,
    ) -> Result<(), NotificationSettingsProjectionError> {
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            INSERT INTO notification_settings_projection (
                owner_bare_jid,
                conversation_jid,
                conversation_kind,
                mode,
                rich_payload_opt_in,
                source_version,
                updated_at_ms,
                source_node,
                source_item_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_bare_jid, conversation_jid) DO UPDATE SET
                conversation_kind = excluded.conversation_kind,
                mode = excluded.mode,
                rich_payload_opt_in = excluded.rich_payload_opt_in,
                source_version = excluded.source_version,
                updated_at_ms = excluded.updated_at_ms,
                source_node = excluded.source_node,
                source_item_id = excluded.source_item_id
            "#,
            crate::db_params![
                projection.owner_bare_jid.to_string(),
                projection.conversation_jid.to_string(),
                projection.conversation_kind.as_db_value(),
                projection.mode.element_name(),
                i64::from(projection.rich_payload_opt_in),
                projection.source_version,
                projection.updated_at_ms,
                projection.source.node(),
                projection.source_item_jid.to_string(),
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn delete(
        &self,
        owner_bare_jid: &BareJid,
        conversation_jid: &BareJid,
    ) -> Result<bool, NotificationSettingsProjectionError> {
        let conn = self.db.guard().await?;
        let affected = conn
            .execute(
                r#"
                DELETE FROM notification_settings_projection
                WHERE owner_bare_jid = ? AND conversation_jid = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), conversation_jid.to_string()],
            )
            .await?;
        Ok(affected > 0)
    }

    pub async fn delete_all_for_source(
        &self,
        owner_bare_jid: &BareJid,
        source: NotificationSettingsSource,
    ) -> Result<u64, NotificationSettingsProjectionError> {
        let conn = self.db.guard().await?;
        let affected = conn
            .execute(
                r#"
                DELETE FROM notification_settings_projection
                WHERE owner_bare_jid = ? AND source_node = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), source.node()],
            )
            .await?;
        Ok(affected)
    }

    pub async fn get(
        &self,
        owner_bare_jid: &BareJid,
        conversation_jid: &BareJid,
    ) -> Result<Option<NotificationSettingsProjection>, NotificationSettingsProjectionError> {
        let conn = self.db.guard().await?;
        let mut rows = conn
            .query(
                r#"
                SELECT owner_bare_jid,
                       conversation_jid,
                       conversation_kind,
                       mode,
                       rich_payload_opt_in,
                       source_version,
                       updated_at_ms,
                       source_node,
                       source_item_id
                FROM notification_settings_projection
                WHERE owner_bare_jid = ? AND conversation_jid = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), conversation_jid.to_string()],
            )
            .await?;
        rows.next().await?.map(decode_projection).transpose()
    }

    pub async fn effective_setting(
        &self,
        owner_bare_jid: &BareJid,
        conversation_jid: &BareJid,
        conversation_kind: ConversationKind,
    ) -> Result<NotificationLevel, NotificationSettingsProjectionError> {
        Ok(self
            .get(owner_bare_jid, conversation_jid)
            .await?
            .map(|projection| projection.mode)
            .unwrap_or_else(|| conversation_kind.default_notification_setting()))
    }

    /// Resolve the Waddle rich-payload opt-in for a conversation.
    ///
    /// Returns the stored per-conversation opt-in
    /// (see [`NotificationSettingsProjection::rich_payload_opt_in`]).
    /// Absence of a projection row — the default — is opt-out (`false`),
    /// preserving the minimal XEP-0357 summary payload.
    pub async fn effective_rich_payload_opt_in(
        &self,
        owner_bare_jid: &BareJid,
        conversation_jid: &BareJid,
    ) -> Result<bool, NotificationSettingsProjectionError> {
        Ok(self
            .get(owner_bare_jid, conversation_jid)
            .await?
            .map(|projection| projection.rich_payload_opt_in)
            .unwrap_or(false))
    }

    /// Resolve the effective XEP-0492 level AND the Waddle rich-payload
    /// opt-in for a conversation in a SINGLE projection read.
    ///
    /// The T1 push evaluator needs both on the delivery path; fetching
    /// the row once (rather than calling [`Self::effective_setting`] and
    /// [`Self::effective_rich_payload_opt_in`] separately) halves the
    /// projection-store IO per delivering candidate — significant on a
    /// large channel fan-out, where the established per-batch caches
    /// cannot help (the key is per-recipient-per-conversation, unique
    /// per candidate).
    pub async fn effective_setting_and_rich_opt_in(
        &self,
        owner_bare_jid: &BareJid,
        conversation_jid: &BareJid,
        conversation_kind: ConversationKind,
    ) -> Result<(NotificationLevel, bool), NotificationSettingsProjectionError> {
        let projection = self.get(owner_bare_jid, conversation_jid).await?;
        let level = projection
            .as_ref()
            .map(|projection| projection.mode)
            .unwrap_or_else(|| conversation_kind.default_notification_setting());
        let rich_payload_opt_in = projection
            .map(|projection| projection.rich_payload_opt_in)
            .unwrap_or(false);
        Ok((level, rich_payload_opt_in))
    }
}

/// Typed outcome of the XEP-0492 push-dispatch gate.
///
/// The gate is consulted at the message → push-fan-out boundary
/// (`OutboundEvent::QueueOfflineDelivery` interpret arm). The recipient's
/// effective notification level (resolved via [`NotificationSettingsProjectionStore::effective_setting`])
/// is combined with the per-message mention bit (resolved via the
/// `mention_bits_for_recipient` helper on the DM emission path) and
/// reduced to one of two typed
/// outcomes: `Deliver` (fan out to the user's registered XEP-0357 Push
/// Service) or `Suppressed { reason }` (drop, never call APNs/FCM).
///
/// Per the typed-payloads hard rule, `reason` carries the resolved typed
/// [`NotificationLevel`] — not a string diagnostic — so callers (and
/// adversarial tests) can branch on the exact suppression cause without
/// stringly-typed sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDispatchDecision {
    /// Push notification MUST be delivered to the recipient's registered
    /// Push Service node.
    Deliver,
    /// Push notification MUST be suppressed; do not call the provider.
    ///
    /// `reason` is the effective XEP-0492 [`NotificationLevel`] that
    /// caused suppression. Callers SHOULD emit a typed log carrying
    /// `reason` so deployments can observe per-conversation suppression
    /// without re-querying the projection.
    Suppressed { reason: NotificationLevel },
}

impl PushDispatchDecision {
    /// Pure reducer for the XEP-0492 push gate.
    ///
    /// This is the single decision point invoked immediately before
    /// device fan-out per the PR plan. It is intentionally pure so the
    /// dedicated XEP-0492 enforcement test suite can exercise every
    /// `(level, is_mention)` pair without spinning up storage or the
    /// WebSocket transport.
    pub fn evaluate(level: NotificationLevel, is_mention: bool) -> Self {
        if level.should_notify(is_mention) {
            Self::Deliver
        } else {
            Self::Suppressed { reason: level }
        }
    }

    /// Returns `true` if the gate decided to deliver.
    pub fn should_deliver(self) -> bool {
        matches!(self, Self::Deliver)
    }
}

fn decode_projection(
    row: crate::db::Row,
) -> Result<NotificationSettingsProjection, NotificationSettingsProjectionError> {
    let owner_raw: String = row.get(0)?;
    let conversation_raw: String = row.get(1)?;
    let conversation_kind_raw: String = row.get(2)?;
    let mode_raw: String = row.get(3)?;
    let rich_payload_opt_in: i64 = row.get(4)?;
    let source_version: i64 = row.get(5)?;
    let updated_at_ms: i64 = row.get(6)?;
    let source_node_raw: String = row.get(7)?;
    let source_item_raw: String = row.get(8)?;

    let owner_bare_jid = owner_raw
        .parse()
        .map_err(|_| NotificationSettingsProjectionError::InvalidOwnerBareJid(owner_raw.clone()))?;
    let conversation_jid = conversation_raw.parse().map_err(|_| {
        NotificationSettingsProjectionError::InvalidConversationJid(conversation_raw.clone())
    })?;
    let conversation_kind = ConversationKind::from_db_value(&conversation_kind_raw)?;
    let mode = NotificationLevel::from_element_name(&mode_raw)
        .ok_or(NotificationSettingsProjectionError::InvalidMode(mode_raw))?;
    let source = NotificationSettingsSource::from_node(&source_node_raw)?;
    let source_item_jid = source_item_raw.parse().map_err(|_| {
        NotificationSettingsProjectionError::InvalidSourceItemJid(source_item_raw.clone())
    })?;

    Ok(NotificationSettingsProjection {
        owner_bare_jid,
        conversation_jid,
        conversation_kind,
        mode,
        rich_payload_opt_in: rich_payload_opt_in != 0,
        source_version,
        updated_at_ms,
        source,
        source_item_jid,
    })
}
