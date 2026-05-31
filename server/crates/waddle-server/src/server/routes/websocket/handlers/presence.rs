use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    muc::{
        messages::build_subject_message,
        room_actor::{JoinWithAffiliation, LeaveByRealJid},
        RoomConfig,
    },
    presence::subscription::{
        build_available_presence, build_subscription_presence, build_unavailable_presence,
        parse_subscription_presence, PresenceAction, SubscriptionStateMachine, SubscriptionType,
    },
    registry::BroadcastOutcome,
    roster::{build_roster_push, AskType, RosterItem, RosterVersion, Subscription},
    xep::NS_DELAY,
    Affiliation, Role, Stanza,
};
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

#[cfg(test)]
pub use muc::parse_room_jid_context;
pub use muc::{get_managed_channel_for_room, handle_muc_join, handle_muc_leave};
use probe::handle_presence_probe;
use regular::handle_regular_presence_update;
pub use subscription::broadcast_unavailable_for_expired_detached_session;
use subscription::{handle_directed_presence, handle_subscription_presence};
pub(super) use subscription::{
    send_current_presence_from_user_to_jid, send_unavailable_presence_from_user_to_jid,
};

pub async fn handle_presence(
    mut presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    strip_client_authored_delay(&mut presence);
    let is_unavailable = presence.type_ == xmpp_parsers::presence::Type::Unavailable;

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(to_jid) = presence
        .to
        .as_ref()
        .filter(|jid| jid.domain().as_str() == muc_domain)
    {
        let room_jid = to_jid.to_bare();
        let Some(nick) = to_jid.resource().map(|resource| resource.as_str()) else {
            warn!(room = %room_jid, "MUC presence missing occupant nick");
            return vec![];
        };

        let Some(sender_jid) = phase.bound_jid() else {
            warn!("MUC presence without authenticated session");
            return vec![];
        };

        if is_unavailable {
            return handle_muc_leave(state, &room_jid, sender_jid, nick).await;
        }

        // XEP-0045 §5.1.3 / §7.7: an in-room presence update from an
        // existing occupant is reflected to all occupants — not
        // re-routed as a fresh join. The dispatcher's prior behavior
        // (always route non-unavailable MUC presence to
        // `handle_muc_join`) silently dropped extension payloads like
        // `<call xmlns='urn:waddle:muc-call:0'/>` because the join's
        // server-built presence template doesn't carry them. Try the
        // update path first; fall through to join if the sender isn't
        // an occupant yet (then the join handler is the correct path
        // and the very next presence update will land here).
        if let Some(replies) = muc_update::try_handle_muc_presence_update(
            state, &room_jid, sender_jid, nick, &presence,
        )
        .await
        {
            return replies;
        }

        let presence_show = presence
            .show
            .clone()
            .map(NotificationPresenceShow::from_xep0045);
        return handle_muc_join(
            state,
            domain,
            &room_jid,
            sender_jid,
            nick,
            presence_show,
            _authenticated_session,
        )
        .await;
    }

    let Some(sender_jid) = phase.bound_jid() else {
        warn!("Presence received without authenticated session");
        return vec![];
    };

    if is_directed_presence_update(&presence) {
        handle_directed_presence(state, sender_jid, presence).await;
        return vec![];
    }

    match parse_subscription_presence(&presence, &sender_jid.to_bare()) {
        Ok(PresenceAction::Subscription(request)) => {
            handle_subscription_presence(state, request).await;
        }
        Ok(PresenceAction::Probe {
            from,
            to,
            to_was_full,
        }) => {
            let to_full = if to_was_full {
                presence
                    .to
                    .as_ref()
                    .and_then(|jid| jid.clone().try_into_full().ok())
            } else {
                None
            };
            handle_presence_probe(state, from, to, to_full).await;
        }
        Ok(PresenceAction::PresenceUpdate(presence_update)) => {
            handle_regular_presence_update(state, sender_jid, presence_update).await;
        }
        Err(error) => {
            warn!(error = %error, "Invalid presence stanza");
        }
    }
    vec![]
}

fn strip_client_authored_delay(presence: &mut xmpp_parsers::presence::Presence) {
    presence
        .payloads
        .retain(|payload| !(payload.name() == "delay" && payload.ns() == NS_DELAY));
}

fn is_directed_presence_update(presence: &xmpp_parsers::presence::Presence) -> bool {
    presence.to.is_some()
        && !matches!(
            presence.type_,
            xmpp_parsers::presence::Type::Subscribe
                | xmpp_parsers::presence::Type::Subscribed
                | xmpp_parsers::presence::Type::Unsubscribe
                | xmpp_parsers::presence::Type::Unsubscribed
                | xmpp_parsers::presence::Type::Probe
        )
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
