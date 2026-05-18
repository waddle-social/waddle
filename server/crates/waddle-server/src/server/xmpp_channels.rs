use kameo::actor::ActorRef;
use serde::Serialize;
use waddle_xmpp::muc::PinPermission;
use waddle_xmpp::xep::{MentionPermission, MentionPermissions};

use crate::db::actor::{DbActor, DbQuery, DbQueryOne};
use crate::db::{row_value, ValueExt};

#[derive(Debug, Serialize)]
pub(crate) struct XmppChannelRecord {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: String,
    pub position: i32,
    pub is_default: bool,
    /// #422: persisted pin permission so disco-info on a dormant
    /// room reflects the configured policy.
    pub pin_permission: PinPermission,
    /// XEP-0513 mention permission policy used for dormant disco-info
    /// and managed-room actor rehydration.
    pub mention_permissions: MentionPermissions,
    pub created_at: String,
    pub updated_at: Option<String>,
}

fn db_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<String, String> {
    row_value(row, index)
        .and_then(ValueExt::as_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}

fn db_optional_string(
    row: &crate::db::actor::RowValues,
    index: usize,
    name: &str,
) -> Result<Option<String>, String> {
    row_value(row, index)
        .and_then(ValueExt::as_optional_string)
        .map_err(|e| format!("Failed to get {name}: {e}"))
}

fn db_i32(row: &crate::db::actor::RowValues, index: usize, name: &str) -> Result<i32, String> {
    match row_value(row, index).map_err(|e| format!("Failed to get {name}: {e}"))? {
        crate::db::Value::Integer(value) => Ok(*value as i32),
        other => Err(format!("Failed to get {name}: unexpected value {other:?}")),
    }
}

fn db_bool(row: &crate::db::actor::RowValues, index: usize, name: &str) -> Result<bool, String> {
    match row_value(row, index).map_err(|e| format!("Failed to get {name}: {e}"))? {
        crate::db::Value::Integer(value) => Ok(*value != 0),
        other => Err(format!("Failed to get {name}: unexpected value {other:?}")),
    }
}

fn parse_channel_record(row: &crate::db::actor::RowValues) -> Result<XmppChannelRecord, String> {
    let pin_permission_raw = db_string(row, 6, "pin_permission")?;
    let pin_permission = PinPermission::from_form_value(&pin_permission_raw).ok_or_else(|| {
        format!("Failed to get pin_permission: unexpected value {pin_permission_raw:?}")
    })?;
    let mentions_count = match row_value(row, 7)
        .map_err(|e| format!("Failed to get mention_permissions_count: {e}"))?
    {
        crate::db::Value::Integer(value) => u32::try_from(*value).map_err(|_| {
            format!("Failed to get mention_permissions_count: unexpected value {value}")
        })?,
        other => {
            return Err(format!(
                "Failed to get mention_permissions_count: unexpected value {other:?}"
            ));
        }
    };
    let mentions_individual_raw = db_string(row, 8, "mention_permissions_individual")?;
    let mentions_individual = MentionPermission::from_form_value(&mentions_individual_raw)
        .ok_or_else(|| {
            format!(
                "Failed to get mention_permissions_individual: unexpected value {mentions_individual_raw:?}"
            )
        })?;
    let mentions_channel_raw = db_string(row, 9, "mention_permissions_channel")?;
    let mentions_channel =
        MentionPermission::from_form_value(&mentions_channel_raw).ok_or_else(|| {
            format!(
                "Failed to get mention_permissions_channel: unexpected value {mentions_channel_raw:?}"
            )
        })?;
    Ok(XmppChannelRecord {
        id: db_string(row, 0, "id")?,
        name: db_string(row, 1, "name")?,
        description: db_optional_string(row, 2, "description")?,
        channel_type: db_string(row, 3, "channel_type")?,
        position: db_i32(row, 4, "position")?,
        is_default: db_bool(row, 5, "is_default")?,
        pin_permission,
        mention_permissions: MentionPermissions {
            count: mentions_count,
            individual: mentions_individual,
            channel: mentions_channel,
        },
        created_at: db_string(row, 10, "created_at")?,
        updated_at: db_optional_string(row, 11, "updated_at")?,
    })
}

pub(crate) async fn get_xmpp_channel(
    actor: ActorRef<DbActor>,
    channel_id: &str,
) -> Result<Option<XmppChannelRecord>, String> {
    let row = actor
        .ask(DbQueryOne {
            sql: r#"
                SELECT id, name, description, channel_type, position, is_default,
                       pin_permission,
                       mention_permissions_count,
                       mention_permissions_individual,
                       mention_permissions_channel,
                       created_at, updated_at
                FROM channels
                WHERE id = ?
            "#
            .to_string(),
            params: vec![channel_id.into()],
        })
        .await
        .map_err(|e| format!("Failed to query channel: {e}"))?;

    row.as_ref().map(parse_channel_record).transpose()
}

pub(crate) async fn list_xmpp_channels(
    actor: ActorRef<DbActor>,
    limit: usize,
    offset: usize,
) -> Result<Vec<XmppChannelRecord>, String> {
    let rows = actor
        .ask(DbQuery {
            sql: r#"
                SELECT id, name, description, channel_type, position, is_default,
                       pin_permission,
                       mention_permissions_count,
                       mention_permissions_individual,
                       mention_permissions_channel,
                       created_at, updated_at
                FROM channels
                ORDER BY position ASC, created_at ASC
                LIMIT ? OFFSET ?
            "#
            .to_string(),
            params: vec![(limit as i64).into(), (offset as i64).into()],
        })
        .await
        .map_err(|e| format!("Failed to query channels: {e}"))?;

    rows.iter().map(parse_channel_record).collect()
}
