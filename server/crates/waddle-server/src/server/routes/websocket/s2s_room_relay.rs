//! Carrying a remote principal's request to the replica that holds its
//! room (ADR-0020 D10), over
//! `waddle.clustering.relay.remote_room_request.v1`.
//!
//! A room's actor lives on the replica that owns the room's claim, and a
//! peer's inbound S2S stream lands on whichever replica accepted it. A
//! stanza for a room another replica holds is carried there before
//! anything is decided or seated for it: with the stanza travel the facts
//! of the stream that carried it (the sender's address, the authentication
//! strength, the trust-profile kind and the snapshot the stream was
//! admitted under), and nothing decided from them. The owner decides peer
//! admission and the sender's standing again under the snapshot it runs,
//! seats the occupant, and answers through its own S2S router, so every
//! stanza for the occupant leaves on one stream in the room's order.
//!
//! The reply to the carrying replica says only whether the owner queued
//! the stanza. A stanza the owner could not take is answered here with a
//! retryable error; one whose hand-over has an unknown outcome is answered
//! the same way, although the owner may still decide it. An owner that did
//! not answer in time or could not be reached is reported as such
//! ([`RoomPlacement::Silent`]), so that the router answers the room's next
//! requests without asking it again for a while.
//!
//! Placing one request, every lookup of the room's claim and catalog row
//! and every ask of its owner included, takes at most [`HOLDER_ASK_LIMIT`],
//! below the router's own bound on it. Time that runs out during an ask
//! reports the owner as silent; time that runs out during a lookup is a
//! retryable refusal and says nothing of the owner.

use std::sync::Weak;
use std::time::Duration;

use async_trait::async_trait;
use jid::BareJid;
use waddle_xmpp::Stanza;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

use super::s2s_rooms::RoomCarrier;
use super::WebSocketState;
use crate::clustering::codec::RemoteStanza;
use crate::clustering::relay::{
    RelayAskError, RelayRemoteRoomRequest, RelayRemoteRoomRequestReply, RelaySendFailure,
    RelayStreamStrength, RelayTrustProfile,
};
use crate::clustering::NodeId;
use crate::s2s::conn::AuthenticationStrength;
use crate::s2s::route::{
    CarriedPrincipal, CarriedRoomOutcome, CarriedRoomRequest, RemotePrincipal, RoomPlacement,
    HOLDER_ASK_LIMIT,
};
use crate::s2s::verify::TrustProfileKind;

/// Receiver-side windows for one owner ask; kameo enforces them on the
/// owner. The owner only queues the stanza, so both are short.
pub(crate) const REMOTE_ROOM_REQUEST_MAILBOX_TIMEOUT: Duration = Duration::from_secs(1);
pub(crate) const REMOTE_ROOM_REQUEST_REPLY_TIMEOUT: Duration = Duration::from_secs(3);
/// The origin's own bound on placing one request, every lookup and ask
/// included: the host's bound on placing a request, which leaves the
/// owner's two windows and half a second for the lookups and the
/// transport of one ask. A second ask, after the claim moved, has what
/// remains.
const PLACE_LIMIT: Duration = HOLDER_ASK_LIMIT;
const _: () = assert!(
    REMOTE_ROOM_REQUEST_MAILBOX_TIMEOUT
        .saturating_add(REMOTE_ROOM_REQUEST_REPLY_TIMEOUT)
        .saturating_add(Duration::from_millis(500))
        .as_millis()
        <= PLACE_LIMIT.as_millis()
);

/// The answer for a request whose room could not be looked up before the
/// placement's deadline: nothing is said of the room's owner.
fn lookup_timed_out() -> RoomPlacement {
    retry_later("The room could not be looked up in time; try again later.")
}

/// [`RoomCarrier`] over the cluster's room claims and relay.
pub(super) struct ClusterRoomCarrier {
    state: Weak<WebSocketState>,
}

impl ClusterRoomCarrier {
    pub(super) fn new(state: &std::sync::Arc<WebSocketState>) -> Self {
        Self {
            state: std::sync::Arc::downgrade(state),
        }
    }
}

/// What the channel catalog, which every replica reads, says of whether
/// `room` is open to other servers.
enum Catalogued {
    /// Its row says so: its `federated` setting is on and it is no group
    /// conversation of this deployment's accounts.
    Open,
    /// It has no row, or its row says otherwise, and nothing else could
    /// have opened it: it is answered as a room that does not exist.
    NotOpen,
    /// It has no row while this deployment opens its new rooms to other
    /// servers, or its row cannot be read: only the room's holder can
    /// tell.
    Unknown,
}

