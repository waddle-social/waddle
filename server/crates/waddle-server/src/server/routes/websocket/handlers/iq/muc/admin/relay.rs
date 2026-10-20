//! XEP-0045 muc#admin list GETs for rooms owned by another node (#1298).
//!
//! A room's affiliation list lives in the actor of the node that owns the
//! room's claim: durable grants plus permission-derived affiliations, which
//! are never persisted. A requester connected to any other node therefore
//! forwards its list request to that owner over
//! `waddle.clustering.relay.muc_admin_query.v1`, and the owner answers through
//! [`super::super::super::muc::admin::answer_admin_list_get`], the same function that answers
//! a local GET. The answer travels as typed data and the origin builds the
//! response stanza itself, so it leaves with the local wire shape. Only GETs
//! are forwarded; a forwarded GET is never forwarded again.
use std::time::Duration;

use jid::FullJid;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::super::super::muc::admin::answer_admin_list_get;
#[cfg(test)]
pub(crate) use super::super::super::muc::admin::AdminRoleEntry;
pub(crate) use super::super::super::muc::admin::{AdminListAnswer, AdminListDenial};
use super::super::super::*;
use crate::clustering::relay::{
    RelayAskError, RelayHandle, RelayMucAdminQuery, RelayMucAdminQueryItem, RelayMucAdminQueryReply,
};
use crate::server::routes::websocket::handlers::iq::errors::resource_constraint_iq_error;

/// Receiver-side windows for one owner ask; kameo enforces them on the owner.
pub(crate) const MUC_ADMIN_QUERY_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const MUC_ADMIN_QUERY_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// The owner's whole answer — room lookup and both actor asks — fits inside
/// the reply window, so the origin hears a definite answer, not a timeout.
const OWNER_ANSWER_BUDGET: Duration = Duration::from_secs(4);
/// The origin's own bound on one ask, relay lookup included.
const ASK_LIMIT: Duration = MUC_ADMIN_QUERY_MAILBOX_TIMEOUT
    .saturating_add(MUC_ADMIN_QUERY_REPLY_TIMEOUT)
    .saturating_add(Duration::from_millis(500));
/// The origin's whole wait — claim reads, the ask and its one re-ask —
/// stays well inside the 15 s stanza-handler backstop.
const ORIGIN_BUDGET: Duration = Duration::from_secs(11);
/// Waddle's own clients send one item; a GET this large is not a list query.
const MAX_FORWARDED_ITEMS: usize = 64;
/// Kameo's remote response ceiling is 10 MiB; leave headroom for framing.
const MAX_ANSWER_BYTES: usize = 8 * 1024 * 1024;
/// An upper bound on one entry's encoding beyond its JID and nickname text:
/// field names, enum tags and length prefixes.
const ENTRY_OVERHEAD_BYTES: usize = 96;

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
pub(in crate::server::routes::websocket::handlers::iq) async fn forward_admin_list_get(
    state: &WebSocketState,
    sender_jid: &FullJid,
    query: &waddle_xmpp::muc::AdminQuery,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> String {
    let error_frame = |error: StanzaError| {
        build_iq_error_xml_typed(&query.iq_id, response_from, response_to, error)
    };
    if query.items.len() > MAX_FORWARDED_ITEMS {
        return error_frame(bad_request_iq_error("Too many MUC admin query items."));
    }
    let message = RelayMucAdminQuery {
        room: query.room_jid.clone(),
        requester: sender_jid.clone(),
        items: query
            .items
            .iter()
            .map(|item| RelayMucAdminQueryItem {
                affiliation: item.affiliation,
                role: item.role,
            })
            .collect(),
        trace: Default::default(),
    };
    let deadline = tokio::time::Instant::now() + ORIGIN_BUDGET;
    match tokio::time::timeout_at(deadline, ask_current_owner(state, &message, deadline)).await {
        Ok(Forwarded::Answer(answer)) => {
            answer.into_frame(query, sender_jid, response_from, response_to)
        }
        // Unclaimed, or claimed by this node with no live actor: the room
        // has no live owner to answer, exactly as before #1298.
        Ok(Forwarded::NoOwner) => error_frame(item_not_found_iq_error("Requested item not found.")),
        Ok(Forwarded::TooLarge) => error_frame(StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::ResourceConstraint,
            "en",
            "the list is too large to fetch from another node",
        )),
        // Ownership is moving, or the owner is unreachable: retryable, never
        // "no such room".
        Ok(Forwarded::Unavailable) | Err(_) => error_frame(resource_constraint_iq_error(
            "the room's owner is unavailable; retry shortly",
        )),
    }
}

