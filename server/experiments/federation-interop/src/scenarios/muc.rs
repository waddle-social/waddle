//! MUC flows (W6a, W6b) and the authority and identity rows that need
//! rooms: joins in both directions with host-authorized MAM and paging,
//! two resources in one remote room, moderation by a remote moderator of
//! a trusted home, and the refusals for homes that are not trusted.

use anyhow::{Context, Result};
use minidom::Element;

use crate::client::session::Session;
use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::topology::{self, Home, HOME_A, HOME_B};
use crate::xml::{ns, stanza};

/// Whether `room` (a bare room JID) is on one of the Waddle deployments'
/// MUC services.
pub fn on_waddle(room: &str) -> bool {
    let domain = room.rsplit('@').next().unwrap_or_default();
    [HOME_A, HOME_B]
        .iter()
        .any(|home| domain == topology::muc_domain(home))
}

/// Join `room/nick` and wait for the self-presence; a room created by the
/// join is unlocked as its owner would: a stock server's with the default
/// configuration, a Waddle room by submitting its owner form with the
/// room opened to accounts at other servers
/// (`urn:waddle:roomconfig:federated`), since a Waddle room is closed to
/// them until an owner opens it.
pub async fn join(session: &mut Session, room: &str, nick: &str) -> Result<Element> {
    join_with(session, room, nick, &[]).await
}

/// [`join`], with `fields` added to the owner form when the join creates a
/// Waddle room.
pub async fn join_with(
    session: &mut Session,
    room: &str,
    nick: &str,
    fields: &[(&str, &str)],
) -> Result<Element> {
    let occupant = [room, "/", nick].concat();
    session.send(&stanza::muc_join(&occupant)).await?;
    let presence = session
        .wait_for(ANSWER, |element| {
            let Some(from) = element.attr("from") else {
                return false;
            };
            if from.split_once('/').map_or(from, |(bare, _)| bare) != room {
                return false;
            }
            if element.name() == "presence" && element.attr("type") == Some("error") {
                return true;
            }
            element.attr("type").is_none()
                && stanza::is_self_presence(element)
                && (from == occupant
                    || (from.contains('/')
                        && stanza::muc_status_codes(element)
                            .iter()
                            .any(|code| code == "210")))
        })
        .await?;
    anyhow::ensure!(
        presence.attr("type") != Some("error"),
        "join refused: {}",
        describe(&presence)
    );
    let created = presence
        .children()
        .filter(|child| child.is("x", ns::MUC_USER))
        .flat_map(|x| x.children())
        .any(|child| child.name() == "status" && child.attr("code") == Some("201"));
    if created {
        let request = if on_waddle(room) {
            let mut form = vec![(ns::WADDLE_ROOM_FEDERATED, "1")];
            form.extend_from_slice(fields);
            stanza::muc_configure(&support::id("configure"), room, &form)
        } else {
            stanza::muc_instant(&support::id("instant"), room)
        };
        let unlocked = ask(session, request).await?;
        anyhow::ensure!(
            is_result(&unlocked),
            "room configuration refused: {}",
            describe(&unlocked)
        );
    }
    Ok(presence)
}

/// Send `count` messages and return their ids.
async fn send_messages(session: &mut Session, room: &str, count: usize) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for index in 0..count {
        let message_id = support::id("room");
        session
            .send(&stanza::groupchat(
                &message_id,
                room,
                &["message ", &index.to_string()].concat(),
            ))
            .await?;
        session
            .wait_for(ANSWER, |element| {
                element.attr("id") == Some(message_id.as_str())
            })
            .await?;
        ids.push(message_id);
    }
    Ok(ids)
}

