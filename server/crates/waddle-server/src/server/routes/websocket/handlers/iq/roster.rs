use super::{
    build_roster_push, build_roster_result, build_roster_result_empty, build_subscription_presence,
    build_xmpp_error_response, chrono, global_database, iq_to_xml, local_account_jid_exists,
    mark_span_error, parse_roster_get, parse_roster_set, warn, AskType, BareJid, BroadcastOutcome,
    DatabaseRosterStorage, FullJid, RosterItem, RosterItemRow, RosterRowChange,
    RosterRowMutationKind, RosterSetResult, RosterStorageError, RosterVersion, Stanza,
    Subscription, SubscriptionStateMachine, SubscriptionType, WebSocketState, XmppError,
};
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::RemoteResourceStateUpdate;

pub(crate) mod push;
use push::{send_roster_push_to_sibling_resources, send_roster_remove_subscription_side_effects};

mod implementation;
pub(super) use implementation::handle_roster_iq;

use implementation::roster_item_to_row;
use implementation::roster_row_to_item;
pub(super) use implementation::roster_storage_for_state;