enum Forwarded {
    Answer(AdminListAnswer),
    NoOwner,
    TooLarge,
    Unavailable,
}

async fn ask_current_owner(
    state: &WebSocketState,
    message: &RelayMucAdminQuery,
    deadline: tokio::time::Instant,
) -> Forwarded {
    // The claim may move between the read and the ask; follow it once.
    let mut reresolved = false;
    loop {
        let owner = match crate::server::routes::interpret::extension_room_owner(
            state,
            &message.room,
        )
        .await
        {
            Ok(None) => return Forwarded::NoOwner,
            Ok(Some(owner)) => owner,
            // The owner's lease is stale or the lookup failed.
            Err(_) => return Forwarded::Unavailable,
        };
        match ask_owner(state, &owner, message.clone(), deadline).await {
            Ok(RelayMucAdminQueryReply::Answered(answer)) => return Forwarded::Answer(answer),
            Ok(RelayMucAdminQueryReply::TooLarge) => return Forwarded::TooLarge,
            Ok(RelayMucAdminQueryReply::NotOwner) if !reresolved => reresolved = true,
            Ok(RelayMucAdminQueryReply::NotOwner) => return Forwarded::Unavailable,
            Err(error) => {
                tracing::warn!(
                    room = %message.room,
                    owner = %owner.node_id,
                    %error,
                    "muc#admin list GET could not reach the room owner"
                );
                return Forwarded::Unavailable;
            }
        }
    }
}

async fn ask_owner(
    state: &WebSocketState,
    owner: &waddle_xmpp::ownership::NodeIdentity,
    message: RelayMucAdminQuery,
    deadline: tokio::time::Instant,
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
    let limit = deadline.min(tokio::time::Instant::now() + ASK_LIMIT);
    tokio::time::timeout_at(limit, handle.muc_admin_query(message))
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
    if message.items.len() > MAX_FORWARDED_ITEMS {
        // Origins bound this; an oversized forward is not a list query.
        return RelayMucAdminQueryReply::Answered(AdminListAnswer::Denied(
            AdminListDenial::Forbidden,
        ));
    }
    let query = waddle_xmpp::muc::AdminQuery {
        room_jid: message.room.clone(),
        iq_id: String::new(),
        from: message.requester.clone().into(),
        items: message
            .items
            .iter()
            .map(|item| waddle_xmpp::muc::AdminItem {
                jid: None,
                nick: None,
                affiliation: item.affiliation,
                role: item.role,
                reason: None,
            })
            .collect(),
        is_get: true,
    };
    let answer = tokio::time::timeout(OWNER_ANSWER_BUDGET, async {
        match get_room_actor_result(state, &message.room).await {
            Ok(Some(room_actor)) => {
                Some(answer_admin_list_get(&room_actor, &message.requester, &query).await)
            }
            // No live actor here: the claim moved, so the origin re-resolves.
            Ok(None) | Err(_) => None,
        }
    })
    .await;
    match answer {
        Ok(Some(answer)) if encoded_size_bound(&answer) > MAX_ANSWER_BYTES => {
            RelayMucAdminQueryReply::TooLarge
        }
        Ok(Some(answer)) => RelayMucAdminQueryReply::Answered(answer),
        Ok(None) => RelayMucAdminQueryReply::NotOwner,
        // A wedged actor answers as a local GET would.
        Err(_) => {
            RelayMucAdminQueryReply::Answered(AdminListAnswer::Denied(AdminListDenial::Internal))
        }
    }
}

/// An upper bound on the answer's encoded size, without encoding it.
fn encoded_size_bound(answer: &AdminListAnswer) -> usize {
    match answer {
        AdminListAnswer::Affiliations(items) => items
            .iter()
            .map(|(jid, _)| jid.as_str().len() + ENTRY_OVERHEAD_BYTES)
            .sum(),
        AdminListAnswer::Roles(entries) => entries
            .iter()
            .map(|entry| entry.nick.len() + entry.real_jid.as_str().len() + ENTRY_OVERHEAD_BYTES)
            .sum(),
        AdminListAnswer::Denied(_) => 0,
    }
}