/// Read the room's archive two messages per page and return how many
/// messages and pages came back.
async fn page_archive(session: &mut Session, room: &str) -> Result<(usize, usize)> {
    let mut after: Option<String> = None;
    let mut messages = 0usize;
    let mut pages = 0usize;
    loop {
        let query_id = support::id("mam");
        session
            .send(&stanza::mam_query(
                &query_id,
                Some(room),
                2,
                after.as_deref(),
            ))
            .await?;
        let fin = session
            .wait_for(ANSWER, |element| {
                element.name() == "iq" && element.attr("id") == Some(query_id.as_str())
            })
            .await?;
        anyhow::ensure!(is_result(&fin), "MAM query refused: {}", describe(&fin));
        let forwarded = session
            .drain_backlog()
            .into_iter()
            .filter(|element| {
                element.children().any(|child| {
                    child.is("result", ns::MAM) && child.attr("queryid") == Some(query_id.as_str())
                })
            })
            .count();
        messages += forwarded;
        pages += 1;
        let set = fin
            .get_child("fin", ns::MAM)
            .and_then(|fin| fin.get_child("set", ns::RSM));
        let complete = fin
            .get_child("fin", ns::MAM)
            .and_then(|fin| fin.attr("complete"))
            == Some("true");
        after = set
            .and_then(|set| set.get_child("last", ns::RSM))
            .map(Element::text);
        if complete || after.is_none() || forwarded == 0 || pages > 20 {
            return Ok((messages, pages));
        }
    }
}

fn room_name(prefix: &str) -> String {
    support::id(prefix)
}

pub async fn local_joins_remote(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(["a home-a client in a room on ", peer.label()].concat());
    let room = [
        room_name("remote-room").as_str(),
        "@",
        &Harness::peer_muc(peer),
    ]
    .concat();
    let mut owner = h.login_peer(peer, "admin", &support::id("owner")).await?;
    join(&mut owner, &room, "owner").await?;
    let mut local = h
        .login_waddle(Home::A, "alice", &support::id("join"))
        .await?;
    let joined = join(&mut local, &room, "alice").await;
    outcome.check(
        "home-a client joins the remote room",
        joined.is_ok(),
        joined.err().map(|e| e.to_string()).unwrap_or_default(),
    );
    let sent = send_messages(&mut local, &room, 3).await?;
    let seen = owner
        .wait_for(ANSWER, |element| {
            element.attr("id") == sent.last().map(String::as_str)
        })
        .await;
    outcome.check(
        "the room's occupants receive home-a's messages",
        seen.is_ok(),
        "",
    );
    let (messages, pages) = page_archive(&mut local, &room).await?;
    outcome.check(
        "host-authorized history with paging",
        messages >= 3 && pages >= 2,
        format!("{messages} messages in {pages} pages"),
    );
    local.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn remote_joins_waddle(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new([peer.label(), " client in a room on home-a"].concat());
    let room = [
        room_name("waddle-room").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join(&mut owner, &room, "owner").await?;
    let mut remote = h.login_peer(peer, "alice", &support::id("join")).await?;
    let joined = join(&mut remote, &room, "remote").await;
    outcome.check(
        "the remote client joins",
        joined.is_ok(),
        joined.err().map(|e| e.to_string()).unwrap_or_default(),
    );
    let sent = send_messages(&mut remote, &room, 3).await?;
    let seen = owner
        .wait_for(ANSWER, |element| {
            element.attr("id") == sent.last().map(String::as_str)
        })
        .await;
    outcome.check(
        "local occupants receive the remote messages",
        seen.is_ok(),
        "",
    );
    let (messages, pages) = page_archive(&mut remote, &room).await?;
    outcome.check(
        "host-authorized history with paging",
        messages >= 3 && pages >= 2,
        format!("{messages} messages in {pages} pages"),
    );
    remote.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn two_resources(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "one home-a account with two resources in a room on ",
            peer.label(),
        ]
        .concat(),
    );
    let room = [room_name("two-res").as_str(), "@", &Harness::peer_muc(peer)].concat();
    let mut owner = h.login_peer(peer, "admin", &support::id("owner")).await?;
    join(&mut owner, &room, "owner").await?;
    let mut first = h.login_waddle(Home::A, "alice", "room-1").await?;
    let mut second = h.login_waddle(Home::A, "alice", "room-2").await?;
    let joined_first = join(&mut first, &room, "alice").await;
    let joined_second = join(&mut second, &room, "alice").await;
    outcome.check(
        "self-presence on both resources",
        joined_first.is_ok() && joined_second.is_ok(),
        "",
    );
    let owner_id = support::id("owner-msg");
    owner
        .send(&stanza::groupchat(&owner_id, &room, "to everyone"))
        .await?;
    let on_first = first
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(owner_id.as_str())
        })
        .await;
    let on_second = second
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(owner_id.as_str())
        })
        .await;
    outcome.check(
        "room traffic on both resources",
        on_first.is_ok() && on_second.is_ok(),
        "",
    );
    let own_id = support::id("own-msg");
    first
        .send(&stanza::groupchat(&own_id, &room, "from one resource"))
        .await?;
    let echo_first = first
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(own_id.as_str())
        })
        .await;
    let echo_second = second
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(own_id.as_str())
        })
        .await;
    outcome.check(
        "the account's own message on both resources",
        echo_first.is_ok() && echo_second.is_ok(),
        "",
    );
    first.close().await;
    second.close().await;
    owner.close().await;
    Ok(outcome)
}

