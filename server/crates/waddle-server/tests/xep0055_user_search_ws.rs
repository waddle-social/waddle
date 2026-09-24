//! XEP-0055 local directory search through the production WebSocket handler.

use minidom::Element;
use sqlx::SqlitePool;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use xmpp_parsers::{iq::Iq, stanza_error::DefinedCondition};

const NS_SEARCH: &str = "jabber:iq:search";

async fn search(client: &mut WsXmppClient, term: &str) -> Iq {
    let request = Iq::Set {
        from: None,
        to: Some("localhost".parse().expect("server JID")),
        id: "directory-search".into(),
        payload: Element::builder("query", NS_SEARCH)
            .append(Element::builder("nick", NS_SEARCH).append(term).build())
            .build(),
    };
    let mut wire = Vec::new();
    Element::from(request)
        .write_to(&mut wire)
        .expect("serialize request");
    client
        .send(&String::from_utf8(wire).expect("UTF-8"))
        .await
        .expect("send request");
    let response = client
        .recv_matching(|frame| frame.contains("directory-search"))
        .await
        .expect("search response");
    Iq::try_from(response.parse::<Element>().expect("response XML")).expect("response IQ")
}

fn result_jids(response: Iq) -> Vec<jid::BareJid> {
    let Iq::Result {
        payload: Some(query),
        ..
    } = response
    else {
        panic!("expected search result: {response:?}");
    };
    assert!(query.is("query", NS_SEARCH));
    query
        .children()
        .map(|item| {
            assert!(item.is("item", NS_SEARCH));
            assert!(item.children().all(|field| field.is("nick", NS_SEARCH)));
            item.attr("jid")
                .expect("canonical result JID")
                .parse()
                .expect("bare JID")
        })
        .collect()
}

async fn seed_oidc(pool: &SqlitePool, username: &str, localpart: &str) {
    sqlx::query("INSERT INTO users (jid, username, xmpp_localpart, display_name, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(format!("{localpart}@localhost"))
        .bind(username)
        .bind(localpart)
        .bind(username)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(pool).await.expect("seed OIDC account");
}

#[tokio::test]
async fn websocket_xep0055_searches_both_stores_with_canonical_unique_exact_results() {
    let directory = tempfile::tempdir().expect("database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("search.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(
        &url,
        &[
            ("native.member", "native-password-123"),
            ("alice+work", "plus-password-123"),
            ("shared.member", "shared-password-123"),
        ],
    );
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "admin",
        server.fixed_account_password(),
        "search",
    )
    .await
    .expect("authenticated client");
    let pool = SqlitePool::connect(&url).await.expect("seed database");
    seed_oidc(&pool, "visible.member", "immutable.member").await;
    seed_oidc(&pool, "shared.profile", "shared.member").await;
    // More than a page sorts before the exact account by username.
    for index in 0..55 {
        let name = format!("aaa{index:02}.target");
        seed_oidc(&pool, &name, &name).await;
    }
    seed_oidc(&pool, "target", "canonical.target").await;
    seed_oidc(&pool, "target.profile", "target").await;
    seed_oidc(&pool, "x", "x").await;
    sqlx::query("INSERT INTO native_users (username, domain, password_hash, salt, stored_key, server_key) SELECT 'remote.member', 'elsewhere.test', password_hash, salt, stored_key, server_key FROM native_users LIMIT 1")
        .execute(&pool).await.expect("foreign-domain account");

    let members = result_jids(search(&mut client, "member").await);
    assert_eq!(
        members.len(),
        3,
        "both stores, one row per JID, local domain only"
    );
    for expected in [
        "native.member@localhost",
        "immutable.member@localhost",
        "shared.member@localhost",
    ] {
        assert!(members.contains(&expected.parse().expect("expected JID")));
    }
    for (query, expected) in [
        ("visible.member", "immutable.member@localhost"),
        ("immutable.member@localhost", "immutable.member@localhost"),
        ("ALICE+WORK", "alice+work@localhost"),
        ("x", "x@localhost"),
    ] {
        assert_eq!(
            result_jids(search(&mut client, query).await),
            vec![expected.parse::<jid::BareJid>().expect("JID")]
        );
    }
    let targets = result_jids(search(&mut client, "target").await);
    assert_eq!(targets.len(), 50);
    assert_eq!(
        targets[0],
        "target@localhost"
            .parse::<jid::BareJid>()
            .expect("exact address")
    );
    assert_eq!(
        targets[1],
        "canonical.target@localhost"
            .parse::<jid::BareJid>()
            .expect("exact username")
    );
    for query in ["no-such-account", "%%", "__", "chat"] {
        assert!(result_jids(search(&mut client, query).await).is_empty());
    }
    seed_oidc(&pool, "chat", "chat").await;
    assert_eq!(
        result_jids(search(&mut client, "chat").await),
        vec!["chat@localhost"
            .parse::<jid::BareJid>()
            .expect("account JID")]
    );
    // A storage error must remain an IQ error, not an empty directory result.
    sqlx::query("ALTER TABLE users RENAME TO unavailable_users")
        .execute(&pool)
        .await
        .expect("fail lookup");
    let Iq::Error { error, .. } = search(&mut client, "native.member").await else {
        panic!("directory failure must be visible");
    };
    assert_eq!(
        error.defined_condition,
        DefinedCondition::InternalServerError
    );
    sqlx::query("ALTER TABLE unavailable_users RENAME TO users")
        .execute(&pool)
        .await
        .expect("restore lookup");
    client.close().await.expect("close connection");
    pool.close().await;
}
