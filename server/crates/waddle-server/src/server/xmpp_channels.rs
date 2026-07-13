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

fn effective_members_only(channel_type: &str, members_only: bool) -> bool {
    members_only
        || matches!(
            waddle_xmpp::ChannelType::parse(channel_type),
            Some(waddle_xmpp::ChannelType::GroupDm)
        )
}

fn canonical_channel_type(channel_type: &str) -> &str {
    match waddle_xmpp::ChannelType::parse(channel_type) {
        Some(channel_type) => channel_type.as_str(),
        None => channel_type,
    }
}

fn parse_channel_record(row: &crate::db::actor::RowValues) -> Result<XmppChannelRecord, String> {
    let pin_permission_raw = db_string(row, 6, "pin_permission")?;
    let pin_permission = PinPermission::from_form_value(&pin_permission_raw).ok_or_else(|| {
        format!("Failed to get pin_permission: unexpected value {pin_permission_raw:?}")
    })?;
    let raw_channel_type = db_string(row, 3, "channel_type")?;
    let channel_type = canonical_channel_type(&raw_channel_type).to_string();
    let members_only = effective_members_only(&channel_type, db_bool(row, 9, "members_only")?);
    Ok(XmppChannelRecord {
        id: db_string(row, 0, "id")?,
        name: db_string(row, 1, "name")?,
        description: db_optional_string(row, 2, "description")?,
        channel_type,
        position: db_i32(row, 4, "position")?,
        is_default: db_bool(row, 5, "is_default")?,
        pin_permission,
        members_only,
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
    let channel_type = canonical_channel_type(&channel.channel_type);
    let members_only = effective_members_only(channel_type, channel.members_only);
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
                channel_type.into(),
                (channel.position as i64).into(),
                channel.is_default.into(),
                channel.pin_permission.as_form_value().into(),
                members_only.into(),
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

#[cfg(test)]
mod tests {
    use super::{
        effective_members_only, parse_channel_record, upsert_xmpp_channel, PinPermission,
        XmppChannelUpsert,
    };
    use crate::db::actor::DbQueryOne;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};

    #[test]
    fn group_dm_catalog_rows_are_members_only_even_if_stored_flag_is_false() {
        assert!(effective_members_only(
            waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM,
            false,
        ));
        assert!(effective_members_only(" group-dm", false));
        assert!(effective_members_only("group-dm ", false));
        assert!(!effective_members_only("text", false));
        assert!(effective_members_only("text", true));
    }

    #[test]
    fn catalog_read_canonicalizes_recognized_channel_types() {
        let row = vec![
            crate::db::Value::Text("group".to_string()),
            crate::db::Value::Text("Group".to_string()),
            crate::db::Value::NullText,
            crate::db::Value::Text(" group-dm ".to_string()),
            crate::db::Value::Integer(0),
            crate::db::Value::Integer(0),
            crate::db::Value::Text("admins-only".to_string()),
            crate::db::Value::Text("2026-07-14T00:00:00Z".to_string()),
            crate::db::Value::NullText,
            crate::db::Value::Integer(0),
            crate::db::Value::Integer(0),
        ];

        let record = parse_channel_record(&row).expect("catalog row");

        assert_eq!(
            record.channel_type,
            waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM,
        );
        assert!(record.members_only);
    }

    #[tokio::test]
    async fn catalog_write_persists_canonical_group_dm_type() {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        let db_actor = db_pool.global_actor().clone();
        upsert_xmpp_channel(
            db_actor.clone(),
            &XmppChannelUpsert {
                id: "group".to_string(),
                name: "Group".to_string(),
                description: None,
                channel_type: " group-dm ".to_string(),
                position: 0,
                is_default: false,
                pin_permission: PinPermission::AdminsOnly,
                members_only: false,
                public_room: false,
            },
        )
        .await
        .expect("catalog upsert");

        let row = db_actor
            .ask(DbQueryOne {
                sql: "SELECT channel_type, members_only FROM channels WHERE id = ?".to_string(),
                params: vec!["group".into()],
            })
            .await
            .expect("catalog query")
            .expect("catalog row");

        assert_eq!(
            row,
            vec![
                waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.into(),
                true.into(),
            ],
        );
    }
}