/// Grant an occupant the moderator role and verify it can kick a local
/// occupant with the same affiliation, as allowed by XEP-0045 section 8.2.
async fn remote_moderation(
    h: &Harness,
    peer: Peer,
    outcome: &mut Outcome,
    label: &str,
) -> Result<bool> {
    let room = [
        room_name("moderated").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join(&mut owner, &room, "owner").await?;
    let mut moderator = h
        .login_peer(peer, "admin", &support::id("moderator"))
        .await?;
    join(&mut moderator, &room, "remote-mod").await?;
    let mut target = h
        .login_waddle(Home::A, "bob", &support::id("target"))
        .await?;
    join(&mut target, &room, "bob").await?;
    let granted = ask(
        &mut owner,
        stanza::muc_role(&support::id("grant"), &room, "remote-mod", "moderator"),
    )
    .await?;
    outcome.note(format!(
        "{label}: moderator grant answered {}",
        describe(&granted)
    ));
    let devoice = ask(
        &mut moderator,
        stanza::muc_role(&support::id("devoice"), &room, "bob", "visitor"),
    )
    .await?;
    outcome.note([label, ": devoice answered ", &describe(&devoice)].concat());
    let kick = ask(
        &mut moderator,
        stanza::muc_role(&support::id("kick"), &room, "bob", "none"),
    )
    .await?;
    outcome.note(format!("{label}: kick answered {}", describe(&kick)));
    let kicked = target
        .wait_for(QUIET, |element| {
            element.name() == "presence"
                && element.attr("type") == Some("unavailable")
                && element
                    .attr("from")
                    .is_some_and(|from| from.ends_with("/bob"))
                && stanza::muc_status_codes(element)
                    .iter()
                    .any(|code| code == "307")
        })
        .await
        .is_ok();
    owner.close().await;
    moderator.close().await;
    target.close().await;
    Ok(kicked && is_result(&kick))
}

pub async fn remote_moderator(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            peer.label(),
            "'s admin as moderator of a home-a room over pkix",
        ]
        .concat(),
    );
    let honored = remote_moderation(h, peer, &mut outcome, "trusted home").await?;
    outcome.check("a trusted home's moderator is honored", honored, "");
    Ok(outcome)
}

pub async fn untrusted_home(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            "home-a restarted with ",
            peer.label(),
            " off the trusted-home list",
        ]
        .concat(),
    );
    let mut profile = h.profile(Home::A, false);
    if peer == Peer::Waddle {
        profile.trusted_homes.retain(|home| home != HOME_B);
    }
    h.dirty.insert(Component::Waddle(Home::A));
    h.start_waddle(&profile).await?;
    let honored = remote_moderation(h, peer, &mut outcome, "untrusted home").await?;
    outcome.check(
        "moderation by a home not on the list is refused",
        !honored,
        "",
    );
    Ok(outcome)
}

