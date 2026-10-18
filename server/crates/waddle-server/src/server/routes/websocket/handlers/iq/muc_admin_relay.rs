//! XEP-0045 muc#admin list GETs for rooms owned by another node (#1298).
//!
//! A room's affiliation list lives in the actor of the node that owns the
//! room's claim: durable grants plus permission-derived affiliations, which
//! are never persisted. A requester connected to any other node therefore
//! forwards its list GET to that owner over
//! `waddle.clustering.relay.muc_admin_query.v1`, and the owner answers through
//! [`super::muc_admin::answer_admin_list_get`], the same function that answers
//! a local GET. Only GETs are forwarded; a forwarded GET is never forwarded
//! again.
use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use xmpp_parsers::iq::{Iq, IqHeader, IqPayload};
use xmpp_parsers::stanza_error::StanzaError;

use super::muc_admin::{answer_admin_list_get, AdminListAnswer};
use super::*;
use crate::clustering::codec::RemoteStanza;
use crate::clustering::relay::{
    RelayAskError, RelayHandle, RelayMucAdminQuery, RelayMucAdminQueryReply,
};
use crate::server::routes::websocket::handlers::iq::errors::resource_constraint_iq_error;

/// Receiver-side windows for one owner ask; kameo enforces them on the owner.
/// The owner makes two bounded actor asks, so the reply window covers both.
pub(crate) const MUC_ADMIN_QUERY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const MUC_ADMIN_QUERY_REPLY_TIMEOUT: Duration = Duration::from_secs(6);
/// The origin's own bound on one ask, relay lookup included.
const ASK_LIMIT: Duration = MUC_ADMIN_QUERY_MAILBOX_TIMEOUT
    .saturating_add(MUC_ADMIN_QUERY_REPLY_TIMEOUT)
    .saturating_add(Duration::from_millis(500));

/// Replaces only the transport hop to the owner.
#[cfg(test)]
pub(crate) type TestMucAdminOwnerRelay = std::sync::Arc<
    dyn Fn(
            waddle_xmpp::ownership::NodeIdentity,
            RelayMucAdminQuery,
        )
            -> futures::future::BoxFuture<'static, Result<RelayMucAdminQueryReply, RelayAskError>>
        + Send
        + Sync,
>;

#[cfg(test)]
tokio::task_local! {
    pub(crate) static TEST_MUC_ADMIN_OWNER_RELAY: TestMucAdminOwnerRelay;
}

/// Origin side: answer a list GET for a room with no local actor.
pub(super) async fn forward_admin_list_get(
    state: &WebSocketState,
    sender_jid: &FullJid,
    iq_with_from: &Iq,
    query: &waddle_xmpp::muc::AdminQuery,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> String {
    let error_frame = |error: StanzaError| {
        build_iq_error_xml_typed(iq_with_from.id(), response_from, response_to, error)
    };
    let message = RelayMucAdminQuery {
        room: query.room_jid.clone(),
        requester: sender_jid.clone(),
        query: RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(iq_with_from.clone()))),
        trace: Default::default(),
    };
    // The claim may move between the read and the ask; follow it once.
    let mut reresolved = false;
    loop {
        let owner =
            match crate::server::routes::interpret::extension_room_owner(state, &query.room_jid)
                .await
            {
                // Unclaimed, or claimed by this node with no live actor: the
                // room has no live owner to answer, exactly as before #1298.
                Ok(None) => {
                    return error_frame(item_not_found_iq_error("Requested item not found."))
                }
                Ok(Some(owner)) => owner,
                // The owner's lease is stale or the lookup failed: ownership is
                // moving. Retryable, never "no such room".
                Err(_) => {
                    return error_frame(resource_constraint_iq_error(
                        "the room is changing owner; retry shortly",
                    ))
                }
            };
        match ask_owner(state, &owner, message.clone()).await {
            Ok(RelayMucAdminQueryReply::Answered(answer)) => {
                if let RemoteStanza(waddle_xmpp::Stanza::Iq(answer)) = *answer {
                    if answers(&answer, iq_with_from, sender_jid) {
                        return iq_to_xml(*answer);
                    }
                }
                tracing::warn!(
                    room = %query.room_jid,
                    owner = %owner.node_id,
                    "room owner answered a muc#admin list GET with a mismatched stanza"
                );
                return error_frame(internal_server_error_iq_error("Internal server error."));
            }
            Ok(RelayMucAdminQueryReply::NotOwner) if !reresolved => reresolved = true,
            Ok(RelayMucAdminQueryReply::NotOwner) => {
                return error_frame(resource_constraint_iq_error(
                    "the room is changing owner; retry shortly",
                ));
            }
            Err(error) => {
                tracing::warn!(
                    room = %query.room_jid,
                    owner = %owner.node_id,
                    %error,
                    "muc#admin list GET could not reach the room owner"
                );
                return error_frame(resource_constraint_iq_error(
                    "the room's owner is unavailable; retry shortly",
                ));
            }
        }
    }
}

