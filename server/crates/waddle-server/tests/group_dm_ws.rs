//! Integration suite for Waddle group-DM provisioning over XEP-0050.

mod ws_common;

use std::time::Duration;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NODE_GROUP_DM_CREATE: &str = "urn:waddle:group-dm:create:0";
const FEATURE_GROUP_DM: &str = "urn:waddle:group-dm:0";
const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("xmpp_parsers serializes UTF-8")
}

async fn user_client(
    server: &TestServer,
    username: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, resource)
        .await
        .expect("user connect")
}

async fn send_command(client: &mut WsXmppClient, node: &str, id: &str, form: Element) -> String {
    let command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(form)
        .build();
    let iq = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), DOMAIN)
        .append(command)
        .build();
    client.send(&element_to_xml(iq)).await.expect("send iq");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("iq response")
}

async fn disco_info(client: &mut WsXmppClient, to: &str, id: &str) -> String {
    let iq = Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(Element::builder("query", "http://jabber.org/protocol/disco#info").build())
        .build();
    client
        .send(&element_to_xml(iq))
        .await
        .expect("send disco info");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("disco info response")
}

async fn create_group_dm(
    client: &mut WsXmppClient,
    id: &str,
    name: &str,
    members: &[&str],
) -> String {
    let resp = send_command(
        client,
        NODE_GROUP_DM_CREATE,
        id,
        submit_form(
            NODE_GROUP_DM_CREATE,
            vec![
                text_field("name", name),
                list_multi_field("member_jids", members),
            ],
        ),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected group-DM create result, got: {resp}"
    );
    extract_field(&resp, "room_jid").expect("room_jid in create response")
}

async fn join_room(client: &mut WsXmppClient, room_jid: &str, nick: &str) {
    let join = format!("<presence xmlns='jabber:client' to='{room_jid}/{nick}'/>");
    client.send(&join).await.expect("send room join");
    client
        .recv_matching(|frame| {
            frame.contains("<presence")
                && (frame.contains(&format!("from='{room_jid}/{nick}'"))
                    || frame.contains(&format!("from=\"{room_jid}/{nick}\"")))
        })
        .await
        .expect("self join presence");
}

async fn send_groupchat(client: &mut WsXmppClient, room_jid: &str, id: &str, body: &str) {
    let message = format!(
        "<message xmlns='jabber:client' type='groupchat' to='{room_jid}' id='{id}'>\
            <body>{body}</body>\
         </message>"
    );
    client.send(&message).await.expect("send groupchat");
    client
        .recv_matching(|frame| frame.contains(id) && frame.contains(body))
        .await
        .expect("groupchat echo");
}

async fn query_room_mam(client: &mut WsXmppClient, room_jid: &str, id: &str) -> Vec<String> {
    let query = format!(
        "<iq xmlns='jabber:client' type='set' to='{room_jid}' id='{id}'>\
            <query xmlns='urn:xmpp:mam:2' queryid='{id}-q'>\
                <x xmlns='jabber:x:data' type='submit'>\
                    <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                </x>\
            </query>\
         </iq>"
    );
    client.send(&query).await.expect("send MAM query");
    client
        .recv_until(|frame| frame_has_iq_id(frame, id))
        .await
        .expect("MAM responses")
}

async fn assert_no_frame_matching_for<F>(
    client: &mut WsXmppClient,
    duration: Duration,
    predicate: F,
    description: &str,
) where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match client.recv_timeout(remaining).await {
            Ok(frame) if predicate(&frame) => {
                panic!("{description}: matched unexpected frame: {frame}");
            }
            Ok(_) => continue,
            Err(error) if error == "Timeout waiting for message" => return,
            Err(error) => panic!("{description}: receive failed: {error}"),
        }
    }
}

fn submit_form(node: &str, fields: Vec<Element>) -> Element {
    let mut form = Element::builder("x", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_DATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(Element::builder("value", NS_DATA).append(node).build())
                .build(),
        );
    for field in fields {
        form = form.append(field);
    }
    form.build()
}

