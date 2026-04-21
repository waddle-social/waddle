use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use waddle_xmpp::{
    connection::ConnectionActor, commands::CommandRegistry, muc::MucRoomRegistry,
    registry::ConnectionRegistry, stream_management::InMemorySmSessionRegistry, AppState,
};
use xmpp_e2e_suite::scenario::{load_scenario_from_dir, Step};

#[path = "../../../crates/waddle-xmpp/tests/common/mod.rs"]
mod common;

struct FileBackedMamTestServer {
    addr: std::net::SocketAddr,
    domain: String,
    tls_credentials: common::TestTlsCredentials,
    shutdown_tx: Option<oneshot::Sender<()>>,
    mam_db_path: PathBuf,
    _temp_dir: tempfile::TempDir,
}

impl FileBackedMamTestServer {
    async fn start() -> Self {
        common::install_crypto_provider();

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let domain = "localhost".to_string();
        let tls_credentials = common::TestTlsCredentials::generate(&domain);
        let tls_acceptor = tls_credentials.tls_acceptor();

        let temp_dir = tempfile::tempdir().expect("temp dir");
        let mam_db_path = temp_dir.path().join("mam.db");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let app_state = Arc::new(common::MockAppState::new(&domain));

        tokio::spawn(run_test_server(
            listener,
            tls_acceptor,
            domain.clone(),
            app_state,
            shutdown_rx,
            mam_db_path.clone(),
        ));

        Self {
            addr,
            domain,
            tls_credentials,
            shutdown_tx: Some(shutdown_tx),
            mam_db_path,
            _temp_dir: temp_dir,
        }
    }

    fn tls_connector(&self) -> tokio_rustls::TlsConnector {
        self.tls_credentials.tls_connector()
    }
}

impl Drop for FileBackedMamTestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

async fn run_test_server<S: AppState>(
    listener: TcpListener,
    tls_acceptor: TlsAcceptor,
    domain: String,
    app_state: Arc<S>,
    mut shutdown_rx: oneshot::Receiver<()>,
    mam_db_path: PathBuf,
) {
    let muc_domain = format!("muc.{domain}");
    let room_registry = Arc::new(MucRoomRegistry::new(muc_domain));
    let connection_registry = Arc::new(ConnectionRegistry::new());
    let db = libsql::Builder::new_local(mam_db_path.to_string_lossy().as_ref())
        .build()
        .await
        .expect("mam db");
    let conn = db.connect().expect("mam conn");
    let mam_storage = Arc::new(waddle_xmpp::mam::LibSqlMamStorage::new(conn));
    let isr_token_store = waddle_xmpp::isr::create_shared_store();
    let sm_session_registry: Arc<dyn waddle_xmpp::stream_management::SmSessionRegistry> =
        Arc::new(InMemorySmSessionRegistry::new());
    let pubsub_storage: Arc<dyn waddle_xmpp::pubsub::PubSubStorage + Send + Sync> =
        Arc::new(waddle_xmpp::pubsub::InMemoryPubSubStorage::new());
    let push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore + Send + Sync> =
        Arc::new(waddle_xmpp::push::InMemoryPushStore::new());
    let push_sender: Arc<dyn waddle_xmpp::push::WebPushSender + Send + Sync> =
        Arc::new(waddle_xmpp::push::HttpWebPushSender::new());
    let extension_manager =
        Arc::new(waddle_extensions::ExtensionManager::from_env().expect("extension manager"));
    let command_registry = Arc::new(CommandRegistry::new());

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, peer_addr)) => {
                        let tls = tls_acceptor.clone();
                        let dom = domain.clone();
                        let state = Arc::clone(&app_state);
                        let rooms = Arc::clone(&room_registry);
                        let conns = Arc::clone(&connection_registry);
                        let mam = Arc::clone(&mam_storage);
                        let isr = Arc::clone(&isr_token_store);
                        let sm_reg = Arc::clone(&sm_session_registry);
                        let pubsub = Arc::clone(&pubsub_storage);
                        let push_store = Arc::clone(&push_store);
                        let push_sender = Arc::clone(&push_sender);
                        let ext = Arc::clone(&extension_manager);
                        let cmd_registry = Arc::clone(&command_registry);
                        let registration_enabled = true;
                        let single_tenant = false;
                        tokio::spawn(async move {
                            let _ = ConnectionActor::handle_connection(
                                stream, peer_addr, tls, dom, state, rooms, conns, mam, isr, sm_reg,
                                registration_enabled, pubsub, ext, single_tenant, push_store, push_sender,
                                cmd_registry
                            ).await;
                        });
                    }
                    Err(_) => break,
                }
            }
            _ = &mut shutdown_rx => break,
        }
    }
}