pub async fn home_removed(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        "one persisted home-b admin affiliation before, during and after removal from home-a's trusted-home list",
    );
    let room = [
        room_name("stored-admin").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join_with(
        &mut owner,
        &room,
        "owner",
        &[("muc#roomconfig_membersonly", "1")],
    )
    .await?;
    for (account, affiliation) in [
        ("admin@home-b.test", "admin"),
        ("bob@home-a.test", "member"),
    ] {
        let grant = ask(
            &mut owner,
            stanza::muc_affiliation(&support::id("grant"), &room, account, affiliation),
        )
        .await?;
        anyhow::ensure!(
            is_result(&grant),
            "fixture grant refused: {}",
            describe(&grant)
        );
    }
    send_messages(&mut owner, &room, 1).await?;
    owner.close().await;

    for (label, trusted) in [("trusted", true), ("removed", false), ("restored", true)] {
        if label != "trusted" {
            let mut profile = h.profile(Home::A, true);
            if !trusted {
                profile.trusted_homes.retain(|home| home != HOME_B);
            }
            h.dirty.insert(Component::Waddle(Home::A));
            h.start_waddle(&profile).await?;
        }
        let mut owner = h
            .login_waddle(Home::A, "admin", &support::id("owner"))
            .await?;
        join(&mut owner, &room, "owner").await?;
        let mut target = h
            .login_waddle(Home::A, "bob", &support::id("target"))
            .await?;
        join(&mut target, &room, "bob").await?;
        let mut moderator = h
            .login_peer(peer, "admin", &support::id("moderator"))
            .await?;
        if trusted {
            let seated = join(&mut moderator, &room, "remote-mod").await?;
            let (messages, _) = page_archive(&mut moderator, &room).await?;
            outcome.check(
                format!("{label}: remote admin reads the stored archive"),
                messages == 1,
                format!("{messages} messages"),
            );
            // The receiving home preserves remote archive results only for a
            // room the client occupies. Leave before probing moderation so
            // every phase tests the affiliation without an occupant role.
            let occupant = seated.attr("from").context("self-presence address")?;
            moderator
                .send(&stanza::presence(
                    ns::CLIENT,
                    Some(occupant),
                    Some("unavailable"),
                ))
                .await?;
            moderator
                .wait_for(ANSWER, |element| {
                    element.attr("from") == Some(occupant)
                        && element.attr("type") == Some("unavailable")
                        && stanza::is_self_presence(element)
                })
                .await?;
            let query = support::id("mam");
            let mam = ask(
                &mut moderator,
                stanza::mam_query(&query, Some(&room), 10, None),
            )
            .await?;
            let count = mam
                .get_child("fin", ns::MAM)
                .and_then(|fin| fin.get_child("set", ns::RSM))
                .and_then(|set| set.get_child("count", ns::RSM))
                .map(Element::text);
            outcome.check(
                format!("{label}: the room authorizes MAM without an occupant role"),
                is_result(&mam)
                    && mam.attr("from") == Some(room.as_str())
                    && count.as_deref() == Some("1"),
                format!("{}; archive count: {count:?}", describe(&mam)),
            );
        } else {
            let (messages, _) = page_archive(&mut owner, &room).await?;
            outcome.check(
                "removed: local owner still reads the stored archive",
                messages == 1,
                format!("{messages} messages"),
            );
            let query = support::id("mam");
            let mam = ask(
                &mut moderator,
                stanza::mam_query(&query, Some(&room), 10, None),
            )
            .await?;
            let no_results = moderator
                .absent(QUIET, |element| {
                    element
                        .get_child("result", ns::MAM)
                        .is_some_and(|result| result.attr("queryid") == Some(query.as_str()))
                })
                .await?;
            outcome.check(
                "removed: the original room denies MAM and the client receives no results",
                denied_by_room(&mam, &room) && no_results,
                describe(&mam),
            );
        }
        let action = ask(
            &mut moderator,
            stanza::muc_role(&support::id("devoice"), &room, "bob", "visitor"),
        )
        .await?;
        let occupant = [room.as_str(), "/bob"].concat();
        let devoiced = !target
            .absent(QUIET, |element| {
                element.name() == "presence"
                    && element.attr("from") == Some(occupant.as_str())
                    && stanza::muc_role_of(element).as_deref() == Some("visitor")
            })
            .await?;
        let expected = if trusted {
            is_result(&action) && devoiced
        } else {
            denied_by_room(&action, &room) && !devoiced
        };
        outcome.check(
            format!("{label}: moderation follows current trust"),
            expected,
            format!("{}; visitor presence: {devoiced}", describe(&action)),
        );
        moderator.close().await;
        target.close().await;
        owner.close().await;
    }
    Ok(outcome)
}

fn denied_by_room(answer: &Element, room: &str) -> bool {
    answer.attr("from") == Some(room)
        && answer.attr("type") == Some("error")
        && stanza::error_condition(answer).as_deref() == Some("forbidden")
}

