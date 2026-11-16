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
//! The cases need no cluster: they encode the message with the codec the
//! relay uses and run the owner side over a WebSocket state in this
//! process.
#![cfg(all(feature = "test-support", feature = "clustering"))]

use waddle_server::clustering::codec::RemoteStanza;
use waddle_server::clustering::relay::{
    RelayActor, RelayDeliverOrdered, RelayRemoteRoomRequest, RelayRemoteRoomRequestReply,
    RelayStreamStrength, RelayTrustProfile,
};
use waddle_server::test_support::{relayed_remote_room_request, websocket_state};
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
