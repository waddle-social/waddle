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
}

impl NotificationSettingsSource {
    pub(crate) fn node(self) -> &'static str {
        match self {
            Self::Xep0402Bookmarks => waddle_xmpp::xep::xep0402::PEP_NODE,
        }
    }

    fn from_node(node: &str) -> Result<Self, NotificationSettingsProjectionError> {
        match node {
            waddle_xmpp::xep::xep0402::PEP_NODE => Ok(Self::Xep0402Bookmarks),
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
                source_version,
                updated_at_ms,
                source_node,
                source_item_id
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_bare_jid, conversation_jid) DO UPDATE SET
                conversation_kind = excluded.conversation_kind,
                mode = excluded.mode,
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
}

/// Typed outcome of the XEP-0492 push-dispatch gate.
///
/// The gate is consulted at the message → push-fan-out boundary
/// (`OutboundEvent::QueueOfflineDelivery` interpret arm). The recipient's
/// effective notification level (resolved via [`NotificationSettingsProjectionStore::effective_setting`])
/// is combined with the per-message mention bit (resolved via
/// [`message_is_mention_for_recipient`]) and reduced to one of two typed
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

pub fn derive_bookmark_projection_mutation(
    owner_bare_jid: &BareJid,
    item_id: &str,
    payload: Option<&Element>,
    conversation_kind: ConversationKind,
    updated_at_ms: i64,
    source_version: i64,
) -> Result<NotificationSettingsProjectionMutation, NotificationSettingsProjectionError> {
    let payload = payload.ok_or_else(|| {
        NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "bookmark publish item must contain a conference payload".to_string(),
        )
    })?;
    let bookmark = validate_xep0402_bookmark_publish(item_id, payload)?;
    derive_validated_bookmark_projection_mutation(
        owner_bare_jid,
        &bookmark,
        payload,
        conversation_kind,
        updated_at_ms,
        source_version,
    )
}

pub fn derive_validated_bookmark_projection_mutation(
    owner_bare_jid: &BareJid,
    bookmark: &waddle_xmpp::xep::xep0402::Bookmark,
    payload: &Element,
    conversation_kind: ConversationKind,
    updated_at_ms: i64,
    source_version: i64,
) -> Result<NotificationSettingsProjectionMutation, NotificationSettingsProjectionError> {
    let notify = bookmark_notify_element(payload)?;
    let Some(notify) = notify else {
        return Ok(NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: bookmark.jid.clone(),
        });
    };

    let mode = match parse_notify_fallback_setting(notify) {
        Ok(Some(mode)) => mode,
        Ok(None) => {
            return Ok(NotificationSettingsProjectionMutation::Delete {
                owner_bare_jid: owner_bare_jid.clone(),
                conversation_jid: bookmark.jid.clone(),
            });
        }
        Err(error) => return Err(error.into()),
    };

    Ok(NotificationSettingsProjectionMutation::Upsert(
        NotificationSettingsProjection {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: bookmark.jid.clone(),
            conversation_kind,
            mode,
            source_version,
            updated_at_ms,
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: bookmark.jid.clone(),
        },
    ))
}

pub fn validate_xep0402_bookmark_publish(
    item_id: &str,
    payload: &Element,
) -> Result<waddle_xmpp::xep::xep0402::Bookmark, NotificationSettingsProjectionError> {
    let item_jid: BareJid = item_id.parse().map_err(|_| {
        NotificationSettingsProjectionError::InvalidConversationJid(item_id.to_string())
    })?;
    if item_jid.node().is_none() {
        return Err(NotificationSettingsProjectionError::InvalidConversationJid(
            item_id.to_string(),
        ));
    }

    let bookmark = waddle_xmpp::xep::xep0402::parse_bookmark(item_id, payload)?;
    validate_xep0402_conference_shape(payload)?;
    validate_xep0492_notify_extensions(payload)?;
    Ok(bookmark)
}

