use super::{
    build_available_presence, build_subscription_presence, debug, warn, AskType, BareJid,
    DatabaseBlockingStorage, DatabaseRosterStorage, Element, FullJid, GetDatabase, Jid, RosterItem,
    RosterItemRow, RosterRowChange, RosterStorageError, Stanza, Subscription,
    SubscriptionStateMachine, SubscriptionType, WebSocketState,
};
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
#[cfg(feature = "clustering")]
use crate::server::routes::interpret::OrderedRelayRouteOriginKind;

mod delivery;
mod directed;
mod roster_state;
mod storage;

pub(super) use delivery::record_stanza_for_detached_available_resources_excluding;
pub(super) use delivery::try_route_presence_to_bare_remote;
pub use delivery::{
    broadcast_unavailable_for_terminated_session, TerminatedPresenceBroadcastOutcome,
};
pub(in crate::server::routes::websocket::handlers) use delivery::{
    send_current_presence_from_user_to_jid, send_current_presence_from_user_to_user,
    send_unavailable_presence_from_user_to_jid, send_unavailable_presence_from_user_to_user,
};
pub(super) use directed::handle_directed_presence;
pub(super) use roster_state::parse_subscription_state;
pub(super) use storage::{recipient_blocks_sender, roster_storage};

use delivery::{
    record_stanza_for_detached_available_resources,
    record_subscription_stanza_for_detached_resources_excluding, send_existing_subscription_ack,
    send_subscription_presence_side_effects, subscription_presence_recipients,
};
#[cfg(feature = "clustering")]
use roster_state::send_current_roster_push_to_resources;
use roster_state::update_subscription_roster_state;

mod implementation;
pub(super) use implementation::handle_subscription_presence;
#[cfg(not(feature = "clustering"))]
pub(super) use implementation::try_handle_remote_subscription_presence;
#[cfg(feature = "clustering")]
pub(super) use implementation::try_handle_remote_subscription_presence;
