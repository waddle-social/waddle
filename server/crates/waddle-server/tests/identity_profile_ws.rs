//! The identity profile through the production WebSocket server: SCRAM
//! login and resource binding with non-canonical input, stanza addresses
//! the frame parser refuses or canonicalizes, XEP-0402 bookmark item ids,
//! and directory search that never folds a name onto another account.

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
        (
            r#"<iq type="set" id="command-case" to="localhost"><command xmlns="http://jabber.org/protocol/commands" node="group-dm:create" action="execute"><x xmlns="jabber:x:data" type="submit"><field var="name" type="text-single"><value>pair</value></field><field var="member_jids" type="jid-multi"><value>admin@localhost</value><value>Bob@localhost</value></field></x></command></iq>"#,
            "command-case",
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

    // A mediated invitation names the account it grants membership to, so
    // its `to` is held to the profile as well.
    let invite = round_trip(
        &mut client,
        r#"<message id="invite-case" to="room@muc.localhost"><x xmlns="http://jabber.org/protocol/muc#user"><invite to="Bob@localhost"/></x></message>"#,
        "invite-case",
    )
    .await;
    assert_eq!(invite.attr("type"), Some("error"));
    assert_eq!(
        stanza_error(&invite).defined_condition,
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

/// XEP-0402 bookmark item ids are room JIDs stored and matched as written,
/// so a spelling the `jid` parser would fold onto another room is refused on
/// publish and on retract instead of keying a second bookmark for the room.
#[tokio::test]
async fn pep_bookmark_item_ids_must_be_canonical() {
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

    let published = round_trip(
        &mut client,
        r#"<iq type="set" id="bookmark-canonical"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="urn:xmpp:bookmarks:1"><item id="room@muc.localhost"><conference xmlns="urn:xmpp:bookmarks:1" name="Room"/></item></publish></pubsub></iq>"#,
        "bookmark-canonical",
    )
    .await;
    assert!(
        matches!(
            Iq::try_from(published).expect("typed IQ"),
            Iq::Result { .. }
        ),
        "a canonical room item id is accepted"
    );

    for (frame, id) in [
        (
            r#"<iq type="set" id="bookmark-publish-case"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="urn:xmpp:bookmarks:1"><item id="Room@muc.localhost"><conference xmlns="urn:xmpp:bookmarks:1" name="Room"/></item></publish></pubsub></iq>"#,
            "bookmark-publish-case",
        ),
        (
            r#"<iq type="set" id="bookmark-publish-dot"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="urn:xmpp:bookmarks:1"><item id="room@muc.localhost."><conference xmlns="urn:xmpp:bookmarks:1" name="Room"/></item></publish></pubsub></iq>"#,
            "bookmark-publish-dot",
        ),
        (
            r#"<iq type="set" id="bookmark-retract-case"><pubsub xmlns="http://jabber.org/protocol/pubsub"><retract node="urn:xmpp:bookmarks:1"><item id="Room@muc.localhost"/></retract></pubsub></iq>"#,
            "bookmark-retract-case",
        ),
    ] {
        let response = round_trip(&mut client, frame, id).await;
        let Iq::Error { error, .. } = Iq::try_from(response).expect("typed IQ") else {
            panic!("{id}: expected an IQ error");
        };
        assert_eq!(error.type_, ErrorType::Modify, "{id}");
        assert_eq!(
            error.defined_condition,
            DefinedCondition::BadRequest,
            "{id}"
        );
    }

    // Only the canonical bookmark is stored.
    let items = round_trip(
        &mut client,
        r#"<iq type="get" id="bookmark-items"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:xmpp:bookmarks:1"/></pubsub></iq>"#,
        "bookmark-items",
    )
    .await;
    let ids: Vec<String> = items
        .get_child("pubsub", "http://jabber.org/protocol/pubsub")
        .and_then(|pubsub| pubsub.get_child("items", "http://jabber.org/protocol/pubsub"))
        .map(|list| {
            list.children()
                .filter_map(|item| item.attr("id").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ids, vec!["room@muc.localhost".to_owned()]);
    client.close().await.expect("close");
}

fn bookmarks_payload(mode: &str) -> Element {
    Element::builder("conference", "urn:xmpp:bookmarks:1")
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Room")
        .append(
            Element::builder("extensions", "urn:xmpp:bookmarks:1")
                .append(
                    Element::builder("notify", "urn:xmpp:notification-settings:1")
                        .append(Element::builder(mode, "urn:xmpp:notification-settings:1").build())
                        .build(),
                )
                .build(),
        )
        .build()
}

fn bookmarks_iq(id: &str, operation: &str, item_id: &str, payload: Option<Element>) -> String {
    let mut item = Element::builder("item", "http://jabber.org/protocol/pubsub")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id);
    if let Some(payload) = payload {
        item = item.append(payload);
    }
    let request = Iq::Set {
        from: None,
        to: None,
        id: id.into(),
        payload: Element::builder("pubsub", "http://jabber.org/protocol/pubsub")
            .append(
                Element::builder(operation, "http://jabber.org/protocol/pubsub")
                    .attr(
                        minidom::rxml::xml_ncname!("node").to_owned(),
                        "urn:xmpp:bookmarks:1",
                    )
                    .append(item.build())
                    .build(),
            )
            .build(),
    };
    String::from(&Element::from(request))
}

/// Store a bookmark under `item_id` exactly as written, bypassing publish.
async fn seed_bookmark(pool: &SqlitePool, item_id: &str, mode: &str) {
    sqlx::query(
        "INSERT INTO pubsub_items \
         (owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms) \
         VALUES ('admin@localhost', 'urn:xmpp:bookmarks:1', ?, ?, 'admin@localhost', 0)",
    )
    .bind(item_id)
    .bind(String::from(&bookmarks_payload(mode)))
    .execute(pool)
    .await
    .expect("seed bookmark item");
}

/// Write the notification-settings row a publish of `mode` for
/// `conversation` writes.
async fn seed_projection(pool: &SqlitePool, conversation: &str, mode: &str) {
    sqlx::query(
        "INSERT OR REPLACE INTO notification_settings_projection \
         (owner_bare_jid, conversation_jid, conversation_kind, mode, rich_payload_opt_in, \
          source_version, updated_at_ms, source_node, source_item_id) \
         VALUES ('admin@localhost', ?, 'private_group', ?, 0, 1000000, 0, \
                 'urn:xmpp:bookmarks:1', ?)",
    )
    .bind(conversation)
    .bind(mode)
    .bind(conversation)
    .execute(pool)
    .await
    .expect("seed projection row");
}

async fn projection_mode(pool: &SqlitePool, conversation: &str) -> Option<String> {
    sqlx::query_scalar(
        "SELECT mode FROM notification_settings_projection \
         WHERE owner_bare_jid = 'admin@localhost' AND conversation_jid = ?",
    )
    .bind(conversation)
    .fetch_optional(pool)
    .await
    .expect("read projection row")
}

async fn bookmark_ids(client: &mut WsXmppClient, id: &str) -> Vec<String> {
    let items = round_trip(
        client,
        &format!(
            r#"<iq type="get" id="{id}"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="urn:xmpp:bookmarks:1"/></pubsub></iq>"#
        ),
        id,
    )
    .await;
    items
        .get_child("pubsub", "http://jabber.org/protocol/pubsub")
        .and_then(|pubsub| pubsub.get_child("items", "http://jabber.org/protocol/pubsub"))
        .map(|list| {
            list.children()
                .filter_map(|item| item.attr("id").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The store can hold XEP-0402 bookmarks under an id that is not canonical,
/// written as given by earlier releases. Their owner removes one by
/// retracting that exact id, which clears the notification setting it fed
/// unless a bookmark under the canonical id still carries one; then that
/// bookmark's setting applies again.
#[tokio::test]
async fn pep_bookmark_retract_removes_a_stored_non_canonical_bookmark() {
    let directory = tempfile::tempdir().expect("database directory");
    let url = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("bookmarks.sqlite3").display()
    );
    let server = TestServer::start_persistent_with_extra_envs(
        &url,
        &[],
        &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &url)],
    );
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "bookmarks",
    )
    .await
    .expect("authenticated client");
    let pool = SqlitePool::connect(&url).await.expect("seed database");

    let published = round_trip(
        &mut client,
        &bookmarks_iq(
            "publish-room",
            "publish",
            "room@muc.localhost",
            Some(bookmarks_payload("never")),
        ),
        "publish-room",
    )
    .await;
    assert!(
        matches!(
            Iq::try_from(published).expect("typed IQ"),
            Iq::Result { .. }
        ),
        "the canonical bookmark is published"
    );
    assert_eq!(
        projection_mode(&pool, "room@muc.localhost")
            .await
            .as_deref(),
        Some("never")
    );

    // A stray bookmark for another room, with the row it fed.
    seed_bookmark(&pool, "Other@muc.localhost", "never").await;
    seed_projection(&pool, "other@muc.localhost", "never").await;
    // A stray spelling of the published room, published after it.
    seed_bookmark(&pool, "Room@muc.localhost", "on-mention").await;
    seed_projection(&pool, "room@muc.localhost", "on-mention").await;

    for (item_id, id) in [
        ("Other@muc.localhost", "retract-other"),
        ("Room@muc.localhost", "retract-room"),
    ] {
        let response =
            round_trip(&mut client, &bookmarks_iq(id, "retract", item_id, None), id).await;
        assert!(
            matches!(Iq::try_from(response).expect("typed IQ"), Iq::Result { .. }),
            "{item_id} is retracted by its exact stored id"
        );
    }
    assert_eq!(
        bookmark_ids(&mut client, "bookmark-items-after-retract").await,
        vec!["room@muc.localhost".to_owned()]
    );
    assert_eq!(
        projection_mode(&pool, "other@muc.localhost").await,
        None,
        "the stray bookmark's setting is cleared"
    );
    assert_eq!(
        projection_mode(&pool, "room@muc.localhost")
            .await
            .as_deref(),
        Some("never"),
        "the canonical bookmark's setting applies again"
    );

    // A non-canonical id naming no stored bookmark is still refused.
    let response = round_trip(
        &mut client,
        &bookmarks_iq("retract-unstored", "retract", "ROOM@muc.localhost", None),
        "retract-unstored",
    )
    .await;
    let Iq::Error { error, .. } = Iq::try_from(response).expect("typed IQ") else {
        panic!("expected an IQ error");
    };
    assert_eq!(error.defined_condition, DefinedCondition::BadRequest);
    client.close().await.expect("close");
    pool.close().await;
}

#[tokio::test]
async fn refused_peer_domain_forms_are_unroutable_not_malformed() {
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

    // ADR-0020 D3: a stanza from a local account to an IP-address, numeric
    // IPv4 or single-label domain receives <remote-server-not-found/>.
    for (index, to) in [
        "2130706433",
        "0x7f.1",
        "bob@127.1",
        "bob@0x7f000001/phone",
        "127.0.0.1",
        "intranet",
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("peer-form-{index}");
        let frame =
            format!(r#"<iq type="get" id="{id}" to="{to}"><ping xmlns="urn:xmpp:ping"/></iq>"#);
        let response = round_trip(&mut client, &frame, &id).await;
        let Iq::Error { error, .. } = Iq::try_from(response).expect("typed IQ") else {
            panic!("{to}: expected an IQ error");
        };
        assert_eq!(error.type_, ErrorType::Cancel, "{to}");
        assert_eq!(
            error.defined_condition,
            DefinedCondition::RemoteServerNotFound,
            "{to}"
        );
    }
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
    let server =
        TestServer::start_persistent_with_extra_accounts(&url, &[("bob", "bob-password-123")]);
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
    // `bob@localhost`, the native account `bob`, which it is not. It is
    // left out, and it does not hide the canonical `bob`.
    seed_oidc(&pool, "Bob").await;
    seed_oidc(&pool, "bob.member").await;

    let bob = "bob@localhost"
        .parse::<jid::BareJid>()
        .expect("canonical JID");
    let member = "bob.member@localhost"
        .parse::<jid::BareJid>()
        .expect("canonical JID");
    assert_eq!(
        search(&mut client, "s-text", "bob").await,
        vec![bob.clone(), member.clone()]
    );
    assert_eq!(
        search(&mut client, "s-address", "bob@localhost").await,
        vec![bob, member.clone()]
    );
    assert_eq!(
        search(&mut client, "s-exact", "bob.member@localhost").await,
        vec![member]
    );
    // A non-canonical address stays free text and matches no name.
    assert!(search(&mut client, "s-folded", "Bob.Member@LOCALHOST")
        .await
        .is_empty());
    client.close().await.expect("close");
    pool.close().await;
}