pub async fn privileged_pkix_vs_dialback(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    use crate::topology::PROSODY;

    let mut outcome =
        Outcome::new("one stored Prosody admin affiliation on PKIX, dialback, and restored PKIX");
    let muc_domain = topology::muc_domain(HOME_A);
    let room = format!("{}@{muc_domain}", room_name("stored-strength"));
    let open = format!("{}@{muc_domain}", room_name("strength-control"));
    let admin = format!("admin@{PROSODY}");
    let profile = prosody_authority_profile(h);

    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join(&mut owner, &open, "owner").await?;
    join_with(
        &mut owner,
        &room,
        "owner",
        &[("muc#roomconfig_membersonly", "1")],
    )
    .await?;
    for (account, affiliation) in [(admin.as_str(), "admin"), ("bob@home-a.test", "member")] {
        let answer = ask(
            &mut owner,
            stanza::muc_affiliation(&support::id("grant"), &room, account, affiliation),
        )
        .await?;
        anyhow::ensure!(
            is_result(&answer),
            "fixture affiliation refused: {}",
            describe(&answer)
        );
    }
    send_messages(&mut owner, &room, 1).await?;
    owner.close().await;

    for (label, pkix) in [("PKIX", true), ("dialback", false), ("restored PKIX", true)] {
        let phase = start_prosody_authority_phase(h, &profile, &muc_domain, pkix).await?;
        let mut local = h
            .login_waddle(Home::A, "bob", &support::id("target"))
            .await?;
        let seated = join(&mut local, &room, "bob").await?;
        outcome.check(
            format!("{label}: target starts as participant"),
            stanza::muc_role_of(&seated).as_deref() == Some("participant"),
            "",
        );
        let mut moderator = h
            .login_peer(Peer::Prosody, "admin", &support::id("moderator"))
            .await?;
        let discovery = ask(
            &mut moderator,
            stanza::disco_info(&support::id("control"), &open),
        )
        .await?;
        outcome.check(
            format!("{label}: same service answers open-room discovery"),
            is_result(&discovery) && discovery.attr("from") == Some(open.as_str()),
            describe(&discovery),
        );
        if pkix {
            let seated = join(&mut moderator, &room, "remote-mod").await?;
            let (messages, _) = page_archive(&mut moderator, &room).await?;
            outcome.check(
                format!("{label}: the stored affiliation grants archive access"),
                messages == 1,
                format!("{messages} messages"),
            );
            // The receiving home preserves remote archive results only for a
            // room the client occupies. Leave before probing moderation so
            // every phase tests the affiliation without an occupant role.
            let occupant = seated.attr("from").context("self-presence address")?;
            moderator
                .send(&stanza::presence(
                    ns::CLIENT,
                    Some(occupant),
                    Some("unavailable"),
                ))
                .await?;
            moderator
                .wait_for(ANSWER, |element| {
                    element.attr("from") == Some(occupant)
                        && element.attr("type") == Some("unavailable")
                        && stanza::is_self_presence(element)
                })
                .await?;
            let query = support::id("mam");
            let mam = ask(
                &mut moderator,
                stanza::mam_query(&query, Some(&room), 10, None),
            )
            .await?;
            let count = mam
                .get_child("fin", ns::MAM)
                .and_then(|fin| fin.get_child("set", ns::RSM))
                .and_then(|set| set.get_child("count", ns::RSM))
                .map(Element::text);
            outcome.check(
                format!("{label}: the room authorizes MAM without an occupant role"),
                is_result(&mam)
                    && mam.attr("from") == Some(room.as_str())
                    && count.as_deref() == Some("1"),
                format!("{}; archive count: {count:?}", describe(&mam)),
            );
        } else {
            let query = support::id("strength-mam");
            let reply = ask(
                &mut moderator,
                stanza::mam_query(&query, Some(&room), 10, None),
            )
            .await?;
            let no_results = moderator
                .absent(QUIET, |element| {
                    element
                        .get_child("result", ns::MAM)
                        .is_some_and(|result| result.attr("queryid") == Some(query.as_str()))
                })
                .await?;
            outcome.check(
                "dialback: the same grant cannot read the archive",
                denied_by_room(&reply, &room) && no_results,
                describe(&reply),
            );
        }
        let action = ask(
            &mut moderator,
            stanza::muc_role(&support::id("devoice"), &room, "bob", "visitor"),
        )
        .await?;
        let observer = format!("{room}/bob");
        let changed = !local
            .absent(QUIET, |element| {
                element.name() == "presence"
                    && element.attr("from") == Some(observer.as_str())
                    && stanza::muc_role_of(element).as_deref() == Some("visitor")
            })
            .await?;
        let authorized = if pkix {
            is_result(&action) && action.attr("from") == Some(room.as_str()) && changed
        } else {
            denied_by_room(&action, &room) && !changed
        };
        outcome.check(
            format!("{label}: moderation follows the authenticated stream strength"),
            authorized,
            format!("{}; target became visitor: {changed}", describe(&action)),
        );

        check_prosody_authority_phase(h, &mut outcome, label, &muc_domain, pkix, phase).await?;
        moderator.close().await;
        local.close().await;
    }
    Ok(outcome)
}

