//! XEP-0490: Message Displayed Synchronization wire-conformance suite.
//!
//! Covers what is distinctive about MDS vs a generic well-known PEP node:
//!
//! - Auto-create on first publish lands the spec-mandated config
//!   (access_model=whitelist, max_items=max, send_last_published_item=
//!   never, persist_items=true).
//! - The client can pass those values verbatim as publish-options —
//!   including the literal `max` token — without precondition-not-met.
//! - The §3 wire shape: item id = JID of chat, payload =
//!   `<displayed xmlns='urn:xmpp:mds:displayed:0'>` containing exactly
//!   one XEP-0359 `<stanza-id>`.
//! - Catch-up on bind via `<items node='urn:xmpp:mds:displayed:0'/>`.
//! - XEP-0163 §3.4 owner-self fan-out reaches the other resource when
//!   it advertises `urn:xmpp:mds:displayed:0+notify` in caps.
//! - Whitelist enforcement: a third-party JID is denied subscribe and
//!   item retrieval (XEP-0060 §6.1 / §7).
//! - Republishing the same chat id overwrites the prior payload (XEP-
//!   0060 max-items / same-id semantics).
//! - MUC `stanza-id` retains its `by=room` attribute end-to-end.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_CAPS: &str = "http://jabber.org/protocol/caps";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";

const MDS_NODE: &str = "urn:xmpp:mds:displayed:0";
const MDS_NS: &str = "urn:xmpp:mds:displayed:0";
const MDS_NOTIFY: &str = "urn:xmpp:mds:displayed:0+notify";
const NS_SID: &str = "urn:xmpp:sid:0";

// ---------------------------------------------------------------------------
// IQ helpers (mirror xep0163_pep_ws.rs; duplicated to keep suites independent)
// ---------------------------------------------------------------------------

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
}

async fn iq_set_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq set");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq set response")
}

async fn iq_get_to(client: &mut WsXmppClient, id: &str, to: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}">{body}</iq>"#
        ))
        .await
        .expect("send iq get");
    client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("iq get response")
}

