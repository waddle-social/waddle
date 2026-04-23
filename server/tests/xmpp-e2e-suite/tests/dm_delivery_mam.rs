// This integration test pulls in the shared test harness plus a large async
// connection setup future, which needs a slightly higher query depth to
// compile reliably in debug test builds.
#![recursion_limit = "256"]

use anyhow::{anyhow, Context, Result};
use kameo::actor::ActorRef;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::TlsAcceptor;
use waddle_xmpp::{
    commands::CommandRegistry,
    connection::ConnectionActor,
    muc::room_registry_actor::RoomRegistryActor,
    registry::{ConnectionRegistry, UserRegistryActor},
    stream_management::InMemorySmSessionRegistry,
    AppState,
};
use xmpp_e2e_suite::scenario::{load_scenario_from_dir, Scenario, Step};

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
    let room_registry = kameo::spawn(RoomRegistryActor::new(muc_domain));
    let connection_registry = Arc::new(ConnectionRegistry::new());
    let user_registry = kameo::spawn(UserRegistryActor::new());
    let mam_storage = Arc::new(
        waddle_xmpp::mam::SqlxMamStorage::open(&format!("sqlite://{}", mam_db_path.display()))
            .await
            .expect("mam storage"),
    );
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
                        let rooms = room_registry.clone();
                        let conns = Arc::clone(&connection_registry);
                        let users = user_registry.clone();
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
                        tokio::spawn(handle_test_connection(
                            stream,
                            peer_addr,
                            tls,
                            dom,
                            state,
                            rooms,
                            conns,
                            users,
                            mam,
                            isr,
                            sm_reg,
                            registration_enabled,
                            pubsub,
                            ext,
                            single_tenant,
                            push_store,
                            push_sender,
                            cmd_registry,
                        ));
                    }
                    Err(_) => break,
                }
            }
            _ = &mut shutdown_rx => break,
        }
    }
}

async fn handle_test_connection<S: AppState>(
    stream: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    tls: TlsAcceptor,
    dom: String,
    state: Arc<S>,
    rooms: ActorRef<RoomRegistryActor>,
    conns: Arc<ConnectionRegistry>,
    users: ActorRef<UserRegistryActor>,
    mam: Arc<waddle_xmpp::mam::SqlxMamStorage>,
    isr: waddle_xmpp::isr::SharedIsrTokenStore,
    sm_reg: Arc<dyn waddle_xmpp::stream_management::SmSessionRegistry>,
    registration_enabled: bool,
    pubsub: Arc<dyn waddle_xmpp::pubsub::PubSubStorage + Send + Sync>,
    ext: Arc<waddle_extensions::ExtensionManager>,
    single_tenant: bool,
    push_store: Arc<dyn waddle_xmpp::push::PushSubscriptionStore + Send + Sync>,
    push_sender: Arc<dyn waddle_xmpp::push::WebPushSender + Send + Sync>,
    cmd_registry: Arc<CommandRegistry>,
) {
    if let Err(error) = ConnectionActor::handle_connection(
        stream,
        peer_addr,
        tls,
        dom,
        state,
        rooms,
        conns,
        users,
        mam,
        isr,
        sm_reg,
        registration_enabled,
        pubsub,
        ext,
        single_tenant,
        push_store,
        push_sender,
        cmd_registry,
    )
    .await
    {
        eprintln!("test connection ended with error: {error:#}");
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
    client
        .read_until("<proceed", common::DEFAULT_TIMEOUT)
        .await?;
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
    client
        .read_until("<success", common::DEFAULT_TIMEOUT)
        .await?;
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
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!("sqlite://{}", db_path.display()))
        .await
        .context("connect mam db")?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mam_messages WHERE body = ?")
        .bind(body)
        .fetch_one(&pool)
        .await
        .context("query mam rows")?;
    Ok(count as u64)
}

