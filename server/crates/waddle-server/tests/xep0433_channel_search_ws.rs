//! XEP-0433 channel search through the production WebSocket handler: only
//! public group chats are ever returned (XEP-0433 Glossary, "Channel": "A
//! public group chat hosted on a Group Chat Service").

use minidom::Element;
use sqlx::SqlitePool;
use waddle_ws_test_support::{TestServer, WsXmppClient};

const NS_SEARCH: &str = "urn:xmpp:channel-search:0:search";

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
    let wire = format!(
        "<iq xmlns='jabber:client' type='get' id='{id}' to='muc.localhost'>\
         <search xmlns='{NS_SEARCH}'><x xmlns='jabber:x:data' type='submit'>\
         <field var='FORM_TYPE' type='hidden'>\
         <value>urn:xmpp:channel-search:0:search-params</value></field>\
         <field var='q'><value>{query}</value></field></x></search></iq>"
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
