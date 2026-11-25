//! XEP-0433 channel search through the production WebSocket handler: only
//! public group chats are ever returned (XEP-0433 Glossary, "Channel": "A
//! public group chat hosted on a Group Chat Service").

use minidom::Element;
use sqlx::SqlitePool;
use waddle_ws_test_support::{TestServer, WsXmppClient};

const NS_SEARCH: &str = "urn:xmpp:channel-search:0:search";
const NS_ERROR: &str = "urn:xmpp:channel-search:0:error";
const NS_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

async fn seed_channel(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    channel_type: &str,
    members_only: bool,
    public_room: bool,
) {
    sqlx::query(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default, \
         pin_permission, members_only, public_room, federated, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 0, 0, 'anyone', ?, ?, 0, '2000-01-01T00:00:00+00:00', \
         '2000-01-01T00:00:00+00:00')",
    )
    .bind(id)
    .bind(name)
    .bind(format!("description of {name}"))
    .bind(channel_type)
    .bind(members_only)
    .bind(public_room)
    .execute(pool)
    .await
    .expect("seed channel");
}

async fn search(client: &mut WsXmppClient, id: &str, query: &str) -> Element {
    // The queries used here are plain text, needing no XML escaping.
    search_fields(
        client,
        id,
        &format!("<field var='q'><value>{query}</value></field>"),
    )
    .await
}

/// Submit the search-params form with the given extra `<field/>` XML.
async fn search_fields(client: &mut WsXmppClient, id: &str, fields: &str) -> Element {
    let wire = format!(
        "<iq xmlns='jabber:client' type='get' id='{id}' to='muc.localhost'>\
         <search xmlns='{NS_SEARCH}'><x xmlns='jabber:x:data' type='submit'>\
         <field var='FORM_TYPE' type='hidden'>\
         <value>urn:xmpp:channel-search:0:search-params</value></field>\
         {fields}</x></search></iq>"
    );
    client.send(&wire).await.expect("send search");
    let response = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("search response");
    response.parse::<Element>().expect("response XML")
}

/// The `address` of every result item.
fn addresses(response: &Element) -> Vec<String> {
    assert_eq!(response.attr("type"), Some("result"), "{response:?}");
    let search = response
        .get_child("result", NS_SEARCH)
        .or_else(|| response.get_child("search", NS_SEARCH))
        .unwrap_or_else(|| panic!("search payload missing: {response:?}"));
    search
        .children()
        .filter(|child| child.name() == "item")
        .map(|item| item.attr("address").expect("item address").to_owned())
        .collect()
}

#[tokio::test]
async fn websocket_xep0433_search_returns_only_public_rooms() {
    let directory = tempfile::tempdir().expect("database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("channel-search.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(&url, &[]);
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "admin",
        server.fixed_account_password(),
        "channel-search",
    )
    .await
    .expect("authenticated client");
    let pool = SqlitePool::connect(&url).await.expect("seed database");
    seed_channel(&pool, "pubroom", "Findable public", "text", false, true).await;
    seed_channel(&pool, "pubmembers", "Findable members", "text", true, true).await;
    seed_channel(&pool, "hidden", "Findable hidden", "text", false, false).await;
    seed_channel(
        &pool,
        "hiddenmembers",
        "Findable secret",
        "text",
        true,
        false,
    )
    .await;
    seed_channel(&pool, "groupdm", "Findable dm", "group-dm", true, true).await;

    let found = addresses(&search(&mut client, "s1", "Findable").await);
    assert!(
        found.iter().any(|a| a.starts_with("pubroom@")),
        "public room must be found: {found:?}"
    );
    // A public members-only room is still public (listed); membership
    // governs joining, not discovery, as in XEP-0045 muc_public.
    assert!(
        found.iter().any(|a| a.starts_with("pubmembers@")),
        "public members-only room must be found: {found:?}"
    );
    for hidden in ["hidden@", "hiddenmembers@", "groupdm@"] {
        assert!(
            !found.iter().any(|a| a.starts_with(hidden)),
            "{hidden} must never be listed: {found:?}"
        );
    }

    // Targeting a hidden room by exact name reveals nothing either.
    for query in [
        "Findable hidden",
        "Findable secret",
        "description of Findable hidden",
    ] {
        let found = addresses(&search(&mut client, "s2", query).await);
        assert!(
            found.iter().all(|a| !a.starts_with("hidden")),
            "query {query:?} leaked a hidden room: {found:?}"
        );
    }

    let _ = client.close().await;
}

