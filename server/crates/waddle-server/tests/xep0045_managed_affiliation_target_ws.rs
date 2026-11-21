//! XEP-0045 §9 affiliation targets in Waddle-managed channels.
//!
//! A managed channel stores every affiliation as an authorization
//! relationship whose subject is a bare account JID. An admin `set` naming a
//! domain-only JID, or one too long for the authorization ID codec, is a
//! client addressing error: the whole batch is answered with `<jid-malformed/>`
//! (type `modify`) before any affiliation changes, not with
//! `<internal-server-error/>` after a partial write. The admin
//! `channels:set-affiliation` command rejects the same targets as a bad
//! request.

use std::time::Duration;

use tokio::sync::Mutex;
use waddle_ws_test_support as ws_common;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::{self, Element};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_CLIENT: &str = "jabber:client";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUC_ADMIN: &str = "http://jabber.org/protocol/muc#admin";
const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
const NODE_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
const NODE_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn has_iq_id(element: &Element, id: &str) -> bool {
    element.name() == "iq" && element.attr("id") == Some(id)
}

async fn send(client: &mut WsXmppClient, element: Element) {
    client
        .send(&String::from(&element))
        .await
        .expect("send stanza");
}

async fn recv_iq(client: &mut WsXmppClient, id: &str) -> Element {
    let frame = client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| has_iq_id(&element, id))
        })
        .await
        .expect("IQ response");
    frame.parse().expect("IQ response element")
}

fn form_field(var: &str, field_type: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), field_type)
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

fn admin_command(id: &str, node: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(form_field("FORM_TYPE", "hidden", node));
    for (var, value) in fields {
        form = form.append(form_field(var, "text-single", value));
    }
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .append(
            Element::builder("command", NS_COMMANDS)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
                .append(form.build())
                .build(),
        )
        .build()
}

fn form_value(response: &Element, var: &str) -> Option<String> {
    let command = response.get_child("command", NS_COMMANDS)?;
    let form = command.get_child("x", NS_DATA)?;
    form.children()
        .find(|field| field.name() == "field" && field.attr("var") == Some(var))?
        .get_child("value", NS_DATA)
        .map(Element::text)
}

fn assert_result(response: &Element) {
    assert_eq!(
        response.attr("type"),
        Some("result"),
        "{}",
        String::from(response)
    );
}

/// The defined condition and type of an IQ error response.
fn stanza_error(response: &Element) -> (String, String) {
    assert_eq!(
        response.attr("type"),
        Some("error"),
        "{}",
        String::from(response)
    );
    let error = response
        .children()
        .find(|child| child.name() == "error")
        .expect("error child");
    let condition = error
        .children()
        .find(|child| child.ns() == NS_STANZAS && child.name() != "text")
        .expect("defined condition");
    (
        condition.name().to_string(),
        error.attr("type").unwrap_or_default().to_string(),
    )
}

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        ADMIN,
        server.fixed_account_password(),
        resource,
    )
    .await
    .expect("admin connect")
}

/// Create a managed channel and make the admin account its owner occupant.
async fn owned_managed_channel(admin: &mut WsXmppClient, name: &str) -> String {
    let id = format!("create-{name}");
    send(
        admin,
        admin_command(&id, NODE_CHANNELS_CREATE, &[("name", name)]),
    )
    .await;
    let created = recv_iq(admin, &id).await;
    assert_result(&created);
    let channel = form_value(&created, "channel_jid").expect("channel_jid");

    let id = format!("own-{name}");
    let owner = format!("{ADMIN}@{DOMAIN}");
    send(
        admin,
        admin_command(
            &id,
            NODE_CHANNELS_SET_AFFILIATION,
            &[
                ("channel_jid", channel.as_str()),
                ("member_jid", owner.as_str()),
                ("affiliation", "owner"),
            ],
        ),
    )
    .await;
    assert_result(&recv_iq(admin, &id).await);

    let occupant = format!("{channel}/{ADMIN}");
    send(
        admin,
        Element::builder("presence", NS_CLIENT)
            .attr(
                minidom::rxml::xml_ncname!("to").to_owned(),
                occupant.as_str(),
            )
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
    .await;
    admin
        .recv_matching(|frame| frame.contains("<presence") && frame.contains(occupant.as_str()))
        .await
        .expect("self-presence");
    while admin.recv_timeout(Duration::from_millis(150)).await.is_ok() {}
    channel
}

fn affiliation_set(room: &str, id: &str, items: &[(&str, &str)]) -> Element {
    let mut query = Element::builder("query", NS_MUC_ADMIN);
    for (jid, affiliation) in items {
        query = query.append(
            Element::builder("item", NS_MUC_ADMIN)
                .attr(minidom::rxml::xml_ncname!("jid").to_owned(), *jid)
                .attr(
                    minidom::rxml::xml_ncname!("affiliation").to_owned(),
                    *affiliation,
                )
                .build(),
        );
    }
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(query.build())
        .build()
}

fn affiliation_get(room: &str, id: &str, affiliation: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), room)
        .append(
            Element::builder("query", NS_MUC_ADMIN)
                .append(
                    Element::builder("item", NS_MUC_ADMIN)
                        .attr(
                            minidom::rxml::xml_ncname!("affiliation").to_owned(),
                            affiliation,
                        )
                        .build(),
                )
                .build(),
        )
        .build()
}

#[tokio::test]
async fn xep_0045_managed_channel_rejects_non_account_affiliation_targets() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "managed-target-admin").await;
    let room = owned_managed_channel(&mut admin, "managed-targets").await;

    let member = format!("member@{DOMAIN}");
    let oversized = format!("{}@{DOMAIN}", "a".repeat(760));
    for (case, target) in [("domain", DOMAIN), ("oversized", oversized.as_str())] {
        // The account item precedes the invalid one: nothing in the batch
        // may be applied.
        let id = format!("set-{case}");
        send(
            &mut admin,
            affiliation_set(
                &room,
                &id,
                &[(member.as_str(), "member"), (target, "outcast")],
            ),
        )
        .await;
        let response = recv_iq(&mut admin, &id).await;
        assert_eq!(
            stanza_error(&response),
            ("jid-malformed".to_string(), "modify".to_string()),
            "{case}"
        );
    }

    let id = "members-after-rejected-batches";
    send(&mut admin, affiliation_get(&room, id, "member")).await;
    let members = recv_iq(&mut admin, id).await;
    assert_result(&members);
    let listed: Vec<String> = members
        .get_child("query", NS_MUC_ADMIN)
        .expect("admin query")
        .children()
        .filter_map(|item| item.attr("jid").map(ToString::to_string))
        .collect();
    assert!(
        !listed.contains(&member),
        "rejected batch was partly applied: {listed:?}"
    );

    let _ = admin.close().await;
}

#[tokio::test]
async fn admin_set_affiliation_rejects_non_account_targets() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "managed-target-command").await;
    let room = owned_managed_channel(&mut admin, "managed-command-targets").await;

    let id = "set-affiliation-domain";
    send(
        &mut admin,
        admin_command(
            id,
            NODE_CHANNELS_SET_AFFILIATION,
            &[
                ("channel_jid", room.as_str()),
                ("member_jid", DOMAIN),
                ("affiliation", "outcast"),
            ],
        ),
    )
    .await;
    let response = recv_iq(&mut admin, id).await;
    assert_eq!(stanza_error(&response).0, "bad-request");

    let _ = admin.close().await;
}