/// Read `room`'s row of the channel catalog.
///
/// A room without a row keeps its `federated` setting in its actor alone.
/// While the deployment's default for new rooms is off
/// (`[host_policy] federate_new_rooms`), such a room is open only if an
/// owner turned the setting on by hand, and it is not carried: were every
/// address without a row carried, a remote sender could tell a room that
/// exists and is closed from one that does not exist by whether its
/// holder answers, and could make this replica ask another one for any
/// address it names. The cost is that a room without a row that an owner
/// opened by hand is reached only through the replica that holds it.
async fn catalogued(state: &WebSocketState, room: &BareJid) -> Catalogued {
    match super::handlers::presence::get_managed_channel_for_room(state, room).await {
        Ok(Some(channel))
            if channel.federated
                && channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM =>
        {
            Catalogued::Open
        }
        Ok(Some(_)) => Catalogued::NotOpen,
        Ok(None) if !state.deps.app_state.federates_new_rooms() => Catalogued::NotOpen,
        Ok(None) | Err(_) => Catalogued::Unknown,
    }
}

/// Whether a failed ask says that the owner did not answer or could not be
/// reached, rather than that it answered without taking the request.
pub(crate) fn owner_silent(error: &RelayAskError) -> bool {
    match error {
        // The owner's relay is not in the swarm.
        RelayAskError::NotFound { .. } => true,
        RelayAskError::Send { failure, .. } => match failure {
            // No answer within the owner's windows or this ask's bound, or
            // no connection to the owner.
            RelaySendFailure::ReplyTimeout | RelaySendFailure::Transport => true,
            // The owner answered: its mailbox is full, its handler failed,
            // or it predates the message.
            RelaySendFailure::MailboxFull | RelaySendFailure::Handler | RelaySendFailure::Codec => {
                false
            }
            // The cached address of the owner's relay is stale; the next
            // ask looks it up again.
            RelaySendFailure::StaleRef => false,
        },
        // This replica is stopping.
        RelayAskError::Cancelled => false,
    }
}

fn retry_later(text: &str) -> RoomPlacement {
    RoomPlacement::Refused(Box::new(StanzaError::new(
        ErrorType::Wait,
        DefinedCondition::ResourceConstraint,
        "en",
        text,
    )))
}

fn relayed_strength(strength: AuthenticationStrength) -> RelayStreamStrength {
    match strength {
        AuthenticationStrength::Pkix => RelayStreamStrength::Pkix,
        AuthenticationStrength::Dialback => RelayStreamStrength::Dialback,
    }
}

fn relayed_profile(profile: TrustProfileKind) -> RelayTrustProfile {
    match profile {
        TrustProfileKind::PublicWebPki => RelayTrustProfile::PublicWebPki,
        TrustProfileKind::PrivateCa => RelayTrustProfile::PrivateCa,
    }
}