fn text_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "text-single")
        .append(Element::builder("value", NS_DATA).append(value).build())
        .build()
}

fn list_multi_field(var: &str, values: &[&str]) -> Element {
    let mut field = Element::builder("field", NS_DATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "list-multi");
    for value in values {
        field = field.append(Element::builder("value", NS_DATA).append(*value).build());
    }
    field.build()
}

fn extract_field(frame: &str, var: &str) -> Option<String> {
    let marker_dq = format!(r#"var="{var}""#);
    let marker_sq = format!(r#"var='{var}'"#);
    let idx = frame.find(&marker_dq).or_else(|| frame.find(&marker_sq))?;
    let after = &frame[idx..];
    let open = after.find("<value>")?;
    let inner = &after[open + "<value>".len()..];
    let close = inner.find("</value>")?;
    Some(inner[..close].to_string())
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type='result'"#) || frame.contains(r#"type="result""#)
}

fn is_error(frame: &str) -> bool {
    frame.contains(r#"type='error'"#) || frame.contains(r#"type="error""#)
}

#[tokio::test]
async fn group_dm_create_provisions_hidden_members_only_room_with_disco_feature() {
    let _serial = TEST_SERIAL.lock().await;
    let db_dir = tempfile::tempdir().expect("temp db dir");
    let db_path = db_dir.path().join("group-dm.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("alice", "alice-pass"), ("bob", "bob-pass")],
    );
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-create-1").await;

    let missing_member_resp = send_command(
        &mut alice,
        NODE_GROUP_DM_CREATE,
        "group-dm-create-missing-member",
        submit_form(
            NODE_GROUP_DM_CREATE,
            vec![
                text_field("name", "Alice, Mallory"),
                list_multi_field("member_jids", &["alice@localhost", "mallory@localhost"]),
            ],
        ),
    )
    .await;
    assert!(
        is_error(&missing_member_resp),
        "expected nonexistent member rejection, got: {missing_member_resp}"
    );
    assert!(
        missing_member_resp.contains("item-not-found"),
        "nonexistent member should use item-not-found: {missing_member_resp}"
    );

    let resp = send_command(
        &mut alice,
        NODE_GROUP_DM_CREATE,
        "group-dm-create",
        submit_form(
            NODE_GROUP_DM_CREATE,
            vec![
                text_field("name", "Rock & Roll"),
                list_multi_field("member_jids", &["alice@localhost", "bob@localhost"]),
            ],
        ),
    )
    .await;

    assert!(is_result(&resp), "expected result, got: {resp}");
    let room_jid = extract_field(&resp, "room_jid").expect("room_jid in response");
    assert!(
        room_jid.ends_with("@muc.localhost"),
        "expected managed MUC room, got: {room_jid}"
    );
    assert_eq!(extract_field(&resp, "is_public").as_deref(), Some("0"));
    assert_eq!(extract_field(&resp, "members_only").as_deref(), Some("1"));
    assert_eq!(extract_field(&resp, "persistent").as_deref(), Some("1"));

    let disco = disco_info(&mut alice, &room_jid, "group-dm-disco").await;
    assert!(
        disco.contains(FEATURE_GROUP_DM),
        "group DM room disco#info missing {FEATURE_GROUP_DM}: {disco}"
    );
    assert!(
        disco.contains("muc_membersonly"),
        "group DM room must be members-only: {disco}"
    );
    assert!(
        !disco.contains("muc_public"),
        "group DM room must stay hidden from public room discovery: {disco}"
    );

    let _ = alice.close().await;
    drop(server);

    let server = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("alice", "alice-pass"), ("bob", "bob-pass")],
    );
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-restart-2").await;
    let disco = disco_info(&mut alice, &room_jid, "group-dm-disco-after-restart").await;
    assert!(
        disco.contains(FEATURE_GROUP_DM),
        "restarted server lost group DM disco feature: {disco}"
    );
    assert!(
        disco.contains("muc_membersonly"),
        "restarted group DM room must stay members-only: {disco}"
    );
    let _ = alice.close().await;
}

#[tokio::test]
async fn group_dm_member_invites_new_member_with_history_access_extension() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-add-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "group-dm-add-bob").await;
    let mut charlie = user_client(&server, "charlie", "charlie-pass", "group-dm-add-charlie").await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-add-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;
    join_room(&mut alice, &room_jid, "alice").await;
    send_groupchat(
        &mut alice,
        &room_jid,
        "before-full-add",
        "pre-add visible to full",
    )
    .await;

    let invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='add-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite from='mallory@localhost' to='charlie@localhost'>\
                    <history-access xmlns='{FEATURE_GROUP_DM}' mode='full'/>\
                </invite>\
            </x>\
         </message>"
    );
    bob.send(&invite).await.expect("send mediated invite");

    let delivered = charlie
        .recv_matching(|frame| {
            frame.contains("add-charlie")
                && frame.contains(&room_jid)
                && frame.contains("charlie@localhost")
        })
        .await
        .expect("charlie receives mediated invite");
    assert!(
        delivered.contains(FEATURE_GROUP_DM) && delivered.contains("mode='full'"),
        "invite must carry Waddle group-DM history-access extension: {delivered}"
    );
    assert!(
        delivered.contains("from='bob@localhost'") || delivered.contains("from=\"bob@localhost\""),
        "server must stamp the actual inviter on mediated invite: {delivered}"
    );
    assert!(
        !delivered.contains("mallory@localhost"),
        "server must not reflect client-spoofed muc#user invite fields: {delivered}"
    );

    let join = format!("<presence xmlns='jabber:client' to='{room_jid}/charlie'/>");
    charlie.send(&join).await.expect("charlie joins added room");
    let joined = charlie
        .recv_matching(|frame| {
            frame.contains("<presence")
                && frame.contains(&format!("from='{room_jid}/charlie'"))
                && frame.contains("affiliation='member'")
        })
        .await
        .expect("charlie member join presence");
    assert!(
        !is_error(&joined),
        "new invitee should be auto-added and allowed to join members-only room: {joined}"
    );

    let mam = query_room_mam(&mut charlie, &room_jid, "mam-full-after-add").await;
    assert!(
        mam.iter()
            .any(|frame| frame.contains("pre-add visible to full")),
        "share-all add should expose pre-add room history via server MAM: {mam:?}"
    );

    let reinvite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='readd-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@localhost'/>\
            </x>\
         </message>"
    );
    bob.send(&reinvite)
        .await
        .expect("send duplicate mediated invite");
    let duplicate_rejection = bob
        .recv_matching(|frame| frame.contains("readd-charlie") && frame.contains("conflict"))
        .await
        .expect("duplicate invite rejection");
    assert!(
        is_error(&duplicate_rejection),
        "duplicate invite must be rejected: {duplicate_rejection}"
    );
    assert_no_frame_matching_for(
        &mut charlie,
        Duration::from_millis(300),
        |frame| frame.contains("readd-charlie"),
        "duplicate invite must not be delivered to an existing member",
    )
    .await;

    let mam = query_room_mam(&mut charlie, &room_jid, "mam-full-after-readd").await;
    assert!(
        mam.iter()
            .any(|frame| frame.contains("pre-add visible to full")),
        "duplicate invite must not overwrite an existing full-history boundary: {mam:?}"
    );
}

