//! Spaces flows (W7a, W7b, W8): XEP-0503 0.3.0 Spaces read with plain
//! XEP-0060 across S2S, Spaces on the peer's stock PubSub service,
//! bookmarks, child rooms joined by MUC JID, community administration
//! from a trusted home, and D4's rule that avatar and banner URLs are
//! never fetched.

use anyhow::Result;
use minidom::Element;

use crate::client::session::Session;
use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::scenarios::harness::{Harness, ANSWER, QUIET};
use crate::scenarios::muc;
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::topology::{self, Home, HOME_A};
use crate::xml::{ns, stanza};

/// The XEP-0402 conference element a Space item carries.
fn room_item(name: &str) -> Element {
    stanza::bookmark(name, "harness")
}

/// Create a Space node on home-a with `rooms` as items.
async fn waddle_space(owner: &mut Session, node: &str, rooms: &[String]) -> Result<()> {
    let service = topology::spaces_domain(HOME_A);
    let created = ask(
        owner,
        stanza::pubsub_create(&support::id("create"), &service, node),
    )
    .await?;
    anyhow::ensure!(
        is_result(&created),
        "Space creation refused: {}",
        describe(&created)
    );
    for room in rooms {
        let published = ask(
            owner,
            stanza::pubsub_publish(
                &support::id("publish"),
                Some(&service),
                node,
                room,
                room_item(room),
            ),
        )
        .await?;
        anyhow::ensure!(
            is_result(&published),
            "room item refused: {}",
            describe(&published)
        );
    }
    Ok(())
}

