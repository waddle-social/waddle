//! The origin's side of a room request carried by the durable mailbox.
//!
//! When the mailbox carries room traffic and another node owns the room, a
//! join, occupant presence update, leave or relayed IQ is committed as a
//! room request and the handler waits for the owner's decision. The owner's frozen
//! response is read in the transaction that consumes the entry, so the
//! stanzas returned here are handed to the requester exactly once. No
//! payload of such a request travels on the ordered relay.

use jid::{BareJid, FullJid};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::ingress::mailbox::{RoomRequestKey, RoomRequestKind};
use waddle_xmpp::Stanza;
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::presence::Presence;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::WebSocketState;
use crate::auth::Session;
use crate::clustering::addressing::Ownership;
use crate::ingress::room_request::{RoomRequestCommit, RoomRequestSubmission};

/// How a room request was routed.
pub(crate) enum RoomRequestRoute {
    /// The mailbox does not carry this request; the caller keeps its other
    /// path. Nothing was written.
    NotCarried,
    /// The request was carried, or refused by this origin. The stanzas are
    /// the complete answer for the requester, in order.
    Answered(Vec<Stanza>),
}

/// A presence error from the room to the requester.
pub(crate) fn room_presence_error(
    room: &BareJid,
    nick: Option<&str>,
    occupant: &FullJid,
    error_type: ErrorType,
    condition: DefinedCondition,
) -> Stanza {
    let mut presence = Presence::new(xmpp_parsers::presence::Type::Error);
    presence.from = Some(
        match nick.and_then(|nick| room.with_resource_str(nick).ok()) {
            Some(occupant_jid) => occupant_jid.into(),
            None => room.clone().into(),
        },
    );
    presence.to = Some(occupant.clone().into());
    presence
        .payloads
        .push(StanzaError::new(error_type, condition, "en", "").into());
    Stanza::Presence(presence)
}

/// Whether `stanzas` contain the owner's refusal of a presence request.
pub(crate) fn presence_request_refused(stanzas: &[Stanza]) -> bool {
    stanzas.iter().any(|stanza| {
        matches!(
            stanza,
            Stanza::Presence(presence)
                if presence.type_ == xmpp_parsers::presence::Type::Error
        )
    })
}

/// The error that answers `request` when it cannot be carried or decided:
/// a presence error from the room for a presence request, an IQ error for
/// an IQ. A message is never a room request and is answered with nothing.
pub(crate) fn room_request_error(
    request: &Stanza,
    room: &BareJid,
    occupant: &FullJid,
    error_type: ErrorType,
    condition: DefinedCondition,
) -> Vec<Stanza> {
    match request {
        Stanza::Presence(presence) => vec![room_presence_error(
            room,
            presence
                .to
                .as_ref()
                .and_then(|to| to.resource())
                .map(|resource| resource.as_str()),
            occupant,
            error_type,
            condition,
        )],
        Stanza::Iq(iq) => vec![Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::Error {
            from: iq.to().cloned(),
            to: Some(occupant.clone().into()),
            id: iq.id().into(),
            error: StanzaError::new(error_type, condition, "en", ""),
            payload: None,
        }))],
        Stanza::Message(_) => Vec::new(),
    }
}

/// Carry a join, presence update or leave to a room owned by another node.
pub(crate) async fn route_presence_request(
    state: &WebSocketState,
    session: Option<&Session>,
    kind: RoomRequestKind,
    room: &BareJid,
    occupant: &FullJid,
    generation: OccupancySessionGeneration,
    stanza: Stanza,
) -> RoomRequestRoute {
    let principal = session.and_then(|session| session.authenticated_principal_ref().ok());
    route_room_request(state, principal, kind, room, occupant, generation, stanza).await
}

/// Carry a room request to a room owned by another node.
///
/// The request key is minted here, before the first owner attempt. A full
/// lane or node is answered with `resource-constraint` and writes nothing.
/// A wait that ends without a decision is answered with
/// `service-unavailable`; the request stays for the owner or an operator.
pub(crate) async fn route_room_request(
    state: &WebSocketState,
    principal: Option<AuthenticatedPrincipalRef>,
    kind: RoomRequestKind,
    room: &BareJid,
    occupant: &FullJid,
    generation: OccupancySessionGeneration,
    stanza: Stanza,
) -> RoomRequestRoute {
    let authority = &state.deps.protocol.ingress;
    if !authority.mailbox_acceptance_enabled() {
        return RoomRequestRoute::NotCarried;
    }
    let Some(principal) = principal else {
        return RoomRequestRoute::NotCarried;
    };
    // A room this node owns, or one nobody has claimed, is served by the
    // local room path. A claim whose owner has not renewed its lease still
    // names another node: the request waits in the mailbox for that owner or
    // its successor.
    if !matches!(
        state
            .deps
            .app_state
            .clustering_claims
            .room_owner(room)
            .await,
        Ok(Ownership::Remote(_) | Ownership::Stale)
    ) {
        return RoomRequestRoute::NotCarried;
    }
    let unavailable =
        |condition| room_request_error(&stanza, room, occupant, ErrorType::Wait, condition);
    let commit = authority
        .commit_room_request(RoomRequestSubmission {
            key: RoomRequestKey::mint(),
            kind,
            room: room.clone(),
            occupant: occupant.clone(),
            generation,
            principal,
            stanza: stanza.clone(),
        })
        .await;
    let pending = match commit {
        Ok(RoomRequestCommit::NotCarried) => return RoomRequestRoute::NotCarried,
        Ok(RoomRequestCommit::Refused(refusal)) => {
            tracing::warn!(%refusal, room = %room, "room request refused at admission");
            return RoomRequestRoute::Answered(unavailable(DefinedCondition::ResourceConstraint));
        }
        Ok(RoomRequestCommit::Committed(pending)) => pending,
        Err(error) => {
            tracing::warn!(%error, room = %room, "room request not committed");
            return RoomRequestRoute::Answered(unavailable(DefinedCondition::ServiceUnavailable));
        }
    };
    match authority.await_room_request(&pending, || false).await {
        Ok(resolution) => RoomRequestRoute::Answered(resolution.responses),
        Err(reason) => {
            tracing::warn!(%reason, room = %room, "room request unresolved");
            RoomRequestRoute::Answered(unavailable(DefinedCondition::ServiceUnavailable))
        }
    }
}
