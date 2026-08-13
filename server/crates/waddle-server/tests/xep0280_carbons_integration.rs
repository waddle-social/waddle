//! XEP-0280 Message Carbons integration over the WebSocket transport.
//!
//! This covers the app-facing `waddle-server` WebSocket path, which is
//! the only supported XMPP C2S transport.

#[cfg(feature = "clustering")]
use base64::Engine;
#[cfg(feature = "clustering")]
use sqlx::PgPool;
#[cfg(feature = "clustering")]
use std::net::TcpListener;
#[cfg(feature = "clustering")]
use tokio::sync::Mutex;
use waddle_ws_test_support as ws_common;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
#[cfg(feature = "clustering")]
static POSTGRES_SERIAL: Mutex<()> = Mutex::const_new(());

async fn enable_carbons(client: &mut WsXmppClient, id: &str) -> Result<(), String> {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#
        ))
        .await?;
    let _ = client.recv_matching(|frame| frame.contains(id)).await?;
    Ok(())
}

async fn enable_resumption(client: &mut WsXmppClient) -> Result<String, String> {
    client
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await?;
    let enabled = client
        .recv_matching(|frame| frame.contains("<enabled"))
        .await?;
    attr_value(&enabled, "id").ok_or_else(|| format!("enabled missing id: {enabled}"))
}

fn attr_value(frame: &str, attr: &str) -> Option<String> {
    let double = format!("{attr}=\"");
    if let Some(start) = frame.find(&double).map(|start| start + double.len()) {
        let end = frame[start..].find('"')?;
        return Some(frame[start..start + end].to_string());
    }
    let single = format!("{attr}='");
    let start = frame.find(&single).map(|start| start + single.len())?;
    let end = frame[start..].find('\'')?;
    Some(frame[start..start + end].to_string())
}

#[cfg(feature = "clustering")]
struct ShadowWsFixture {
    server: TestServer,
    admin: PgPool,
    schema: String,
}

#[cfg(feature = "clustering")]
impl ShadowWsFixture {
    async fn open(test_name: &str, shadow_enabled: bool) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (xep0280 shadow parity)");
            return None;
        };
        let schema = format!(
            "waddle_test_x0280_shadow_{test_name}_{}",
            uuid::Uuid::new_v4().simple()
        );
        let admin = PgPool::connect(&database_url)
            .await
            .expect("connect postgres admin pool");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated postgres schema");

        let schema_url = postgres_url_with_search_path(&database_url, &schema);
        let swarm_port = reserve_swarm_port();
        let listen_addr = format!("/ip4/127.0.0.1/tcp/{swarm_port}");
        let pool_env = generate_keypair_pool();
        let shadow_enabled_env = if shadow_enabled { "true" } else { "false" };
        let envs: Vec<(String, String)> = vec![
            ("WADDLE_DB_DRIVER".to_string(), "postgres".to_string()),
            ("WADDLE_DATABASE_URL".to_string(), schema_url.clone()),
            (
                "WADDLE_XMPP_SM_DATABASE_URL".to_string(),
                schema_url.clone(),
            ),
            (
                "WADDLE_XMPP_MAM_DATABASE_URL".to_string(),
                schema_url.clone(),
            ),
            (
                "WADDLE_XMPP_INBOX_DATABASE_URL".to_string(),
                schema_url.clone(),
            ),
            (
                "WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL".to_string(),
                schema_url.clone(),
            ),
            ("WADDLE_XMPP_PUBSUB_DATABASE_URL".to_string(), schema_url),
            ("WADDLE_CLUSTERING_ENABLED".to_string(), "true".to_string()),
            (
                "WADDLE_DEPLOYMENT_UUID".to_string(),
                "018f47b2-4b2e-7a3a-9a4c-52a5a6a90280".to_string(),
            ),
            ("WADDLE_DB_LINEAGE_ACTION".to_string(), "enroll".to_string()),
            ("WADDLE_CLUSTERING_LISTEN_ADDRS".to_string(), listen_addr),
            ("WADDLE_CLUSTERING_KEYPAIR_POOL".to_string(), pool_env),
            (
                "WADDLE_INGRESS_SHADOW_ENABLED".to_string(),
                shadow_enabled_env.to_string(),
            ),
        ];
        let env_refs: Vec<(&str, &str)> = envs
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let server = TestServer::start_with_extra_envs(&[], &env_refs);
        Some(Self {
            server,
            admin,
            schema,
        })
    }

    async fn poison_shadow_storage(&self) {
        sqlx::query(&format!(
            "DROP TABLE {}.ingress_effect_intents",
            self.schema
        ))
        .execute(&self.admin)
        .await
        .expect("drop shadow effect-intent table");
    }

    async fn close(self) {
        let Self {
            server,
            admin,
            schema,
        } = self;
        drop(server);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated postgres schema");
    }
}

