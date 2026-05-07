use super::*;

pub(in crate::server::routes::websocket::handlers::presence) async fn roster_storage(
    state: &WebSocketState,
) -> Option<DatabaseRosterStorage> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => Some(DatabaseRosterStorage::new(db)),
        Err(error) => {
            warn!(error = %error, "Failed to access roster database for presence");
            None
        }
    }
}

async fn blocking_storage(state: &WebSocketState) -> Option<DatabaseBlockingStorage> {
    match state
        .deps
        .app_state
        .db_pool
        .global_actor()
        .clone()
        .ask(GetDatabase)
        .await
    {
        Ok(db) => Some(DatabaseBlockingStorage::new(db)),
        Err(error) => {
            warn!(error = %error, "Failed to access blocking database for presence");
            None
        }
    }
}

pub(in crate::server::routes::websocket::handlers::presence) async fn recipient_blocks_sender(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    let Some(storage) = blocking_storage(state).await else {
        return false;
    };
    match storage.is_blocked(recipient, sender).await {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(error = %error, recipient = %recipient, sender = %sender, "Failed to check blocking state");
            true
        }
    }
}
