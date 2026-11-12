//! Account-resource reads for handlers, taken from the selected authority.
//!
//! With the connection registry selected each function is the registry or
//! actor read the handlers have always made. With the account coordinator
//! selected it is the coordinator's answer. A coordinator that does not
//! answer yields an empty result and a warning, which is how these reads
//! already behave when the actor tree does not answer; the registry is not
//! written in that mode and is never read instead.

use jid::{BareJid, FullJid};
use tracing::warn;
use waddle_xmpp::registry::PresenceState;

use super::presence::show_name;
use crate::account_coordinator::AccountCoordinatorError;
use crate::server::routes::websocket::WebSocketState;

fn answered<T: Default>(read: &'static str, result: Result<T, AccountCoordinatorError>) -> T {
    result.unwrap_or_else(|error| {
        warn!(read, %error, "account coordinator did not answer a resource read");
        T::default()
    })
}

/// Every connected resource of `account`, with no presence filter.
pub(crate) async fn resources(state: &WebSocketState, account: &BareJid) -> Vec<FullJid> {
    match state
        .deps
        .app_state
        .account_authority()
        .resources(account)
        .await
    {
        Some(result) => answered("resources", result),
        None => {
            waddle_xmpp::registry::get_resources_for_user(
                &state.deps.protocol.user_registry,
                account,
            )
            .await
        }
    }
}

/// Every presence-available resource of `account` with its priority.
pub(crate) async fn available_resources(
    state: &WebSocketState,
    account: &BareJid,
) -> Vec<(FullJid, i8)> {
    match state
        .deps
        .app_state
        .account_authority()
        .available_resources(account)
        .await
    {
        Some(result) => answered("available_resources", result),
        None => state
            .deps
            .protocol
            .connection_registry
            .available_resources_for_user(account),
    }
}

/// The connected resources of `account` that want roster pushes.
pub(crate) async fn roster_interested(state: &WebSocketState, account: &BareJid) -> Vec<FullJid> {
    match state
        .deps
        .app_state
        .account_authority()
        .roster_interested(account)
        .await
    {
        Some(result) => answered("roster_interested", result),
        None => state
            .deps
            .protocol
            .connection_registry
            .roster_interested_resources_for_user(account),
    }
}

/// The presence `resource` last announced, when it is available.
pub(crate) async fn presence_state(
    state: &WebSocketState,
    resource: &FullJid,
) -> Option<PresenceState> {
    match state
        .deps
        .app_state
        .account_authority()
        .presence(resource)
        .await
    {
        Some(result) => answered("presence", result).map(|presence| PresenceState {
            show: presence
                .show
                .as_ref()
                .map(|show| show_name(show).to_owned()),
            status: presence.status,
            priority: presence.priority,
            payloads: presence.payloads,
        }),
        None => state
            .deps
            .protocol
            .connection_registry
            .presence_state(resource),
    }
}