#[cfg(feature = "clustering")]
fn reserve_swarm_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind temporary swarm port")
        .local_addr()
        .expect("temporary swarm port address")
        .port()
}

#[cfg(feature = "clustering")]
fn generate_keypair_pool() -> String {
    let keypair = libp2p::identity::ed25519::Keypair::generate();
    base64::engine::general_purpose::STANDARD.encode(keypair.secret().as_ref())
}

#[cfg(feature = "clustering")]
fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse postgres URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

#[cfg(feature = "clustering")]
fn normalize_sm_enabled_frame(frame: &str) -> String {
    normalize_attr_value(frame, "id", "$stream-id")
}

#[cfg(feature = "clustering")]
fn normalize_attr_value(frame: &str, attr: &str, replacement: &str) -> String {
    let double = format!(r#"{attr}=""#);
    if let Some(start) = frame.find(&double) {
        let value_start = start + double.len();
        if let Some(value_end) = frame[value_start..].find('"') {
            let value_end = value_start + value_end;
            return format!(
                "{}{}{}",
                &frame[..value_start],
                replacement,
                &frame[value_end..]
            );
        }
    }

    let single = format!("{attr}='");
    if let Some(start) = frame.find(&single) {
        let value_start = start + single.len();
        if let Some(value_end) = frame[value_start..].find('\'') {
            let value_end = value_start + value_end;
            return format!(
                "{}{}{}",
                &frame[..value_start],
                replacement,
                &frame[value_end..]
            );
        }
    }

    frame.to_string()
}

#[cfg(feature = "clustering")]
async fn run_shadow_parity_exchange(server: &TestServer) -> Vec<String> {
    let password = server.fixed_account_password().to_string();
    let mut transcript = Vec::new();
    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        "shadow-desktop",
    )
    .await
    .expect("desktop connection");
    desktop
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="shadow-enable-desktop"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#,
        )
        .await
        .expect("send carbons enable");
    for frame in desktop
        .recv_until(|frame| frame.contains("shadow-enable-desktop"))
        .await
        .expect("enable carbons on desktop")
    {
        transcript.push(format!("desktop:{frame}"));
    }

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        "shadow-phone",
    )
    .await
    .expect("phone connection");
    phone
        .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
        .await
        .expect("send stream management enable");
    for frame in phone
        .recv_until(|frame| frame.contains("<enabled"))
        .await
        .expect("enable stream management")
    {
        transcript.push(format!("phone:{}", normalize_sm_enabled_frame(&frame)));
    }

    phone
        .send(
            r#"<message xmlns="jabber:client" to="ghost@localhost" type="chat" id="shadow-carbon-1"><body>shadow parity body</body></message>"#,
        )
        .await
        .expect("send message");

    for frame in desktop
        .recv_until(|frame| {
            frame.contains("urn:xmpp:carbons:2")
                && frame.contains("<sent")
                && frame.contains("shadow parity body")
        })
        .await
        .expect("desktop receives sent carbon")
    {
        transcript.push(format!("desktop:{frame}"));
    }

    let _ = phone.close().await;
    let _ = desktop.close().await;
    transcript
}

