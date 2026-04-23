use jid::{BareJid, FullJid};
use tracing::{debug, info, warn};
use waddle_xmpp::{
    muc::{
        room_actor::{JoinWithAffiliation, LeaveByRealJid},
        RoomConfig,
    },
    Affiliation, Role, Stanza,
};
use xmpp_parsers::minidom::Element;

use super::super::{element_to_xml, get_or_create_room_actor, get_room_actor, WebSocketState};
use crate::auth::Session;
use crate::server::routes::channels::get_channel_from_db;
use waddle_xmpp::protocol::ConnectionPhase;

pub async fn handle_presence(
    presence: xmpp_parsers::presence::Presence,
    domain: &str,
    muc_domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    let to = presence.to.as_ref().map(|jid| jid.to_string());
    let is_unavailable = presence.type_ == xmpp_parsers::presence::Type::Unavailable;

    // Check if this is a MUC presence (to room@muc.domain/nick)
    if let Some(ref to_jid) = to {
        if to_jid.contains(muc_domain) {
            let parts: Vec<&str> = to_jid.split('/').collect();
            let room_jid_str = parts.first().copied().unwrap_or(to_jid);
            let nick = parts.get(1).copied().unwrap_or("anonymous");

            let Ok(room_jid) = room_jid_str.parse::<BareJid>() else {
                warn!(room = %room_jid_str, "Invalid room JID");
                return vec![];
            };

            let Some(sender_jid) = phase.bound_jid() else {
                warn!("MUC presence without authenticated session");
                return vec![];
            };

            if is_unavailable {
                return handle_muc_leave(state, &room_jid, sender_jid, nick).await;
            }

            return handle_muc_join(
                state,
                domain,
                &room_jid,
                sender_jid,
                nick,
                _authenticated_session,
            )
            .await;
        }
    }

    debug!("Presence stanza received");
    vec![]
}

/// Handle MUC room join
pub async fn handle_muc_join(
    state: &WebSocketState,
    domain: &str,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
    _authenticated_session: &Option<Session>,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC join request");

    let existing_room_actor = get_room_actor(state, room_jid).await;
    let (room_actor, created_instant_room) = match existing_room_actor {
        Some(actor) => (actor, false),
        None => {
            let managed_channel = get_managed_channel_for_room(state, room_jid).await;
            let config = managed_channel
                .as_ref()
                .map(|channel| RoomConfig {
                    name: channel.name.clone(),
                    description: channel.description.clone(),
                    members_only: false,
                    forum: channel.channel_type == "forum",
                    ..Default::default()
                })
                .unwrap_or_else(|| RoomConfig {
                    name: room_jid
                        .node()
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "Room".to_string()),
                    members_only: false,
                    ..Default::default()
                });

            let (waddle_id, channel_id) = managed_channel
                .as_ref()
                .map(|channel| (channel.waddle_id.clone(), channel.id.clone()))
                .unwrap_or_else(|| parse_room_jid_context(room_jid));

            let Some(actor) =
                get_or_create_room_actor(state, room_jid, config, waddle_id, channel_id).await
            else {
                return vec![];
            };
            (actor, managed_channel.is_none())
        }
    };

    let effective_affiliation = if created_instant_room {
        Affiliation::Owner
    } else {
        Affiliation::Member
    };

    let join_outcome = match room_actor
        .ask(JoinWithAffiliation {
            sender_jid: sender_jid.clone(),
            nick: nick.to_string(),
            effective_affiliation,
            local_domain: domain.to_string(),
        })
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            let nick_collision = matches!(
                &error,
                kameo::error::SendError::HandlerError(
                    waddle_xmpp::muc::room_actor::RoomActorError::NickAlreadyInUse(_)
                )
            );
            if nick_collision {
                warn!(
                    room = %room_jid,
                    nick = %nick,
                    sender = %sender_jid,
                    "MUC nick collision; returning conflict"
                );
                return vec![build_muc_conflict_presence_xml(room_jid, nick, sender_jid)];
            }
            warn!(room = %room_jid, nick = %nick, error = ?error, "Failed to join MUC room");
            return vec![];
        }
    };

    let occupant_count = join_outcome.occupant_count;

    info!(room = %room_jid, nick = %nick, occupants = occupant_count, "User joined MUC room");

    let mut responses = Vec::new();

    // Replay one occupant presence per nick to the joiner. Same-bare multi-session
    // joins must not turn into duplicate room occupants on the wire.
    let mut replayed_nicks = std::collections::HashSet::new();
    for existing in join_outcome
        .existing_occupants
        .iter()
        .filter(|existing| existing.nick != nick)
        .filter(|existing| replayed_nicks.insert(existing.nick.clone()))
    {
        responses.push(build_muc_join_presence_xml(
            room_jid,
            &existing.nick,
            sender_jid,
            affiliation_str(existing.affiliation),
            role_str(existing.role),
            &existing.jid,
            false,
        ));
    }

    // Broadcast the new occupant's presence to all existing occupants.
    // Non-blocking: a zombied/slow consumer must never stall the join path,
    // which is how "Timed out waiting for self-presence" cascades start.
    // Drop accounting is handled inside `try_send_to` (logs + metrics);
    // per-occupant outcome is discarded here because a missed join
    // presence self-heals via the next MUC presence/probe round-trip.
    if !join_outcome.is_same_bare_multi_session_join {
        for existing in &join_outcome.existing_occupants {
            let presence_stanza =
                create_presence_stanza(room_jid, nick, sender_jid, &existing.jid, false);
            let stanza = Stanza::Presence(presence_stanza);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&existing.jid, stanza);
        }
    }

    // Send self-presence to the joining user (with status code 110)
    responses.push(build_muc_join_presence_xml(
        room_jid,
        nick,
        sender_jid,
        affiliation_str(join_outcome.new_occupant_affiliation),
        role_str(join_outcome.new_occupant_role),
        sender_jid,
        true,
    ));

    // Send room subject
    let room_name = room_jid
        .node()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "Waddle".to_string());
    responses.push(build_muc_subject_message_xml(
        room_jid, sender_jid, &room_name,
    ));

    responses
}

