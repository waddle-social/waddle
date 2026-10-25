#[cfg(feature = "clustering")]
use crate::clustering::ordered_relay::MucProxyOrigin;
#[cfg(feature = "clustering")]
use crate::clustering::ordered_relay::OrderedRelayMucProxyKind;
#[cfg(feature = "clustering")]
use crate::clustering::route_bridge::OrderedRelayMucProxyOutcome;
use crate::server::routes::interpret::OrderedRelayRouteOrigin;
use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::muc::messages::build_subject_message;
use waddle_xmpp::muc::room_actor::{JoinAffiliationGrant, JoinWithAffiliation, LeaveByRealJid};
use waddle_xmpp::muc::RoomConfig;
use waddle_xmpp::presence::subscription::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PresenceAction, SubscriptionStateMachine, SubscriptionType,
};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::roster::{build_roster_push, AskType, RosterItem, RosterVersion, Subscription};
use waddle_xmpp::xep::NS_DELAY;
use waddle_xmpp::{Affiliation, Role, Stanza};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::{
    element_to_xml, get_or_create_room_actor, get_room_actor, stanza_to_xml, WebSocketState,
};
use crate::auth::Session;
use crate::db::actor::GetDatabase;
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterStorageError,
};
use crate::notification_activity::NotificationPresenceShow;
use crate::permissions::{CheckPermission, Object, ObjectType, Permission, Subject};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::xmpp_state::{get_xmpp_channel, XmppChannelRecord};
use waddle_xmpp::protocol::ConnectionPhase;

mod muc;
mod muc_update;
mod probe;
mod regular;
mod subscription;

pub use muc::parse_room_jid_context;
pub(crate) use muc::registered_remote_resource_write_accepted_delivery;
pub(crate) use muc::RegisteredRemoteDelivery;
pub(crate) use muc::{deliver_room_presence_locally, route_room_presence_to_occupant};
pub use muc::{
    get_managed_channel_for_room, handle_muc_join_with_ordered_relay, handle_muc_leave,
    resolve_managed_channel_entry_access, resolve_muc_room_archive_access, MucJoinRequest,
    RoomArchiveAccess,
};
#[cfg(any(test, feature = "clustering"))]
pub use muc::{handle_muc_join, MucJoinConnectionContext};
#[cfg(feature = "clustering")]
pub(crate) use muc_update::try_handle_muc_presence_update;
use probe::handle_presence_probe;
use regular::handle_regular_presence_update;
pub use subscription::{
    broadcast_unavailable_for_terminated_session, TerminatedPresenceBroadcastOutcome,
};
use subscription::{
    handle_directed_presence, handle_subscription_presence, try_handle_remote_subscription_presence,
};
pub(super) use subscription::{
    send_current_presence_from_user_to_jid, send_unavailable_presence_from_user_to_jid,
};

/// `registry_owner` is the connection's registry ownership token
/// (`WsConnState::registry_owner`); the regular-presence path uses it to
/// owner-gate its JID-keyed registry writes (issue #1208). `None` means the
/// connection owns no registry slot (never registered, or its registration
/// was rolled back) and is treated as a non-owner: those writes are skipped.
#[cfg(test)]
pub async fn handle_presence(
    presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: Option<&Session>,
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<String> {
    // Present the generation the connection's join recorded (see
    // `tests::current_test_occupancy_session`), not a fresh mint: a fresh
    // one would make every presence update `Superseded` against its own
    // occupancy.
    let occupancy_session = match phase.bound_jid() {
        Some(jid) => {
            crate::server::routes::websocket::tests::current_test_occupancy_session(state, jid)
        }
        None => waddle_xmpp_core::OccupancySessionGeneration::mint(),
    };
    handle_presence_with_occupancy_session(
        presence,
        domain,
        muc_domain,
        state,
        phase,
        _authenticated_session,
        (occupancy_session, registry_owner),
    )
    .await
}

#[cfg(test)]
pub async fn handle_presence_with_occupancy_session(
    presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: Option<&Session>,
    connection: (
        waddle_xmpp_core::OccupancySessionGeneration,
        Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ),
) -> Vec<String> {
    let (occupancy_session, registry_owner) = connection;
    handle_presence_with_ordered_relay(
        presence,
        PresenceHandlerContext {
            domain,
            muc_domain,
            state,
            phase,
            authenticated_session,
            occupancy_session: &occupancy_session,
            registry_owner,
            ordered_relay_origin: None,
        },
    )
    .await
}

#[cfg(test)]
mod delay_strip_tests {
    use super::*;

    #[test]
    fn strips_client_supplied_delay_payload() {
        let xml = "<presence xmlns='jabber:client' from='alice@example.com/web'>\
                    <delay xmlns='urn:xmpp:delay' from='evil.example' stamp='2024-06-01T09:30:00Z'/>\
                    <status>ready</status>\
                    </presence>";
        let mut presence =
            xmpp_parsers::presence::Presence::try_from(xml.parse::<Element>().expect("valid xml"))
                .expect("presence");

        strip_client_authored_delay(&mut presence);

        assert!(presence
            .payloads
            .iter()
            .all(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY)));
    }
}

#[cfg(test)]
mod tests;

mod implementation;
#[cfg(feature = "clustering")]
pub(crate) use implementation::handle_ordered_relay_presence_request;
pub use implementation::handle_presence_with_ordered_relay;
#[cfg(test)]
use implementation::strip_client_authored_delay;
pub use implementation::PresenceHandlerContext;