#[tokio::test]
async fn sent_carbon_delivered_to_opted_in_sibling_over_websocket() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();

    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("desktop-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("desktop connection");
    enable_carbons(&mut desktop, "carbons-enable-desktop")
        .await
        .expect("enable carbons");

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");

    phone
        .send(
            r#"<message xmlns="jabber:client" to="ghost@localhost" type="chat" id="ws-carbon-1"><body>websocket sent carbon proof</body></message>"#,
        )
        .await
        .expect("send dm");

    let carbon = desktop
        .recv_matching(|frame| {
            frame.contains("urn:xmpp:carbons:2")
                && frame.contains("<sent")
                && frame.contains("websocket sent carbon proof")
        })
        .await
        .expect("desktop receives sent carbon");

    assert!(
        carbon.contains("urn:xmpp:carbons:2"),
        "expected carbon namespace in frame: {carbon}"
    );

    let _ = phone.close().await;
    let _ = desktop.close().await;
}

#[tokio::test]
async fn sent_carbon_replays_to_detached_resumable_sibling() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let desktop_resource = format!("desktop-detached-{}", uuid::Uuid::new_v4());

    let mut desktop = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &desktop_resource,
    )
    .await
    .expect("desktop connection");
    enable_carbons(&mut desktop, "carbons-enable-detached")
        .await
        .expect("enable carbons");
    let stream_id = enable_resumption(&mut desktop)
        .await
        .expect("enable resumption");
    drop(desktop);

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");
    let mut resumed = None;
    let mut replay = None;
    for attempt in 0..20 {
        let body = format!("detached carbon proof {attempt}");
        phone
            .send(&format!(
                r#"<message xmlns="jabber:client" to="ghost@localhost" type="chat" id="ws-carbon-detached-{attempt}"><body>{body}</body></message>"#
            ))
            .await
            .expect("send dm");

        let mut candidate = WsXmppClient::connect(&server.ws_url())
            .await
            .expect("resume connection");
        candidate
            .authenticate(DOMAIN, USERNAME, &password)
            .await
            .expect("authenticate resume connection");
        candidate
            .send(&format!(
                r#"<resume xmlns="urn:xmpp:sm:3" previd="{stream_id}" h="0"/>"#
            ))
            .await
            .expect("send resume");
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            candidate.recv_matching(|frame| frame.contains(&body)),
        )
        .await
        {
            Ok(Ok(frame)) => {
                replay = Some(frame);
                resumed = Some(candidate);
                break;
            }
            _ => {
                drop(candidate);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    let replay = replay.expect("detached carbon replay");
    assert!(
        replay.contains("urn:xmpp:carbons:2") && replay.contains("<sent"),
        "expected sent carbon replay: {replay}"
    );

    let _ = phone.close().await;
    if let Some(resumed) = resumed {
        let _ = resumed.close().await;
    }
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn sent_carbon_wire_is_byte_identical_with_shadow_disabled_and_enabled() {
    let _guard = POSTGRES_SERIAL.lock().await;

    let Some(disabled) = ShadowWsFixture::open("wire_disabled", false).await else {
        return;
    };
    let disabled_frame = run_shadow_parity_exchange(&disabled.server).await;
    disabled.close().await;

    let Some(enabled) = ShadowWsFixture::open("wire_enabled", true).await else {
        return;
    };
    let enabled_frame = run_shadow_parity_exchange(&enabled.server).await;
    enabled.close().await;

    assert_eq!(
        enabled_frame, disabled_frame,
        "enabling ingress shadow must not change the deterministic sent-carbon transcript"
    );
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn sent_carbon_wire_is_byte_identical_when_shadow_storage_is_poisoned() {
    let _guard = POSTGRES_SERIAL.lock().await;

    let Some(disabled) = ShadowWsFixture::open("wire_poison_baseline", false).await else {
        return;
    };
    let disabled_frame = run_shadow_parity_exchange(&disabled.server).await;
    disabled.close().await;

    let Some(poisoned) = ShadowWsFixture::open("wire_poisoned", true).await else {
        return;
    };
    poisoned.poison_shadow_storage().await;
    let poisoned_frame = run_shadow_parity_exchange(&poisoned.server).await;
    poisoned.close().await;

    assert_eq!(
        poisoned_frame, disabled_frame,
        "poisoned shadow storage must not change the deterministic sent-carbon transcript"
    );
}
