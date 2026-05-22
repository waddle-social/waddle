use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::{ChannelInfo, ChannelRoomInfo, SpaceAccessModel, SpaceDetails, XmppError};

use crate::db::actor::{DbActor, DbQuery, DbQueryOne};
use crate::db::{row_value, Value, ValueExt};

pub(crate) async fn list_space_channels(
    space_db_actor: Option<&ActorRef<DbActor>>,
) -> Result<Vec<ChannelInfo>, XmppError> {
    debug!("Listing space channels for auto-join");

    let actor = space_db_actor.ok_or_else(|| {
        warn!("Database pool not configured for auto-join channel enumeration");
        XmppError::internal("Database pool not configured".to_string())
    })?;

    const PAGE_SIZE: usize = 200;
    let mut offset = 0usize;
    let mut result = Vec::new();

    loop {
        let rows = actor
            .ask(DbQuery {
                sql: r#"
                        SELECT id, name, channel_type
                        FROM channels
                        ORDER BY position ASC, created_at ASC
                        LIMIT ? OFFSET ?
                    "#
                .to_string(),
                params: vec![Value::from(PAGE_SIZE as i64), Value::from(offset as i64)],
            })
            .await
            .map_err(|e| {
                warn!(error = %e, "Failed to list channels");
                XmppError::internal(format!("Failed to list channels: {}", e))
            })?;

        let page_len = rows.len();
        for row in rows {
            let id = row_value(&row, 0)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel id: {}", e)))?;
            let name = row_value(&row, 1)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel name: {}", e)))?;
            let channel_type = row_value(&row, 2)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel type: {}", e)))?;
            result.push(ChannelInfo {
                id,
                name,
                channel_type,
            });
        }

        if page_len < PAGE_SIZE {
            break;
        }
        offset += PAGE_SIZE;
    }

    debug!(count = result.len(), "Found space channels");
    Ok(result)
}

pub(crate) async fn get_channel_room_info(
    space_db_actor: Option<&ActorRef<DbActor>>,
    channel_id: &str,
) -> Result<Option<ChannelRoomInfo>, XmppError> {
    debug!(
        channel_id = %channel_id,
        "Looking up channel-backed room metadata"
    );

    let Some(actor) = space_db_actor else {
        debug!("Database pool not configured for channel room lookup");
        return Ok(None);
    };

    match actor
        .ask(DbQueryOne {
            sql: r#"
                    SELECT id, name, channel_type
                    FROM channels
                    WHERE id = ?
                "#
            .to_string(),
            params: vec![Value::from(channel_id.to_string())],
        })
        .await
    {
        Ok(Some(row)) => {
            let id = row_value(&row, 0)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel id: {}", e)))?;
            let name = row_value(&row, 1)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel name: {}", e)))?;
            let channel_type = row_value(&row, 2)
                .and_then(|value| value.as_string())
                .map_err(|e| XmppError::internal(format!("Failed to parse channel type: {}", e)))?;
            Ok(Some(ChannelRoomInfo {
                waddle_id: "space".to_string(),
                channel: ChannelInfo {
                    id,
                    name,
                    channel_type,
                },
            }))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            warn!(
                channel_id = %channel_id,
                error = %e,
                "Failed to query channel from database actor"
            );
            Ok(None)
        }
    }
}

pub(crate) fn get_space_details(domain: &str) -> SpaceDetails {
    SpaceDetails {
        id: "space".to_string(),
        name: domain.to_string(),
        description: None,
        owner_id: "server".to_string(),
        icon_url: None,
        is_public: true,
        access_model: SpaceAccessModel::Open,
        created_at: "1970-01-01T00:00:00Z".to_string(),
    }
}
