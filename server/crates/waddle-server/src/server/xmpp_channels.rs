use kameo::actor::ActorRef;
use serde::Serialize;
use waddle_xmpp::muc::PinPermission;

use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne};
use crate::db::{row_value, ValueExt};

#[derive(Debug, Clone, Serialize)]
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
    /// Whether the room is members-only. This is the durable XMPP catalog bit
    /// used to rebuild room actors after restart.
    pub members_only: bool,
    /// Whether the room is public in MUC service discovery.
    pub public_room: bool,
    pub created_at: String,
    pub updated_at: Option<String>,
}

pub(crate) struct XmppChannelUpsert {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub channel_type: String,
    pub position: i32,
    pub is_default: bool,
    pub pin_permission: PinPermission,
    pub members_only: bool,
    pub public_room: bool,
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
    Ok(XmppChannelRecord {
        id: db_string(row, 0, "id")?,
        name: db_string(row, 1, "name")?,
        description: db_optional_string(row, 2, "description")?,
        channel_type: db_string(row, 3, "channel_type")?,
        position: db_i32(row, 4, "position")?,
        is_default: db_bool(row, 5, "is_default")?,
        pin_permission,
        members_only: db_bool(row, 9, "members_only")?,
        public_room: db_bool(row, 10, "public_room")?,
        created_at: db_string(row, 7, "created_at")?,
        updated_at: db_optional_string(row, 8, "updated_at")?,
    })
}

pub(crate) async fn get_xmpp_channel(
    actor: ActorRef<DbActor>,
    channel_id: &str,
) -> Result<Option<XmppChannelRecord>, String> {
    let row = actor
        .ask(DbQueryOne {
            sql: r#"
                SELECT id, name, description, channel_type, position, is_default, pin_permission, created_at, updated_at, members_only, public_room
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
                SELECT id, name, description, channel_type, position, is_default, pin_permission, created_at, updated_at, members_only, public_room
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

pub(crate) async fn upsert_xmpp_channel(
    actor: ActorRef<DbActor>,
    channel: &XmppChannelUpsert,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO channels (
                    id, name, description, channel_type, position, is_default,
                    pin_permission, members_only, public_room, created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    channel_type = excluded.channel_type,
                    position = excluded.position,
                    is_default = excluded.is_default,
                    pin_permission = excluded.pin_permission,
                    members_only = excluded.members_only,
                    public_room = excluded.public_room,
                    updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                channel.id.clone().into(),
                channel.name.clone().into(),
                channel.description.clone().into(),
                channel.channel_type.clone().into(),
                (channel.position as i64).into(),
                channel.is_default.into(),
                channel.pin_permission.as_form_value().into(),
                channel.members_only.into(),
                channel.public_room.into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
        .map_err(|e| format!("Failed to upsert channel: {e}"))?;
    Ok(())
}

pub(crate) async fn delete_xmpp_channel(
    actor: ActorRef<DbActor>,
    channel_id: &str,
) -> Result<(), String> {
    actor
        .ask(DbExecute {
            sql: "DELETE FROM channels WHERE id = ?".to_string(),
            params: vec![channel_id.into()],
        })
        .await
        .map_err(|e| format!("Failed to delete channel: {e}"))?;
    Ok(())
}
