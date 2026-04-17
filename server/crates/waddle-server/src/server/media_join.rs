use std::sync::Arc;

use jid::BareJid;
use thiserror::Error;

use crate::db::DatabasePool;
use crate::media::{MediaBackend, MediaBackendError, MediaSession, MediaSessionRequest, MediaType};
use crate::permissions::{Object, ObjectType, PermissionService, Subject};

#[derive(Debug, Error)]
pub enum MediaJoinError {
    #[error("permission denied")]
    PermissionDenied,
    #[error("channel not found")]
    ChannelNotFound,
    #[error("database error: {0}")]
    Database(String),
    #[error(transparent)]
    Backend(#[from] MediaBackendError),
}

pub async fn create_channel_media_session(
    db_pool: &Arc<DatabasePool>,
    media_backend: &Arc<dyn MediaBackend>,
    requester_jid: &BareJid,
    requester_user_id: &str,
    waddle_id: &str,
    channel_id: &str,
    media_type: MediaType,
) -> Result<MediaSession, MediaJoinError> {
    let permission_service = PermissionService::new(Arc::new(db_pool.global().clone()));
    let subject = Subject::user(requester_user_id);
    let channel = Object::new(ObjectType::Channel, channel_id);

    let can_view = permission_service
        .check(&subject, "view", &channel)
        .await
        .map_err(|err| MediaJoinError::Database(err.to_string()))?
        .allowed;

    if !can_view {
        return Err(MediaJoinError::PermissionDenied);
    }

    let waddle_db = db_pool
        .get_waddle_db(waddle_id)
        .await
        .map_err(|err| MediaJoinError::Database(err.to_string()))?;
    let conn = waddle_db
        .guard()
        .await
        .map_err(|err| MediaJoinError::Database(err.to_string()))?;
    let mut rows = conn
        .query(
            "SELECT name FROM channels WHERE id = ? LIMIT 1",
            libsql::params![channel_id],
        )
        .await
        .map_err(|err| MediaJoinError::Database(err.to_string()))?;

    let row = rows
        .next()
        .await
        .map_err(|err| MediaJoinError::Database(err.to_string()))?;
    if row.is_none() {
        return Err(MediaJoinError::ChannelNotFound);
    }

    let participant_id = requester_user_id.trim().to_string();
    let participant_name = requester_jid
        .node()
        .map(|node| node.as_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(requester_user_id)
        .to_string();

    media_backend
        .create_session(MediaSessionRequest {
            waddle_id: waddle_id.to_string(),
            channel_id: channel_id.to_string(),
            participant_id,
            participant_name,
            media_type,
        })
        .map_err(MediaJoinError::from)
}
