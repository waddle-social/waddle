//! Stock ejabberd receiving an enrolled Space deletion over federation.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result};
use minidom::Element;
use rxml::xml_ncname;
use tokio::process::Command;

use crate::evidence::{Outcome, Unverifiable};
use crate::manifest::Peer;
use crate::scenarios::harness::{Component, Harness, ANSWER, QUIET};
use crate::scenarios::support::{self, ask, describe, is_result};
use crate::services::{spicedb::SpiceDb, waddle::S2s};
use crate::topology::{self, Home, EJABBERD};
use crate::xml::{ns, stanza};

const SEED: &str = "INTEROP_ENROLLED_SPACE_SEED_BIN";

pub async fn ejabberd_receives_enrolled_space_deletion(h: &mut Harness) -> Result<Outcome> {
    let binary = std::env::var_os(SEED).ok_or_else(|| Unverifiable(format!("{SEED} is unset")))?;
    let home = Home::A;
    let postgres = h.postgres.get(&home).context("home-a database")?;
    let owner_url = postgres
        .owner_fixture_url()
        .ok_or_else(|| Unverifiable("no private owner socket for the enrolled fixture".into()))?;
    postgres.create_database("spicedb_enrolled").await?;
    let spice = SpiceDb::start(
        &h.bins.spicedb,
        &h.logs,
        "stock-enrolled-spicedb",
        h.ports.enrolled_space_spicedb(),
        postgres.url("spicedb_enrolled"),
    )
    .await?;
    let node = support::id("stock-enrolled-space");
    let mut seed = Command::new(binary);
    seed.args([
        "provision_enrolled_space_for_stock_peer",
        "--exact",
        "--ignored",
        "--nocapture",
    ])
    .env("INTEROP_ENROLLED_SPACE_DISPOSABLE", "1")
    .env("INTEROP_ENROLLED_SPACE_POSTGRES_URL", owner_url)
    .env("INTEROP_ENROLLED_SPACE_SPICEDB_ENDPOINT", spice.endpoint())
    .env("INTEROP_ENROLLED_SPACE_SPICEDB_KEY", &spice.preshared_key)
    .env("INTEROP_ENROLLED_SPACE_NODE", &node)
    .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(120), seed.output())
        .await
        .map_err(|_| Unverifiable("enrolled Space provisioning timed out".into()))?
        .map_err(|_| Unverifiable("enrolled Space provisioning could not run".into()))?;
    anyhow::ensure!(
        output.status.success(),
        "enrolled Space provisioning failed"
    );
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| anyhow::anyhow!("invalid enrolled Space provisioning output"))?;
    let handoff = text
        .lines()
        .find_map(|line| line.strip_prefix("INTEROP_ENROLLED_SPACE_RUNTIME "))
        .context("the owner fixture did not return an attested runtime handoff")?;
    let mut environment: BTreeMap<String, String> = serde_json::from_str(handoff)
        .map_err(|_| anyhow::anyhow!("invalid enrolled Space runtime handoff"))?;
    environment.insert("WADDLE_SPICEDB_ENDPOINT".to_owned(), spice.endpoint());
    environment.insert(
        "WADDLE_SPICEDB_PRESHARED_KEY".to_owned(),
        spice.preshared_key.clone(),
    );
    let profile = h.profile(home, true);
    let config = h.write_profile(&profile)?;
    let ports = h.ports;
    h.dirty.insert(Component::Waddle(home));
    let waddle = h.waddle(home)?;
    waddle.stop().await?;
    waddle
        .start_with_environment(ports, S2s::Enabled { config }, &environment)
        .await?;
    h.settle_stock(Peer::Ejabberd)
        .await
        .map_err(anyhow::Error::msg)?;

    let outcome = receive_deletion(h, &node).await;
    h.waddle(home)?.stop().await?;
    outcome
}

async fn receive_deletion(h: &Harness, node: &str) -> Result<Outcome> {
    let service = topology::spaces_domain(Home::A.domain());
    let recipient = format!("alice@{EJABBERD}");
    let mut owner = h
        .login_waddle(Home::A, "admin", &support::id("space-owner"))
        .await?;
    let mut reader = h
        .login_peer(Peer::Ejabberd, "alice", &support::id("space-subscriber"))
        .await?;
    reader
        .send(&stanza::presence(ns::CLIENT, None, None))
        .await?;
    let granted = ask(
        &mut owner,
        stanza::pubsub_affiliations_set(
            &support::id("grant"),
            &service,
            node,
            &[(&recipient, "member")],
        ),
    )
    .await?;
    anyhow::ensure!(
        is_result(&granted),
        "canonical membership refused: {}",
        describe(&granted)
    );
    let subscribed = ask(
        &mut reader,
        stanza::pubsub_subscribe(&support::id("subscribe"), &service, node, &recipient),
    )
    .await?;
    anyhow::ensure!(
        is_result(&subscribed)
            && subscribed
                .get_child("pubsub", ns::PUBSUB)
                .and_then(|pubsub| pubsub.get_child("subscription", ns::PUBSUB))
                .is_some_and(|subscription| {
                    subscription.attr("subscription") == Some("subscribed")
                        && subscription.attr("jid") == Some(recipient.as_str())
                        && subscription.attr("node") == Some(node)
                }),
        "ejabberd account's subscription refused: {}",
        describe(&subscribed),
    );
    let deleted = ask(
        &mut owner,
        stanza::set(
            &support::id("delete"),
            Some(&service),
            Element::builder("pubsub", ns::PUBSUB_OWNER)
                .append(
                    Element::builder("delete", ns::PUBSUB_OWNER)
                        .attr(xml_ncname!("node").to_owned(), node)
                        .build(),
                )
                .build(),
        ),
    )
    .await?;
    anyhow::ensure!(
        is_result(&deleted),
        "enrolled owner delete refused: {}",
        describe(&deleted)
    );
    let deletion = |message: &Element| {
        message.is("message", ns::CLIENT)
            && message
                .get_child("event", ns::PUBSUB_EVENT)
                .and_then(|event| event.get_child("delete", ns::PUBSUB_EVENT))
                .is_some_and(|delete| delete.attr("node") == Some(node))
    };
    let notice = reader.wait_for(ANSWER, deletion).await?;
    let mut outcome = Outcome::new("stock ejabberd C2S receiving a Waddle headline over S2S");
    outcome.check(
        "enrolled owner grant, stock account subscription, and owner delete",
        true,
        "all three completed as XMPP IQ results",
    );
    outcome.check(
        "headline to the subscribing bare JID",
        notice.attr("from") == Some(service.as_str())
            && notice.attr("to") == Some(recipient.as_str())
            && notice.attr("type") == Some("headline")
            && notice.attr("id").is_some_and(|id| !id.is_empty()),
        describe(&notice),
    );
    outcome.check(
        "one deletion notice",
        reader.absent(QUIET, deletion).await?,
        "the same stock client received no repeat delete event",
    );
    reader.close().await;
    owner.close().await;
    Ok(outcome)
}
