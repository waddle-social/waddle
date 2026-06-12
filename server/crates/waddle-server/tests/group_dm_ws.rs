//! Integration suite for Waddle group-DM provisioning over XEP-0050.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";
const NODE_GROUP_DM_CREATE: &str = "urn:waddle:group-dm:create:0";
const FEATURE_GROUP_DM: &str = "urn:waddle:group-dm:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn frame_has_iq_id(frame: &str, id: &str) -> bool {
    frame.contains(&format!(r#"id='{id}'"#)) || frame.contains(&format!(r#"id="{id}""#))
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

async fn send_command(client: &mut WsXmppClient, node: &str, id: &str, form_xml: &str) -> String {
    let body = format!(
        r#"<command xmlns="{NS_COMMANDS}" node="{node}" action="execute">{form_xml}</command>"#
    );
    client
        .send(&format!(
            r#"<iq type="set" id="{id}" to="{DOMAIN}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("iq response")
}

async fn disco_info(client: &mut WsXmppClient, to: &str, id: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="{to}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send disco info");
    client
        .recv_matching(|frame| frame.contains("<iq") && frame_has_iq_id(frame, id))
        .await
        .expect("disco info response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
}

fn list_multi_field(var: &str, values: &[&str]) -> String {
    let values_xml = values
        .iter()
        .map(|value| format!("<value>{value}</value>"))
        .collect::<String>();
    format!(r#"<field var="{var}" type="list-multi">{values_xml}</field>"#)
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
        &submit_form(
            NODE_GROUP_DM_CREATE,
            &format!(
                "{}{}",
                text_field("name", "Alice, Mallory"),
                list_multi_field("member_jids", &["alice@localhost", "mallory@localhost"])
            ),
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
        &submit_form(
            NODE_GROUP_DM_CREATE,
            &format!(
                "{}{}",
                text_field("name", "Alice, Bob"),
                list_multi_field("member_jids", &["alice@localhost", "bob@localhost"])
            ),
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
