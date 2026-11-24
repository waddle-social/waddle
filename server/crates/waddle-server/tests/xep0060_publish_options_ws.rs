//! XEP-0060 §7.1.5 publish-options over WebSocket C2S, as used for PEP
//! (XEP-0163), private storage (XEP-0223), bookmarks (XEP-0402) and
//! displayed-state sync (XEP-0490).
//!
//! - Every form field is a precondition: an existing node whose configuration
//!   does not match fails with `<conflict/>` + `<precondition-not-met/>`.
//! - A missing node is auto-created with the preconditions as its
//!   configuration.
//! - An unknown field or a wrong FORM_TYPE rejects the publication.
//! - The server domain and the account answer disco#info with the
//!   `pubsub/pep` identity and the `publish-options` feature.

use waddle_ws_test_support as ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const PUBLISH_OPTIONS_FEATURE: &str = "http://jabber.org/protocol/pubsub#publish-options";
const FORM_TYPE: &str = "http://jabber.org/protocol/pubsub#publish-options";
const BOOKMARKS_NODE: &str = "urn:xmpp:bookmarks:1";
const MDS_NODE: &str = "urn:xmpp:mds:displayed:0";
const PRIVATE_NODE: &str = "urn:example:private-storage:0";

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn iq(client: &mut WsXmppClient, kind: &str, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="{kind}" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq response")
}

fn form(fields: &[(&str, &str)]) -> String {
    form_with_type(FORM_TYPE, fields)
}

fn form_with_type(form_type: &str, fields: &[(&str, &str)]) -> String {
    let fields: String = fields
        .iter()
        .map(|(var, value)| format!(r#"<field var="{var}"><value>{value}</value></field>"#))
        .collect();
    format!(
        r#"<publish-options><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>{form_type}</value></field>{fields}</x></publish-options>"#
    )
}

fn publish(node: &str, item_id: &str, options: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{node}"><item id="{item_id}"><data xmlns="urn:example:data"/></item></publish>{options}</pubsub>"#
    )
}

fn bookmark_publish(options: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}"><publish node="{BOOKMARKS_NODE}"><item id="room@conference.localhost"><conference xmlns="{BOOKMARKS_NODE}" name="Room" autojoin="true"/></item></publish>{options}</pubsub>"#
    )
}

fn is_precondition_not_met(resp: &str) -> bool {
    resp.contains("type='error'")
        && resp.contains("<conflict")
        && resp.contains("precondition-not-met")
        && resp.contains("http://jabber.org/protocol/pubsub#errors")
}

async fn config_value(client: &mut WsXmppClient, id: &str, node: &str, var: &str) -> String {
    let resp = iq(
        client,
        "get",
        id,
        &format!("{ADMIN}@{DOMAIN}"),
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"/></pubsub>"#),
    )
    .await;
    assert!(resp.contains("type='result'"), "configure get: {resp}");
    let field = resp
        .split(&format!("var='{var}'"))
        .nth(1)
        .unwrap_or_else(|| panic!("{var} missing from configure form: {resp}"));
    let value = field
        .split("<value>")
        .nth(1)
        .and_then(|rest| rest.split("</value>").next())
        .unwrap_or_else(|| panic!("{var} has no value: {resp}"));
    value.to_string()
}

#[tokio::test]
async fn domain_and_account_disco_advertise_pep_identity_and_publish_options() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-disco").await;
    let bare = format!("{ADMIN}@{DOMAIN}");
    let query = format!(r#"<query xmlns="{NS_DISCO_INFO}"/>"#);

    for (id, to) in [("disco-domain", DOMAIN), ("disco-account", bare.as_str())] {
        let resp = iq(&mut admin, "get", id, to, &query).await;
        assert!(resp.contains("type='result'"), "{to} disco: {resp}");
        assert!(
            resp.contains("category='pubsub'") && resp.contains("type='pep'"),
            "{to} must carry the pubsub/pep identity: {resp}"
        );
        assert!(
            resp.contains(&format!("var='{PUBLISH_OPTIONS_FEATURE}'")),
            "{to} must advertise publish-options: {resp}"
        );
    }
}

#[tokio::test]
async fn private_storage_publish_options_create_a_whitelist_persistent_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-0223").await;
    let bare = format!("{ADMIN}@{DOMAIN}");
    let options = form(&[
        ("pubsub#persist_items", "true"),
        ("pubsub#access_model", "whitelist"),
    ]);

    let resp = iq(
        &mut admin,
        "set",
        "p1",
        &bare,
        &publish(PRIVATE_NODE, "item-1", &options),
    )
    .await;
    assert!(
        resp.contains("type='result'"),
        "auto-create publish: {resp}"
    );

    assert_eq!(
        config_value(&mut admin, "c1", PRIVATE_NODE, "pubsub#access_model").await,
        "whitelist"
    );
    assert_eq!(
        config_value(&mut admin, "c2", PRIVATE_NODE, "pubsub#persist_items").await,
        "1"
    );

    // The same preconditions on the now-existing node are met.
    let again = iq(
        &mut admin,
        "set",
        "p2",
        &bare,
        &publish(PRIVATE_NODE, "item-2", &options),
    )
    .await;
    assert!(again.contains("type='result'"), "met precondition: {again}");
}

