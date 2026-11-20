use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;

use super::{denied_by_room, join, join_with, page_archive, room_name, send_messages};
use crate::client::session::{Account, Session};
use crate::evidence::Outcome;
use crate::manifest::Peer;
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::topology::{self, Home, HOME_A, PROSODY};
use crate::xml::{ns, stanza};

pub async fn foreign_reassignment(h: &mut Harness, _peer: Peer) -> Result<Outcome> {
    let mut outcome = Outcome::new("a Prosody account deleted and recreated under the same JID");
    let mut profile = h.profile(Home::A, true);
    profile.trusted_homes.retain(|home| home != PROSODY);
    h.dirty.insert(Component::Waddle(Home::A));
    h.dirty.insert(Component::Prosody);
    h.start_waddle(&profile).await?;
    let variant = h.prosody_variant();
    let old = Account {
        domain: PROSODY.to_owned(),
        username: support::id("reassigned"),
        password: uuid::Uuid::new_v4().simple().to_string(),
    };
    let new = Account {
        password: uuid::Uuid::new_v4().simple().to_string(),
        ..old.clone()
    };
    let bare = format!("{}@{PROSODY}", old.username);
    let port = h.ports.prosody_c2s();
    h.prosody.stop().await?;
    h.prosody
        .set_account(PROSODY, &old.username, Some(&old.password))
        .await?;
    h.prosody.start(&variant).await?;

    let room = format!(
        "{}@{}",
        room_name("reassignment"),
        topology::muc_domain(HOME_A)
    );
    let mut owner = h
        .login_waddle(Home::A, "admin", "reassignment-owner")
        .await?;
    join_with(
        &mut owner,
        &room,
        "owner",
        &[("muc#roomconfig_membersonly", "1")],
    )
    .await?;
    let grant = ask(
        &mut owner,
        stanza::muc_affiliation(&support::id("member"), &room, &bare, "member"),
    )
    .await?;
    anyhow::ensure!(
        is_result(&grant),
        "membership grant refused: {}",
        describe(&grant)
    );
    send_messages(&mut owner, &room, 1).await?;
    let mut original = Session::tcp(port, &old, "original", "original holder").await?;
    check_reassigned_member(&mut original, &room, "original", &bare, &mut outcome).await?;
    let original_occupant = format!("{room}/original");
    original
        .send(&stanza::presence(
            ns::CLIENT,
            Some(&original_occupant),
            Some("unavailable"),
        ))
        .await?;
    original
        .wait_for(ANSWER, |element| {
            element.name() == "presence"
                && element.attr("from") == Some(original_occupant.as_str())
                && element.attr("type") == Some("unavailable")
        })
        .await
        .context("original holder leaves the room before deletion")?;
    original.close().await;

    h.prosody.stop().await?;
    h.prosody.set_account(PROSODY, &old.username, None).await?;
    h.prosody.start(&variant).await?;
    let absent = Session::tcp(port, &old, "absent", "deleted account").await;
    anyhow::ensure!(
        absent.is_err_and(|error| error.to_string() == "deleted account: SASL failed"),
        "Prosody did not explicitly reject authentication for the deleted account"
    );
    h.prosody.stop().await?;
    h.prosody
        .set_account(PROSODY, &new.username, Some(&new.password))
        .await?;
    h.prosody.start(&variant).await?;
    for (from_home_a, domain) in [(false, HOME_A), (true, PROSODY)] {
        let ready = support::ping_until_result(
            h,
            Peer::Prosody,
            from_home_a,
            domain,
            Duration::from_secs(90),
        )
        .await?;
        anyhow::ensure!(
            is_result(&ready),
            "Prosody restart federation control: {}",
            describe(&ready)
        );
    }
    let stale = Session::tcp(port, &old, "stale", "old credential").await;
    outcome.check(
        "old credential is rejected after the account is recreated",
        stale.is_err_and(|error| error.to_string() == "old credential: SASL failed"),
        "",
    );
    let mut reassigned = Session::tcp(port, &new, "new", "new holder").await?;
    check_reassigned_member(&mut reassigned, &room, "reassigned", &bare, &mut outcome).await?;
    let bob_grant = ask(
        &mut owner,
        stanza::muc_affiliation(&support::id("member"), &room, "bob@home-a.test", "member"),
    )
    .await?;
    anyhow::ensure!(
        is_result(&bob_grant),
        "target membership grant refused: {}",
        describe(&bob_grant)
    );
    let mut target = h
        .login_waddle(Home::A, "bob", "reassignment-target")
        .await?;
    let target_join = join(&mut target, &room, "bob").await?;
    anyhow::ensure!(
        stanza::muc_role_of(&target_join).as_deref() == Some("participant"),
        "moderation target did not join as a participant"
    );
    let moderation = ask(
        &mut reassigned,
        stanza::muc_role(&support::id("devoice"), &room, "bob", "visitor"),
    )
    .await?;
    let target_occupant = format!("{room}/bob");
    let devoice = |element: &Element| {
        element.name() == "presence"
            && element.attr("from") == Some(target_occupant.as_str())
            && stanza::muc_role_of(element).as_deref() == Some("visitor")
    };
    outcome.check(
        "room denies moderation by the new holder",
        denied_by_room(&moderation, &room) && target.absent(QUIET, devoice).await?,
        describe(&moderation),
    );
    let moderation = ask(
        &mut owner,
        stanza::muc_role(&support::id("control"), &room, "bob", "visitor"),
    )
    .await?;
    outcome.check(
        "the owner can devoice the same target",
        is_result(&moderation) && !target.absent(ANSWER, devoice).await?,
        describe(&moderation),
    );
    target.close().await;
    reassigned.close().await;

    let revoked = ask(
        &mut owner,
        stanza::muc_affiliation(&support::id("revoke"), &room, &bare, "none"),
    )
    .await?;
    anyhow::ensure!(
        is_result(&revoked),
        "membership revocation refused: {}",
        describe(&revoked)
    );
    let mut revoked_holder = Session::tcp(port, &new, "revoked", "revoked holder").await?;
    check_reassigned_revocation(&mut revoked_holder, &mut owner, &room, &mut outcome).await?;
    revoked_holder.close().await;
    owner.close().await;
    h.prosody.stop().await?;
    h.prosody.set_account(PROSODY, &new.username, None).await?;
    Ok(outcome)
}

