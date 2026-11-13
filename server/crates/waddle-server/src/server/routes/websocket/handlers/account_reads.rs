//! Account-resource reads for handlers, taken from the account coordinator.
//!
//! A coordinator that does not answer yields an empty result and a warning,
//! which is how these reads behave when any routing read fails. The
//! connection registry is not an account authority and is never read
//! instead.

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
    answered(
        "resources",
        state
            .deps
            .app_state
            .account_authority()
            .resources(account)
            .await,
    )
}

/// Every presence-available resource of `account` with its priority.
pub(crate) async fn available_resources(
    state: &WebSocketState,
    account: &BareJid,
) -> Vec<(FullJid, i8)> {
    answered(
        "available_resources",
        state
            .deps
            .app_state
            .account_authority()
            .available_resources(account)
            .await,
    )
}

/// The connected resources of `account` that want roster pushes.
pub(crate) async fn roster_interested(state: &WebSocketState, account: &BareJid) -> Vec<FullJid> {
    answered(
        "roster_interested",
        state
            .deps
            .app_state
            .account_authority()
            .roster_interested(account)
            .await,
    )
}

/// The presence `resource` last announced, when it is available.
pub(crate) async fn presence_state(
    state: &WebSocketState,
    resource: &FullJid,
) -> Option<PresenceState> {
    answered(
        "presence",
        state
            .deps
            .app_state
            .account_authority()
            .presence(resource)
            .await,
    )
    .map(|presence| PresenceState {
        show: presence
            .show
            .as_ref()
            .map(|show| show_name(show).to_owned()),
        status: presence.status,
        priority: presence.priority,
        payloads: presence.payloads,
    })
}
