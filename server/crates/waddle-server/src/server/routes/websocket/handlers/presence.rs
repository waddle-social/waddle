use jid::{BareJid, FullJid, Jid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    muc::{
        messages::build_subject_message,
        room_actor::{JoinAffiliationGrant, JoinWithAffiliation, LeaveByRealJid},
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

#[cfg(any(test, feature = "clustering"))]
pub use muc::handle_muc_join;
#[cfg(test)]
pub use muc::parse_room_jid_context;
pub(crate) use muc::route_room_presence_to_occupant;
pub use muc::{
    get_managed_channel_for_room, handle_muc_join_with_ordered_relay, handle_muc_leave,
    resolve_muc_room_archive_access, MucJoinRequest, RoomArchiveAccess,
};
pub(crate) use muc::{registered_remote_resource_delivery, RegisteredRemoteDelivery};
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
    _authenticated_session: &Option<Session>,
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Vec<String> {
    handle_presence_with_ordered_relay(
        presence,
        PresenceHandlerContext {
            domain,
            muc_domain,
            state,
            phase,
            authenticated_session: _authenticated_session,
            registry_owner,
            ordered_relay_origin: None,
        },
    )
    .await
}

pub async fn handle_presence_with_ordered_relay(
    presence: xmpp_parsers::presence::Presence,
    context: PresenceHandlerContext<'_>,
) -> Vec<String> {
    handle_presence_impl(presence, context).await
}

pub struct PresenceHandlerContext<'a> {
    pub domain: &'a str,
    pub muc_domain: &'a str,
    pub state: &'a WebSocketState,
    pub phase: &'a ConnectionPhase,
    pub authenticated_session: &'a Option<Session>,
    pub registry_owner: Option<&'a std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub ordered_relay_origin: Option<crate::server::routes::interpret::OrderedRelayRouteOrigin>,
}

#[cfg(feature = "clustering")]
pub(crate) async fn handle_ordered_relay_presence_request(
    state: &WebSocketState,
    target: &BareJid,
    presence: xmpp_parsers::presence::Presence,
    ordered_relay_origin: crate::server::routes::interpret::OrderedRelayRouteOrigin,
) -> Result<(), ()> {
    let Some(from) = presence.from.as_ref().map(|from| from.to_bare()) else {
        warn!(
            target = %target,
            "ordered relay presence request missing authoritative from"
        );
        return Err(());
    };
    match parse_subscription_presence(&presence, &from) {
        Ok(PresenceAction::Subscription(request)) => {
            if &request.to != target {
                warn!(
                    target = %target,
                    request_to = %request.to,
                    "ordered relay subscription request target mismatch"
                );
                return Err(());
            }
            handle_subscription_presence(state, request, Some(&ordered_relay_origin)).await;
            Ok(())
        }
        Ok(PresenceAction::Probe {
            from,
            to,
            to_was_full,
        }) => {
            if &to != target {
                warn!(
                    target = %target,
                    probe_to = %to,
                    "ordered relay presence probe target mismatch"
                );
                return Err(());
            }
            let to_full = if to_was_full {
                presence
                    .to
                    .as_ref()
                    .and_then(|jid| jid.clone().try_into_full().ok())
            } else {
                None
            };
            handle_presence_probe(state, from, to, to_full, Some(&ordered_relay_origin)).await;
            Ok(())
        }
        Ok(PresenceAction::PresenceUpdate(_)) => {
            warn!(
                target = %target,
                "ordered relay presence request carried a non-request presence update"
            );
            Err(())
        }
        Err(error) => {
            warn!(
                target = %target,
                error = %error,
                "failed to parse ordered relay presence request"
            );
            Err(())
        }
    }
}

async fn handle_presence_impl(
    mut presence: xmpp_parsers::presence::Presence,
    context: PresenceHandlerContext<'_>,
) -> Vec<String> {
    strip_client_authored_delay(&mut presence);
    let is_unavailable = presence.type_ == xmpp_parsers::presence::Type::Unavailable;

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(to_jid) = presence
        .to
        .as_ref()
        .filter(|jid| jid.domain().as_str() == context.muc_domain)
    {
        let room_jid = to_jid.to_bare();
        let Some(nick) = to_jid.resource().map(|resource| resource.as_str()) else {
            warn!(room = %room_jid, "MUC presence missing occupant nick");
            return vec![];
        };

        let Some(sender_jid) = context.phase.bound_jid() else {
            warn!("MUC presence without authenticated session");
            return vec![];
        };

        if is_unavailable {
            return handle_muc_leave(
                context.state,
                &room_jid,
                sender_jid,
                nick,
                context.ordered_relay_origin.as_ref(),
            )
            .await;
        }

        if presence.type_ != xmpp_parsers::presence::Type::None {
            warn!(
                room = %room_jid,
                nick,
                presence_type = ?presence.type_,
                "Dropping typed MUC presence that is neither available nor unavailable"
            );
            return vec![];
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
            context.state,
            &room_jid,
            sender_jid,
            nick,
            &presence,
        )
        .await
        {
            return replies;
        }

        #[cfg(feature = "clustering")]
        if !muc_update::is_muc_join_presence(&presence) {
            if let Some(origin) = context.ordered_relay_origin.as_ref() {
                if let Some(bridge) = context
                    .state
                    .deps
                    .app_state
                    .clustering_claims
                    .ordered_relay_delivery_bridge
                    .as_ref()
                {
                    let mut routed_presence = presence.clone();
                    routed_presence.from = Some(jid::Jid::from(sender_jid.clone()));
                    routed_presence.to = Some(to_jid.clone());
                    let stanza = Stanza::Presence(routed_presence);
                    match bridge
                        .try_proxy_muc_remote(
                            &room_jid,
                            &stanza,
                            crate::clustering::ordered_relay::OrderedRelayMucProxyKind::OccupantPresence,
                            origin,
                        )
                        .await
                    {
                        Some(
                            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Delivered(
                                replies,
                            ),
                        ) => {
                            return replies
                                .into_iter()
                                .map(|reply| stanza_to_xml(&reply))
                                .collect();
                        }
                        Some(
                            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::MaybeCommitted,
                        )
                        | Some(
                            crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::JoinMaybeCommitted,
                        ) => return Vec::new(),
                        Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Unavailable)
                        | Some(crate::clustering::route_bridge::OrderedRelayMucProxyOutcome::Dropped)
                        | None => {}
                    }
                }
            }
        }

        let presence_show = presence
            .show
            .clone()
            .map(NotificationPresenceShow::from_xep0045);
        return handle_muc_join_with_ordered_relay(
            context.state,
            MucJoinRequest {
                domain: context.domain,
                room_jid: &room_jid,
                sender_jid,
                nick,
                presence_show,
                authenticated_session: context.authenticated_session,
                ordered_relay_origin: context.ordered_relay_origin,
            },
        )
        .await;
    }

    let Some(sender_jid) = context.phase.bound_jid() else {
        warn!("Presence received without authenticated session");
        return vec![];
    };

    if is_directed_presence_update(&presence) {
        handle_directed_presence(
            context.state,
            sender_jid,
            presence,
            context.ordered_relay_origin.as_ref(),
        )
        .await;
        return vec![];
    }

    match parse_subscription_presence(&presence, &sender_jid.to_bare()) {
        Ok(PresenceAction::Subscription(request)) => {
            if try_handle_remote_subscription_presence(
                context.state,
                &request,
                context.ordered_relay_origin.as_ref(),
            )
            .await
            {
                return vec![];
            }
            handle_subscription_presence(
                context.state,
                request,
                context.ordered_relay_origin.as_ref(),
            )
            .await;
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
            let mut probe =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Probe);
            probe.from = Some(Jid::from(from.clone()));
            probe.to = Some(match to_full.as_ref() {
                Some(full) => Jid::from(full.clone()),
                None => Jid::from(to.clone()),
            });
            let stanza = Stanza::Presence(probe);
            if subscription::try_route_presence_to_bare_remote(
                context.state,
                &to,
                &stanza,
                context.ordered_relay_origin.as_ref(),
            )
            .await
            {
                return vec![];
            }
            handle_presence_probe(
                context.state,
                from,
                to,
                to_full,
                context.ordered_relay_origin.as_ref(),
            )
            .await;
        }
        Ok(PresenceAction::PresenceUpdate(presence_update)) => {
            handle_regular_presence_update(
                context.state,
                sender_jid,
                context.registry_owner,
                presence_update,
                context.ordered_relay_origin.as_ref(),
            )
            .await;
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
