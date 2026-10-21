//! XEP-0030 service discovery on user bot accounts (`bots.<domain>`).
//!
//! An enabled bot is a leaf entity: disco#info answers `client/bot` with the
//! `disco#info` feature and disco#items is empty. Unknown or disabled bots
//! and the bare bots domain host no entity and answer `<item-not-found/>`
//! (XEP-0030 §3.1, §4.1).
#![cfg(feature = "test-support")]

use minidom::Element;
use sqlx::SqlitePool;
use waddle_ws_test_support::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::disco::{DiscoInfoResult, DiscoItemsResult};
use xmpp_parsers::iq::Iq;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

const DOMAIN: &str = "localhost";
const BOTS_DOMAIN: &str = "bots.localhost";
const NS_DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
const NS_DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

struct Fixture {
    server: TestServer,
    pool: SqlitePool,
    // Keep the database directory alive until the server and pool drop.
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let directory = tempfile::tempdir().expect("database directory");
        let url = format!(
            "sqlite://{}?mode=rwc",
            directory.path().join("disco-bots.sqlite3").display()
        );
        let server = TestServer::start_persistent_with_extra_accounts(&url, &[]);
        let pool = SqlitePool::connect(&url).await.expect("seed database");
        Self {
            server,
            pool,
            _directory: directory,
        }
    }

    async fn connect(&self) -> WsXmppClient {
        WsXmppClient::connect_and_auth(
            &self.server.ws_url(),
            DOMAIN,
            "admin",
            self.server.fixed_account_password(),
            "disco-bots",
        )
        .await
        .expect("authenticated client")
    }

    async fn seed_bot(&self, name: &str, display_name: Option<&str>, disabled: bool) -> String {
        let localpart = format!("admin+{name}");
        let jid = format!("{localpart}@{BOTS_DOMAIN}");
        let (disabled_at, disabled_reason) = if disabled {
            (Some("2026-01-02T00:00:00.000000Z"), Some("owner_missing"))
        } else {
            (None, None)
        };
        sqlx::query(
            "INSERT INTO users (jid, username, xmpp_localpart, localpart_key, display_name, \
             kind, owner_jid, disabled_at, disabled_reason, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 'bot', ?, ?, ?, ?, ?)",
        )
        .bind(&jid)
        .bind(&localpart)
        .bind(&localpart)
        .bind(&localpart)
        .bind(display_name)
        .bind(format!("admin@{DOMAIN}"))
        .bind(disabled_at)
        .bind(disabled_reason)
        .bind("2026-01-01T00:00:00.000000Z")
        .bind("2026-01-01T00:00:00.000000Z")
        .execute(&self.pool)
        .await
        .expect("seed bot account");
        jid
    }
}

fn parse_iq(frame: &str) -> Iq {
    Iq::try_from(frame.parse::<Element>().expect("IQ XML")).expect("typed IQ")
}

async fn disco_info(client: &mut WsXmppClient, to: &str, id: &str) -> Iq {
    parse_iq(&disco_info_query(client, to, id).await.expect("disco#info"))
}

async fn disco_items(client: &mut WsXmppClient, to: &str, id: &str) -> Iq {
    let request = Iq::Get {
        from: None,
        to: Some(to.parse().expect("disco#items target")),
        id: id.into(),
        payload: Element::builder("query", NS_DISCO_ITEMS).build(),
    };
    client
        .send(&String::from(&Element::from(request)))
        .await
        .expect("send disco#items");
    parse_iq(
        &client
            .recv_matching(|frame| frame.contains(id))
            .await
            .expect("disco#items"),
    )
}

fn assert_item_not_found(response: Iq) {
    let Iq::Error { error, .. } = response else {
        panic!("expected an IQ error: {response:?}");
    };
    let error: StanzaError = error;
    assert_eq!(error.type_, ErrorType::Cancel);
    assert_eq!(error.defined_condition, DefinedCondition::ItemNotFound);
}

#[tokio::test]
async fn disco_info_bot_jid_is_client_bot() {
    let fixture = Fixture::start().await;
    let named = fixture
        .seed_bot("standup", Some("Standup Bot"), false)
        .await;
    let unnamed = fixture.seed_bot("deploy", None, false).await;
    let mut client = fixture.connect().await;

    for (bot, name) in [
        (named.as_str(), "Standup Bot"),
        (unnamed.as_str(), "admin+deploy"),
    ] {
        let response = disco_info(&mut client, bot, "bot-info").await;
        let Iq::Result {
            from,
            payload: Some(payload),
            ..
        } = response
        else {
            panic!("expected a disco#info result: {response:?}");
        };
        assert_eq!(from.map(|from| from.to_string()).as_deref(), Some(bot));
        let info = DiscoInfoResult::try_from(payload).expect("typed disco#info");
        assert_eq!(info.node, None);
        assert_eq!(info.identities.len(), 1);
        let identity = &info.identities[0];
        assert_eq!(identity.category, "client");
        assert_eq!(identity.type_, "bot");
        assert_eq!(identity.name.as_deref(), Some(name));
        let features: Vec<&str> = info.features.iter().map(|f| f.var.as_str()).collect();
        assert_eq!(features, vec![NS_DISCO_INFO]);
    }
    client.close().await.expect("close");
}

#[tokio::test]
async fn disco_info_unknown_bot_item_not_found() {
    let fixture = Fixture::start().await;
    let disabled = fixture.seed_bot("standup", None, true).await;
    let mut client = fixture.connect().await;

    for target in [
        "admin+ghost@bots.localhost",
        disabled.as_str(),
        "admin@bots.localhost",
    ] {
        assert_item_not_found(disco_info(&mut client, target, "unknown-bot-info").await);
    }
    client.close().await.expect("close");
}

#[tokio::test]
async fn disco_info_bots_domain_item_not_found() {
    let fixture = Fixture::start().await;
    let mut client = fixture.connect().await;

    assert_item_not_found(disco_info(&mut client, BOTS_DOMAIN, "bots-domain-info").await);
    client.close().await.expect("close");
}

#[tokio::test]
async fn disco_items_bot_jid_empty() {
    let fixture = Fixture::start().await;
    let bot = fixture.seed_bot("standup", None, false).await;
    let disabled = fixture.seed_bot("retired", None, true).await;
    let mut client = fixture.connect().await;

    let response = disco_items(&mut client, &bot, "bot-items").await;
    let Iq::Result {
        payload: Some(payload),
        ..
    } = response
    else {
        panic!("expected a disco#items result: {response:?}");
    };
    let items = DiscoItemsResult::try_from(payload).expect("typed disco#items");
    assert!(items.items.is_empty(), "a bot hosts no items: {items:?}");

    for target in [BOTS_DOMAIN, "admin+ghost@bots.localhost", disabled.as_str()] {
        assert_item_not_found(disco_items(&mut client, target, "missing-bot-items").await);
    }
    client.close().await.expect("close");
}