#[async_trait]
impl RoomCarrier for ClusterRoomCarrier {
    async fn place(
        &self,
        room: &BareJid,
        principal: &RemotePrincipal,
        stanza: &Stanza,
    ) -> RoomPlacement {
        let Some(state) = self.state.upgrade() else {
            return RoomPlacement::Here;
        };
        let deadline = tokio::time::Instant::now() + PLACE_LIMIT;
        // The claim may move between the read and the ask; follow it once.
        let mut reresolved = false;
        loop {
            let lookup = crate::server::routes::interpret::extension_room_owner(&state, room);
            let Ok(resolved) = tokio::time::timeout_at(deadline, lookup).await else {
                return lookup_timed_out();
            };
            // A room the catalog does not say is open to other servers is
            // answered here, as a room that does not exist is: were it
            // carried, a holder that is busy, moving or out of reach would
            // answer otherwise and tell the two apart.
            if !matches!(resolved, Ok(None)) {
                let Ok(catalogued) =
                    tokio::time::timeout_at(deadline, catalogued(&state, room)).await
                else {
                    return lookup_timed_out();
                };
                if matches!(catalogued, Catalogued::NotOpen) {
                    return RoomPlacement::Here;
                }
            }
            let owner = match resolved {
                // This replica holds the room, or no replica does.
                Ok(None) => return RoomPlacement::Here,
                Ok(Some(owner)) => owner,
                // The owner's lease is stale or the lookup failed.
                Err(_) => {
                    return retry_later("The room is moving between server nodes; try again later.")
                }
            };
            let message = RelayRemoteRoomRequest {
                room: room.clone(),
                sender: principal.address().clone(),
                strength: relayed_strength(principal.strength()),
                trust_profile: principal.trust_profile().map(relayed_profile),
                snapshot: principal.generation(),
                stanza: RemoteStanza(stanza.clone()),
                trace: Default::default(),
            };
            // Time the lookups spent is not the owner's: an ask with none
            // left says nothing of it.
            if tokio::time::Instant::now() >= deadline {
                return lookup_timed_out();
            }
            match ask_owner(&state, &owner, message, deadline).await {
                Ok(RelayRemoteRoomRequestReply::Queued) => return RoomPlacement::Carried,
                Ok(RelayRemoteRoomRequestReply::NotOwner) if !reresolved => reresolved = true,
                Ok(RelayRemoteRoomRequestReply::Busy) => {
                    return retry_later("The room is busy; try again later.")
                }
                Ok(
                    RelayRemoteRoomRequestReply::NotOwner | RelayRemoteRoomRequestReply::Refused,
                ) => {
                    return retry_later(
                        "The room is held by another server node that did not take the \
                         request; try again later.",
                    )
                }
                // A node that predates this message answers `UnknownMessage`
                // and ran nothing; any other failure may have queued the
                // stanza, and is answered the same way. An owner that did
                // not answer or could not be reached is reported as silent.
                Err(error) => {
                    tracing::warn!(
                        owner = %owner.node_id,
                        %error,
                        "a remote principal's room request could not be carried to the room owner"
                    );
                    if owner_silent(&error) {
                        return RoomPlacement::Silent;
                    }
                    return retry_later(
                        "The room is held by another server node that could not be reached; \
                         try again later.",
                    );
                }
            }
        }
    }
}

/// Ask `owner` to take `message`, until `deadline`: an ask the deadline
/// outlasts, or one begun after it, is a reply timeout.
async fn ask_owner(
    state: &WebSocketState,
    owner: &waddle_xmpp::ownership::NodeIdentity,
    message: RelayRemoteRoomRequest,
    deadline: tokio::time::Instant,
) -> Result<RelayRemoteRoomRequestReply, RelayAskError> {
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return Err(RelayAskError::NotFound {
            node_id: NodeId::new(owner.node_id.clone()),
        });
    };
    let mut handle = bridge.remote_room_request_handle(owner);
    tokio::time::timeout_at(deadline, handle.remote_room_request(message))
        .await
        .unwrap_or_else(|_| {
            Err(RelayAskError::Send {
                failure: crate::clustering::relay::RelaySendFailure::ReplyTimeout,
                effect: crate::clustering::relay::RelaySendEffect::MaybeCommitted,
                message: "the room owner did not answer in time".to_owned(),
            })
        })
}

/// Owner side: queue a carried stanza for the local room host, when this
/// replica still holds the room.
pub(crate) async fn relayed_remote_room_request(
    state: &WebSocketState,
    message: RelayRemoteRoomRequest,
) -> RelayRemoteRoomRequestReply {
    match crate::server::routes::interpret::extension_room_owner(state, &message.room).await {
        // This replica holds the room, or no replica does: it is decided
        // here.
        Ok(None) => {}
        // The claim moved, or names a node whose lease lapsed: the origin
        // resolves the owner again.
        Ok(Some(_)) | Err(_) => return RelayRemoteRoomRequestReply::NotOwner,
    }
    let request = CarriedRoomRequest {
        principal: CarriedPrincipal {
            address: message.sender,
            strength: match message.strength {
                RelayStreamStrength::Pkix => AuthenticationStrength::Pkix,
                RelayStreamStrength::Dialback => AuthenticationStrength::Dialback,
            },
            trust_profile: message.trust_profile.map(|profile| match profile {
                RelayTrustProfile::PublicWebPki => TrustProfileKind::PublicWebPki,
                RelayTrustProfile::PrivateCa => TrustProfileKind::PrivateCa,
            }),
            generation: message.snapshot,
        },
        stanza: message.stanza.0,
    };
    match state.deps.remote_route.carried_room_request(request).await {
        CarriedRoomOutcome::Queued => RelayRemoteRoomRequestReply::Queued,
        CarriedRoomOutcome::Busy => RelayRemoteRoomRequestReply::Busy,
        CarriedRoomOutcome::Refused => RelayRemoteRoomRequestReply::Refused,
    }
}