#[tokio::test]
async fn group_dm_default_add_hides_pre_add_history_from_mam() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-history-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "group-dm-history-bob").await;
    let mut charlie = user_client(
        &server,
        "charlie",
        "charlie-pass",
        "group-dm-history-charlie",
    )
    .await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-history-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;
    join_room(&mut alice, &room_jid, "alice").await;
    send_groupchat(
        &mut alice,
        &room_jid,
        "before-default-add",
        "pre-add hidden by default",
    )
    .await;

    let foreign_invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='foreign-add-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@example.org'/>\
            </x>\
         </message>"
    );
    bob.send(&foreign_invite)
        .await
        .expect("send invalid-domain mediated invite");
    let foreign_rejection = bob
        .recv_matching(|frame| {
            frame.contains("foreign-add-charlie") && frame.contains("bad-request")
        })
        .await
        .expect("invalid-domain invite rejection");
    assert!(
        is_error(&foreign_rejection) && !foreign_rejection.contains("item-not-found"),
        "invitee validation must preserve typed stanza error: {foreign_rejection}"
    );

    let missing_invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='missing-add-mallory'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='mallory@localhost'/>\
            </x>\
         </message>"
    );
    bob.send(&missing_invite)
        .await
        .expect("send missing invitee mediated invite");
    let missing_rejection = bob
        .recv_matching(|frame| {
            frame.contains("missing-add-mallory") && frame.contains("item-not-found")
        })
        .await
        .expect("missing invitee rejection");
    assert!(
        is_error(&missing_rejection) && !missing_rejection.contains("bad-request"),
        "missing local invitee must preserve item-not-found stanza error: {missing_rejection}"
    );

    let invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='default-add-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@localhost'/>\
            </x>\
         </message>"
    );
    bob.send(&invite)
        .await
        .expect("send default mediated invite");
    let delivered = charlie
        .recv_matching(|frame| frame.contains("default-add-charlie"))
        .await
        .expect("charlie receives default invite");
    assert!(
        delivered.contains("mode='from-join'") || delivered.contains("mode=\"from-join\""),
        "default invite must stamp the effective restricted access mode: {delivered}"
    );
    assert!(
        !delivered.contains("mode='full'") && !delivered.contains("mode=\"full\""),
        "default invite must not grant full-history payload: {delivered}"
    );

    let mam = query_room_mam(&mut charlie, &room_jid, "mam-default-after-add").await;
    assert!(
        !mam.iter()
            .any(|frame| frame.contains("pre-add hidden by default")),
        "default add must hide pre-add room history server-side: {mam:?}"
    );
}