async fn connect_with_rooms(
    rooms: &[(&str, &str, &str)],
) -> (tempfile::TempDir, TestServer, WsXmppClient) {
    let directory = tempfile::tempdir().expect("database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("channel-search.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(&url, &[]);
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "admin",
        server.fixed_account_password(),
        "channel-search",
    )
    .await
    .expect("authenticated client");
    let pool = SqlitePool::connect(&url).await.expect("seed database");
    for (id, name, description) in rooms {
        seed_channel(&pool, id, name, "text", false, true).await;
        sqlx::query("UPDATE channels SET description = ? WHERE id = ?")
            .bind(description)
            .bind(id)
            .execute(&pool)
            .await
            .expect("set description");
    }
    (directory, server, client)
}

fn collect_vars(element: &Element, vars: &mut Vec<String>) {
    if let Some(var) = element.attr("var") {
        vars.push(var.to_owned());
    }
    for child in element.children() {
        collect_vars(child, vars);
    }
}

/// The error `type`, defined condition and application condition of an
/// error reply.
fn error_shape(response: &Element) -> (String, String, Option<String>) {
    assert_eq!(response.attr("type"), Some("error"), "{response:?}");
    let error = response
        .children()
        .find(|child| child.name() == "error")
        .unwrap_or_else(|| panic!("error payload missing: {response:?}"));
    let defined = error
        .children()
        .find(|child| child.ns() == NS_STANZAS && child.name() != "text")
        .expect("defined condition")
        .name()
        .to_owned();
    let application = error
        .children()
        .find(|child| child.ns() == NS_ERROR)
        .map(|child| child.name().to_owned());
    (
        error.attr("type").expect("error type").to_owned(),
        defined,
        application,
    )
}

/// XEP-0433 "No Search Conditions": "it MUST reply with a <bad-request/>
/// error of type cancel. In addition, the <no-search-conditions/>
/// application defined condition MUST be included."
#[tokio::test]
async fn websocket_xep0433_form_without_q_is_no_search_conditions() {
    let (_dir, _server, mut client) = connect_with_rooms(&[("pub", "Public", "d")]).await;
    let response = search_fields(&mut client, "nq", "").await;
    assert_eq!(
        error_shape(&response),
        (
            "cancel".to_owned(),
            "bad-request".to_owned(),
            Some("no-search-conditions".to_owned())
        )
    );
    let _ = client.close().await;
}

/// XEP-0433 "Invalid Search Terms": "If the q field was supplied ... and did
/// not yield any term suitable for search, the Search Service MUST reply with
/// an <bad-request/> error and the <invalid-search-terms/> application defined
/// condition. The error type MUST be modify."
#[tokio::test]
async fn websocket_xep0433_blank_q_is_invalid_search_terms() {
    let (_dir, _server, mut client) = connect_with_rooms(&[("pub", "Public", "d")]).await;
    for (id, fields) in [
        ("bq1", "<field var='q'><value></value></field>"),
        ("bq2", "<field var='q'><value>   </value></field>"),
        ("bq3", "<field var='q'/>"),
    ] {
        let response = search_fields(&mut client, id, fields).await;
        assert_eq!(
            error_shape(&response),
            (
                "modify".to_owned(),
                "bad-request".to_owned(),
                Some("invalid-search-terms".to_owned())
            ),
            "{fields}"
        );
    }
    let _ = client.close().await;
}

