//! Integration suite for the persistent channel ↔ space link.
//!
//! Today admin V2's `channels:list` accepts a `space_jid` filter but no
//! persistent link exists in the server, so the filter is a no-op. PR
//! #691 wires up the link projection. This suite drives the four
//! end-to-end behaviors over the production WebSocket transport:
//!
//! 1. `channels:create` with `space_jid=A` and `space_jid=B` records
//!    independent link rows.
//! 2. `channels:list space_jid=A` returns only the channels linked to A
//!    (not the channels linked to B, not unlinked channels).
//! 3. `channels:list` without `space_jid` returns the full set.
//! 4. `channels:delete <chA>` drops the link row, so the filter narrows
//!    further.
//! 5. `spaces:delete <spB>` cascades to destroy the linked channel and
//!    drop its link row.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA: &str = "jabber:x:data";

const NODE_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
const NODE_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";

const NODE_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
const NODE_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
const NODE_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, ADMIN, &password, resource)
        .await
        .expect("admin connect")
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
        .recv_matching(|frame| {
            frame.contains("<iq")
                && (frame.contains(&format!(r#"id="{id}""#))
                    || frame.contains(&format!(r#"id='{id}'"#)))
        })
        .await
        .expect("iq response")
}

fn submit_form(node: &str, extra: &str) -> String {
    format!(
        r#"<x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{node}</value></field>{extra}</x>"#
    )
}

fn text_field(var: &str, value: &str) -> String {
    format!(r#"<field var="{var}" type="text-single"><value>{value}</value></field>"#)
}

fn is_result(frame: &str) -> bool {
    frame.contains(r#"type="result""#) || frame.contains(r#"type='result'"#)
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

async fn create_space(admin: &mut WsXmppClient, name: &str, id: &str) -> String {
    let resp = send_command(
        admin,
        NODE_SPACES_CREATE,
        id,
        &submit_form(NODE_SPACES_CREATE, &text_field("name", name)),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected spaces:create result, got: {resp}"
    );
    extract_field(&resp, "space_jid").expect("space_jid in spaces:create response")
}

async fn create_channel_in_space(
    admin: &mut WsXmppClient,
    name: &str,
    space_jid: &str,
    id: &str,
) -> String {
    let extra = format!(
        "{}{}",
        text_field("name", name),
        text_field("space_jid", space_jid)
    );
    let resp = send_command(
        admin,
        NODE_CHANNELS_CREATE,
        id,
        &submit_form(NODE_CHANNELS_CREATE, &extra),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected channels:create result, got: {resp}"
    );
    extract_field(&resp, "channel_jid").expect("channel_jid in channels:create response")
}

async fn list_channels(admin: &mut WsXmppClient, space_jid: Option<&str>, id: &str) -> String {
    let extra = match space_jid {
        Some(jid) => text_field("space_jid", jid),
        None => String::new(),
    };
    let resp = send_command(
        admin,
        NODE_CHANNELS_LIST,
        id,
        &submit_form(NODE_CHANNELS_LIST, &extra),
    )
    .await;
    assert!(
        is_result(&resp),
        "expected channels:list result, got: {resp}"
    );
    resp
}

#[tokio::test]
async fn channels_list_space_jid_filter_narrows_results_and_survives_lifecycle() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut admin = admin_client(&server, "admin-csl-1").await;

    // Two spaces, two channels — one channel per space.
    let space_a = create_space(&mut admin, "AlphaSpace", "csl-mk-space-a").await;
    let space_b = create_space(&mut admin, "BetaSpace", "csl-mk-space-b").await;

    let channel_a =
        create_channel_in_space(&mut admin, "alpha-room", &space_a, "csl-mk-chan-a").await;
    let channel_b =
        create_channel_in_space(&mut admin, "beta-room", &space_b, "csl-mk-chan-b").await;

    // (2) `channels:list space_jid=A` returns only channel A.
    let only_a = list_channels(&mut admin, Some(&space_a), "csl-list-only-a").await;
    assert!(
        only_a.contains(&channel_a),
        "filter by space A should include channel A '{channel_a}', got: {only_a}"
    );
    assert!(
        !only_a.contains(&channel_b),
        "filter by space A should exclude channel B '{channel_b}', got: {only_a}"
    );

    // (3) `channels:list` without filter returns both channels.
    let unfiltered = list_channels(&mut admin, None, "csl-list-all").await;
    assert!(
        unfiltered.contains(&channel_a),
        "unfiltered list missing channel A '{channel_a}', got: {unfiltered}"
    );
    assert!(
        unfiltered.contains(&channel_b),
        "unfiltered list missing channel B '{channel_b}', got: {unfiltered}"
    );

    // (4) Delete channel A and confirm the filter narrows further.
    let del_a = send_command(
        &mut admin,
        NODE_CHANNELS_DELETE,
        "csl-del-a",
        &submit_form(
            NODE_CHANNELS_DELETE,
            &format!(
                "{}{}",
                text_field("channel_jid", &channel_a),
                text_field("confirm", "yes")
            ),
        ),
    )
    .await;
    assert!(
        is_result(&del_a),
        "expected channels:delete result, got: {del_a}"
    );

    let after_delete = list_channels(&mut admin, Some(&space_a), "csl-list-after-del-a").await;
    assert!(
        !after_delete.contains(&channel_a),
        "deleted channel A '{channel_a}' should not appear in space-A filter, got: {after_delete}"
    );
    assert!(
        !after_delete.contains(&channel_b),
        "channel B '{channel_b}' is in space B; space-A filter should still exclude it, got: {after_delete}"
    );

    // (5) Delete space B and confirm the cascade tore down channel B
    // and dropped its link row (so the space-B filter is empty).
    let del_b = send_command(
        &mut admin,
        NODE_SPACES_DELETE,
        "csl-del-space-b",
        &submit_form(
            NODE_SPACES_DELETE,
            &format!(
                "{}{}",
                text_field("space_jid", &space_b),
                text_field("confirm", "yes")
            ),
        ),
    )
    .await;
    assert!(
        is_result(&del_b),
        "expected spaces:delete result, got: {del_b}"
    );

    let after_space_delete =
        list_channels(&mut admin, Some(&space_b), "csl-list-after-del-space-b").await;
    assert!(
        !after_space_delete.contains(&channel_b),
        "cascade-deleted channel B '{channel_b}' should be gone from space-B filter, got: {after_space_delete}"
    );

    // And the unfiltered list should no longer include either channel.
    let final_unfiltered = list_channels(&mut admin, None, "csl-list-final").await;
    assert!(
        !final_unfiltered.contains(&channel_a),
        "deleted channel A '{channel_a}' should be absent from unfiltered list, got: {final_unfiltered}"
    );
    assert!(
        !final_unfiltered.contains(&channel_b),
        "cascade-deleted channel B '{channel_b}' should be absent from unfiltered list, got: {final_unfiltered}"
    );

    let _ = admin.close().await;
}
