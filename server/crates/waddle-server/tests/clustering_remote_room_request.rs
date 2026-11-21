//! The cluster relay message that carries a remote principal's room
//! request to the replica that holds the room
//! (`waddle.clustering.relay.remote_room_request.v1`; ADR-0020 D10).
//!
//! The message is a new relay message id: the ordered-relay envelope and
//! its id are unchanged, so a replica that predates it answers
//! `UnknownMessage` before any handler runs and the carrying replica
//! answers the stanza's sender with a retryable error. It carries the
//! stanza and the facts of the stream that carried it, and its reply says
//! only whether the owner queued the stanza.
//!
//! A failed ask says that the owner is silent, so that the room's requests
//! are answered for a while without asking it, only where the owner did
//! not answer or could not be reached; an owner that answered without
//! taking the request, or a replica that is stopping, is not silent.
//!
//! The cases need no cluster: they encode the message with the codec the
//! relay uses, run the owner side over a WebSocket state in this process
//! and classify the relay's typed failures.
#![cfg(all(feature = "test-support", feature = "clustering"))]

use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::relay::{
    RelayActor, RelayAskError, RelayDeliverOrdered, RelayRemoteRoomRequest,
    RelayRemoteRoomRequestReply, RelaySendEffect, RelaySendFailure, RelayStreamStrength,
    RelayTrustProfile,
};
use waddle_server::clustering::NodeId;
use waddle_server::test_support::{
    relayed_remote_room_request, room_owner_silent, websocket_state,
};
use waddle_xmpp::Stanza;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const ROOM: &str = "lounge@muc.example.com";
const BOB: &str = "bob@home-b.test/phone";

fn join() -> Stanza {
    let mut presence = Presence::new(PresenceType::None);
    presence.from = Some(BOB.parse().expect("sender"));
    presence.to = Some([ROOM, "/bob"].concat().parse().expect("occupant"));
    presence.id = Some("join-1".to_owned());
    Stanza::Presence(presence)
}

fn request() -> RelayRemoteRoomRequest {
    RelayRemoteRoomRequest {
        room: ROOM.parse().expect("room"),
        sender: BOB.parse().expect("sender"),
        strength: RelayStreamStrength::Pkix,
        trust_profile: Some(RelayTrustProfile::PrivateCa),
        snapshot: uuid::Uuid::from_u128(7),
        stanza: RemoteStanza(join()),
        trace: Default::default(),
    }
}

#[test]
fn the_message_is_a_new_relay_id_and_leaves_the_ordered_envelope_id_alone() {
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayRemoteRoomRequest>>::REMOTE_ID,
        "waddle.clustering.relay.remote_room_request.v1"
    );
    assert_eq!(
        <RelayActor as kameo::remote::RemoteMessage<RelayDeliverOrdered>>::REMOTE_ID,
        "waddle.clustering.relay.deliver_ordered.v16",
        "a new relay message must not bump the ordered-relay envelope id"
    );
}

#[test]
fn the_message_and_its_replies_round_trip_through_the_relay_codec() {
    let message = request();
    let decoded: RelayRemoteRoomRequest =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&message).expect("encode request"))
            .expect("decode request");
    assert_eq!(decoded.room, message.room);
    assert_eq!(decoded.sender, message.sender);
    assert_eq!(decoded.strength, RelayStreamStrength::Pkix);
    assert_eq!(decoded.trust_profile, Some(RelayTrustProfile::PrivateCa));
    assert_eq!(decoded.snapshot, message.snapshot);
    assert_eq!(decoded.stanza, message.stanza);
    // A stream that proved less carries less.
    let weaker = RelayRemoteRoomRequest {
        strength: RelayStreamStrength::Dialback,
        trust_profile: None,
        ..request()
    };
    let decoded: RelayRemoteRoomRequest =
        rmp_serde::from_slice(&rmp_serde::to_vec_named(&weaker).expect("encode request"))
            .expect("decode request");
    assert_eq!(decoded.strength, RelayStreamStrength::Dialback);
    assert_eq!(decoded.trust_profile, None);
    for reply in [
        RelayRemoteRoomRequestReply::Queued,
        RelayRemoteRoomRequestReply::Busy,
        RelayRemoteRoomRequestReply::NotOwner,
        RelayRemoteRoomRequestReply::Refused,
    ] {
        let decoded: RelayRemoteRoomRequestReply =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&reply).expect("encode reply"))
                .expect("decode reply");
        assert_eq!(decoded, reply);
    }
}

#[tokio::test]
async fn a_replica_that_hosts_no_rooms_for_remote_occupants_queues_nothing() {
    // S2S is off on this replica: its remote route refuses everything, a
    // carried request included.
    let state = websocket_state().await;
    let reply = relayed_remote_room_request(&state, request()).await;
    assert_eq!(reply, RelayRemoteRoomRequestReply::Refused);
}

fn failed(failure: RelaySendFailure) -> RelayAskError {
    RelayAskError::Send {
        failure,
        effect: RelaySendEffect::MaybeCommitted,
        message: String::new(),
    }
}

#[test]
fn an_owner_is_silent_only_where_it_did_not_answer_or_could_not_be_reached() {
    // No answer within the windows, no connection, or no relay in the
    // swarm: nothing says the owner took the request.
    assert!(room_owner_silent(&failed(RelaySendFailure::ReplyTimeout)));
    assert!(room_owner_silent(&failed(RelaySendFailure::Transport)));
    assert!(room_owner_silent(&RelayAskError::NotFound {
        node_id: NodeId::new("owner-node".to_owned()),
    }));
    // The owner answered: its mailbox was full, its handler failed, or it
    // predates the message. A stale cached address is looked up again.
    for answered in [
        RelaySendFailure::MailboxFull,
        RelaySendFailure::Handler,
        RelaySendFailure::Codec,
        RelaySendFailure::StaleRef,
    ] {
        assert!(!room_owner_silent(&failed(answered)), "{answered:?}");
    }
    // This replica is stopping: that says nothing of the owner.
    assert!(!room_owner_silent(&RelayAskError::Cancelled));
}