/// The owner's answer must be the response to this exact request.
fn answers(answer: &Iq, request: &Iq, requester: &FullJid) -> bool {
    let (header, payload) = answer.clone().split();
    matches!(payload, IqPayload::Result(_) | IqPayload::Error(_))
        && header.id == request.id()
        && header.to == Some(Jid::from(requester.clone()))
}

async fn ask_owner(
    state: &WebSocketState,
    owner: &waddle_xmpp::ownership::NodeIdentity,
    message: RelayMucAdminQuery,
) -> Result<RelayMucAdminQueryReply, RelayAskError> {
    #[cfg(test)]
    if let Ok(relay) = TEST_MUC_ADMIN_OWNER_RELAY.try_with(std::sync::Arc::clone) {
        return relay(owner.clone(), message).await;
    }
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return Err(RelayAskError::NotFound {
            node_id: crate::clustering::NodeId::new(owner.node_id.clone()),
        });
    };
    let mut handle: RelayHandle = bridge.muc_admin_query_handle(owner);
    tokio::time::timeout(ASK_LIMIT, handle.muc_admin_query(message))
        .await
        .unwrap_or_else(|_| {
            Err(RelayAskError::Send {
                failure: crate::clustering::relay::RelaySendFailure::ReplyTimeout,
                effect: crate::clustering::relay::RelaySendEffect::NoEffect,
                message: "the room owner did not answer in time".to_owned(),
            })
        })
}

/// Owner side: answer a forwarded list GET from the local room actor.
pub(crate) async fn relayed_muc_admin_query(
    state: &WebSocketState,
    message: RelayMucAdminQuery,
) -> RelayMucAdminQueryReply {
    let RelayMucAdminQuery {
        room,
        requester,
        query,
        ..
    } = message;
    let RemoteStanza(waddle_xmpp::Stanza::Iq(request)) = query else {
        return answered_error(
            &room,
            &requester,
            "",
            bad_request_iq_error("Malformed MUC admin query."),
        );
    };
    let request = *request;
    let well_formed =
        matches!(&request, Iq::Get { .. }) && request.from() == Some(&Jid::from(requester.clone()));
    let parsed = parse_admin_query(&request, state.deps.service_domains.muc.as_str());
    let admin_query = match parsed {
        Ok(admin_query) if well_formed && admin_query.is_get && admin_query.room_jid == room => {
            admin_query
        }
        _ => {
            return answered_error(
                &room,
                &requester,
                request.id(),
                bad_request_iq_error("Malformed MUC admin query."),
            )
        }
    };
    let room_actor = match get_room_actor_result(state, &room).await {
        Ok(Some(room_actor)) => room_actor,
        // No live actor here: the claim moved, so the origin re-resolves.
        Ok(None) | Err(_) => return RelayMucAdminQueryReply::NotOwner,
    };
    match answer_admin_list_get(&room_actor, &requester, &admin_query, request.id()).await {
        AdminListAnswer::Result(iq) => {
            RelayMucAdminQueryReply::Answered(Box::new(RemoteStanza(waddle_xmpp::Stanza::Iq(iq))))
        }
        AdminListAnswer::Error(error) => answered_error(&room, &requester, request.id(), *error),
    }
}

fn answered_error(
    room: &BareJid,
    requester: &FullJid,
    id: &str,
    error: StanzaError,
) -> RelayMucAdminQueryReply {
    let iq = IqPayload::Error(error).assemble(IqHeader {
        from: Some(Jid::from(room.clone())),
        to: Some(Jid::from(requester.clone())),
        id: id.to_owned(),
    });
    RelayMucAdminQueryReply::Answered(Box::new(RemoteStanza(waddle_xmpp::Stanza::Iq(Box::new(
        iq,
    )))))
}