fn prosody_authority_profile(h: &mut Harness) -> crate::services::waddle_config::Profile {
    use crate::topology::PROSODY;
    let mut profile = h.profile(Home::A, true);
    profile.allow.retain(|domain| domain == PROSODY);
    profile.dialback = vec![PROSODY.to_owned()];
    profile.trusted_homes = vec![PROSODY.to_owned()];
    profile.endpoints.retain(|(domain, _)| domain == PROSODY);
    h.dirty.insert(Component::Waddle(Home::A));
    h.dirty.insert(Component::Prosody);

    profile
}

async fn start_prosody_authority_phase(
    h: &mut Harness,
    profile: &crate::services::waddle_config::Profile,
    muc_domain: &str,
    pkix: bool,
) -> Result<(usize, usize, usize)> {
    use crate::topology::PROSODY;
    let mut prosody = h.prosody_variant();
    if !pkix {
        prosody.certificate_less_outbound =
            Some(crate::services::prosody::CertificateLessOutbound {
                host: PROSODY,
                target: muc_domain.to_owned(),
            });
    }
    h.prosody.stop().await?;
    h.start_waddle(profile).await?;
    h.prosody.start(&prosody).await?;
    let relay = h.relay(topology::Server::Prosody, topology::Server::Waddle(Home::A))?;
    let baseline = relay.connection_count();
    let before_pkix = support::established(h, Home::A, "inbound", "pkix").await;
    let before_dialback = support::established(h, Home::A, "inbound", "dialback").await;
    Ok((baseline, before_pkix, before_dialback))
}

async fn check_prosody_authority_phase(
    h: &Harness,
    outcome: &mut Outcome,
    label: &str,
    muc_domain: &str,
    pkix: bool,
    phase: (usize, usize, usize),
) -> Result<()> {
    use crate::topology::PROSODY;
    let relay = h.relay(topology::Server::Prosody, topology::Server::Waddle(Home::A))?;
    let (_, before_pkix, before_dialback) = phase;
    let exact_tls_pair = relay.connections().iter().skip(phase.0).any(|connection| {
        use crate::xml::framer::{Framer, Item};
        let mut framer = Framer::new();
        framer.push(connection.initiator_plaintext());
        matches!(framer.next_item(), Ok(Some(Item::Header(header)))
                if header.attribute("from").as_deref() == Some(PROSODY)
                && header.attribute("to").as_deref() == Some(muc_domain))
            && connection.server_hello().is_some()
    });
    let after_pkix = support::established(h, Home::A, "inbound", "pkix").await;
    let after_dialback = support::established(h, Home::A, "inbound", "dialback").await;
    let expected_stream = if pkix {
        after_pkix == before_pkix + 1 && after_dialback == before_dialback
    } else {
        after_dialback == before_dialback + 1 && after_pkix == before_pkix
    };
    outcome.check(format!("{label}: expected authentication on the Prosody-to-MUC TLS pair"),
            exact_tls_pair && expected_stream,
            format!("TLS pair: {exact_tls_pair}; PKIX {before_pkix} -> {after_pkix}; dialback {before_dialback} -> {after_dialback}"));
    Ok(())
}