#[tokio::test]
async fn mismatched_precondition_on_existing_node_is_conflict_precondition_not_met() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-mismatch").await;
    let bare = format!("{ADMIN}@{DOMAIN}");

    let created = iq(
        &mut admin,
        "set",
        "p1",
        &bare,
        &publish(
            PRIVATE_NODE,
            "item-1",
            &form(&[("pubsub#access_model", "whitelist")]),
        ),
    )
    .await;
    assert!(created.contains("type='result'"), "{created}");

    let resp = iq(
        &mut admin,
        "set",
        "p2",
        &bare,
        &publish(
            PRIVATE_NODE,
            "item-2",
            &form(&[("pubsub#access_model", "open")]),
        ),
    )
    .await;
    assert!(is_precondition_not_met(&resp), "mismatch: {resp}");

    // The failed publish stored nothing and changed nothing.
    let items = iq(
        &mut admin,
        "get",
        "i1",
        &bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{PRIVATE_NODE}"/></pubsub>"#),
    )
    .await;
    assert!(
        items.contains("item-1") && !items.contains("item-2"),
        "{items}"
    );
    assert_eq!(
        config_value(&mut admin, "c1", PRIVATE_NODE, "pubsub#access_model").await,
        "whitelist"
    );
}

#[tokio::test]
async fn open_precondition_never_creates_a_private_well_known_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-pinned").await;
    let bare = format!("{ADMIN}@{DOMAIN}");

    // MDS is pinned whitelist: an `open` precondition cannot be honoured.
    let resp = iq(
        &mut admin,
        "set",
        "p1",
        &bare,
        &publish(
            MDS_NODE,
            "romeo@montague.lit",
            &form(&[("pubsub#access_model", "open")]),
        ),
    )
    .await;
    assert!(is_precondition_not_met(&resp), "open on MDS: {resp}");

    // Nothing was created: the node does not exist.
    let items = iq(
        &mut admin,
        "get",
        "i1",
        &bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{MDS_NODE}"/></pubsub>"#),
    )
    .await;
    assert!(
        items.contains("item-not-found") || !items.contains("romeo@montague.lit"),
        "{items}"
    );
}

#[tokio::test]
async fn unknown_field_and_wrong_form_type_reject_the_publication() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-unknown").await;
    let bare = format!("{ADMIN}@{DOMAIN}");

    let unknown = iq(
        &mut admin,
        "set",
        "p1",
        &bare,
        &publish(
            PRIVATE_NODE,
            "item-1",
            &form(&[("pubsub#nonexistent_option", "x")]),
        ),
    )
    .await;
    assert!(
        unknown.contains("type='error'") && unknown.contains("<bad-request"),
        "unknown field: {unknown}"
    );

    let wrong_type = iq(
        &mut admin,
        "set",
        "p2",
        &bare,
        &publish(
            PRIVATE_NODE,
            "item-1",
            &form_with_type(
                "http://jabber.org/protocol/pubsub#node_config",
                &[("pubsub#access_model", "whitelist")],
            ),
        ),
    )
    .await;
    assert!(
        wrong_type.contains("type='error'") && wrong_type.contains("<bad-request"),
        "wrong FORM_TYPE: {wrong_type}"
    );

    let no_form = iq(
        &mut admin,
        "set",
        "p3",
        &bare,
        &publish(PRIVATE_NODE, "item-1", "<publish-options/>"),
    )
    .await;
    assert!(
        no_form.contains("type='error'") && no_form.contains("<bad-request"),
        "no form: {no_form}"
    );

    // None of the rejected publishes created the node.
    let items = iq(
        &mut admin,
        "get",
        "i1",
        &bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{PRIVATE_NODE}"/></pubsub>"#),
    )
    .await;
    assert!(items.contains("item-not-found"), "{items}");
}

#[tokio::test]
async fn xep0402_bookmark_publish_options_match_the_pinned_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-0402").await;
    let bare = format!("{ADMIN}@{DOMAIN}");

    // XEP-0402 §5: persist_items=true, max_items=max, send_last_published_item
    // =never, access_model=whitelist.
    let spec = form(&[
        ("pubsub#persist_items", "true"),
        ("pubsub#max_items", "max"),
        ("pubsub#send_last_published_item", "never"),
        ("pubsub#access_model", "whitelist"),
    ]);
    let ok = iq(&mut admin, "set", "p1", &bare, &bookmark_publish(&spec)).await;
    assert!(ok.contains("type='result'"), "spec form: {ok}");
    let again = iq(&mut admin, "set", "p2", &bare, &bookmark_publish(&spec)).await;
    assert!(
        again.contains("type='result'"),
        "spec form, existing node: {again}"
    );

    let open = iq(
        &mut admin,
        "set",
        "p3",
        &bare,
        &bookmark_publish(&form(&[("pubsub#access_model", "open")])),
    )
    .await;
    assert!(is_precondition_not_met(&open), "open bookmarks: {open}");
}

#[tokio::test]
async fn xep0490_publish_options_are_enforced_on_an_existing_displayed_node() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "pubopts-0490").await;
    let bare = format!("{ADMIN}@{DOMAIN}");
    let spec = form(&[
        ("pubsub#persist_items", "true"),
        ("pubsub#max_items", "max"),
        ("pubsub#send_last_published_item", "never"),
        ("pubsub#access_model", "whitelist"),
    ]);
    let ok = iq(
        &mut admin,
        "set",
        "p1",
        &bare,
        &publish(MDS_NODE, "a@b.example", &spec),
    )
    .await;
    assert!(ok.contains("type='result'"), "{ok}");

    let wrong = iq(
        &mut admin,
        "set",
        "p2",
        &bare,
        &publish(MDS_NODE, "a@b.example", &form(&[("pubsub#max_items", "1")])),
    )
    .await;
    assert!(
        is_precondition_not_met(&wrong),
        "max_items=1 on MDS: {wrong}"
    );
}
