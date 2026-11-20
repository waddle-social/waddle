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

/// Open the Space `node` on a Waddle Spaces `service` to accounts at
/// other servers, as its owner does with the Space configuration field
/// `urn:waddle:spaces:federated`: a Waddle Space is closed to them until
/// then.
async fn federate_space(owner: &mut Session, service: &str, node: &str) -> Result<()> {
    let configured = ask(
        owner,
        stanza::pubsub_configure(
            &support::id("configure"),
            service,
            node,
            &[(ns::WADDLE_SPACE_FEDERATED, "1")],
        ),
    )
    .await?;
    anyhow::ensure!(
        is_result(&configured),
        "opening the Space to other servers refused: {}",
        describe(&configured)
    );
    Ok(())
}

/// Create a Space node on home-a, open it to accounts at other servers and
/// publish `rooms`, which exist and are open to them too, as its items.
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
    federate_space(owner, &service, node).await?;
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
    let mut rooms: Vec<String> = (0..3)
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
    let mut listed_rooms = item_ids(&items);
    listed_rooms.sort_unstable();
    rooms.sort_unstable();
    outcome.check(
        "lists every published child room exactly once",
        is_result(&items) && listed_rooms == rooms,
        format!("expected {rooms:?}, returned {listed_rooms:?}"),
    );
    for room in rooms {
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
    let room = [support::id("room").as_str(), "@", &Harness::peer_muc(peer)].concat();
    if peer == Peer::Waddle {
        // A Waddle Space lists rooms of its own deployment that exist, and
        // other servers reach both only once their owner opens them.
        federate_space(&mut owner, &service, &node).await?;
        muc::join(&mut owner, &room, "owner").await?;
    }
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
    let remote_jid = ["admin@", topology::HOME_B].concat();
    let granted = ask(
        &mut owner,
        stanza::pubsub_affiliations_set(
            &support::id("affiliate"),
            &service,
            &node,
            &[(remote_jid.as_str(), "owner")],
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
    // A trusted home's owner administers the Space (XEP-0060 section 8):
    // it reads the configuration, without whether the Space is open to
    // other servers, which only home-a's own sessions read or change.
    let form = ask(
        &mut remote,
        stanza::pubsub_owner_get(&support::id("form"), &service, "configure", &node),
    )
    .await?;
    let field = |var: &str| {
        stanza::find(&form, "field", ns::DATA_FORMS, &|field| {
            field.attr("var") == Some(var)
        })
        .is_some()
    };
    outcome.check(
        "the remote owner reads the configuration (section 8.2)",
        is_result(&form) && field("pubsub#access_model") && !field(ns::WADDLE_SPACE_FEDERATED),
        describe(&form),
    );
    let listed = ask(
        &mut remote,
        stanza::pubsub_owner_get(
            &support::id("affiliations"),
            &service,
            "affiliations",
            &node,
        ),
    )
    .await?;
    let lists_itself = stanza::find(&listed, "affiliation", ns::PUBSUB_OWNER, &|entry| {
        entry.attr("jid") == Some(remote_jid.as_str()) && entry.attr("affiliation") == Some("owner")
    })
    .is_some();
    outcome.check(
        "the remote owner reads the affiliations (section 8.9.1)",
        is_result(&listed) && lists_itself,
        describe(&listed),
    );
    let member = ["alice@", HOME_A].concat();
    let membership = ask(
        &mut remote,
        stanza::pubsub_affiliations_set(
            &support::id("member"),
            &service,
            &node,
            &[(member.as_str(), "member")],
        ),
    )
    .await?;
    outcome.check(
        "the remote owner grants a membership (section 8.9.2)",
        is_result(&membership),
        describe(&membership),
    );
    let subscriptions = ask(
        &mut remote,
        stanza::pubsub_owner_get(
            &support::id("subscriptions"),
            &service,
            "subscriptions",
            &node,
        ),
    )
    .await?;
    outcome.check(
        "the remote owner reads the subscriptions (section 8.8.1)",
        is_result(&subscriptions),
        describe(&subscriptions),
    );
    // Placing a room in a Space is decided by the Space's `owner`
    // relationship and by authority over the room, which no writer issues
    // to an account at another server: the remote owner's affiliation
    // places nothing, and the publish is refused (XEP-0060 section
    // 7.1.3.1; the routing design, "What a remote owner of a Space can
    // do"). The room exists on home-a and is open to other servers, so
    // nothing else refuses it.
    let room = [
        support::id("added").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    muc::join(&mut owner, &room, "owner").await?;
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
        "the remote owner places no room in the Space",
        stanza::error_condition(&published).as_deref() == Some("forbidden"),
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
