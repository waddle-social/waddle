//! XEP-0115 across S2S, inbound: the entity capabilities of an occupant at
//! another server in a room of this deployment (ADR-0020 D10).
//!
//! A remote occupant's presence reaches the room with its availability and
//! its `<c/>` element. The presence extensions of this deployment are not
//! carried, and neither is a `muc#user` element, which XEP-0045 has the
//! room author: the room's own presence for the occupant carries the
//! room's.
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
    item, jid, local_receives, room_with_remote_occupant, statuses, ARRIVES, BOB, BOB_OCCUPANT,
    NS_MUC_USER,
};
use waddle_server::s2s::route::RouterLimits;
use xmpp_parsers::presence::{Presence, Show, Type as PresenceType};

const NS_CAPS: &str = "http://jabber.org/protocol/caps";

fn no_change(_: &mut s2s_inbound_muc_support::Spec) {}

#[tokio::test]
async fn a_remote_occupants_presence_carries_its_caps_and_none_of_this_deployments_extensions() {
    let (hosted, mut alice, _own) =
        room_with_remote_occupant(RouterLimits::default(), no_change).await;
    // A change of availability by the seated occupant (XEP-0045 §7.7).
    let mut update = Presence::new(PresenceType::None);
    update.from = Some(jid(BOB));
    update.to = Some(jid(BOB_OCCUPANT));
    update.id = Some("caps-1".to_owned());
    update.show = Some(Show::Away);
    update.payloads.push(
        Element::builder("c", NS_CAPS)
            .attr(minidom::rxml::xml_ncname!("hash").to_owned(), "sha-1")
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "https://client.example",
            )
            .attr(
                minidom::rxml::xml_ncname!("ver").to_owned(),
                "QgayPKawpkPSDYmwT/WM94uAlu0=",
            )
            .build(),
    );
    // What is not carried: an extension of this deployment, and a
    // `muc#user` element that claims a standing and the self status.
    update
        .payloads
        .push(Element::builder("hats", "urn:waddle:hats:0").build());
    update.payloads.push(
        Element::builder("x", NS_MUC_USER)
            .append(
                Element::builder("item", NS_MUC_USER)
                    .attr(
                        minidom::rxml::xml_ncname!("affiliation").to_owned(),
                        "owner",
                    )
                    .attr(minidom::rxml::xml_ncname!("role").to_owned(), "moderator")
                    .build(),
            )
            .append(
                Element::builder("status", NS_MUC_USER)
                    .attr(minidom::rxml::xml_ncname!("code").to_owned(), "307")
                    .build(),
            )
            .build(),
    );
    hosted.from_peer(update).await;
    let seen = local_receives(&mut alice, ARRIVES, |stanza| {
        stanza.name() == "presence"
            && stanza.attr("from") == Some(BOB_OCCUPANT)
            && stanza.children().any(|child| child.name() == "show")
    })
    .await
    .expect("the remote occupant's presence for the local occupant");
    let caps = seen
        .children()
        .find(|child| child.is("c", NS_CAPS))
        .expect("the occupant's entity capabilities");
    assert_eq!(caps.attr("hash"), Some("sha-1"));
    assert_eq!(caps.attr("ver"), Some("QgayPKawpkPSDYmwT/WM94uAlu0="));
    assert!(
        seen.children()
            .all(|child| !child.ns().starts_with("urn:waddle:hats")),
        "{seen:?}"
    );
    // The `muc#user` element is the room's: the occupant's standing as
    // the room holds it, and no status the occupant supplied.
    assert_eq!(
        item(&seen),
        Some(("participant".to_owned(), "none".to_owned()))
    );
    assert!(!statuses(&seen).contains(&"307".to_owned()), "{seen:?}");
    assert!(hosted.seat("lounge@muc.example.com", BOB).is_some());
}