pub async fn moderation_dialback_only(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("same Prosody occupant requesting a kick on PKIX and dialback");
    let muc_domain = topology::muc_domain(HOME_A);
    let room = format!("{}@{muc_domain}", room_name("moderator-strength"));
    let target_jid = format!("{room}/bob");
    let profile = prosody_authority_profile(h);

    for (label, pkix) in [("PKIX", true), ("dialback", false)] {
        let phase = start_prosody_authority_phase(h, &profile, &muc_domain, pkix).await?;
        let mut owner = h
            .login_waddle(Home::A, "admin", &support::id("owner"))
            .await?;
        join(&mut owner, &room, "owner").await?;
        let mut target = h
            .login_waddle(Home::A, "bob", &support::id("target"))
            .await?;
        let target_presence = join(&mut target, &room, "bob").await?;
        let mut moderator = h
            .login_peer(Peer::Prosody, "admin", &support::id("moderator"))
            .await?;
        let moderator_presence = join(&mut moderator, &room, "remote-mod").await?;
        outcome.check(
            format!("{label}: both occupants joined as unaffiliated participants"),
            [&target_presence, &moderator_presence]
                .iter()
                .all(|presence| {
                    stanza::muc_role_of(presence).as_deref() == Some("participant")
                        && stanza::find(presence, "item", ns::MUC_USER, &|item| {
                            item.attr("affiliation") == Some("none")
                        })
                        .is_some()
                }),
            "",
        );
        let grant = ask(
            &mut owner,
            stanza::muc_role(&support::id("role"), &room, "remote-mod", "moderator"),
        )
        .await?;
        let expected = grant.attr("from") == Some(room.as_str())
            && if pkix {
                is_result(&grant)
            } else {
                grant.attr("type") == Some("error")
                    && stanza::error_condition(&grant).as_deref() == Some("not-allowed")
            };
        outcome.check(
            format!("{label}: room answers moderator role issuance"),
            expected,
            describe(&grant),
        );
        let kick = ask(
            &mut moderator,
            stanza::muc_role(&support::id("kick"), &room, "bob", "none"),
        )
        .await?;
        let removed = !target
            .absent(QUIET, |element| {
                element.name() == "presence"
                    && element.attr("from") == Some(target_jid.as_str())
                    && element.attr("type") == Some("unavailable")
                    && stanza::muc_status_codes(element)
                        .iter()
                        .any(|code| code == "307")
            })
            .await?;
        let expected = if pkix {
            is_result(&kick) && kick.attr("from") == Some(room.as_str()) && removed
        } else {
            denied_by_room(&kick, &room) && !removed
        };
        outcome.check(
            format!("{label}: kick follows the moderator role"),
            expected,
            format!(
                "{}; target received kick presence: {removed}",
                describe(&kick)
            ),
        );
        check_prosody_authority_phase(h, &mut outcome, label, &muc_domain, pkix, phase).await?;
        moderator.close().await;
        target.close().await;
        owner.close().await;
    }
    Ok(outcome)
}

pub async fn own_edit_delete(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            peer.label(),
            " client editing and retracting its own message in a home-a room",
        ]
        .concat(),
    );
    let room = [
        room_name("edits").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join(&mut owner, &room, "owner").await?;
    let mut author = h.login_peer(peer, "alice", &support::id("author")).await?;
    join(&mut author, &room, "author").await?;
    let original = support::id("original");
    author
        .send(&stanza::groupchat(&original, &room, "first draft"))
        .await?;
    let reflected = author
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(original.as_str())
        })
        .await?;
    let correction = support::id("correction");
    author
        .send(&stanza::correction(
            &correction,
            &room,
            &original,
            "second draft",
        ))
        .await?;
    let edited = owner
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(correction.as_str())
        })
        .await;
    outcome.check("the author's correction is delivered", edited.is_ok(), "");
    let stanza_id =
        stanza::stanza_id(&reflected, &room).context("the room assigned no stanza-id")?;
    let retraction = support::id("retract");
    author
        .send(&stanza::retraction(&retraction, &room, &stanza_id))
        .await?;
    let retracted = owner
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(retraction.as_str())
        })
        .await;
    outcome.check(
        "the author's retraction is delivered",
        retracted.is_ok(),
        "",
    );
    author.close().await;
    owner.close().await;
    Ok(outcome)
}

