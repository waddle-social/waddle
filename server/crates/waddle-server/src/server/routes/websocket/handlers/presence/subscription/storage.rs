use super::{warn, BareJid, DatabaseBlockingStorage, DatabaseRosterStorage, WebSocketState};

pub(in crate::server::routes::websocket::handlers::presence) async fn roster_storage(
    state: &WebSocketState,
) -> Option<DatabaseRosterStorage> {
    Some(state.deps.app_state.db_pool.roster_storage().clone())
}

pub(in crate::server::routes::websocket::handlers::presence) fn blocking_storage(
    state: &WebSocketState,
) -> &DatabaseBlockingStorage {
    state.deps.app_state.db_pool.blocking_storage()
}

pub(in crate::server::routes::websocket::handlers::presence) async fn recipient_blocks_sender(
    state: &WebSocketState,
    recipient: &BareJid,
    sender: &BareJid,
) -> bool {
    match blocking_storage(state).is_blocked(recipient, sender).await {
        Ok(blocked) => blocked,
        Err(error) => {
            warn!(error = %error, recipient = %recipient, sender = %sender, "Failed to check blocking state");
            true
        }
    }
}