#[tokio::test]
async fn group_dm_restricted_member_cannot_grant_full_history() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
        ("dave", "dave-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-escalate-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "group-dm-escalate-bob").await;
    let mut charlie = user_client(
        &server,
        "charlie",
        "charlie-pass",
        "group-dm-escalate-charlie",
    )
    .await;
    let mut dave = user_client(&server, "dave", "dave-pass", "group-dm-escalate-dave").await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-escalate-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;
    join_room(&mut alice, &room_jid, "alice").await;
    send_groupchat(
        &mut alice,
        &room_jid,
        "before-charlie-add",
        "hidden from restricted chain",
    )
    .await;

    let add_charlie = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='default-add-charlie-for-escalation'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@localhost'/>\
            </x>\
         </message>"
    );
    bob.send(&add_charlie)
        .await
        .expect("send default mediated invite");
    charlie
        .recv_matching(|frame| frame.contains("default-add-charlie-for-escalation"))
        .await
        .expect("charlie receives default invite");

    let add_dave = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='restricted-full-add-dave'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='dave@localhost'>\
                    <history-access xmlns='{FEATURE_GROUP_DM}' mode='full'/>\
                </invite>\
            </x>\
         </message>"
    );
    charlie
        .send(&add_dave)
        .await
        .expect("restricted member sends full invite");
    let delivered = dave
        .recv_matching(|frame| frame.contains("restricted-full-add-dave"))
        .await
        .expect("dave receives mediated invite");
    assert!(
        delivered.contains("mode='from-join'") || delivered.contains("mode=\"from-join\""),
        "server must stamp the effective restricted access mode: {delivered}"
    );
    assert!(
        !delivered.contains("mode='full'") && !delivered.contains("mode=\"full\""),
        "restricted inviter must not grant full-history payload: {delivered}"
    );

    let mam = query_room_mam(&mut dave, &room_jid, "mam-restricted-chain").await;
    assert!(
        !mam.iter()
            .any(|frame| frame.contains("hidden from restricted chain")),
        "restricted inviter must not escalate MAM history access: {mam:?}"
    );
}