fn validate_xep0402_conference_shape(
    payload: &Element,
) -> Result<(), NotificationSettingsProjectionError> {
    if payload
        .attr("autojoin")
        .is_some_and(|value| !matches!(value, "true" | "false" | "1" | "0"))
    {
        return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "autojoin must be an XML Schema boolean".to_string(),
        ));
    }

    let mut nick_count = 0_u8;
    let mut password_count = 0_u8;
    let mut extensions_count = 0_u8;
    for child in payload.children() {
        if child.ns() != waddle_xmpp::xep::xep0402::NS_BOOKMARKS2 {
            return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                "foreign elements must be nested inside XEP-0402 extensions".to_string(),
            ));
        }
        match child.name() {
            "nick" => nick_count = nick_count.saturating_add(1),
            "password" => password_count = password_count.saturating_add(1),
            "extensions" => extensions_count = extensions_count.saturating_add(1),
            other => {
                return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                    format!("unexpected XEP-0402 conference child: {other}"),
                ));
            }
        }
    }
    if nick_count > 1 || password_count > 1 || extensions_count > 1 {
        return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "nick, password, and extensions may appear at most once".to_string(),
        ));
    }
    Ok(())
}

fn validate_xep0492_notify_extensions(
    bookmark_payload: &Element,
) -> Result<(), NotificationSettingsProjectionError> {
    let Some(extensions) =
        bookmark_payload.get_child("extensions", waddle_xmpp::xep::xep0402::NS_BOOKMARKS2)
    else {
        return Ok(());
    };

    let mut notify_count = 0_u8;
    for child in extensions.children() {
        if child.ns() == waddle_xmpp::xep::xep0402::NS_BOOKMARKS2 {
            return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                "XEP-0402 extensions must contain extension namespaces".to_string(),
            ));
        }
        if child.ns() == waddle_xmpp::xep::NS_NOTIFICATION_SETTINGS {
            if !is_notify_element(child) {
                return Err(NotificationSettingsProjectionError::InvalidNotify(
                    waddle_xmpp::xep::NotificationSettingsError::NotNotifyElement,
                ));
            }
            validate_notify_element(child)?;
            notify_count = notify_count.saturating_add(1);
        }
    }
    if notify_count > 1 {
        return Err(NotificationSettingsProjectionError::MultipleNotifyElements);
    }
    Ok(())
}

fn bookmark_notify_element(
    bookmark_payload: &Element,
) -> Result<Option<&Element>, NotificationSettingsProjectionError> {
    let Some(extensions) =
        bookmark_payload.get_child("extensions", waddle_xmpp::xep::xep0402::NS_BOOKMARKS2)
    else {
        return Ok(None);
    };
    let mut notify_children = extensions
        .children()
        .filter(|child| is_notify_element(child));
    let notify = notify_children.next();
    if notify_children.next().is_some() {
        return Err(NotificationSettingsProjectionError::MultipleNotifyElements);
    }
    Ok(notify.filter(|element| is_notify_element(element)))
}