async fn establish_bound_session(
    client: &mut common::RawXmppClient,
    server: &FileBackedMamTestServer,
    username: &str,
    resource: &str,
) -> Result<String> {
    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", common::DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    client
        .send("<starttls xmlns='urn:ietf:params:xml:ns:xmpp-tls'/>")
        .await?;
    client.read_until("<proceed", common::DEFAULT_TIMEOUT).await?;
    client.clear();

    let connector = server.tls_connector();
    client.upgrade_tls(connector, &server.domain).await?;

    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", common::DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    let auth_data = common::encode_sasl_plain(
        &format!("{}@{}", username, server.domain),
        &common::test_secret("auth"),
    );
    client
        .send(&format!(
            "<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>{auth_data}</auth>"
        ))
        .await?;
    client.read_until("<success", common::DEFAULT_TIMEOUT).await?;
    client.clear();

    client
        .send(&format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' \
            to='{}' version='1.0'>",
            server.domain
        ))
        .await?;
    client
        .read_until("</stream:features>", common::DEFAULT_TIMEOUT)
        .await?;
    client.clear();

    client
        .send(&format!(
            "<iq type='set' id='bind-1' xmlns='jabber:client'>\
                <bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'>\
                    <resource>{resource}</resource>\
                </bind>\
            </iq>",
        ))
        .await?;
    let bind_response = client.read_until("</iq>", common::DEFAULT_TIMEOUT).await?;
    client.clear();
    common::extract_bound_jid(&bind_response).ok_or_else(|| anyhow!("missing bound jid"))
}

async fn count_rows_by_body(db_path: &Path, body: &str) -> Result<u64> {
    let db = libsql::Builder::new_local(db_path.to_string_lossy().as_ref())
        .build()
        .await
        .context("open mam db")?;
    let conn = db.connect().context("connect mam db")?;
    let mut rows = conn
        .query("SELECT COUNT(*) FROM mam_messages WHERE body = ?1", [body])
        .await
        .context("query mam rows")?;
    let row = rows
        .next()
        .await
        .context("read row stream")?
        .ok_or_else(|| anyhow!("count query returned no rows"))?;
    let count: i64 = row.get(0).context("decode count")?;
    Ok(count as u64)
}

#[tokio::test]
async fn cue_scenario_runs_end_to_end() -> Result<()> {
    common::init_test_env();
    let scenario_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let scenario = load_scenario_from_dir(&scenario_dir, "xmpp_e2e_suite")?;

    let server = FileBackedMamTestServer::start().await;
    let mut clients: HashMap<String, common::RawXmppClient> = HashMap::new();

    for user in &scenario.users {
        for device in &user.devices {
            let mut client = common::RawXmppClient::connect(server.addr).await?;
            let _jid = establish_bound_session(&mut client, &server, &device.username, &device.resource)
                .await
                .with_context(|| format!("bind failed for device '{}'", device.id))?;
            client.clear();
            clients.insert(device.id.clone(), client);
        }
    }

    for step in &scenario.steps {
        execute_step(step, &mut clients, &server.mam_db_path).await?;
    }

    Ok(())
}

async fn execute_step(
    step: &Step,
    clients: &mut HashMap<String, common::RawXmppClient>,
    mam_db_path: &Path,
) -> Result<()> {
    if let Some(send) = &step.send {
        let client = clients
            .get_mut(send.actor.as_str())
            .ok_or_else(|| anyhow!("unknown actor '{}'", send.actor))?;
        client.send(send.stanza.as_str()).await?;
        return Ok(());
    }

    if let Some(expect_stanza) = &step.expect_stanza {
        let client = clients
            .get_mut(expect_stanza.target.as_str())
            .ok_or_else(|| anyhow!("unknown target '{}'", expect_stanza.target))?;
        let received = client.read_until("</message>", Duration::from_secs(3)).await?;
        for expected in &expect_stanza.contains {
            if !received.contains(expected) {
                return Err(anyhow!(
                    "expected target '{}' stanza to contain '{}', got: {}",
                    expect_stanza.target,
                    expected,
                    received
                ));
            }
        }
        return Ok(());
    }

    if let Some(expect_db) = &step.expect_db {
        if expect_db.table != "mam_messages" {
            return Err(anyhow!(
                "unsupported expectDb.table '{}'; only mam_messages is supported in V1",
                expect_db.table
            ));
        }
        let expected_body = expect_db
            .where_clause
            .get("body")
            .ok_or_else(|| anyhow!("expectDb.where.body is required"))?;
        let count = count_rows_by_body(mam_db_path, expected_body).await?;
        if count < expect_db.min_rows {
            return Err(anyhow!(
                "expected at least {} rows in mam_messages with body='{}', got {}",
                expect_db.min_rows,
                expected_body,
                count
            ));
        }
        return Ok(());
    }

    Err(anyhow!("invalid step"))
}