/// Handle MUC room leave
pub async fn handle_muc_leave(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_jid: &FullJid,
    nick: &str,
) -> Vec<String> {
    info!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave request");

    let Some(room_actor) = get_room_actor(state, room_jid).await else {
        debug!(room = %room_jid, "Room not found for leave");
        return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
    };

    let outcome = match room_actor
        .ask(LeaveByRealJid {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            debug!(room = %room_jid, nick = %nick, sender = %sender_jid, "MUC leave for absent occupant");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
        Err(error) => {
            warn!(room = %room_jid, nick = %nick, sender = %sender_jid, error = ?error, "Failed to leave MUC room");
            return vec![build_muc_self_unavailable_xml(room_jid, nick, sender_jid)];
        }
    };

    // Broadcast unavailable presence to remaining occupants (non-blocking).
    // Drop accounting is handled inside `try_send_to`.
    if outcome.removed_last_session {
        for occupant_jid in &outcome.remaining_occupants {
            let from_jid = room_jid
                .clone()
                .with_resource_str(&outcome.nick)
                .unwrap_or_else(|_| sender_jid.clone());
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(jid::Jid::from(from_jid));
            presence.to = Some(jid::Jid::from(occupant_jid.clone()));
            let stanza = Stanza::Presence(presence);
            let _outcome = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(occupant_jid, stanza);
        }
    }

    vec![build_muc_self_unavailable_xml(
        room_jid,
        &outcome.nick,
        sender_jid,
    )]
}

fn build_muc_join_presence_xml(
    room_jid: &BareJid,
    nick: &str,
    to_jid: &FullJid,
    affiliation: &str,
    role: &str,
    real_jid: &FullJid,
    include_self_status: bool,
) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let mut user_payload = Element::builder("x", "http://jabber.org/protocol/muc#user").append(
        Element::builder("item", "http://jabber.org/protocol/muc#user")
            .attr("affiliation", affiliation)
            .attr("role", role)
            .attr("jid", real_jid.to_string())
            .build(),
    );

    if include_self_status {
        user_payload = user_payload.append(
            Element::builder("status", "http://jabber.org/protocol/muc#user")
                .attr("code", "110")
                .build(),
        );
    }

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .append(user_payload.build())
            .build(),
    )
}