/// A transport failure, not an answer from the room's host.
fn transport_condition(answer: &Element) -> bool {
    matches!(
        stanza::error_condition(answer).as_deref(),
        Some("remote-server-not-found" | "remote-server-timeout")
    )
}

pub async fn private_discovery_and_mam(h: &mut Harness, peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new(
        [
            peer.label(),
            " client probing a members-only home-a room it is not in, after reading an open one",
        ]
        .concat(),
    );
    let open = [
        room_name("open").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let room = [
        room_name("private").as_str(),
        "@",
        &topology::muc_domain(HOME_A),
    ]
    .concat();
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("owner"))
        .await?;
    join(&mut owner, &open, "owner").await?;
    send_messages(&mut owner, &open, 1).await?;
    join_with(
        &mut owner,
        &room,
        "owner",
        &[("muc#roomconfig_membersonly", "1")],
    )
    .await?;
    let mut outsider = h
        .login_peer(peer, "alice", &support::id("outsider"))
        .await?;
    // Control: on the same path, the room's host answers this client about
    // an open room, so a refusal below is the room's, not a transport
    // failure or a room the host would not name to anybody.
    let info = ask(
        &mut outsider,
        stanza::disco_info(&support::id("info"), &open),
    )
    .await?;
    outcome.check(
        "control: disco#info of an open room is answered",
        is_result(&info),
        describe(&info),
    );
    join(&mut outsider, &open, "outsider").await?;
    let (messages, _) = page_archive(&mut outsider, &open).await?;
    outcome.check(
        "control: an occupant reads the open room's archive",
        messages >= 1,
        format!("{messages} messages"),
    );
    let items = ask(
        &mut outsider,
        stanza::disco_items(&support::id("items"), &room),
    )
    .await?;
    outcome.check(
        "private discovery denied",
        !transport_condition(&items)
            && (!is_result(&items)
                || items
                    .children()
                    .all(|query| query.children().next().is_none())),
        describe(&items),
    );
    let mam_id = support::id("mam");
    let mam = ask(
        &mut outsider,
        stanza::mam_query(&mam_id, Some(&room), 10, None),
    )
    .await?;
    outcome.check(
        "MAM denied by the room's host",
        !is_result(&mam) && !transport_condition(&mam),
        describe(&mam),
    );
    outsider.close().await;
    owner.close().await;
    Ok(outcome)
}

pub async fn same_room_names(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("rooms with one local name on home-a and home-b");
    let name = room_name("lobby");
    let room_a = [name.as_str(), "@", &topology::muc_domain(HOME_A)].concat();
    let room_b = [name.as_str(), "@", &topology::muc_domain(HOME_B)].concat();
    let mut owner_a = h.login_waddle(Home::A, "admin", &support::id("a")).await?;
    join(&mut owner_a, &room_a, "owner").await?;
    let mut owner_b = h.login_waddle(Home::B, "admin", &support::id("b")).await?;
    join(&mut owner_b, &room_b, "owner").await?;
    let mut alice = h
        .login_waddle(Home::A, "alice", &support::id("both"))
        .await?;
    join(&mut alice, &room_a, "alice").await?;
    join(&mut alice, &room_b, "alice").await?;
    let message_id = support::id("lobby");
    owner_b
        .send(&stanza::groupchat(
            &message_id,
            &room_b,
            "only in home-b's lobby",
        ))
        .await?;
    let arrived = alice
        .wait_for(ANSWER, |element| {
            element.attr("id") == Some(message_id.as_str())
        })
        .await?;
    outcome.check(
        "home-b's message comes from home-b's room only",
        arrived
            .attr("from")
            .is_some_and(|from| from.starts_with(&room_b)),
        arrived.attr("from").unwrap_or_default().to_owned(),
    );
    let absent = owner_a
        .absent(QUIET, |element| {
            element.attr("id") == Some(message_id.as_str())
        })
        .await?;
    outcome.check("home-a's room of the same name does not see it", absent, "");
    alice.close().await;
    owner_a.close().await;
    owner_b.close().await;
    Ok(outcome)
}
