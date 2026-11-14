//! The identity profile through the production WebSocket server: SCRAM
//! login and resource binding with non-canonical input, stanza addresses
//! the frame parser refuses or canonicalizes, and directory search that
//! never folds a name onto another account.

use minidom::Element;
use sqlx::SqlitePool;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const DOMAIN: &str = "localhost";
const NS_CLIENT: &str = "jabber:client";
const NS_SEARCH: &str = "jabber:iq:search";
const ADA_PASSWORD: &str = "ada-password-123";

fn parse(frame: &str) -> Element {
    frame.parse().expect("response XML")
}

fn stanza_error(stanza: &Element) -> StanzaError {
    StanzaError::try_from(
        stanza
            .get_child("error", NS_CLIENT)
            .expect("stanza error")
            .clone(),
    )
    .expect("typed stanza error")
}

async fn round_trip(client: &mut WsXmppClient, frame: &str, id: &str) -> Element {
    client.send(frame).await.expect("send stanza");
    let marker = format!("id=\"{id}\"");
    let alternate = format!("id='{id}'");
    parse(
        &client
            .recv_matching(|response| response.contains(&marker) || response.contains(&alternate))
            .await
            .expect("correlated response"),
    )
}

#[tokio::test]
async fn scram_login_takes_the_username_exactly() {
    let server = TestServer::start_with_extra_accounts(&[("ada", ADA_PASSWORD)]);
    for username in ["ADA", "Ada", "ａｄａ"] {
        assert!(
            WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, ADA_PASSWORD, "web")
                .await
                .is_err(),
            "{username} must not sign in as ada"
        );
    }
    let client =
        WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, "ada", ADA_PASSWORD, "web")
            .await
            .expect("canonical login");
    assert_eq!(client.full_jid.as_deref(), Some("ada@localhost/web"));
    client.close().await.expect("close");
}

#[tokio::test]
async fn bind_refuses_a_resourcepart_preparation_would_change() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("connect");
    client
        .authenticate(DOMAIN, "admin", server.fixed_account_password())
        .await
        .expect("authenticate");
    // Fullwidth `ｗｅｂ` would bind as `web` under Resourceprep.
    let refused = round_trip(
        &mut client,
        r#"<iq type="set" id="bind-wide"><bind xmlns="urn:ietf:params:xml:ns:xmpp-bind"><resource>&#xFF57;&#xFF45;&#xFF42;</resource></bind></iq>"#,
        "bind-wide",
    )
    .await;
    assert_eq!(refused.attr("type"), Some("error"));
    let error = stanza_error(&refused);
    assert_eq!(error.type_, ErrorType::Modify);
    assert_eq!(error.defined_condition, DefinedCondition::BadRequest);

    assert_eq!(
        client.bind("web").await.expect("canonical resource"),
        "admin@localhost/web"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn stanzas_with_addresses_outside_the_profile_get_jid_malformed() {
    let server = TestServer::start();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "web",
    )
    .await
    .expect("authenticated client");

    for (frame, id) in [
        (
            r#"<iq type="get" id="ping-case" to="Admin@localhost"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "ping-case",
        ),
        (
            r#"<iq type="get" id="ping-domain" to="LOCALHOST"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "ping-domain",
        ),
        (
            r#"<iq type="get" id="ping-ref" to="stra&#xDF;e@localhost"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "ping-ref",
        ),
        (
            r#"<iq type="set" id="roster-case"><query xmlns="jabber:iq:roster"><item jid="Bob@localhost"/></query></iq>"#,
            "roster-case",
        ),
    ] {
        let response = round_trip(&mut client, frame, id).await;
        let Iq::Error {
            error, to, from, ..
        } = Iq::try_from(response).expect("typed IQ")
        else {
            panic!("{id}: expected an IQ error");
        };
        assert_eq!(error.type_, ErrorType::Modify, "{id}");
        assert_eq!(
            error.defined_condition,
            DefinedCondition::JidMalformed,
            "{id}"
        );
        // The refused addresses are not echoed.
        assert_eq!((to, from), (None, None), "{id}");
    }

    let message = round_trip(
        &mut client,
        r#"<message id="msg-case" to="Bob@localhost" type="chat"><body>hi</body></message>"#,
        "msg-case",
    )
    .await;
    assert!(message.is("message", NS_CLIENT));
    assert_eq!(message.attr("type"), Some("error"));
    assert_eq!(
        stanza_error(&message).defined_condition,
        DefinedCondition::JidMalformed
    );

    // A trailing dot is canonicalized, not refused: the server answers its
    // own ping.
    let pong = round_trip(
        &mut client,
        r#"<iq type="get" id="ping-dot" to="localhost."><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "ping-dot",
    )
    .await;
    assert!(matches!(
        Iq::try_from(pong).expect("typed IQ"),
        Iq::Result { .. }
    ));
    client.close().await.expect("close");
}

async fn search(client: &mut WsXmppClient, id: &str, term: &str) -> Vec<jid::BareJid> {
    let request = Iq::Set {
        from: None,
        to: Some(DOMAIN.parse().expect("server JID")),
        id: id.into(),
        payload: Element::builder("query", NS_SEARCH)
            .append(Element::builder("nick", NS_SEARCH).append(term).build())
            .build(),
    };
    let response = round_trip(client, &String::from(&Element::from(request)), id).await;
    let Iq::Result {
        payload: Some(query),
        ..
    } = Iq::try_from(response).expect("typed IQ")
    else {
        panic!("{id}: expected a search result");
    };
    query
        .children()
        .map(|item| {
            item.attr("jid")
                .expect("result JID")
                .parse()
                .expect("bare JID")
        })
        .collect()
}

async fn seed_oidc(pool: &SqlitePool, localpart: &str) {
    sqlx::query("INSERT INTO users (jid, username, xmpp_localpart, display_name, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(format!("{localpart}@localhost"))
        .bind(localpart)
        .bind(localpart)
        .bind(localpart)
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(pool)
        .await
        .expect("seed OIDC account");
}

#[tokio::test]
async fn directory_search_never_presents_a_folded_address() {
    let directory = tempfile::tempdir().expect("database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("identity.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_accounts(&url, &[]);
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "search",
    )
    .await
    .expect("authenticated client");
    let pool = SqlitePool::connect(&url).await.expect("seed database");
    // `Bob` was written before the profile; preparation would fold it onto
    // `bob@localhost`, an address it does not hold.
    seed_oidc(&pool, "Bob").await;
    seed_oidc(&pool, "bob.member").await;

    let bob = "bob.member@localhost"
        .parse::<jid::BareJid>()
        .expect("canonical JID");
    assert_eq!(
        search(&mut client, "s-text", "bob").await,
        vec![bob.clone()]
    );
    assert_eq!(
        search(&mut client, "s-exact", "bob.member@localhost").await,
        vec![bob]
    );
    // A non-canonical address stays free text and matches no name.
    assert!(search(&mut client, "s-folded", "Bob.Member@LOCALHOST")
        .await
        .is_empty());
    client.close().await.expect("close");
    pool.close().await;
}