fn decode_projection(
    row: crate::db::Row,
) -> Result<NotificationSettingsProjection, NotificationSettingsProjectionError> {
    let owner_raw: String = row.get(0)?;
    let conversation_raw: String = row.get(1)?;
    let conversation_kind_raw: String = row.get(2)?;
    let mode_raw: String = row.get(3)?;
    let source_version: i64 = row.get(4)?;
    let updated_at_ms: i64 = row.get(5)?;
    let source_node_raw: String = row.get(6)?;
    let source_item_raw: String = row.get(7)?;

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
        source_version,
        updated_at_ms,
        source,
        source_item_jid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(value: &str) -> BareJid {
        value.parse().expect("valid bare JID")
    }

    async fn migrated_in_memory_store() -> NotificationSettingsProjectionStore {
        let storage = crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("pubsub storage");
        NotificationSettingsProjectionStore::new(storage.database())
    }

    #[tokio::test]
    async fn effective_setting_uses_xep0492_default_by_conversation_kind() {
        let store = migrated_in_memory_store().await;
        let owner = bare("alice@example.com");
        let direct = bare("bob@example.com");
        let public = bare("town@muc.example.com");

        assert_eq!(
            store
                .effective_setting(&owner, &direct, ConversationKind::Direct)
                .await
                .expect("direct default"),
            NotificationLevel::Always
        );
        assert_eq!(
            store
                .effective_setting(&owner, &public, ConversationKind::PrivateGroup)
                .await
                .expect("private group default"),
            NotificationLevel::Always
        );
        assert_eq!(
            store
                .effective_setting(&owner, &public, ConversationKind::PublicGroup)
                .await
                .expect("public default"),
            NotificationLevel::OnMention
        );
    }

    #[tokio::test]
    async fn projection_store_persists_file_backing() {
        let artifacts =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-artifacts");
        std::fs::create_dir_all(&artifacts).expect("artifacts dir");
        let path = artifacts.join(format!(
            "notification-settings-projection-{}.db",
            uuid::Uuid::new_v4()
        ));
        let url = format!("sqlite://{}", path.display());

        let owner = bare("alice@example.com");
        let conversation = bare("room@muc.example.com");

        {
            let storage = crate::pubsub::DatabasePubSubStorage::open(Some(&url))
                .await
                .expect("pubsub storage");
            let store = NotificationSettingsProjectionStore::new(storage.database());
            store
                .upsert(&NotificationSettingsProjection {
                    owner_bare_jid: owner.clone(),
                    conversation_jid: conversation.clone(),
                    conversation_kind: ConversationKind::PrivateGroup,
                    mode: NotificationLevel::Never,
                    source_version: 7,
                    updated_at_ms: 42,
                    source: NotificationSettingsSource::Xep0402Bookmarks,
                    source_item_jid: conversation.clone(),
                })
                .await
                .expect("upsert");
        }

        {
            let storage = crate::pubsub::DatabasePubSubStorage::open(Some(&url))
                .await
                .expect("reopen pubsub storage");
            let store = NotificationSettingsProjectionStore::new(storage.database());
            let loaded = store
                .get(&owner, &conversation)
                .await
                .expect("get")
                .expect("row");
            assert_eq!(loaded.mode, NotificationLevel::Never);
            assert_eq!(loaded.conversation_kind, ConversationKind::PrivateGroup);
            assert_eq!(loaded.source_version, 7);
            assert_eq!(loaded.updated_at_ms, 42);
        }

        for cleanup in [
            path.clone(),
            std::path::PathBuf::from(format!("{}-shm", path.display())),
            std::path::PathBuf::from(format!("{}-wal", path.display())),
        ] {
            let _ = std::fs::remove_file(cleanup);
        }
    }

    #[tokio::test]
    async fn delete_all_for_source_removes_only_owner_bookmark_projection_rows() {
        let store = migrated_in_memory_store().await;
        let alice = bare("alice@example.com");
        let bob = bare("bob@example.com");
        let room_one = bare("one@muc.example.com");
        let room_two = bare("two@muc.example.com");

        for (owner, conversation) in [
            (alice.clone(), room_one.clone()),
            (alice.clone(), room_two.clone()),
            (bob.clone(), room_one.clone()),
        ] {
            store
                .upsert(&NotificationSettingsProjection {
                    owner_bare_jid: owner,
                    conversation_jid: conversation.clone(),
                    conversation_kind: ConversationKind::PrivateGroup,
                    mode: NotificationLevel::Never,
                    source_version: 7,
                    updated_at_ms: 42,
                    source: NotificationSettingsSource::Xep0402Bookmarks,
                    source_item_jid: conversation,
                })
                .await
                .expect("upsert");
        }

        let deleted = store
            .delete_all_for_source(&alice, NotificationSettingsSource::Xep0402Bookmarks)
            .await
            .expect("delete all");
        assert_eq!(deleted, 2);
        assert!(store
            .get(&alice, &room_one)
            .await
            .expect("alice room one")
            .is_none());
        assert!(store
            .get(&alice, &room_two)
            .await
            .expect("alice room two")
            .is_none());
        assert!(store
            .get(&bob, &room_one)
            .await
            .expect("bob room one")
            .is_some());
    }

    #[test]
    fn derives_projection_from_xep0402_bookmark_notify_extension() {
        let owner = bare("alice@example.com");
        let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
                <extensions>\
                    <notify xmlns='urn:xmpp:notification-settings:1'><never /></notify>\
                </extensions>\
            </conference>"
            .parse()
            .expect("valid bookmark payload");

        let mutation = derive_bookmark_projection_mutation(
            &owner,
            "room@muc.example.com",
            Some(&payload),
            ConversationKind::PrivateGroup,
            7,
            11,
        )
        .expect("derive");

        let NotificationSettingsProjectionMutation::Upsert(projection) = mutation else {
            panic!("expected upsert mutation");
        };
        assert_eq!(projection.owner_bare_jid, owner);
        assert_eq!(projection.conversation_jid, bare("room@muc.example.com"));
        assert_eq!(projection.mode, NotificationLevel::Never);
        assert_eq!(projection.source_version, 11);
    }

    #[test]
    fn malformed_xep0492_notify_is_rejected() {
        let owner = bare("alice@example.com");
        let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
                <extensions>\
                    <notify xmlns='urn:xmpp:notification-settings:1'>\
                        <always />\
                        <never />\
                    </notify>\
                </extensions>\
            </conference>"
            .parse()
            .expect("valid XML payload");

        let error = derive_bookmark_projection_mutation(
            &owner,
            "room@muc.example.com",
            Some(&payload),
            ConversationKind::PrivateGroup,
            7,
            11,
        )
        .expect_err("malformed official XEP-0492 payload must be rejected");

        assert!(
            matches!(
                error,
                NotificationSettingsProjectionError::InvalidNotify(
                    waddle_xmpp::xep::NotificationSettingsError::MultipleFallbackSettings
                )
            ),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn missing_xep0492_notify_deletes_existing_projection() {
        let owner = bare("alice@example.com");
        let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1' />"
            .parse()
            .expect("valid bookmark payload");

        let mutation = derive_bookmark_projection_mutation(
            &owner,
            "room@muc.example.com",
            Some(&payload),
            ConversationKind::PrivateGroup,
            7,
            11,
        )
        .expect("derive");

        assert_eq!(
            mutation,
            NotificationSettingsProjectionMutation::Delete {
                owner_bare_jid: owner,
                conversation_jid: bare("room@muc.example.com"),
            }
        );
    }

    #[test]
    fn xep0469_pinning_inside_extensions_is_valid_bookmark_payload() {
        let owner = bare("alice@example.com");
        let payload: Element = "<conference xmlns='urn:xmpp:bookmarks:1'>\
                <extensions>\
                    <pinned xmlns='urn:xmpp:bookmarks-pinning:0' />\
                </extensions>\
            </conference>"
            .parse()
            .expect("valid XML payload");

        validate_xep0402_bookmark_publish("room@muc.example.com", &payload)
            .expect("XEP-0469 pinning belongs inside XEP-0402 extensions");
        let mutation = derive_bookmark_projection_mutation(
            &owner,
            "room@muc.example.com",
            Some(&payload),
            ConversationKind::PrivateGroup,
            7,
            11,
        )
        .expect("derive");
        assert_eq!(
            mutation,
            NotificationSettingsProjectionMutation::Delete {
                owner_bare_jid: owner,
                conversation_jid: bare("room@muc.example.com"),
            }
        );
    }
}