/// XEP-0045 §7.2.9 conflict presence: the requested nick is already in use
/// by a different user. The joiner receives a `<presence type='error'/>` and
/// no room state changes.
fn build_muc_conflict_presence_xml(room_jid: &BareJid, nick: &str, to_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| to_jid.clone());

    let error_payload = Element::builder("error", waddle_xmpp::ns::JABBER_CLIENT)
        .attr("type", "cancel")
        .append(Element::builder("conflict", "urn:ietf:params:xml:ns:xmpp-stanzas").build())
        .build();

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "error")
            .append(error_payload)
            .build(),
    )
}

fn build_muc_subject_message_xml(room_jid: &BareJid, to_jid: &FullJid, room_name: &str) -> String {
    element_to_xml(
        Element::builder("message", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", room_jid.to_string())
            .attr("to", to_jid.to_string())
            .attr("type", "groupchat")
            .append(
                Element::builder("subject", waddle_xmpp::ns::JABBER_CLIENT)
                    .append(format!("Welcome to {}!", room_name))
                    .build(),
            )
            .build(),
    )
}

fn build_muc_self_unavailable_xml(room_jid: &BareJid, nick: &str, sender_jid: &FullJid) -> String {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| sender_jid.clone());

    element_to_xml(
        Element::builder("presence", waddle_xmpp::ns::JABBER_CLIENT)
            .attr("from", from_jid.to_string())
            .attr("to", sender_jid.to_string())
            .attr("type", "unavailable")
            .append(
                Element::builder("x", "http://jabber.org/protocol/muc#user")
                    .append(
                        Element::builder("item", "http://jabber.org/protocol/muc#user")
                            .attr("affiliation", "member")
                            .attr("role", "none")
                            .build(),
                    )
                    .append(
                        Element::builder("status", "http://jabber.org/protocol/muc#user")
                            .attr("code", "110")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    )
}

/// Create a presence stanza for MUC
fn create_presence_stanza(
    room_jid: &BareJid,
    nick: &str,
    real_jid: &FullJid,
    to_jid: &FullJid,
    _is_self: bool,
) -> xmpp_parsers::presence::Presence {
    let from_jid = room_jid
        .clone()
        .with_resource_str(nick)
        .unwrap_or_else(|_| real_jid.clone());

    let mut presence = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::None);
    presence.from = Some(jid::Jid::from(from_jid));
    presence.to = Some(jid::Jid::from(to_jid.clone()));

    // In a full implementation, we'd add the MUC user extension here
    // For now, the XML generation handles it

    presence
}

/// Convert Affiliation to string
fn affiliation_str(affiliation: Affiliation) -> &'static str {
    match affiliation {
        Affiliation::Owner => "owner",
        Affiliation::Admin => "admin",
        Affiliation::Member => "member",
        Affiliation::Outcast => "outcast",
        Affiliation::None => "none",
    }
}

/// Convert Role to string
fn role_str(role: Role) -> &'static str {
    match role {
        Role::Moderator => "moderator",
        Role::Participant => "participant",
        Role::Visitor => "visitor",
        Role::None => "none",
    }
}

/// Derive waddle_id and channel_id from a room's bare JID node.

///
/// Convention: node is "waddleId_channelId" (first underscore separates).
/// Falls back to ("default", "default") if the node can't be parsed.
pub fn parse_room_jid_context(room_jid: &jid::BareJid) -> (String, String) {
    if let Some((waddle_id, channel_id)) = waddle_xmpp::parse_managed_room_jid(room_jid) {
        return (waddle_id, channel_id);
    }
    ("default".to_string(), "default".to_string())
}

pub async fn get_managed_channel_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Option<crate::server::routes::channels::ChannelResponse> {
    let (waddle_id, channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid)?;
    let waddle_db = state
        .deps
        .app_state
        .db_pool
        .get_waddle_db(&waddle_id)
        .await
        .ok()?;
    get_channel_from_db(&waddle_db, &waddle_id, &channel_id)
        .await
        .ok()
        .flatten()
}