async fn check_reassigned_member(
    session: &mut Session,
    room: &str,
    nick: &str,
    bare: &str,
    outcome: &mut Outcome,
) -> Result<()> {
    let seated = join(session, room, nick)
        .await
        .with_context(|| format!("{nick} holder joins"))?;
    let (archived, _) = page_archive(session, room)
        .await
        .with_context(|| format!("{nick} holder reads the archive"))?;
    let member = seated
        .get_child("x", ns::MUC_USER)
        .and_then(|x| x.get_child("item", ns::MUC_USER))
        .and_then(|item| item.attr("affiliation"))
        == Some("member");
    outcome.check(
        format!("{nick} holder binds the JID, joins as a participant and reads the archive"),
        session
            .jid
            .split_once('/')
            .is_some_and(|(jid, _)| jid == bare)
            && member
            && stanza::muc_role_of(&seated).as_deref() == Some("participant")
            && archived == 1,
        format!("{archived} archived messages"),
    );
    Ok(())
}

async fn check_reassigned_revocation(
    holder: &mut Session,
    owner: &mut Session,
    room: &str,
    outcome: &mut Outcome,
) -> Result<()> {
    let occupant = format!("{room}/revoked");
    holder.send(&stanza::muc_join(&occupant)).await?;
    let refusal = holder
        .wait_for(ANSWER, |element| {
            element.name() == "presence"
                && element.attr("from") == Some(occupant.as_str())
                && (element.attr("type") == Some("error") || stanza::is_self_presence(element))
        })
        .await?;
    outcome.check(
        "room denies rejoining after revocation",
        stanza::error_condition(&refusal).as_deref() == Some("registration-required"),
        describe(&refusal),
    );
    let query = support::id("revoked-mam");
    let mam = ask(holder, stanza::mam_query(&query, Some(room), 10, None)).await?;
    let no_results = holder
        .absent(QUIET, |element| {
            element.children().any(|child| {
                child.is("result", ns::MAM) && child.attr("queryid") == Some(query.as_str())
            })
        })
        .await?;
    let (control, _) = page_archive(owner, room).await?;
    outcome.check(
        "revoked holder cannot read the archive and the owner still can",
        denied_by_room(&mam, room) && no_results && control == 1,
        describe(&mam),
    );
    Ok(())
}