/// XEP-0433 "Unsupported Sort Key": "MUST reply with <feature-not-implemented/>
/// and the <invalid-sort-key> application defined condition and a modify type".
#[tokio::test]
async fn websocket_xep0433_unsupported_sort_key_is_rejected() {
    let (_dir, _server, mut client) = connect_with_rooms(&[("pub", "Public", "d")]).await;
    for (id, key) in [
        ("k1", "{urn:example:order}bogus"),
        // Defined by the XEP but not offered: no occupant counts are kept.
        ("k2", "{urn:xmpp:channel-search:0:order}nusers"),
    ] {
        let response = search_fields(
            &mut client,
            id,
            &format!("<field var='q'><value>Public</value></field><field var='key'><value>{key}</value></field>"),
        )
        .await;
        assert_eq!(
            error_shape(&response),
            (
                "modify".to_owned(),
                "feature-not-implemented".to_owned(),
                Some("invalid-sort-key".to_owned())
            ),
            "{key}"
        );
    }
    let _ = client.close().await;
}

/// The search form template lists the supported fields with defaults, and
/// the toggles and sort key it offers are honoured.
#[tokio::test]
async fn websocket_xep0433_toggles_key_and_types_are_honoured() {
    let (_dir, _server, mut client) = connect_with_rooms(&[
        ("zeta", "Zeta Room", "talks about apples"),
        ("alpha", "Alpha Room", "talks about pears"),
        ("beta", "Apple Room", "talks about nothing"),
    ])
    .await;

    // Form template.
    client
        .send(&format!(
            "<iq xmlns='jabber:client' type='get' id='tpl' to='muc.localhost'><search xmlns='{NS_SEARCH}'/></iq>"
        ))
        .await
        .expect("send");
    let template = client
        .recv_matching(|frame| frame.contains("tpl"))
        .await
        .expect("template")
        .parse::<Element>()
        .expect("xml");
    let mut vars = Vec::new();
    collect_vars(&template, &mut vars);
    for var in [
        "q",
        "sinaddress",
        "sinname",
        "sindescription",
        "types",
        "key",
    ] {
        assert!(
            vars.iter().any(|v| v == var),
            "template lacks {var}: {vars:?}"
        );
    }

    // Default scope searches name and description, ordered by address.
    let found = addresses(&search(&mut client, "d1", "apple").await);
    let locals: Vec<&str> = found.iter().filter_map(|a| a.split('@').next()).collect();
    assert_eq!(locals, ["beta", "zeta"], "{found:?}");

    // Name only.
    let found = addresses(
        &search_fields(
            &mut client,
            "d2",
            "<field var='q'><value>apple</value></field>\
             <field var='sindescription'><value>0</value></field>",
        )
        .await,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("beta@"));

    // Description only.
    let found = addresses(
        &search_fields(
            &mut client,
            "d3",
            "<field var='q'><value>apple</value></field>\
             <field var='sinname'><value>false</value></field>",
        )
        .await,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("zeta@"));

    // Address only.
    let found = addresses(
        &search_fields(
            &mut client,
            "d4",
            "<field var='q'><value>alp</value></field>\
             <field var='sinname'><value>0</value></field>\
             <field var='sindescription'><value>0</value></field>",
        )
        .await,
    );
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("alpha@"));

    // A type filter that excludes XEP-0045 yields an empty result, not an error.
    let found = addresses(
        &search_fields(
            &mut client,
            "d5",
            "<field var='q'><value>Room</value></field>\
             <field var='types'><value>xep-0369</value></field>",
        )
        .await,
    );
    assert!(found.is_empty(), "{found:?}");
    let found = addresses(
        &search_fields(
            &mut client,
            "d6",
            "<field var='q'><value>Room</value></field>\
             <field var='types'><value>xep-0045</value></field>",
        )
        .await,
    );
    assert_eq!(found.len(), 3, "{found:?}");

    let _ = client.close().await;
}