#[tokio::test]
async fn group_dm_invite_to_offline_member_is_queued_for_next_presence() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-offline-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "group-dm-offline-bob").await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-offline-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;

    let invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='offline-add-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@localhost'>\
                    <history-access xmlns='{FEATURE_GROUP_DM}' mode='full'/>\
                </invite>\
            </x>\
         </message>"
    );
    bob.send(&invite)
        .await
        .expect("send offline mediated invite");

    let mut charlie = user_client(
        &server,
        "charlie",
        "charlie-pass",
        "group-dm-offline-charlie",
    )
    .await;
    charlie
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send initial presence");
    let delivered = charlie
        .recv_matching(|frame| frame.contains("offline-add-charlie") && frame.contains(&room_jid))
        .await
        .expect("queued mediated invite flushes when invitee comes online");
    assert!(
        delivered.contains("from='bob@localhost'") || delivered.contains("from=\"bob@localhost\""),
        "queued invite must preserve trusted inviter stamp: {delivered}"
    );
    assert!(
        delivered.contains(FEATURE_GROUP_DM) && delivered.contains("mode='full'"),
        "queued invite must preserve history-access extension: {delivered}"
    );
}

#[tokio::test]
async fn group_dm_mam_rejects_non_member() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-deny-alice").await;
    let mut charlie =
        user_client(&server, "charlie", "charlie-pass", "group-dm-deny-charlie").await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-deny-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;

    let query = format!(
        "<iq xmlns='jabber:client' type='set' to='{room_jid}' id='mam-deny-non-member'>\
            <query xmlns='urn:xmpp:mam:2' queryid='mam-deny-non-member-q'>\
                <x xmlns='jabber:x:data' type='submit'>\
                    <field var='FORM_TYPE' type='hidden'><value>urn:xmpp:mam:2</value></field>\
                </x>\
            </query>\
         </iq>"
    );
    charlie.send(&query).await.expect("send denied MAM query");
    let denied = charlie
        .recv_matching(|frame| frame_has_iq_id(frame, "mam-deny-non-member"))
        .await
        .expect("MAM denial");
    assert!(
        is_error(&denied) && denied.contains("forbidden"),
        "non-member MAM query must be forbidden: {denied}"
    );
}

#[tokio::test]
async fn group_dm_invite_from_blocked_member_is_suppressed() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", "alice-pass"),
        ("bob", "bob-pass"),
        ("charlie", "charlie-pass"),
    ]);
    let mut alice = user_client(&server, "alice", "alice-pass", "group-dm-block-alice").await;
    let mut bob = user_client(&server, "bob", "bob-pass", "group-dm-block-bob").await;
    let mut charlie =
        user_client(&server, "charlie", "charlie-pass", "group-dm-block-charlie").await;

    let room_jid = create_group_dm(
        &mut alice,
        "group-dm-block-create",
        "Alice, Bob",
        &["alice@localhost", "bob@localhost"],
    )
    .await;

    charlie
        .send(
            "<iq xmlns='jabber:client' type='set' id='charlie-blocks-bob'>\
                <block xmlns='urn:xmpp:blocking'>\
                    <item jid='bob@localhost'/>\
                </block>\
             </iq>",
        )
        .await
        .expect("charlie blocks bob");
    let block_result = charlie
        .recv_matching(|frame| frame_has_iq_id(frame, "charlie-blocks-bob"))
        .await
        .expect("block result");
    assert!(is_result(&block_result), "block failed: {block_result}");

    let invite = format!(
        "<message xmlns='jabber:client' type='normal' to='{room_jid}' id='blocked-add-charlie'>\
            <x xmlns='{NS_MUC_USER}'>\
                <invite to='charlie@localhost'>\
                    <history-access xmlns='{FEATURE_GROUP_DM}' mode='full'/>\
                </invite>\
            </x>\
         </message>"
    );
    bob.send(&invite)
        .await
        .expect("send blocked mediated invite");

    assert_no_frame_matching_for(
        &mut charlie,
        Duration::from_millis(300),
        |frame| frame.contains("blocked-add-charlie"),
        "blocked inviter's mediated invite must not reach invitee",
    )
    .await;
}
