//! XEP-0085 across S2S, inbound: the chat states of an occupant at another
//! server in a room of this deployment (ADR-0020 D10).
//!
//! The room relays a remote occupant's chat state to its occupants (§5.5)
//! and archives nothing for it. A chat state feeds this deployment's push
//! filter only for its own accounts: none is recorded for a remote
//! occupant, which has no device here.
//!
//! The deployments are those of `s2s_inbound_muc_support`: `example.com`
//! runs the production room host, S2S router and connection manager over a
//! WebSocket state with an in-memory SQLite database, and `home-b.test` is
//! a second connection manager that keeps what it receives. No PostgreSQL,
//! SpiceDB or second process is involved.
#![cfg(feature = "test-support")]

// The fixture serves several suites; each uses a part of it.
#[allow(dead_code, unused_imports)]
mod s2s_inbound_muc_support;

use minidom::Element;
use s2s_inbound_muc_support::{
    local_receives_payload, remote_payload, room_with_remote_occupant, BOB, BOB_OCCUPANT, ROOM,
};
use waddle_server::s2s::route::RouterLimits;

const NS_CHATSTATES: &str = "http://jabber.org/protocol/chatstates";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupants_chat_state_reaches_the_room_and_records_no_activity() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    for (id, state) in [("state-1", "composing"), ("state-2", "paused")] {
        hosted
            .from_peer(remote_payload(
                BOB,
                ROOM,
                id,
                Element::builder(state, NS_CHATSTATES).build(),
            ))
            .await;
        let relayed = local_receives_payload(&mut alice, BOB_OCCUPANT, state, NS_CHATSTATES).await;
        assert!(
            relayed.is_some(),
            "the {state} state for the local occupant"
        );
        assert!(hosted
            .captured
            .with_id(id)
            .iter()
            .all(|stanza| stanza.attr("type") != Some("error")));
    }
    assert_eq!(
        hosted
            .rows_naming("notification_activity", "owner_bare_jid", "bob@home-b.test")
            .await,
        0
    );
    assert!(hosted.seat(ROOM, BOB).is_some());
}