fn count_item_elements(xml: &str) -> usize {
    count_occurrences(xml, "<item ") + count_occurrences(xml, "<item>")
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

/// The XEP-0490 §3 publish-options form verbatim. Tests assert the
/// server accepts this exact shape (including `max` for max_items).
const MDS_PUBLISH_OPTIONS_XML: &str = r#"<publish-options>
            <x xmlns="jabber:x:data" type="submit">
              <field var="FORM_TYPE" type="hidden">
                <value>http://jabber.org/protocol/pubsub#publish-options</value>
              </field>
              <field var="pubsub#persist_items"><value>true</value></field>
              <field var="pubsub#max_items"><value>max</value></field>
              <field var="pubsub#send_last_published_item"><value>never</value></field>
              <field var="pubsub#access_model"><value>whitelist</value></field>
            </x>
          </publish-options>"#;

fn mds_publish_xml(chat_jid: &str, stanza_id: &str, stanza_id_by: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB}">
          <publish node="{MDS_NODE}">
            <item id="{chat_jid}">
              <displayed xmlns="{MDS_NS}">
                <stanza-id xmlns="{NS_SID}" by="{stanza_id_by}" id="{stanza_id}"/>
              </displayed>
            </item>
          </publish>
          {MDS_PUBLISH_OPTIONS_XML}
        </pubsub>"#
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn xep0490_first_publish_with_spec_publish_options_succeeds() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "mds-pubopts-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // XEP-0490 §3: first publish carries the publish-options form
    // with whitelist + max + never. Server MUST auto-create the node
    // and accept the precondition shape (incl. the literal `max`).
    let resp = iq_set_to(
        &mut admin,
        "mds-pubopts-publish-1",
        &admin_bare,
        &mds_publish_xml(
            "romeo@montague.lit",
            "0f710f2b-52ed-4d52-b928-784dad74a52b",
            &admin_bare,
        ),
    )
    .await;
    assert!(
        resp.contains(r#"type='result'"#),
        "first MDS publish with spec publish-options must succeed: {resp}"
    );
    assert!(
        !resp.contains("precondition-not-met"),
        "must not return precondition-not-met for the spec's exact form: {resp}"
    );
}

#[tokio::test]
async fn xep0490_catchup_returns_all_displayed_items() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "mds-catchup-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Two chats, two publishes — both must come back on the §3.1
    // catch-up retrieve.
    let _ = iq_set_to(
        &mut admin,
        "mds-cu-pub-1",
        &admin_bare,
        &mds_publish_xml(
            "romeo@montague.lit",
            "0f710f2b-52ed-4d52-b928-784dad74a52b",
            &admin_bare,
        ),
    )
    .await;

    let _ = iq_set_to(
        &mut admin,
        "mds-cu-pub-2",
        &admin_bare,
        &mds_publish_xml(
            "example@conference.shakespeare.lit",
            "ca21deaf-812c-48f1-8f16-339a674f2864",
            "example@conference.shakespeare.lit",
        ),
    )
    .await;

    let resp = iq_get_to(
        &mut admin,
        "mds-cu-get-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{MDS_NODE}"/></pubsub>"#),
    )
    .await;

    assert!(resp.contains(r#"type='result'"#), "catch-up get: {resp}");
    assert!(
        resp.contains(r#"id='romeo@montague.lit'"#),
        "catch-up missing DM item: {resp}"
    );
    assert!(
        resp.contains(r#"id='example@conference.shakespeare.lit'"#),
        "catch-up missing MUC item: {resp}"
    );
    assert_eq!(
        count_item_elements(&resp),
        2,
        "two distinct chat ids must both be returned (no max_items eviction): {resp}"
    );
    // The MUC stanza-id retained its by=room JID — clients need this
    // to know which scope the id is valid in.
    assert!(
        resp.contains(r#"by='example@conference.shakespeare.lit'"#),
        "MUC stanza-id by= must survive round-trip: {resp}"
    );
}

#[tokio::test]
async fn xep0490_republish_same_chat_overwrites_prior_item() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "mds-overwrite-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let _ = iq_set_to(
        &mut admin,
        "mds-ow-pub-1",
        &admin_bare,
        &mds_publish_xml("romeo@montague.lit", "stanza-id-old", &admin_bare),
    )
    .await;

    let _ = iq_set_to(
        &mut admin,
        "mds-ow-pub-2",
        &admin_bare,
        &mds_publish_xml("romeo@montague.lit", "stanza-id-new", &admin_bare),
    )
    .await;

    let resp = iq_get_to(
        &mut admin,
        "mds-ow-get-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{MDS_NODE}"/></pubsub>"#),
    )
    .await;

    // Same item-id (the chat JID) means the second publish must
    // overwrite the first; only one item with the new stanza-id is
    // observable.
    assert_eq!(
        count_item_elements(&resp),
        1,
        "republishing same chat must yield exactly one item: {resp}"
    );
    assert!(
        resp.contains(r#"id='stanza-id-new'"#),
        "the new stanza-id must replace the old one: {resp}"
    );
    assert!(
        !resp.contains(r#"id='stanza-id-old'"#),
        "the old stanza-id must be evicted: {resp}"
    );
}

#[tokio::test]
async fn xep0490_third_party_cannot_subscribe_or_read_items() {
    let _serial = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut admin = admin_client(&server, "mds-whitelist-admin").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    // Admin auto-creates the MDS node by publishing.
    let _ = iq_set_to(
        &mut admin,
        "mds-wl-pub-1",
        &admin_bare,
        &mds_publish_xml(
            "romeo@montague.lit",
            "0f710f2b-52ed-4d52-b928-784dad74a52b",
            &admin_bare,
        ),
    )
    .await;

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        "mds-whitelist-bob",
    )
    .await
    .expect("bob connect");

    let bob_bare = format!("bob@{DOMAIN}");

    // §3 mandates access_model=whitelist. A non-owner subscribe
    // attempt MUST fail with an authz error.
    let sub = iq_set_to(
        &mut bob,
        "mds-wl-sub-1",
        &admin_bare,
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="{MDS_NODE}" jid="{bob_bare}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        sub.contains(r#"type='error'"#),
        "third-party subscribe to MDS node must be rejected (whitelist): {sub}"
    );
    assert!(
        !sub.contains(r#"type='result'"#),
        "no success on third-party subscribe: {sub}"
    );

    // Item retrieval likewise must be denied.
    let items = iq_get_to(
        &mut bob,
        "mds-wl-items-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{MDS_NODE}"/></pubsub>"#),
    )
    .await;
    assert!(
        items.contains(r#"type='error'"#),
        "third-party items query must be rejected (whitelist): {items}"
    );

    let _ = bob.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0490_other_resource_with_notify_caps_receives_event() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let mut alice_a = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "mds-self-A",
    )
    .await
    .expect("alice-A");
    let mut alice_b = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        "mds-self-B",
    )
    .await
    .expect("alice-B");
    let alice_bare = format!("alice@{DOMAIN}");
    let alice_b_full = alice_b.full_jid.clone().expect("alice-B full jid");

    // Both resources go presence-available.
    alice_a
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice-A presence");
    alice_b
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice-B presence");

    // alice-B advertises the +notify filter via XEP-0115 caps so the
    // PEP §3.4 owner-self fan-out reaches it.
    let features = [NS_DISCO_INFO, MDS_NOTIFY];
    let caps_node = "https://alice.example/mds-caps";
    let ver = caps_verification_string("client", "pc", "Alice B", &features);
    alice_b
        .send(&format!(
            r#"<presence xmlns="jabber:client"><c xmlns="{NS_CAPS}" hash="sha-1" node="{caps_node}" ver="{ver}"/></presence>"#
        ))
        .await
        .expect("alice-B caps presence");

    // Server caps-disco's alice-B for its features. Reply with the
    // advertised set.
    let disco_query = alice_b
        .recv_matching(|f| {
            f.contains("<iq") && f.contains(r#"type='get'"#) && f.contains(NS_DISCO_INFO)
        })
        .await
        .expect("server caps disco to alice-B");
    let iq_id = extract_iq_id(&disco_query);
    let feature_xml: String = features
        .iter()
        .map(|f| format!(r#"<feature var="{f}"/>"#))
        .collect();
    alice_b
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="result" id="{iq_id}" from="{alice_b_full}"><query xmlns="{NS_DISCO_INFO}" node="{caps_node}#{ver}"><identity category="client" type="pc" name="Alice B"/>{feature_xml}</query></iq>"#
        ))
        .await
        .expect("alice-B disco reply");

    ping_anchor(
        &mut alice_b,
        &format!("mds-self-anchor-{}", uuid::Uuid::new_v4()),
    )
    .await;

    // alice-A publishes a displayed item.
    let pub_resp = iq_set_to(
        &mut alice_a,
        "mds-self-pub",
        &alice_bare,
        &mds_publish_xml(
            "romeo@montague.lit",
            "0f710f2b-52ed-4d52-b928-784dad74a52b",
            &alice_bare,
        ),
    )
    .await;
    assert!(pub_resp.contains(r#"type='result'"#), "publish: {pub_resp}");

    let event = wait_for_event_message(&mut alice_b, MDS_NODE, Duration::from_secs(2))
        .await
        .expect("alice-B advertising MDS +notify MUST receive the §3.4 owner-self fan-out event");
    // Per XEP-0490 the event MUST carry the chat-jid item id.
    assert!(
        event.contains(r#"id='romeo@montague.lit'"#),
        "event must carry chat-jid item id: {event}"
    );
    // Per XEP-0163 §4.3 the from is the bare account JID.
    assert!(
        event.contains(&format!(r#"from='{alice_bare}'"#))
            || event.contains(&format!(r#"from='{alice_bare}'"#)),
        "event from must be the account bare JID: {event}"
    );
    // Per XEP-0060 §12.18 the message MUST be type=headline.
    assert!(
        event.contains(r#"type='headline'"#) || event.contains(r#"type='headline'"#),
        "PEP event must be type=headline: {event}"
    );
    // Payload must round-trip the typed displayed payload.
    assert!(
        event.contains("<displayed"),
        "event must carry the displayed payload: {event}"
    );
    assert!(
        event.contains(r#"id='0f710f2b-52ed-4d52-b928-784dad74a52b'"#),
        "displayed stanza-id must round-trip: {event}"
    );

    let _ = alice_a.close().await;
    let _ = alice_b.close().await;
}

#[tokio::test]
async fn xep0490_muc_stanza_id_by_attribute_round_trips_through_publish_and_catchup() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "mds-muc-1").await;
    let admin_bare = format!("{ADMIN}@{DOMAIN}");

    let room_jid = "example@conference.shakespeare.lit";
    let stanza_id = "ca21deaf-812c-48f1-8f16-339a674f2864";

    // XEP-0490 §3 for group chats: `by` on the inner <stanza-id/>
    // points at the room, not the user's server. The server is opaque
    // about the value (it's a PubSub payload) but the catch-up MUST
    // preserve it verbatim.
    let _ = iq_set_to(
        &mut admin,
        "mds-muc-pub-1",
        &admin_bare,
        &mds_publish_xml(room_jid, stanza_id, room_jid),
    )
    .await;

    let items = iq_get_to(
        &mut admin,
        "mds-muc-get-1",
        &admin_bare,
        &format!(r#"<pubsub xmlns="{NS_PUBSUB}"><items node="{MDS_NODE}"/></pubsub>"#),
    )
    .await;

    assert!(
        items.contains(&format!(r#"id='{room_jid}'"#)),
        "item: {items}"
    );
    assert!(
        items.contains(&format!(r#"by='{room_jid}'"#)),
        "MUC stanza-id by= must carry the room JID through publish + catch-up: {items}"
    );
    assert!(
        items.contains(&format!(r#"id='{stanza_id}'"#)),
        "stanza-id id must survive: {items}"
    );
}

// ---------------------------------------------------------------------------
// Caps + ping helpers (copied verbatim from xep0163_pep_ws.rs)
// ---------------------------------------------------------------------------

fn caps_verification_string(
    identity_category: &str,
    identity_type: &str,
    identity_name: &str,
    features: &[&str],
) -> String {
    use waddle_xmpp::disco::info::{Feature, Identity};
    use waddle_xmpp::xep::xep0115::compute_caps_hash;
    let identities = vec![Identity::new(
        identity_category,
        identity_type,
        Some(identity_name),
    )];
    let features: Vec<Feature> = features.iter().map(|f| Feature::new(f)).collect();
    compute_caps_hash(&identities, &features)
}

fn extract_iq_id(frame: &str) -> String {
    use ws_common::extract_attr_after;
    extract_attr_after(frame, "<iq", "id").expect("iq has id attribute")
}

async fn ping_anchor(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send ping");
    let _ = client
        .recv_matching(|frame| frame.contains(&format!(r#"id='{id}'"#)) && frame.contains("<iq"))
        .await
        .expect("ping result");
}

async fn wait_for_event_message(
    client: &mut WsXmppClient,
    node: &str,
    dur: Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) => {
                if frame.contains("<message")
                    && frame.contains(NS_PUBSUB_EVENT)
                    && (frame.contains(&format!(r#"node='{node}'"#))
                        || frame.contains(&format!(r#"node='{node}'"#)))
                {
                    return Some(frame);
                }
            }
            Err(_) => return None,
        }
    }
}