async fn join_muc_room(
    client: &mut common::RawXmppClient,
    room_jid: &str,
    nick: &str,
) -> Result<()> {
    client
        .send(&format!(
            "<presence to='{room_jid}/{nick}' xmlns='jabber:client'>\
                <x xmlns='http://jabber.org/protocol/muc'>\
                    <history maxstanzas='0'/>\
                </x>\
            </presence>"
        ))
        .await?;
    let _join_response = client.read_until("110", common::DEFAULT_TIMEOUT).await?;
    client.clear();
    Ok(())
}

#[tokio::test]
async fn cue_scenarios_run_end_to_end() -> Result<()> {
    common::init_test_env();
    let scenario_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios");
    let scenario_files = discover_scenario_files(&scenario_root)?;

    for scenario_file in scenario_files {
        let scenario = load_scenario_from_file(&scenario_root, &scenario_file)?;
        run_scenario(&scenario)
            .await
            .with_context(|| format!("scenario '{}' failed", scenario.name))?;
    }

    Ok(())
}

fn discover_scenario_files(scenario_root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(scenario_root)
        .with_context(|| format!("read scenarios dir {}", scenario_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("cue") {
            continue;
        }
        if path.file_name().and_then(|name| name.to_str()) == Some("schema.cue") {
            continue;
        }
        files.push(path);
    }
    files.sort();
    if files.is_empty() {
        return Err(anyhow!(
            "no scenario files found in {}; expected at least one .cue file",
            scenario_root.display()
        ));
    }
    Ok(files)
}

fn load_scenario_from_file(scenario_root: &Path, scenario_file: &Path) -> Result<Scenario> {
    let temp_dir = tempfile::tempdir().context("create temp scenario dir")?;
    let cue_mod_path = scenario_root.join("cue.mod");
    let schema_path = scenario_root.join("schema.cue");
    let temp_cue_mod = temp_dir.path().join("cue.mod");
    let temp_schema = temp_dir.path().join("schema.cue");
    let temp_scenario = temp_dir.path().join("scenario.cue");

    copy_dir_recursive(&cue_mod_path, &temp_cue_mod).with_context(|| {
        format!(
            "copy cue module from {} to {}",
            cue_mod_path.display(),
            temp_cue_mod.display()
        )
    })?;
    fs::copy(&schema_path, &temp_schema).with_context(|| {
        format!(
            "copy schema from {} to {}",
            schema_path.display(),
            temp_schema.display()
        )
    })?;
    fs::copy(scenario_file, &temp_scenario).with_context(|| {
        format!(
            "copy scenario from {} to {}",
            scenario_file.display(),
            temp_scenario.display()
        )
    })?;

    load_scenario_from_dir(temp_dir.path(), "xmpp_e2e_suite").with_context(|| {
        format!(
            "load scenario package from temporary dir for {}",
            scenario_file.display()
        )
    })
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("create target directory {}", target.display()))?;
    for entry in fs::read_dir(source)
        .with_context(|| format!("read source directory {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            fs::copy(&path, &destination).with_context(|| {
                format!(
                    "copy source file {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

async fn run_scenario(scenario: &Scenario) -> Result<()> {
    let server = FileBackedMamTestServer::start().await;
    let mut clients: HashMap<String, common::RawXmppClient> = HashMap::new();

    for (user_key, user) in &scenario.users {
        for (device_key, device) in &user.devices {
            let mut client = common::RawXmppClient::connect(server.addr).await?;
            let _jid =
                establish_bound_session(&mut client, &server, &device.username, &device.resource)
                    .await
                    .with_context(|| {
                        format!("bind failed for device '{}.{}'", user_key, device_key)
                    })?;
            client.clear();
            clients.insert(actor_key(user_key, device_key), client);
        }
    }

    for step in &scenario.steps {
        execute_step(step, &mut clients, &server.mam_db_path).await?;
    }

    Ok(())
}

#[tokio::test]
async fn muc_groupchat_fans_out_to_all_devices() -> Result<()> {
    common::init_test_env();
    let server = FileBackedMamTestServer::start().await;
    let room = format!("fanout@muc.{}", server.domain);
    let body = "muc fanout message to all joined devices";

    let devices = [
        ("alice", "phone"),
        ("alice", "desktop"),
        ("bob", "phone"),
        ("bob", "tablet"),
    ];
    let mut clients: HashMap<String, common::RawXmppClient> = HashMap::new();

    for (username, resource) in devices {
        let mut client = common::RawXmppClient::connect(server.addr).await?;
        establish_bound_session(&mut client, &server, username, resource).await?;
        client.clear();
        clients.insert(actor_key(username, resource), client);
    }

    for (username, resource) in devices {
        let actor = actor_key(username, resource);
        let client = clients
            .get_mut(actor.as_str())
            .ok_or_else(|| anyhow!("unknown actor '{}'", actor))?;
        join_muc_room(client, room.as_str(), username).await?;
    }

    let sender_actor = actor_key("alice", "phone");
    let sender = clients
        .get_mut(sender_actor.as_str())
        .ok_or_else(|| anyhow!("missing sender '{}'", sender_actor))?;
    sender
        .send(&format!(
            "<message xmlns='jabber:client' to='{room}' type='groupchat' id='fanout-1'>\
                <body>{body}</body>\
            </message>"
        ))
        .await?;

    for (username, resource) in devices {
        let actor = actor_key(username, resource);
        let client = clients
            .get_mut(actor.as_str())
            .ok_or_else(|| anyhow!("missing recipient '{}'", actor))?;
        let received = client
            .read_until("</message>", common::DEFAULT_TIMEOUT)
            .await?;
        if !received.contains(body) {
            return Err(anyhow!(
                "expected '{}' to receive groupchat body '{}', got: {}",
                actor,
                body,
                received
            ));
        }
        if !received.contains(room.as_str()) {
            return Err(anyhow!(
                "expected '{}' groupchat stanza to reference room '{}', got: {}",
                actor,
                room,
                received
            ));
        }
    }

    Ok(())
}

async fn execute_step(
    step: &Step,
    clients: &mut HashMap<String, common::RawXmppClient>,
    mam_db_path: &Path,
) -> Result<()> {
    if let Some(send) = &step.send {
        let actor = actor_key(send.actor.user.as_str(), send.actor.device.as_str());
        let client = clients
            .get_mut(actor.as_str())
            .ok_or_else(|| anyhow!("unknown actor '{}'", actor))?;
        client.send(send.stanza.as_str()).await?;
        return Ok(());
    }

    if let Some(expect_stanza) = &step.expect_stanza {
        let target = actor_key(
            expect_stanza.target.user.as_str(),
            expect_stanza.target.device.as_str(),
        );
        let client = clients
            .get_mut(target.as_str())
            .ok_or_else(|| anyhow!("unknown target '{}'", target))?;
        let received = client
            .read_until("</message>", common::DEFAULT_TIMEOUT)
            .await?;
        for expected in &expect_stanza.contains {
            if !received.contains(expected) {
                return Err(anyhow!(
                    "expected target '{}' stanza to contain '{}', got: {}",
                    target,
                    expected,
                    received
                ));
            }
        }
        // `read_until` returns the full retained buffer; clear after each
        // assertion so later expectStanza steps on the same actor only inspect
        // newly delivered stanzas.
        client.clear();
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
        let poll_interval = Duration::from_millis(100);
        let deadline = tokio::time::Instant::now() + common::DEFAULT_TIMEOUT;

        loop {
            let count = count_rows_by_body(mam_db_path, expected_body).await?;

            if count >= expect_db.min_rows {
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow!(
                    "expected at least {} rows in mam_messages with body='{}', got {}",
                    expect_db.min_rows,
                    expected_body,
                    count
                ));
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    Err(anyhow!("invalid step"))
}

fn actor_key(user: &str, device: &str) -> String {
    format!("{user}.{device}")
}
