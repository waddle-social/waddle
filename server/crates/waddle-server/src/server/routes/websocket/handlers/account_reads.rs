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

/// Every presence-available resource of `account` with its last announced
/// presence state, fetched in a single coordinator read.
pub(crate) async fn available_resources_with_presence(
    state: &WebSocketState,
    account: &BareJid,
) -> Vec<(FullJid, Option<PresenceState>)> {
    available_resources_with_presence_for_app_state(&state.deps.app_state, account).await
}

/// Same as [`available_resources_with_presence`], taking `&AppState` directly
/// for off-task work that owns `Arc<AppState>`.
pub(crate) async fn available_resources_with_presence_for_app_state(
    app_state: &crate::server::AppState,
    account: &BareJid,
) -> Vec<(FullJid, Option<PresenceState>)> {
    answered(
        "available_resources_with_presence",
        app_state
            .account_authority()
            .available_resources_with_presence(account)
            .await,
    )
    .into_iter()
    .map(|(jid, presence)| (jid, presence.map(into_presence_state)))
    .collect()
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

fn into_presence_state(
    presence: waddle_xmpp::ownership::account_coordinator::AccountResourcePresence,
) -> PresenceState {
    PresenceState {
        show: presence
            .show
            .as_ref()
            .map(|show| show_name(show).to_owned()),
        status: presence.status,
        priority: presence.priority,
        payloads: presence.payloads,
    }
}