fn item_ids(answer: &Element) -> Vec<String> {
    answer
        .get_child("pubsub", ns::PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", ns::PUBSUB))
        .map(|items| {
            items
                .children()
                .filter_map(|item| item.attr("id").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

pub async fn peer_reads_waddle_space(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            peer.label(),
            " client reading a home-a Space with plain XEP-0060",
        ]
        .concat(),
    );
    let node = support::id("space");
    let room = [
        support::id("child").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    muc::join(&mut owner, &room, "owner").await?;
    waddle_space(&mut owner, &node, std::slice::from_ref(&room)).await?;
    let mut reader = h.login_peer(peer, "alice", &support::id("reader")).await?;
    let items = ask(
        &mut reader,
        stanza::pubsub_items(
            &support::id("items"),
            Some(&topology::spaces_domain(HOME_A)),
            &node,
        ),
    )
    .await?;
    outcome.check(
        "the Space's room items are readable",
        is_result(&items) && item_ids(&items).contains(&room),
        describe(&items),
    );
    reader.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn child_rooms(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            peer.label(),
            " client joining every child room of a home-a Space",
        ]
        .concat(),
    );
    let node = support::id("space");
    let rooms: Vec<String> = (0..3)
        .map(|_| {
            [
                support::id("child").as_str(),
                "@",
                &topology::muc_domain(HOME_A),
            ]
            .concat()
        })
        .collect();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    for room in &rooms {
        muc::join(&mut owner, room, "owner").await?;
    }
    waddle_space(&mut owner, &node, &rooms).await?;
    let mut member = h.login_peer(peer, "alice", &support::id("member")).await?;
    let items = ask(
        &mut member,
        stanza::pubsub_items(
            &support::id("items"),
            Some(&topology::spaces_domain(HOME_A)),
            &node,
        ),
    )
    .await?;
    for room in item_ids(&items) {
        let joined = muc::join(&mut member, &room, "member").await;
        outcome.check(
            ["joins ", &room].concat(),
            joined.is_ok(),
            joined.err().map(|e| e.to_string()).unwrap_or_default(),
        );
    }
    member.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn space_on_peer_pubsub(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "a home-a client using a Space on ",
            peer.label(),
            "'s PubSub service",
        ]
        .concat(),
    );
    let service = Harness::peer_pubsub(peer);
    let node = support::id("peer-space");
    let mut owner = h.login_peer(peer, "admin", &support::id("owner")).await?;
    let created = ask(
        &mut owner,
        stanza::pubsub_create(&support::id("create"), &service, &node),
    )
    .await?;
    anyhow::ensure!(
        is_result(&created),
        "node creation refused: {}",
        describe(&created)
    );
    let mut local = h
        .login_waddle(Home::A, "alice", &support::id("local"))
        .await?;
    let bare = local.bare();
    let subscribed = ask(
        &mut local,
        stanza::pubsub_subscribe(&support::id("sub"), &service, &node, &bare),
    )
    .await?;
    outcome.check(
        "home-a client subscribes",
        is_result(&subscribed),
        describe(&subscribed),
    );
    let room = [support::id("room").as_str(), "@", &Harness::peer_muc(peer)].concat();
    let published = ask(
        &mut owner,
        stanza::pubsub_publish(
            &support::id("pub"),
            Some(&service),
            &node,
            &room,
            room_item(&room),
        ),
    )
    .await?;
    outcome.check(
        "the peer publishes a room item",
        is_result(&published),
        describe(&published),
    );
    let event = local
        .wait_for(ANSWER, |element| {
            element
                .children()
                .any(|child| child.is("event", ns::PUBSUB_EVENT))
        })
        .await;
    outcome.check(
        "the notification reaches home-a's client",
        event.is_ok(),
        "",
    );
    let items = ask(
        &mut local,
        stanza::pubsub_items(&support::id("items"), Some(&service), &node),
    )
    .await?;
    outcome.check(
        "home-a's client reads the items",
        is_result(&items) && item_ids(&items).contains(&room),
        describe(&items),
    );
    local.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn bookmarks_restore(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "a fresh home-a device restoring rooms on ",
            peer.label(),
            " from XEP-0402 bookmarks",
        ]
        .concat(),
    );
    let room = [
        support::id("bookmarked").as_str(),
        "@",
        &Harness::peer_muc(peer),
    ]
    .concat();
    let mut owner = h.login_peer(peer, "admin", &support::id("owner")).await?;
    muc::join(&mut owner, &room, "owner").await?;
    let mut first = h.login_waddle(Home::A, "alice", "device-1").await?;
    let published = ask(
        &mut first,
        stanza::pubsub_publish(
            &support::id("bm"),
            None,
            ns::BOOKMARKS,
            &room,
            stanza::bookmark("remote", "alice"),
        ),
    )
    .await?;
    outcome.check(
        "bookmark stored",
        is_result(&published),
        describe(&published),
    );
    first.close().await;
    let mut fresh = h.login_waddle(Home::A, "alice", "device-2").await?;
    let items = ask(
        &mut fresh,
        stanza::pubsub_items(&support::id("bm"), None, ns::BOOKMARKS),
    )
    .await?;
    let restored = item_ids(&items).contains(&room);
    outcome.check(
        "the fresh device restores the remote room",
        restored,
        describe(&items),
    );
    if restored {
        let joined = muc::join(&mut fresh, &room, "alice").await;
        outcome.check(
            "and joins it",
            joined.is_ok(),
            joined.err().map(|e| e.to_string()).unwrap_or_default(),
        );
    }
    outcome.unverified(
        "joined-space preferences",
        "the web pilot (W8) defines the preference store; driven through the pilot, not this harness",
    );
    fresh.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn community_admin(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("home-b's admin administering a home-a Space as owner");
    let node = support::id("community");
    let service = topology::spaces_domain(HOME_A);
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    waddle_space(&mut owner, &node, &[]).await?;
    let affiliation = Element::builder("affiliation", "http://jabber.org/protocol/pubsub#owner")
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            ["admin@", topology::HOME_B].concat(),
        )
        .attr(
            minidom::rxml::xml_ncname!("affiliation").to_owned(),
            "owner",
        )
        .build();
    let affiliations = Element::builder("affiliations", "http://jabber.org/protocol/pubsub#owner")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .append(affiliation)
        .build();
    let granted = ask(
        &mut owner,
        stanza::set(
            &support::id("affiliate"),
            Some(&service),
            Element::builder("pubsub", "http://jabber.org/protocol/pubsub#owner")
                .append(affiliations)
                .build(),
        ),
    )
    .await?;
    outcome.check(
        "owner affiliation granted to the trusted home's admin",
        is_result(&granted),
        describe(&granted),
    );
    let mut remote = h
        .login_waddle(Home::B, "admin", &support::id("remote"))
        .await?;
    let room = [
        support::id("added").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let published = ask(
        &mut remote,
        stanza::pubsub_publish(
            &support::id("add"),
            Some(&service),
            &node,
            &room,
            room_item(&room),
        ),
    )
    .await?;
    outcome.check(
        "the remote owner adds a room to the community",
        is_result(&published),
        describe(&published),
    );
    remote.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn no_avatar_fetch(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome =
        Outcome::new("a home-a Space whose avatar and banner point at the harness fetch recorder");
    let node = support::id("images");
    let service = topology::spaces_domain(HOME_A);
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    waddle_space(&mut owner, &node, &[]).await?;
    let base = ["http://127.0.0.1:", &h.ports.fetch_recorder().to_string()].concat();
    for (item, path) in [
        ("urn:xmpp:spaces:avatar:metadata:0", "/avatar.png"),
        ("urn:xmpp:spaces:banner:metadata:0", "/banner.png"),
    ] {
        let url = [base.as_str(), path].concat();
        let published = ask(
            &mut owner,
            stanza::pubsub_publish(
                &support::id("image"),
                Some(&service),
                &node,
                item,
                stanza::image_metadata(&url),
            ),
        )
        .await?;
        outcome.check(
            ["publish ", item].concat(),
            is_result(&published),
            describe(&published),
        );
    }
    let mut reader = h.login_peer(peer, "alice", &support::id("reader")).await?;
    let _ = ask(
        &mut reader,
        stanza::pubsub_items(&support::id("items"), Some(&service), &node),
    )
    .await?;
    tokio::time::sleep(QUIET).await;
    let fetches = h.fetches.requests();
    outcome.check(
        "no fetch from either deployment",
        fetches.is_empty(),
        format!("{fetches:?}"),
    );
    outcome.unverified(
        "no fetch from the web pilot",
        "the web pilot (W8) is driven separately",
    );
    reader.close().await;
    owner.close().await;
    Ok(outcome)
}
