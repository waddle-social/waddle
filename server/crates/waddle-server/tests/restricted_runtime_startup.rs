//! Restricted PostgreSQL runtime role: owner phase, then runtime attachment.
//!
//! A fresh database gets separate owner roles (a NOLOGIN schema owner and
//! its migrator login) and a runtime login that holds no DDL privilege. The
//! library owner phase prepares every schema; the runtime login then runs the
//! pinned preflight, attaches the attested serving pool, and attaches each
//! startup store read-only. A missing or drifted relation fails closed.
//!
//! `restricted_runtime_role_serves_without_ddl` then starts the real server
//! under the runtime login and runs a WebSocket session between two accounts.
//!
//! PostgreSQL is optional in local development, so the tests skip unless
//! `WADDLE_TEST_POSTGRES_URL` is configured. Publish-gating lanes require
//! PostgreSQL through `WADDLE_TEST_REQUIRE_POSTGRES=1` and fail if it is missing.
#![cfg(feature = "test-support")]

#[cfg(feature = "clustering")]
#[path = "restricted_runtime_startup/clustered.rs"]
mod clustered;

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use minidom::Element;
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::{Connection, PgConnection, Row};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use waddle_server::call_teardown_outbox::CallTeardownOutboxStore;
use waddle_server::channel_space_links::DatabaseChannelSpaceLinkStore;
use waddle_server::config::{IngressConfig, LineageConfig};
use waddle_server::db::bootstrap::RuntimePoolLimits;
use waddle_server::db::lineage::{AttestedLineage, DeploymentUuid, LineageAction, PgSchemaName};
use waddle_server::db::physical_attestation::{
    install, DarkPinnedEntryError, PgRoleName, PhysicalAttestationConfig,
};
use waddle_server::db::runtime_startup::{RuntimeStartupError, RuntimeStartupIdentity};
use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, DatabasePool};
use waddle_server::ingress_substrate::{
    AliasGcBudget, AliasGcProgress, PostgresIngressSubstrate, ALIAS_RETENTION,
};
use waddle_server::ingress_uow::{
    verify_attested_room_observation_schema, RoomObservationSchemaError,
};
use waddle_server::mam_runtime::{DatabaseMamStorage, DatabaseMamStorageError};
use waddle_server::muc_destroy_completion_outbox::MucDestroyCompletionOutboxStore;
use waddle_server::room_effect_outbox::RoomEffectOutboxStore;
use waddle_server::server::push_notification_runtime::verify_attested_push_notification_stores;
use waddle_server::server::runtime_owner::{
    prepare_restricted_runtime_owner, RestrictedRuntimeOwnerError, RestrictedRuntimeOwnerPlan,
};
use waddle_server::server::{ResolvedXmppDatabaseUrl, WebhookDeliveryAttachError};
use waddle_server::spaces_metadata::DatabaseSpacesMetadataStore;
use waddle_ws_test_support::iq::{round_trip, send_iq};
use waddle_ws_test_support::{require_test_postgres_url, TestServer, WsXmppClient};
use waddle_xmpp::ingress::{MessageKey, SemanticDigest};
use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::ping::Ping;

const RELEASE_GIT_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const SESSION_KEY: &str = "integration-test-session-key-32-bytes-long";
const SCHEMA: &str = "guarded";
const TRUSTED_SCHEMA: &str = "attestor";
const RESTRICTED: &str = "NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";

/// What crossed the runtime login's PostgreSQL connections: every error the
/// server returned and every statement that would change the catalog.
#[derive(Default)]
struct Wire {
    errors: Vec<(String, String)>,
    catalog_statements: Vec<String>,
}

/// Incremental PostgreSQL wire-protocol framing for one direction of one
/// connection. The frontend starts with untyped startup packets; an SSL or
/// GSS negotiation request is answered by one unframed byte.
struct Frames {
    buffer: Vec<u8>,
    startup: bool,
}

impl Frames {
    const SSL_REQUEST: u32 = 80_877_103;
    const GSS_REQUEST: u32 = 80_877_104;

    fn feed(&mut self, bytes: &[u8], unframed_replies: &AtomicUsize) -> Vec<(u8, Vec<u8>)> {
        self.buffer.extend_from_slice(bytes);
        let mut messages = Vec::new();
        loop {
            if !self.startup {
                let pending = unframed_replies.load(Ordering::SeqCst);
                if pending > 0 && !self.buffer.is_empty() && self.buffer[0] != b'E' {
                    // The one-byte answer to an SSL or GSS request.
                    self.buffer.remove(0);
                    unframed_replies.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            }
            let header = if self.startup { 4 } else { 5 };
            if self.buffer.len() < header {
                return messages;
            }
            let length_at = header - 4;
            let length = u32::from_be_bytes(
                self.buffer[length_at..header]
                    .try_into()
                    .expect("four length bytes"),
            ) as usize;
            let total = length_at + length;
            if length < 4 || self.buffer.len() < total {
                return messages;
            }
            let payload = self.buffer[header..total].to_vec();
            let kind = if self.startup { 0 } else { self.buffer[0] };
            self.buffer.drain(..total);
            if self.startup {
                let code = payload
                    .get(..4)
                    .map(|code| u32::from_be_bytes(code.try_into().expect("four code bytes")));
                if matches!(code, Some(Self::SSL_REQUEST | Self::GSS_REQUEST)) {
                    unframed_replies.fetch_add(1, Ordering::SeqCst);
                } else {
                    self.startup = false;
                }
            } else {
                messages.push((kind, payload));
            }
        }
    }
}

fn c_string(bytes: &[u8]) -> (String, &[u8]) {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    (
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        bytes.get(end + 1..).unwrap_or_default(),
    )
}

/// Whether any statement in `sql` creates, alters, drops or re-grants.
fn changes_catalog(sql: &str) -> bool {
    sql.split(';').any(|statement| {
        let keyword = statement
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        matches!(
            keyword.as_str(),
            "CREATE" | "ALTER" | "DROP" | "GRANT" | "REVOKE" | "TRUNCATE" | "COMMENT" | "REINDEX"
        )
    })
}

fn record_frontend(wire: &Mutex<Wire>, kind: u8, payload: &[u8]) {
    let sql = match kind {
        b'Q' => c_string(payload).0,
        b'P' => c_string(c_string(payload).1).0,
        _ => return,
    };
    if changes_catalog(&sql) {
        wire.lock()
            .expect("wire record")
            .catalog_statements
            .push(sql);
    }
}

fn record_backend(wire: &Mutex<Wire>, kind: u8, payload: &[u8]) {
    if kind != b'E' {
        return;
    }
    let (mut sqlstate, mut message, mut rest) = (String::new(), String::new(), payload);
    while let Some((&field, value)) = rest.split_first() {
        if field == 0 {
            break;
        }
        let (text, remaining) = c_string(value);
        match field {
            b'C' => sqlstate = text,
            b'M' => message = text,
            _ => {}
        }
        rest = remaining;
    }
    wire.lock()
        .expect("wire record")
        .errors
        .push((sqlstate, message));
}

async fn relay<R, W>(
    mut reader: R,
    mut writer: W,
    wire: Arc<Mutex<Wire>>,
    unframed_replies: Arc<AtomicUsize>,
    frontend: bool,
) where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut frames = Frames {
        buffer: Vec::new(),
        startup: frontend,
    };
    let mut chunk = vec![0_u8; 16 * 1024];
    while let Ok(read) = reader.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        for (kind, payload) in frames.feed(&chunk[..read], &unframed_replies) {
            if frontend {
                record_frontend(&wire, kind, &payload);
            } else {
                record_backend(&wire, kind, &payload);
            }
        }
        if writer.write_all(&chunk[..read]).await.is_err() {
            break;
        }
    }
    let _ = writer.shutdown().await;
}

enum Upstream {
    Tcp(String),
    Unix(PathBuf),
}

/// The Unix socket the runtime login connects through. Restricted startup
/// accepts unauthenticated transport only over a Unix socket, so this
/// endpoint fronts the lane's PostgreSQL whether that listens on TCP or on
/// its own socket. Every connection is relayed unchanged and recorded in
/// [`Wire`], which is how the tests see the runtime's errors and statements
/// without access to the PostgreSQL log.
struct SocketEndpoint {
    directory: tempfile::TempDir,
    port: u16,
    wire: Arc<Mutex<Wire>>,
    forwarder: tokio::task::JoinHandle<()>,
}

impl SocketEndpoint {
    async fn for_url(url: &str) -> Self {
        let options = PgConnectOptions::from_str(url).expect("PostgreSQL test URL");
        let port = options.get_port();
        let upstream = match options.get_socket().cloned().or_else(|| {
            options
                .get_host()
                .starts_with('/')
                .then(|| options.get_host().into())
        }) {
            Some(directory) => Upstream::Unix(directory.join(format!(".s.PGSQL.{port}"))),
            None => Upstream::Tcp(format!("{}:{port}", options.get_host())),
        };
        let directory = tempfile::Builder::new()
            .prefix("wrr")
            .tempdir()
            .expect("socket directory");
        let listener =
            tokio::net::UnixListener::bind(directory.path().join(format!(".s.PGSQL.{port}")))
                .expect("bind PostgreSQL socket endpoint");
        let wire = Arc::new(Mutex::new(Wire::default()));
        let upstream = Arc::new(upstream);
        let recorded = Arc::clone(&wire);
        let forwarder = tokio::spawn(async move {
            while let Ok((inbound, _)) = listener.accept().await {
                let upstream = Arc::clone(&upstream);
                let wire = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let unframed = Arc::new(AtomicUsize::new(0));
                    let (client_read, client_write) = inbound.into_split();
                    match upstream.as_ref() {
                        Upstream::Tcp(address) => {
                            let Ok(outbound) = tokio::net::TcpStream::connect(address).await else {
                                return;
                            };
                            let (server_read, server_write) = outbound.into_split();
                            tokio::join!(
                                relay(
                                    client_read,
                                    server_write,
                                    Arc::clone(&wire),
                                    Arc::clone(&unframed),
                                    true
                                ),
                                relay(server_read, client_write, wire, unframed, false),
                            );
                        }
                        Upstream::Unix(path) => {
                            let Ok(outbound) = tokio::net::UnixStream::connect(path).await else {
                                return;
                            };
                            let (server_read, server_write) = outbound.into_split();
                            tokio::join!(
                                relay(
                                    client_read,
                                    server_write,
                                    Arc::clone(&wire),
                                    Arc::clone(&unframed),
                                    true
                                ),
                                relay(server_read, client_write, wire, unframed, false),
                            );
                        }
                    }
                });
            }
        });
        Self {
            directory,
            port,
            wire,
            forwarder,
        }
    }

    /// Forget what was recorded so far, such as the deliberate refusals a
    /// test provoked before the interval it asserts on.
    fn reset_wire(&self) {
        *self.wire.lock().expect("wire record") = Wire::default();
    }

    fn take_wire(&self) -> Wire {
        std::mem::take(&mut *self.wire.lock().expect("wire record"))
    }
}

impl Drop for SocketEndpoint {
    fn drop(&mut self) {
        self.forwarder.abort();
    }
}

struct Roles {
    database_owner: String,
    schema_owner: String,
    migrator: String,
    runtime: String,
    old_runtime: String,
    installer: String,
    definer: String,
}

impl Roles {
    fn all(&self) -> [&String; 7] {
        [
            &self.migrator,
            &self.runtime,
            &self.old_runtime,
            &self.installer,
            &self.schema_owner,
            &self.definer,
            &self.database_owner,
        ]
    }
}

/// A database prepared by the owner phase, with the operator handoff the
/// runtime needs to start against it.
struct Prepared {
    cluster: PgPool,
    admin: PgPool,
    database: String,
    roles: Roles,
    endpoint: SocketEndpoint,
    lineage: AttestedLineage,
    runtime_url: String,
    handoff: String,
    owner_url: String,
    plan: RestrictedRuntimeOwnerPlan,
}

/// Roles, a database and its two schemas, before any owner phase has run.
struct Provisioned {
    cluster: PgPool,
    admin: PgPool,
    database: String,
    roles: Roles,
    base: PgConnectOptions,
    owner_url: String,
    plan: RestrictedRuntimeOwnerPlan,
}

/// Run the owner phase on a fresh handle: the migrator login assumes the
/// NOLOGIN schema owner.
async fn run_owner_phase(
    owner_url: &str,
    plan: &RestrictedRuntimeOwnerPlan,
) -> Result<AttestedLineage, RestrictedRuntimeOwnerError> {
    let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_owned());
    owner_config.pool_size = 2;
    let owner = Database::from_config("restricted runtime owner", &owner_config)
        .await
        .expect("owner database");
    prepare_restricted_runtime_owner(&owner, plan)
        .await
        .map(|evidence| evidence.attested_lineage().clone())
}

async fn drop_fixture(cluster: PgPool, admin: PgPool, database: &str, roles: &Roles) {
    admin.close().await;
    execute(&cluster, &format!("DROP DATABASE {database} WITH (FORCE)")).await;
    for role in roles.all() {
        execute(&cluster, &format!("DROP ROLE IF EXISTS {role}")).await;
    }
    cluster.close().await;
}

impl Provisioned {
    async fn new(url: &str) -> Self {
        let cluster = PgPool::connect(url).await.expect("cluster administrator");
        let suffix = Uuid::new_v4().simple().to_string();
        let database = format!("restricted_db_{suffix}");
        let roles = Roles {
            database_owner: format!("restricted_dbowner_{suffix}"),
            schema_owner: format!("restricted_owner_{suffix}"),
            migrator: format!("restricted_migrator_{suffix}"),
            runtime: format!("restricted_runtime_{suffix}"),
            old_runtime: format!("restricted_old_{suffix}"),
            installer: format!("restricted_installer_{suffix}"),
            definer: format!("restricted_definer_{suffix}"),
        };
        for role in [&roles.database_owner, &roles.schema_owner, &roles.definer] {
            execute(
                &cluster,
                &format!("CREATE ROLE {role} NOLOGIN NOINHERIT {RESTRICTED}"),
            )
            .await;
        }
        for role in [
            &roles.migrator,
            &roles.runtime,
            &roles.old_runtime,
            &roles.installer,
        ] {
            execute(
                &cluster,
                &format!("CREATE ROLE {role} LOGIN NOINHERIT {RESTRICTED}"),
            )
            .await;
        }
        for statement in [
            format!(
                "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                roles.schema_owner, roles.migrator
            ),
            format!(
                "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE",
                roles.definer, roles.installer
            ),
            format!("CREATE DATABASE {database} OWNER {}", roles.database_owner),
            format!("REVOKE CONNECT, TEMPORARY ON DATABASE {database} FROM PUBLIC"),
            format!(
                "GRANT CONNECT ON DATABASE {database} TO {}, {}, {}",
                roles.migrator, roles.runtime, roles.installer
            ),
        ] {
            execute(&cluster, &statement).await;
        }

        let base = PgConnectOptions::from_str(url).expect("PostgreSQL test URL");
        let admin = PgPool::connect_with(base.clone().database(&database))
            .await
            .expect("database administrator");
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!(
                "CREATE SCHEMA {SCHEMA} AUTHORIZATION {}",
                roles.schema_owner
            ),
            format!(
                "CREATE SCHEMA {TRUSTED_SCHEMA} AUTHORIZATION {}",
                roles.definer
            ),
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!(
                "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {}, {}",
                roles.schema_owner, roles.definer
            ),
        ] {
            execute(&admin, &statement).await;
        }

        let owner_url = url_with(
            url,
            &database,
            &roles.migrator,
            &format!("-c role={} -c search_path={SCHEMA}", roles.schema_owner),
        );
        let plan = RestrictedRuntimeOwnerPlan {
            lineage: LineageConfig {
                deployment_uuid: Some(DeploymentUuid(Uuid::new_v4())),
                action: Some(LineageAction::Enroll),
            },
            migrator: role(&roles.migrator),
            schema_owner: role(&roles.schema_owner),
            runtime: role(&roles.runtime),
            attestation_definer: role(&roles.definer),
            vapid_root_key: SESSION_KEY.as_bytes().to_vec(),
            #[cfg(feature = "clustering")]
            clustering: None,
        };
        Self {
            cluster,
            admin,
            database,
            roles,
            base,
            owner_url,
            plan,
        }
    }

    async fn cleanup(self) {
        drop_fixture(self.cluster, self.admin, &self.database, &self.roles).await;
    }
}

impl Prepared {
    async fn new(url: &str) -> Self {
        Self::from_provisioned(Provisioned::new(url).await, url).await
    }

    /// Run the owner phase with the provisioned plan, then install the
    /// attestor and build the runtime handoff.
    async fn from_provisioned(provisioned: Provisioned, url: &str) -> Self {
        let lineage = run_owner_phase(&provisioned.owner_url, &provisioned.plan)
            .await
            .expect("owner phase prepares the runtime schemas");
        Self::attest(provisioned, lineage, url).await
    }

    /// Install the attestor on a database the owner phase has prepared and
    /// build the runtime handoff for it.
    async fn attest(provisioned: Provisioned, lineage: AttestedLineage, url: &str) -> Self {
        let Provisioned {
            cluster,
            admin,
            database,
            roles,
            base,
            owner_url,
            plan,
        } = provisioned;

        // The attestor is installed by its own login, after the owner phase.
        let operation_id = Uuid::new_v4();
        install(
            &base
                .clone()
                .database(&database)
                .username(&roles.installer)
                .options([("search_path", SCHEMA)]),
            PhysicalAttestationConfig::new(
                lineage.clone(),
                role(&roles.installer),
                role(&roles.runtime),
                role(&roles.definer),
                PgSchemaName(TRUSTED_SCHEMA.to_owned()),
            )
            .expect("attestation configuration"),
            operation_id,
        )
        .await
        .expect("install physical attestor");

        let endpoint = SocketEndpoint::for_url(url).await;
        let runtime_url = format!(
            "postgresql:///{database}?host={}&port={}&user={}&options=-c%20search_path%3D{SCHEMA}",
            endpoint.directory.path().display(),
            endpoint.port,
            roles.runtime
        );
        let handoff = handoff(&admin, &roles, &lineage, &runtime_url, operation_id).await;
        Self {
            cluster,
            admin,
            database,
            roles,
            endpoint,
            lineage,
            runtime_url,
            handoff,
            owner_url,
            plan,
        }
    }

    /// Run the owner phase again on the already prepared database.
    async fn rerun_owner_phase(&self) -> Result<AttestedLineage, RestrictedRuntimeOwnerError> {
        run_owner_phase(&self.owner_url, &self.plan).await
    }

    /// Environment that starts the server under the runtime login.
    fn runtime_env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("WADDLE_DB_DRIVER", "postgres".to_owned()),
            ("WADDLE_DATABASE_URL", self.runtime_url.clone()),
            (
                "WADDLE_DEPLOYMENT_UUID",
                self.lineage.deployment_uuid.to_string(),
            ),
            ("WADDLE_GIT_SHA", RELEASE_GIT_SHA.to_owned()),
            ("WADDLE_DB_RUNTIME_ATTESTATION", self.handoff.clone()),
            ("WADDLE_DB_MIGRATOR_LOGIN", self.roles.migrator.clone()),
            (
                "WADDLE_XMPP_MAM_SCHEMA_OWNER",
                self.roles.schema_owner.clone(),
            ),
            ("WADDLE_XMPP_MAM_CLAIM_SCHEMA", SCHEMA.to_owned()),
            ("WADDLE_SESSION_KEY", SESSION_KEY.to_owned()),
        ]
    }

    /// Run the binary to completion, for a startup that must refuse.
    async fn refused_startup(&self, extra: &[(&str, &str)]) -> (String, bool) {
        let temp = tempfile::tempdir().expect("startup directory");
        let port_file = temp.path().join("port");
        let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_waddle-server"));
        for (key, _) in std::env::vars_os() {
            let key_text = key.to_string_lossy();
            if key_text.starts_with("WADDLE_") || key_text.starts_with("OTEL_") {
                server.env_remove(key);
            }
        }
        server
            .env(
                "WADDLE_OCCUPANT_ID_SECRET",
                "integration-test-occupant-id-secret-32-bytes-long",
            )
            .env("WADDLE_CERTS_EPHEMERAL", "true")
            .env("WADDLE_TEST_FIXED_ACCOUNT_ENABLED", "true")
            .env(
                "WADDLE_TEST_FIXED_ACCOUNT_PASSWORD",
                "refused-startup-password",
            )
            .env("WADDLE_SERVER_OWNER_LOCALPARTS", "admin")
            .env("WADDLE_XMPP_DOMAIN", "localhost")
            .env("WADDLE_XMPP_PUSH_SERVICE_ALLOW_IN_MEMORY", "true")
            .env("WADDLE_UPLOAD_DIR", temp.path().join("uploads"))
            .env("WADDLE_HTTP_ADDR", "127.0.0.1:0")
            .env("WADDLE_HTTP_PORT_FILE", &port_file)
            .kill_on_drop(true);
        for (key, value) in self.runtime_env() {
            server.env(key, value);
        }
        for (key, value) in extra {
            server.env(key, value);
        }
        let output = tokio::time::timeout(Duration::from_secs(90), server.output())
            .await
            .expect("refused startup exits")
            .expect("run server binary");
        assert!(
            !output.status.success(),
            "startup must refuse, but the server exited successfully"
        );
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (text, port_file.exists())
    }

    async fn cleanup(self) {
        drop_fixture(self.cluster, self.admin, &self.database, &self.roles).await;
        drop(self.endpoint);
    }
}

fn role(name: &str) -> PgRoleName {
    PgRoleName::new(name).expect("generated role name")
}

async fn execute(pool: &PgPool, statement: &str) {
    sqlx::query(statement)
        .execute(pool)
        .await
        .unwrap_or_else(|error| panic!("{statement}: {error}"));
}

fn url_with(base: &str, database: &str, user: &str, options: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL test URL");
    let retained: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !matches!(key.as_ref(), "user" | "password" | "dbname" | "options"))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
        .append_pair("dbname", database)
        .append_pair("user", user)
        .append_pair("options", options);
    url.to_string()
}

/// The operator handoff. Every pin is read by the administrator, never from
/// the runtime connection it will be checked against.
async fn handoff(
    admin: &PgPool,
    roles: &Roles,
    lineage: &AttestedLineage,
    runtime_url: &str,
    operation_id: Uuid,
) -> String {
    // Record the endpoint exactly as the runtime's connection options name it.
    let endpoint = PgConnectOptions::from_str(runtime_url).expect("runtime URL");
    let socket_directory = endpoint
        .get_socket()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| {
            endpoint
                .get_host()
                .starts_with('/')
                .then(|| endpoint.get_host().to_owned())
        });
    let row = sqlx::query(
        "SELECT db.datdba::bigint, n.nspowner::bigint, runtime.oid::bigint, \
                installer.oid::bigint, definer.oid::bigint, trusted.oid::bigint, \
                lineage.oid::bigint, lineage_idx.indexrelid::bigint, attestor.oid::bigint, \
                ledger.oid::bigint, ledger_idx.indexrelid::bigint, epoch.oid::bigint, \
                epoch_idx.indexrelid::bigint, manifest.oid::bigint, old_runtime.oid::bigint \
         FROM pg_catalog.pg_database db \
         JOIN pg_catalog.pg_namespace n ON n.nspname = $1 \
         JOIN pg_catalog.pg_class lineage \
           ON lineage.relnamespace = n.oid AND lineage.relname = '_lineage' \
         JOIN pg_catalog.pg_index lineage_idx \
           ON lineage_idx.indrelid = lineage.oid AND lineage_idx.indisprimary \
         JOIN pg_catalog.pg_class ledger \
           ON ledger.relnamespace = n.oid AND ledger.relname = '_migrations' \
         JOIN pg_catalog.pg_index ledger_idx \
           ON ledger_idx.indrelid = ledger.oid AND ledger_idx.indisprimary \
         JOIN pg_catalog.pg_class epoch \
           ON epoch.relnamespace = n.oid AND epoch.relname = 'ingress_protocol_epoch' \
         JOIN pg_catalog.pg_index epoch_idx \
           ON epoch_idx.indrelid = epoch.oid AND epoch_idx.indisprimary \
         JOIN pg_catalog.pg_class manifest \
           ON manifest.relnamespace = n.oid AND manifest.relname = 'ingress_epoch_guard_manifest' \
         JOIN pg_catalog.pg_namespace trusted ON trusted.nspname = $2 \
         JOIN pg_catalog.pg_proc attestor \
           ON attestor.pronamespace = trusted.oid AND attestor.proname = 'read_physical_identity' \
         JOIN pg_catalog.pg_roles runtime ON runtime.rolname = $3 \
         JOIN pg_catalog.pg_roles installer ON installer.rolname = $4 \
         JOIN pg_catalog.pg_roles definer ON definer.rolname = $5 \
         JOIN pg_catalog.pg_roles old_runtime ON old_runtime.rolname = $6 \
         WHERE db.datname = pg_catalog.current_database()",
    )
    .bind(SCHEMA)
    .bind(TRUSTED_SCHEMA)
    .bind(&roles.runtime)
    .bind(&roles.installer)
    .bind(&roles.definer)
    .bind(&roles.old_runtime)
    .fetch_one(admin)
    .await
    .expect("administrator records the startup pins");
    let oid = |index| u32::try_from(row.get::<i64, _>(index)).expect("OID fits u32");
    let physical = lineage
        .postgres_identity
        .as_ref()
        .expect("PostgreSQL identity");
    serde_json::json!({
        "version": 3,
        "release_git_sha": RELEASE_GIT_SHA,
        "deployment_uuid": lineage.deployment_uuid.to_string(),
        "lineage_uuid": lineage.lineage_uuid.to_string(),
        "system_identifier": physical.system_identifier.to_string(),
        "database_oid": physical.database.oid,
        "database_name": physical.database.name.0,
        "schema_oid": physical.schema.oid,
        "schema_name": physical.schema.name.0,
        "installer_login": roles.installer,
        "runtime_login": roles.runtime,
        "definer_role": roles.definer,
        "trusted_schema": TRUSTED_SCHEMA,
        "cutover": {
            "transition_id": Uuid::new_v4().to_string(),
            "primary": {
                "provider": "test-postgresql",
                "cluster_id": "disposable-cluster",
                "instance_id": "disposable-primary",
                "endpoint_host": endpoint.get_host(),
                "endpoint_port": endpoint.get_port(),
                "endpoint_socket_dir": socket_directory,
                "observation_record_id": Uuid::new_v4().to_string(),
            },
            "old_runtime": {
                "login": roles.old_runtime,
                "role_oid": oid(14),
                "credential_cutoff_record_id": Uuid::new_v4().to_string(),
            },
            "listener_cutoff_record_id": Uuid::new_v4().to_string(),
            "backend_cutoff_record_id": Uuid::new_v4().to_string(),
            "prepared_transaction_cutoff_record_id": Uuid::new_v4().to_string(),
            "mutator_policy": {
                "mode": "quiesced_for_serving_interval",
                "record_id": Uuid::new_v4().to_string(),
            },
        },
        "pins": {
            "database_owner_role_oid": oid(0),
            "schema_owner_role_oid": oid(1),
            "runtime_role_oid": oid(2),
            "installer_role_oid": oid(3),
            "definer_role_oid": oid(4),
            "trusted_schema_oid": oid(5),
            "lineage_relation_oid": oid(6),
            "lineage_primary_index_oid": oid(7),
            "attestor_function_oid": oid(8),
            "installation_operation_id": operation_id.to_string(),
            "ledger_relation_oid": oid(9),
            "ledger_primary_index_oid": oid(10),
            "epoch_relation_oid": oid(11),
            "epoch_primary_index_oid": oid(12),
            "guard_manifest_relation_oid": oid(13),
        }
    })
    .to_string()
}

/// The catalog state a restricted runtime must leave alone: every relation,
/// column, routine (with its `SECURITY DEFINER` flag and configuration),
/// constraint, trigger and type in the application and attestor schemas with
/// owner and ACL, every schema, the database ACL, default privileges, and
/// the role memberships of this fixture's roles. DDL or a grant of any kind
/// changes this list. An object created and dropped again between two
/// readings does not; [`Wire`] covers that for the runtime login.
async fn catalog_entries(admin: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT entry FROM ( \
           SELECT pg_catalog.format('class %s %s %s %s %s %s', c.oid, c.relname, c.relkind, \
                    c.relowner, c.relnatts, c.relacl) AS entry \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            WHERE n.nspname IN ($1, $2) \
           UNION ALL \
           SELECT pg_catalog.format('attribute %s %s %s %s %s %s', a.attrelid, a.attnum, \
                    a.attname, a.atttypid, a.attnotnull, a.attacl) \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            WHERE n.nspname IN ($1, $2) AND a.attnum > 0 \
           UNION ALL \
           SELECT pg_catalog.format('proc %s %s %s %s %s %s %s', p.oid, p.proname, p.proowner, \
                    p.proacl, pg_catalog.md5(p.prosrc), p.prosecdef, p.proconfig) \
             FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
            WHERE n.nspname IN ($1, $2) \
           UNION ALL \
           SELECT pg_catalog.format('constraint %s %s %s', k.oid, k.conname, k.conrelid) \
             FROM pg_catalog.pg_constraint k \
             JOIN pg_catalog.pg_namespace n ON n.oid = k.connamespace \
            WHERE n.nspname IN ($1, $2) \
           UNION ALL \
           SELECT pg_catalog.format('trigger %s %s %s', t.oid, t.tgname, t.tgrelid) \
             FROM pg_catalog.pg_trigger t \
             JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
            WHERE n.nspname IN ($1, $2) \
           UNION ALL \
           SELECT pg_catalog.format('namespace %s %s %s %s', n.oid, n.nspname, n.nspowner, n.nspacl) \
             FROM pg_catalog.pg_namespace n \
            WHERE n.nspname NOT LIKE 'pg\\_%' AND n.nspname <> 'information_schema' \
           UNION ALL \
           SELECT pg_catalog.format('type %s %s %s %s', t.oid, t.typname, t.typowner, t.typacl) \
             FROM pg_catalog.pg_type t \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace \
            WHERE n.nspname IN ($1, $2) \
           UNION ALL \
           SELECT pg_catalog.format('default_acl %s %s %s %s', da.defaclrole, \
                    da.defaclnamespace, da.defaclobjtype, da.defaclacl) \
             FROM pg_catalog.pg_default_acl da \
           UNION ALL \
           SELECT pg_catalog.format('membership %s %s %s %s %s', m.roleid::regrole, \
                    m.member::regrole, m.admin_option, m.inherit_option, m.set_option) \
             FROM pg_catalog.pg_auth_members m \
             JOIN pg_catalog.pg_roles r ON r.oid IN (m.roleid, m.member) \
            WHERE r.rolname LIKE '%' || pg_catalog.substr(pg_catalog.current_database(), 15) \
           UNION ALL \
           SELECT pg_catalog.format('database %s %s', d.datdba, d.datacl) \
             FROM pg_catalog.pg_database d \
            WHERE d.datname = pg_catalog.current_database() \
         ) entries ORDER BY entry",
    )
    .bind(SCHEMA)
    .bind(TRUSTED_SCHEMA)
    .fetch_all(admin)
    .await
    .expect("catalog entries")
}

/// Fail with the entries that differ, not the whole catalog.
async fn assert_catalog_unchanged(admin: &PgPool, before: &[String], context: &str) {
    let after = catalog_entries(admin).await;
    let removed: Vec<&String> = before
        .iter()
        .filter(|entry| !after.contains(entry))
        .collect();
    let added: Vec<&String> = after
        .iter()
        .filter(|entry| !before.contains(entry))
        .collect();
    assert!(
        removed.is_empty() && added.is_empty(),
        "{context}: removed {removed:#?}, added {added:#?}"
    );
}

async fn runtime_connection(prepared: &Prepared) -> PgConnection {
    PgConnection::connect_with(
        &PgConnectOptions::from_str(&prepared.runtime_url).expect("runtime URL"),
    )
    .await
    .expect("runtime login connects")
}

fn sqlstate(error: &sqlx::Error) -> Option<String> {
    error
        .as_database_error()
        .and_then(|error| error.code())
        .map(|code| code.into_owned())
}

async fn probe(url: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET {url}: {error}"))
        .status()
}

const POOL_LIMITS: RuntimePoolLimits = RuntimePoolLimits {
    main_connections: 2,
    ingress_connections: 1,
    acquire_timeout: Duration::from_secs(10),
};

fn startup_identity(prepared: &Prepared) -> RuntimeStartupIdentity {
    let lineage = LineageConfig {
        deployment_uuid: Some(prepared.lineage.deployment_uuid),
        action: None,
    };
    RuntimeStartupIdentity::from_json(&prepared.handoff, &lineage, RELEASE_GIT_SHA)
        .expect("operator handoff")
}

/// Run the library half of restricted startup: the pinned read-only
/// preflight, then the attested serving pool it authorizes.
async fn attach_serving_pool(prepared: &Prepared) -> DatabasePool {
    let identity = startup_identity(prepared);
    let preparation = identity
        .preflight(&prepared.runtime_url, POOL_LIMITS)
        .await
        .expect("pinned preflight under the runtime login");
    identity
        .attach_serving(preparation, &prepared.runtime_url, POOL_LIMITS, None)
        .await
        .expect("attested serving pool under the runtime login")
}

async fn assert_runtime_role_cannot_run_ddl(prepared: &Prepared) {
    let mut runtime = runtime_connection(prepared).await;
    for statement in [
        format!("CREATE TABLE {SCHEMA}.runtime_probe (id integer)"),
        "CREATE TEMPORARY TABLE runtime_probe (id integer)".to_owned(),
        format!("ALTER TABLE {SCHEMA}.webhook_deliveries ADD COLUMN probe integer"),
        format!("CREATE INDEX runtime_probe ON {SCHEMA}.webhook_deliveries (attempt_count)"),
        format!("DROP TABLE {SCHEMA}.webhook_deliveries"),
        format!("TRUNCATE {SCHEMA}.webhook_deliveries"),
    ] {
        let error = sqlx::query(&statement)
            .execute(&mut runtime)
            .await
            .expect_err("the runtime role must not run DDL");
        assert_eq!(
            sqlstate(&error).as_deref(),
            Some("42501"),
            "{statement} must fail with insufficient privilege: {error}"
        );
    }
    runtime.close().await.expect("close runtime probe");
}

async fn assert_runtime_owns_nothing(prepared: &Prepared) {
    let owned: i64 = sqlx::query_scalar(
        "SELECT (SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c \
                  JOIN pg_catalog.pg_roles r ON r.oid = c.relowner WHERE r.rolname = $1) \
              + (SELECT pg_catalog.count(*) FROM pg_catalog.pg_proc p \
                  JOIN pg_catalog.pg_roles r ON r.oid = p.proowner WHERE r.rolname = $1) \
              + (SELECT pg_catalog.count(*) FROM pg_catalog.pg_namespace n \
                  JOIN pg_catalog.pg_roles r ON r.oid = n.nspowner WHERE r.rolname = $1)",
    )
    .bind(&prepared.roles.runtime)
    .fetch_one(&prepared.admin)
    .await
    .expect("count runtime-owned objects");
    assert_eq!(owned, 0, "the runtime role must own no catalog object");
}

/// Attach every store a restricted runtime attaches whether or not it is
/// clustered, in startup order, through the selection functions startup
/// calls. MAM and the ingress authority are attached by the caller, which
/// knows the topology.
async fn attach_single_node_stores(
    prepared: &Prepared,
    pool: &DatabasePool,
    inherited: &ResolvedXmppDatabaseUrl,
) {
    let main = pool.global();
    MucDestroyCompletionOutboxStore::attach_existing(main.clone())
        .await
        .expect("attach MUC destroy completion outbox");
    waddle_server::test_support::create_inbox_startup_for_test(inherited, main)
        .await
        .expect("attach inbox");
    DatabaseSpacesMetadataStore::attach_existing(main.clone())
        .await
        .expect("attach spaces metadata");
    DatabaseChannelSpaceLinkStore::attach_existing(main.clone())
        .await
        .expect("attach channel-space links");
    waddle_server::server::create_pubsub_storage(main, inherited)
        .await
        .expect("attach PubSub");
    CallTeardownOutboxStore::attach_existing(
        main.clone(),
        SharedNodeIdentity::new(NodeIdentity::local()),
    )
    .await
    .expect("attach call teardown outbox");
    RoomEffectOutboxStore::attach_serving(main.clone())
        .await
        .expect("attach room effect outbox");
    waddle_server::test_support::attach_pending_startup(inherited, main)
        .await
        .expect("attach pending delivery");
    waddle_server::test_support::attach_sm_startup(inherited, main, false)
        .await
        .expect("attach stream management");
    // Push registrations, Push Service, VAPID, notification outbox and
    // notification activity, with the root key the owner phase sealed under.
    let _push = verify_attested_push_notification_stores(
        main.clone(),
        SESSION_KEY.as_bytes(),
        "push.localhost".parse().expect("push service JID"),
    )
    .await
    .expect("attach Push and notification stores");
    waddle_server::test_support::attach_webhook_delivery_ledger(main)
        .await
        .expect("attach webhook delivery ledger");
    verify_attested_room_observation_schema(
        main,
        &prepared.lineage,
        &role(&prepared.roles.migrator),
    )
    .await
    .expect("verify room observation schema");
}

/// The owner phase prepares every store the non-clustered runtime attaches
/// at startup, and the runtime role attaches each one read-only on its
/// attested pool. The attachments run in startup order through the same
/// selection functions startup calls.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_prepares_the_stores_the_runtime_role_attaches() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    prepared.endpoint.reset_wire();
    assert_runtime_role_cannot_run_ddl(&prepared).await;
    // The relay in front of PostgreSQL records exactly those attempts, which
    // is what lets the serving test assert that it recorded none.
    let wire = prepared.endpoint.take_wire();
    assert_eq!(
        wire.catalog_statements.len(),
        6,
        "{:#?}",
        wire.catalog_statements
    );
    assert_eq!(
        wire.errors
            .iter()
            .filter(|(sqlstate, _)| sqlstate == "42501")
            .count(),
        6,
        "{:#?}",
        wire.errors
    );
    let before = catalog_entries(&prepared.admin).await;

    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());

    attach_single_node_stores(&prepared, &pool, &inherited).await;
    DatabaseMamStorage::attach_single_node(
        main,
        role(&prepared.roles.schema_owner),
        PgSchemaName(SCHEMA.to_owned()),
    )
    .await
    .expect("attach MAM");
    let ingress = waddle_server::test_support::create_ingress_authority_at_startup(
        &ingress_config(),
        &pool,
        &runtime_lineage(&prepared),
    )
    .await
    .expect("ingress takes the epoch row lock under the runtime role");
    drop(ingress);

    // Attachment is read-only: no DDL ran and no ACL changed, so the catalog
    // is byte-for-byte what the owner phase left, and the runtime owns nothing.
    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "runtime attachment changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;

    drop(pool);
    prepared.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_runtime_refuses_instead_of_creating_or_clustering() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;

    // Clustering has no owner-prepared attachment: refuse before a listener.
    let (output, listener_published) = prepared
        .refused_startup(&[("WADDLE_CLUSTERING_ENABLED", "true")])
        .await;
    assert!(
        output.contains("cannot serve with clustering enabled"),
        "clustered restricted startup must refuse with its typed error: {output}"
    );
    assert!(
        !listener_published,
        "a refused startup published a listener"
    );

    // The webhook ledger refuses a privilege the owner phase did not grant,
    // and a missing ledger is an attachment error, not something to create.
    let pool = attach_serving_pool(&prepared).await;
    execute(
        &prepared.admin,
        &format!(
            "GRANT TRUNCATE ON {SCHEMA}.webhook_deliveries TO {}",
            prepared.roles.runtime
        ),
    )
    .await;
    assert!(matches!(
        waddle_server::test_support::attach_webhook_delivery_ledger(pool.global()).await,
        Err(WebhookDeliveryAttachError::UnsafeRuntimePrivileges {
            component: "webhook_deliveries"
        })
    ));
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.webhook_deliveries"),
    )
    .await;
    assert!(matches!(
        waddle_server::test_support::attach_webhook_delivery_ledger(pool.global()).await,
        Err(WebhookDeliveryAttachError::OwnerPreparationMissing(_))
    ));
    drop(pool);
    assert!(
        !relation_exists(&prepared.admin, "webhook_deliveries").await,
        "the runtime created the missing webhook ledger"
    );

    // One relation the server attaches at startup is missing. The real
    // binary must fail closed on that attachment and must not create it.
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.clustering_muc_destroy_outbox"),
    )
    .await;
    let before = catalog_entries(&prepared.admin).await;
    let (output, listener_published) = prepared.refused_startup(&[]).await;
    assert!(
        output.contains("failed to attach MUC destroy completion outbox"),
        "a missing owner-prepared relation must fail its attachment: {output}"
    );
    assert!(
        !listener_published,
        "a refused startup published a listener"
    );
    assert!(
        !relation_exists(&prepared.admin, "clustering_muc_destroy_outbox").await,
        "the runtime created the missing relation"
    );
    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "a refused startup changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;

    prepared.cleanup().await;
}

fn ingress_config() -> IngressConfig {
    IngressConfig {
        pool_size: POOL_LIMITS.ingress_connections,
        ..IngressConfig::default()
    }
}

fn runtime_lineage(prepared: &Prepared) -> LineageConfig {
    LineageConfig {
        deployment_uuid: Some(prepared.lineage.deployment_uuid),
        action: None,
    }
}

async fn preflight(prepared: &Prepared) -> Result<(), RuntimeStartupError> {
    startup_identity(prepared)
        .preflight(&prepared.runtime_url, POOL_LIMITS)
        .await
        .map(|_| ())
}

/// The runtime holds `UPDATE (id)` on the epoch row so that it can lock it
/// `FOR SHARE`. That grant cannot change the epoch, and the startup
/// preflight requires exactly it: SELECT alone, or anything wider, refuses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_locks_the_epoch_row_but_cannot_change_it() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let runtime = &prepared.roles.runtime;
    let epoch = format!("{SCHEMA}.ingress_protocol_epoch");

    let mut connection = runtime_connection(&prepared).await;
    let locked: i64 =
        sqlx::query_scalar(&format!("SELECT epoch FROM {epoch} WHERE id = 1 FOR SHARE"))
            .fetch_one(&mut connection)
            .await
            .expect("the runtime locks the epoch row for share");
    assert_eq!(locked, 0);
    // The column grant reaches the guard trigger, which refuses every
    // change that is not an owner-run epoch step; every other statement is
    // refused for lack of privilege before any trigger or constraint runs.
    for (statement, sqlstate) in [
        (format!("UPDATE {epoch} SET id = 2"), "P0001"),
        (format!("UPDATE {epoch} SET id = 1"), "P0001"),
        (format!("UPDATE {epoch} SET epoch = 1"), "42501"),
        (
            format!("UPDATE {epoch} SET activated_at = pg_catalog.now()"),
            "42501",
        ),
        (
            format!("UPDATE {epoch} SET lineage_uuid = pg_catalog.gen_random_uuid()"),
            "42501",
        ),
        (
            format!("INSERT INTO {epoch} (id, epoch) VALUES (2, 0)"),
            "42501",
        ),
        (format!("DELETE FROM {epoch}"), "42501"),
    ] {
        let error = sqlx::query(&statement)
            .execute(&mut connection)
            .await
            .expect_err("the runtime must not change the epoch row");
        assert_eq!(
            self::sqlstate(&error).as_deref(),
            Some(sqlstate),
            "{statement}: {error}"
        );
    }
    connection.close().await.expect("close runtime probe");
    let unchanged: (i32, i64, bool, bool) = sqlx::query_as(&format!(
        "SELECT id, epoch, activated_at IS NULL, lineage_uuid IS NULL FROM {epoch}"
    ))
    .fetch_one(&prepared.admin)
    .await
    .expect("read the epoch row");
    assert_eq!(unchanged, (1, 0, true, true));

    preflight(&prepared)
        .await
        .expect("the preflight accepts the serving epoch grant");
    // Each deviation is applied and then undone; the grant the owner phase
    // made is restored at the end of every round.
    let restore = format!("GRANT UPDATE (id) ON {epoch} TO {runtime}");
    for (deviation, undo) in [
        (
            format!("REVOKE UPDATE (id) ON {epoch} FROM {runtime}"),
            restore.clone(),
        ),
        (
            format!("GRANT UPDATE ON {epoch} TO {runtime}"),
            format!("REVOKE UPDATE ON {epoch} FROM {runtime}; {restore}"),
        ),
        (
            format!("GRANT UPDATE (epoch) ON {epoch} TO {runtime}"),
            format!("REVOKE UPDATE (epoch) ON {epoch} FROM {runtime}"),
        ),
        (
            format!("GRANT UPDATE (id) ON {epoch} TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE UPDATE (id) ON {epoch} FROM {runtime}; {restore}"),
        ),
        (
            format!("GRANT SELECT (id) ON {epoch} TO {runtime}"),
            format!("REVOKE SELECT (id) ON {epoch} FROM {runtime}"),
        ),
        (
            format!("GRANT DELETE ON {epoch} TO {runtime}"),
            format!("REVOKE DELETE ON {epoch} FROM {runtime}"),
        ),
        (
            format!(
                "GRANT UPDATE (table_name) ON {SCHEMA}.ingress_epoch_guard_manifest TO {runtime}"
            ),
            format!(
                "REVOKE UPDATE (table_name) ON {SCHEMA}.ingress_epoch_guard_manifest FROM {runtime}"
            ),
        ),
    ] {
        execute(&prepared.admin, &deviation).await;
        assert!(
            matches!(
                preflight(&prepared).await,
                Err(RuntimeStartupError::PinnedPreflightIndeterminate(
                    DarkPinnedEntryError::Catalog(_)
                ))
            ),
            "the preflight must refuse after: {deviation}"
        );
        for statement in undo.split("; ") {
            execute(&prepared.admin, statement).await;
        }
        preflight(&prepared).await.unwrap_or_else(|error| {
            panic!("the preflight must pass after undoing {deviation}: {error}")
        });
    }

    prepared.cleanup().await;
}

/// Alias garbage collection runs on the serving pool under the runtime role.
/// A terminal message past its retention is recorded and collected through
/// the production code path; the collection and the orphan-proof sweep that
/// follows it each begin through the typed ingress entry, and the runtime
/// grants cover every statement they issue.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alias_gc_collects_an_expired_message_under_the_runtime_role() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let store = PostgresIngressSubstrate::open(pool.global().clone()).expect("ingress substrate");
    let digest = |byte: u8| SemanticDigest::from_storage(1, [byte; 32]).expect("fixture digest");
    let expired = MessageKey::new();
    let retained = MessageKey::new();
    let mut tx = store.begin().await.expect("attested ingress writer");
    for (key, byte) in [(expired, 1), (retained, 2)] {
        store
            .record_message(&mut tx, key, &digest(byte), None)
            .await
            .expect("record a canonical message");
    }
    store
        .terminalize_message(
            &mut tx,
            expired,
            chrono::Utc::now() - ALIAS_RETENTION - chrono::Duration::seconds(1),
        )
        .await
        .expect("terminalize past the retention cutoff");
    tx.commit().await.expect("commit the fixture messages");
    prepared.endpoint.reset_wire();

    let outcome = store
        .gc_expired_aliases(
            chrono::Utc::now(),
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + Duration::from_secs(30),
                lock_timeout: Duration::from_secs(5),
                statement_timeout: Duration::from_secs(10),
                scan_timeout: Duration::from_secs(10),
                progress: AliasGcProgress::default(),
            },
        )
        .await
        .expect("alias GC under the runtime role");
    assert_eq!(outcome.deleted_messages, 1);
    assert!(outcome.completed, "the pass and its orphan sweep finish");

    let remaining: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT message_key FROM {SCHEMA}.ingress_messages ORDER BY message_key"
    ))
    .fetch_all(&prepared.admin)
    .await
    .expect("read the remaining canonical messages");
    assert_eq!(remaining, vec![retained.to_storage()]);
    let wire = prepared.endpoint.take_wire();
    assert!(
        wire.errors.is_empty(),
        "alias GC drew errors on a runtime connection: {:#?}",
        wire.errors
    );

    drop(store);
    drop(pool);
    prepared.cleanup().await;
}

/// MAM admits `pg_monitor` in the schema ACL only for the owner-granted,
/// non-grantable USAGE that room observation preparation issues.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mam_admits_only_the_exact_monitor_schema_grant() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let mam = || {
        DatabaseMamStorage::attach_single_node(
            main,
            role(&prepared.roles.schema_owner),
            PgSchemaName(SCHEMA.to_owned()),
        )
    };
    mam().await.expect("MAM attaches with the monitor grant");
    let owner = &prepared.roles.schema_owner;
    let database_owner = &prepared.roles.database_owner;
    let monitor_grantor = |from: &str, to: &str| {
        format!(
            "UPDATE pg_catalog.pg_namespace SET nspacl = pg_catalog.array_replace(nspacl, \
             'pg_monitor=U/{from}'::pg_catalog.aclitem, 'pg_monitor=U/{to}'::pg_catalog.aclitem) \
             WHERE nspname = '{SCHEMA}'"
        )
    };

    // A clustered selection without a live claim pair refuses before any
    // attachment; the single-node selection is the one that attaches.
    #[cfg(feature = "clustering")]
    {
        let handoff = waddle_server::server::MamOwnerClaimHandoff::new(
            role(owner),
            PgSchemaName(SCHEMA.to_owned()),
        )
        .expect("MAM owner handoff");
        let inherited = ResolvedXmppDatabaseUrl::InheritedGlobal(prepared.runtime_url.clone());
        let refused = waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&handoff),
            true,
            false,
            main,
        )
        .await
        .expect_err("clustered attested MAM must refuse without a claim pair");
        assert_eq!(
            refused.to_string(),
            "clustered attested MAM storage requires live claim handles"
        );
        waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&handoff),
            false,
            false,
            main,
        )
        .await
        .expect("single-node attested MAM selection attaches");
    }

    for (deviation, undo) in [
        (
            format!("GRANT CREATE ON SCHEMA {SCHEMA} TO pg_monitor"),
            format!("REVOKE CREATE ON SCHEMA {SCHEMA} FROM pg_monitor"),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO pg_monitor WITH GRANT OPTION"),
            format!(
                "REVOKE USAGE ON SCHEMA {SCHEMA} FROM pg_monitor; GRANT USAGE ON SCHEMA {SCHEMA} TO pg_monitor"
            ),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO pg_read_all_stats"),
            format!("REVOKE USAGE ON SCHEMA {SCHEMA} FROM pg_read_all_stats"),
        ),
        (
            format!("GRANT USAGE ON SCHEMA {SCHEMA} TO {}", prepared.roles.old_runtime),
            format!(
                "REVOKE USAGE ON SCHEMA {SCHEMA} FROM {}",
                prepared.roles.old_runtime
            ),
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.mam_messages TO pg_monitor"),
            format!("REVOKE SELECT ON {SCHEMA}.mam_messages FROM pg_monitor"),
        ),
        // The same USAGE item, recorded as granted by a role that is not
        // the schema owner. Only a direct catalog edit produces that item
        // without also adding the grantor to the ACL.
        (monitor_grantor(owner, database_owner), monitor_grantor(database_owner, owner)),
    ] {
        execute(&prepared.admin, &deviation).await;
        assert!(
            matches!(
                mam().await,
                Err(DatabaseMamStorageError::UnsafeMamPrivileges)
            ),
            "MAM must refuse after: {deviation}"
        );
        for statement in undo.split("; ") {
            execute(&prepared.admin, statement).await;
        }
        mam()
            .await
            .unwrap_or_else(|error| panic!("MAM must attach after undoing {deviation}: {error}"));
    }

    drop(pool);
    prepared.cleanup().await;
}

/// Room observation exempts one owner-owned `SECURITY DEFINER` routine, the
/// PubSub attach-lock function, and only in its exact owner-prepared form.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn room_observation_exempts_only_the_exact_pubsub_lock_function() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let pool = attach_serving_pool(&prepared).await;
    let main = pool.global();
    let migrator = role(&prepared.roles.migrator);
    let observation =
        || verify_attested_room_observation_schema(main, &prepared.lineage, &migrator);
    observation()
        .await
        .expect("room observation verifies beside the PubSub lock function");

    let owner = &prepared.roles.schema_owner;
    let runtime = &prepared.roles.runtime;
    let lock = format!("{SCHEMA}.pubsub_runtime_attach_lock()");
    let original: String =
        sqlx::query_scalar("SELECT pg_catalog.pg_get_functiondef(pg_catalog.to_regprocedure($1))")
            .bind(&lock)
            .fetch_one(&prepared.admin)
            .await
            .expect("read the owner-prepared lock function");
    let definer = "LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp";
    let relay = format!("{SCHEMA}.runtime_relay()");
    let overload = format!("{SCHEMA}.pubsub_runtime_attach_lock(integer)");
    let elsewhere = "public.pubsub_runtime_attach_lock()";
    for (deviation, undo) in [
        // A different body under the same name, signature and ACL.
        (
            vec![format!(
                "CREATE OR REPLACE FUNCTION {lock} RETURNS void {definer} AS 'SELECT NULL::void'"
            )],
            vec![original.clone()],
        ),
        // The same body with a different configuration.
        (
            vec![format!(
                "ALTER FUNCTION {lock} SET search_path = pg_catalog, {SCHEMA}, pg_temp"
            )],
            vec![format!(
                "ALTER FUNCTION {lock} SET search_path = pg_catalog, pg_temp"
            )],
        ),
        // An extra grantee, and a grantable runtime grant.
        (
            vec![format!(
                "GRANT EXECUTE ON FUNCTION {lock} TO {}",
                prepared.roles.old_runtime
            )],
            vec![format!(
                "REVOKE EXECUTE ON FUNCTION {lock} FROM {}",
                prepared.roles.old_runtime
            )],
        ),
        (
            vec![format!(
                "GRANT EXECUTE ON FUNCTION {lock} TO {runtime} WITH GRANT OPTION"
            )],
            vec![
                format!("REVOKE EXECUTE ON FUNCTION {lock} FROM {runtime}"),
                format!("GRANT EXECUTE ON FUNCTION {lock} TO {runtime}"),
            ],
        ),
        // An overload of the same name, and an unrelated definer routine.
        (
            vec![
                format!("CREATE FUNCTION {overload} RETURNS void {definer} AS 'SELECT NULL::void'"),
                format!("ALTER FUNCTION {overload} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {overload} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {overload} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {overload}")],
        ),
        // EXECUTE reaching every role through PUBLIC.
        (
            vec![format!("GRANT EXECUTE ON FUNCTION {lock} TO PUBLIC")],
            vec![format!("REVOKE EXECUTE ON FUNCTION {lock} FROM PUBLIC")],
        ),
        // The same name and shape in another schema.
        (
            vec![
                format!(
                    "CREATE FUNCTION {elsewhere} RETURNS void {definer} AS 'SELECT NULL::void'"
                ),
                format!("ALTER FUNCTION {elsewhere} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {elsewhere} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {elsewhere} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {elsewhere}")],
        ),
        (
            vec![
                format!("CREATE FUNCTION {relay} RETURNS void {definer} AS 'SELECT NULL::void'"),
                format!("ALTER FUNCTION {relay} OWNER TO {owner}"),
                format!("REVOKE ALL ON FUNCTION {relay} FROM PUBLIC"),
                format!("GRANT EXECUTE ON FUNCTION {relay} TO {runtime}"),
            ],
            vec![format!("DROP FUNCTION {relay}")],
        ),
    ] {
        for statement in &deviation {
            execute(&prepared.admin, statement).await;
        }
        assert!(
            matches!(
                observation().await,
                Err(RoomObservationSchemaError::UnsafeRuntime)
            ),
            "room observation must refuse after: {deviation:?}"
        );
        for statement in &undo {
            execute(&prepared.admin, statement).await;
        }
        observation().await.unwrap_or_else(|error| {
            panic!("room observation must verify after undoing {deviation:?}: {error}")
        });
    }

    drop(pool);
    prepared.cleanup().await;
}

/// Catalog entries a repeated owner phase must leave alone. Pending delivery
/// owner preparation drops and recreates its ordinal trigger on every run, so
/// that one trigger is compared by name and relation, without its OID.
async fn owner_phase_stable_entries(admin: &PgPool) -> Vec<String> {
    const RECREATED: &str = "pending_notification_recovery_ordinal_immutable";
    let mut entries: Vec<String> = catalog_entries(admin)
        .await
        .into_iter()
        .map(|entry| {
            let mut fields = entry.split(' ');
            match (fields.next(), fields.next(), fields.next(), fields.next()) {
                (Some("trigger"), Some(_), Some(RECREATED), Some(relation)) => {
                    format!("trigger {RECREATED} {relation}")
                }
                _ => entry,
            }
        })
        .collect();
    entries.sort();
    entries
}

/// The owner phase can run again on a prepared database, classifies every
/// relation before granting, and reports a privilege it did not grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_is_idempotent_and_fails_closed_on_unclassified_state() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    let runtime = &prepared.roles.runtime;
    let before = owner_phase_stable_entries(&prepared.admin).await;
    let lineage = prepared
        .rerun_owner_phase()
        .await
        .expect("a second owner phase succeeds on a prepared database");
    assert_eq!(lineage, prepared.lineage);
    assert_eq!(
        owner_phase_stable_entries(&prepared.admin).await,
        before,
        "a repeated owner phase changed the catalog"
    );

    // A relation the matrix does not name fails the phase and gets nothing.
    execute(
        &prepared.admin,
        &format!("CREATE TABLE {SCHEMA}.unclassified_probe (id integer)"),
    )
    .await;
    execute(
        &prepared.admin,
        &format!(
            "ALTER TABLE {SCHEMA}.unclassified_probe OWNER TO {}",
            prepared.roles.schema_owner
        ),
    )
    .await;
    assert!(matches!(
        prepared.rerun_owner_phase().await,
        Err(RestrictedRuntimeOwnerError::UnclassifiedRelation { relation })
            if relation == "unclassified_probe"
    ));
    let granted: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT') \
             OR pg_catalog.has_table_privilege($1, $2, 'INSERT') \
             OR pg_catalog.has_table_privilege($1, $2, 'UPDATE') \
             OR pg_catalog.has_table_privilege($1, $2, 'DELETE')",
    )
    .bind(runtime)
    .bind(format!("{SCHEMA}.unclassified_probe"))
    .fetch_one(&prepared.admin)
    .await
    .expect("inspect the unclassified relation");
    assert!(!granted, "the runtime was granted an unclassified relation");
    execute(
        &prepared.admin,
        &format!("DROP TABLE {SCHEMA}.unclassified_probe"),
    )
    .await;

    // A privilege beyond the matrix, or any privilege held with the grant
    // option, is reported and left in place.
    let sequence = format!("{SCHEMA}.roster_items_id_seq");
    for (grant, undo, relation, privilege) in [
        (
            format!("GRANT DELETE ON {SCHEMA}.users TO {runtime}"),
            format!("REVOKE DELETE ON {SCHEMA}.users FROM {runtime}"),
            "users",
            "DELETE",
        ),
        (
            format!("GRANT UPDATE ON {SCHEMA}.ingress_owner_rejections TO {runtime}"),
            format!("REVOKE UPDATE ON {SCHEMA}.ingress_owner_rejections FROM {runtime}"),
            "ingress_owner_rejections",
            "UPDATE",
        ),
        (
            format!("GRANT UPDATE (epoch) ON {SCHEMA}.ingress_protocol_epoch TO {runtime}"),
            format!("REVOKE UPDATE (epoch) ON {SCHEMA}.ingress_protocol_epoch FROM {runtime}"),
            "ingress_protocol_epoch",
            "UPDATE(epoch)",
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.messages TO {runtime}"),
            format!("REVOKE SELECT ON {SCHEMA}.messages FROM {runtime}"),
            "messages",
            "SELECT",
        ),
        (
            format!("GRANT SELECT ON {SCHEMA}.users TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR SELECT ON {SCHEMA}.users FROM {runtime}"),
            "users",
            "SELECT WITH GRANT OPTION",
        ),
        (
            format!(
                "GRANT UPDATE (next_seq) ON {SCHEMA}.mam_archive_sequences TO {runtime} \
                 WITH GRANT OPTION"
            ),
            format!(
                "REVOKE GRANT OPTION FOR UPDATE (next_seq) ON {SCHEMA}.mam_archive_sequences \
                 FROM {runtime}"
            ),
            "mam_archive_sequences",
            "UPDATE WITH GRANT OPTION",
        ),
        (
            format!("GRANT USAGE ON SEQUENCE {sequence} TO {runtime} WITH GRANT OPTION"),
            format!("REVOKE GRANT OPTION FOR USAGE ON SEQUENCE {sequence} FROM {runtime}"),
            "roster_items_id_seq",
            "USAGE WITH GRANT OPTION",
        ),
    ] {
        execute(&prepared.admin, &grant).await;
        let result = prepared.rerun_owner_phase().await;
        assert!(
            matches!(
                &result,
                Err(RestrictedRuntimeOwnerError::ExcessRuntimePrivilege {
                    relation: found,
                    privilege: excess,
                }) if found == relation && excess == privilege
            ),
            "the owner phase must report {grant}: {:?}",
            result.err()
        );
        execute(&prepared.admin, &undo).await;
    }

    // A runtime role that is not restricted is refused the same way the
    // store attachments would refuse it.
    let owner = &prepared.roles.schema_owner;
    for (deviation, undo, component) in [
        (
            format!(
                "GRANT TEMPORARY ON DATABASE {} TO {runtime}",
                prepared.database
            ),
            format!(
                "REVOKE TEMPORARY ON DATABASE {} FROM {runtime}",
                prepared.database
            ),
            "database privileges",
        ),
        (
            format!("GRANT CREATE ON SCHEMA {SCHEMA} TO {runtime}"),
            format!("REVOKE CREATE ON SCHEMA {SCHEMA} FROM {runtime}"),
            "schema privileges",
        ),
        (
            format!("GRANT pg_read_all_stats TO {runtime}"),
            format!("REVOKE pg_read_all_stats FROM {runtime}"),
            "role membership",
        ),
        (
            format!("ALTER ROLE {runtime} CREATEDB"),
            format!("ALTER ROLE {runtime} NOCREATEDB"),
            "login attributes",
        ),
        (
            format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA {SCHEMA} \
                 GRANT SELECT ON TABLES TO {runtime}"
            ),
            format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA {SCHEMA} \
                 REVOKE SELECT ON TABLES FROM {runtime}"
            ),
            "default privileges",
        ),
    ] {
        execute(&prepared.admin, &deviation).await;
        let result = prepared.rerun_owner_phase().await;
        assert!(
            matches!(
                &result,
                Err(RestrictedRuntimeOwnerError::UnsafeRuntimeRole { component: found })
                    if *found == component
            ),
            "the owner phase must refuse {deviation}: {:?}",
            result.err()
        );
        execute(&prepared.admin, &undo).await;
    }
    prepared
        .rerun_owner_phase()
        .await
        .expect("the owner phase succeeds once the excess privileges are gone");

    prepared.cleanup().await;
}

/// PUBLIC privileges on the target schema, as the default `public` schema
/// carries, are refused before the owner phase runs any DDL.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_phase_refuses_a_schema_public_can_use() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let provisioned = Provisioned::new(&url).await;
    execute(
        &provisioned.admin,
        &format!("GRANT USAGE ON SCHEMA {SCHEMA} TO PUBLIC"),
    )
    .await;
    assert!(matches!(
        run_owner_phase(&provisioned.owner_url, &provisioned.plan).await,
        Err(RestrictedRuntimeOwnerError::PublicSchemaPrivilege { schema }) if schema == SCHEMA
    ));
    let relations: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
    )
    .bind(SCHEMA)
    .fetch_one(&provisioned.admin)
    .await
    .expect("count relations");
    assert_eq!(relations, 0, "a refused owner phase created relations");
    provisioned.cleanup().await;
}

async fn relation_exists(admin: &PgPool, relation: &str) -> bool {
    sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
        .bind(format!("{SCHEMA}.{relation}"))
        .fetch_one(admin)
        .await
        .expect("inspect relation")
}

const DOMAIN: &str = "localhost";
const NS_ROSTER: &str = "jabber:iq:roster";
const NS_VCARD: &str = "vcard-temp";
const NS_PRIVATE: &str = "jabber:iq:private";
const NS_PREFS: &str = "urn:waddle:test:restricted-prefs";
const NS_BLOCKING: &str = "urn:xmpp:blocking";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MAM: &str = "urn:xmpp:mam:2";
const NS_UPLOAD: &str = "urn:xmpp:http:upload:0";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA_FORMS: &str = "jabber:x:data";
const WIRE_BUDGET: Duration = Duration::from_secs(20);

async fn send_element(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize stanza");
    client
        .send(&String::from_utf8(bytes).expect("stanza UTF-8"))
        .await
        .expect("send stanza");
}

fn iq_request(set: bool, id: &str, to: Option<&str>, payload: Element) -> Iq {
    let to = to.map(|to| to.parse().expect("IQ recipient"));
    let id = id.to_owned();
    if set {
        Iq::Set {
            from: None,
            to,
            id,
            payload,
        }
    } else {
        Iq::Get {
            from: None,
            to,
            id,
            payload,
        }
    }
}

/// Send a request and return the payload of its `result` reply.
async fn iq_result(client: &mut WsXmppClient, request: Iq) -> Option<Element> {
    match round_trip(client, request).await {
        Iq::Result { payload, .. } => payload,
        other => panic!("expected an IQ result, received {other:?}"),
    }
}

/// Receive frames until `done` accepts the stanzas collected so far.
async fn collect_until(
    client: &mut WsXmppClient,
    what: &str,
    done: impl Fn(&[Element]) -> bool,
) -> Vec<Element> {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + WIRE_BUDGET;
    while !done(&collected) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = client
            .recv_timeout(remaining)
            .await
            .unwrap_or_else(|error| panic!("{what}: {error}; collected {collected:?}"));
        if let Ok(element) = frame.parse::<Element>() {
            collected.push(element);
        }
    }
    collected
}

fn has_descendant(element: &Element, name: &str, namespace: &str) -> bool {
    element
        .children()
        .any(|child| child.is(name, namespace) || has_descendant(child, name, namespace))
}

fn contains_text(element: &Element, needle: &str) -> bool {
    element.text().contains(needle) || element.children().any(|child| contains_text(child, needle))
}

fn text_element(name: &str, namespace: &str, text: &str) -> Element {
    Element::builder(name, namespace)
        .append(text.to_owned())
        .build()
}

fn chat_message(to: &str, kind: MessageType, id: &str, body: &str) -> Element {
    let mut message = Message::new(Some(to.parse().expect("message recipient")));
    message.id = Some(Id(id.to_owned()));
    message.type_ = kind;
    message.bodies.insert(Lang::new(), body.to_owned());
    message.into()
}

fn muc_presence(occupant: &str, leave: bool) -> Element {
    let mut presence = Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), occupant);
    presence = if leave {
        presence.attr(minidom::rxml::xml_ncname!("type").to_owned(), "unavailable")
    } else {
        presence.append(Element::builder("x", NS_MUC).build())
    };
    presence.build()
}

/// Run a MAM query and return the forwarded results up to its `<fin/>`.
async fn mam_query(client: &mut WsXmppClient, id: &str, archive: Option<&str>) -> Vec<Element> {
    send_iq(
        client,
        iq_request(true, id, archive, Element::builder("query", NS_MAM).build()),
    )
    .await;
    collect_until(client, "MAM query", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| stanza.attr("id") == Some(id) && has_descendant(stanza, "fin", NS_MAM))
    })
    .await
}

/// A roster get marks the resource interested, so the following roster set
/// produces a push to it; a second get returns the stored contact.
async fn exercise_roster(alice: &mut WsXmppClient) {
    iq_result(
        alice,
        iq_request(
            false,
            "roster-interest",
            None,
            Element::builder("query", NS_ROSTER).build(),
        ),
    )
    .await;
    let item = Element::builder("item", NS_ROSTER)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "bob@localhost",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), "Bob")
        .build();
    send_iq(
        alice,
        iq_request(
            true,
            "roster-set",
            None,
            Element::builder("query", NS_ROSTER).append(item).build(),
        ),
    )
    .await;
    let is_push = |stanza: &Element| {
        stanza.name() == "iq"
            && stanza.attr("type") == Some("set")
            && has_descendant(stanza, "item", NS_ROSTER)
    };
    collect_until(alice, "roster set result and push", |stanzas| {
        stanzas.iter().any(is_push)
            && stanzas.iter().any(|stanza| {
                stanza.attr("id") == Some("roster-set") && stanza.attr("type") == Some("result")
            })
    })
    .await;
    let roster = iq_result(
        alice,
        iq_request(
            false,
            "roster-get",
            None,
            Element::builder("query", NS_ROSTER).build(),
        ),
    )
    .await
    .expect("roster payload");
    assert!(
        roster
            .children()
            .any(|item| item.attr("jid") == Some("bob@localhost")),
        "the roster must hold the stored contact: {roster:?}"
    );
}

/// vCard set and get, then private XML storage written twice and read back.
async fn exercise_profile_storage(alice: &mut WsXmppClient) {
    let vcard = Element::builder("vCard", NS_VCARD)
        .append(text_element("FN", NS_VCARD, "Alice Restricted"))
        .build();
    iq_result(alice, iq_request(true, "vcard-set", None, vcard)).await;
    let stored = iq_result(
        alice,
        iq_request(
            false,
            "vcard-get",
            None,
            Element::builder("vCard", NS_VCARD).build(),
        ),
    )
    .await
    .expect("vCard payload");
    assert!(contains_text(&stored, "Alice Restricted"), "{stored:?}");

    // The second write replaces the first through the store's upsert.
    for (id, theme) in [("private-set-1", "light"), ("private-set-2", "dark")] {
        let prefs = Element::builder("prefs", NS_PREFS)
            .append(text_element("theme", NS_PREFS, theme))
            .build();
        iq_result(
            alice,
            iq_request(
                true,
                id,
                None,
                Element::builder("query", NS_PRIVATE).append(prefs).build(),
            ),
        )
        .await;
    }
    let private = iq_result(
        alice,
        iq_request(
            false,
            "private-get",
            None,
            Element::builder("query", NS_PRIVATE)
                .append(Element::builder("prefs", NS_PREFS).build())
                .build(),
        ),
    )
    .await
    .expect("private XML payload");
    assert!(
        contains_text(&private, "dark") && !contains_text(&private, "light"),
        "private storage must return the replaced element: {private:?}"
    );
}

/// Block, list and unblock one JID.
async fn exercise_blocking(alice: &mut WsXmppClient) {
    let item = || {
        Element::builder("item", NS_BLOCKING)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "spammer@localhost",
            )
            .build()
    };
    let blocklist = |id: &str| {
        iq_request(
            false,
            id,
            None,
            Element::builder("blocklist", NS_BLOCKING).build(),
        )
    };
    iq_result(
        alice,
        iq_request(
            true,
            "block",
            None,
            Element::builder("block", NS_BLOCKING)
                .append(item())
                .build(),
        ),
    )
    .await;
    let blocked = iq_result(alice, blocklist("blocklist-1"))
        .await
        .expect("blocklist payload");
    assert!(
        blocked
            .children()
            .any(|item| item.attr("jid") == Some("spammer@localhost")),
        "{blocked:?}"
    );
    iq_result(
        alice,
        iq_request(
            true,
            "unblock",
            None,
            Element::builder("unblock", NS_BLOCKING)
                .append(item())
                .build(),
        ),
    )
    .await;
    let unblocked = iq_result(alice, blocklist("blocklist-2"))
        .await
        .expect("blocklist payload");
    assert_eq!(unblocked.children().count(), 0, "{unblocked:?}");
}

/// The server owner creates a room, a second account joins, a groupchat
/// message is reflected, the room archive returns it, and the member leaves.
async fn exercise_muc(admin: &mut WsXmppClient, alice: &mut WsXmppClient) {
    let room = format!("restricted-{}@muc.localhost", Uuid::new_v4().simple());
    let joined = |stanzas: &[Element]| {
        stanzas.iter().any(|stanza| {
            stanza.name() == "message"
                && has_descendant(stanza, "subject", xmpp_parsers::ns::JABBER_CLIENT)
        })
    };
    send_element(admin, muc_presence(&format!("{room}/owner"), false)).await;
    collect_until(admin, "room creation", joined).await;
    send_element(alice, muc_presence(&format!("{room}/alice"), false)).await;
    collect_until(alice, "room join", joined).await;

    let body = format!("restricted-room-{}", Uuid::new_v4());
    send_element(
        admin,
        chat_message(&room, MessageType::Groupchat, "room-message", &body),
    )
    .await;
    collect_until(alice, "room message", |stanzas| {
        stanzas.iter().any(|stanza| contains_text(stanza, &body))
    })
    .await;
    let archived = mam_query(admin, "room-mam", Some(&room)).await;
    assert!(
        archived.iter().any(|stanza| contains_text(stanza, &body)),
        "the room archive must return the message: {archived:?}"
    );

    send_element(alice, muc_presence(&format!("{room}/alice"), true)).await;
    collect_until(alice, "room leave", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| stanza.name() == "presence" && stanza.attr("type") == Some("unavailable"))
    })
    .await;
}

/// Request an upload slot, register a push device, and enable then disable
/// push for its node.
async fn exercise_upload_and_push(alice: &mut WsXmppClient) {
    let request = Element::builder("request", NS_UPLOAD)
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "restricted.txt",
        )
        .attr(minidom::rxml::xml_ncname!("size").to_owned(), "12")
        .attr(
            minidom::rxml::xml_ncname!("content-type").to_owned(),
            "text/plain",
        )
        .build();
    let slot = iq_result(
        alice,
        iq_request(false, "upload-slot", Some("upload.localhost"), request),
    )
    .await
    .expect("upload slot payload");
    assert!(slot.is("slot", NS_UPLOAD), "{slot:?}");

    let node = register_push_device(alice).await;
    for (id, name) in [("push-enable", "enable"), ("push-disable", "disable")] {
        let payload = Element::builder(name, NS_PUSH)
            .attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                "push.localhost",
            )
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
            .build();
        iq_result(alice, iq_request(true, id, Some(DOMAIN), payload)).await;
    }
}

fn form_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(text_element("value", NS_DATA_FORMS, value))
        .build()
}

fn find_field_value(element: &Element, var: &str) -> Option<String> {
    if element.is("field", NS_DATA_FORMS) && element.attr("var") == Some(var) {
        return element.get_child("value", NS_DATA_FORMS).map(Element::text);
    }
    element
        .children()
        .find_map(|child| find_field_value(child, var))
}

/// Register a Web Push device with the Push Service through its ad-hoc
/// command and return the node the service assigned.
async fn register_push_device(client: &mut WsXmppClient) -> String {
    let command = |action: &str, session: Option<&str>| {
        let mut command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                "register-device",
            )
            .attr(minidom::rxml::xml_ncname!("action").to_owned(), action);
        if let Some(session) = session {
            command = command.attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), session);
        }
        command
    };
    let executing = iq_result(
        client,
        iq_request(
            true,
            "push-register-execute",
            Some("push.localhost"),
            command("execute", None).build(),
        ),
    )
    .await
    .expect("register-device session");
    let session = executing
        .attr("sessionid")
        .expect("register-device session ID")
        .to_owned();
    let mut form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    for (var, value) in [
        (
            "FORM_TYPE",
            "urn:waddle:push-service:commands:register-device:0",
        ),
        ("platform", "web"),
        ("environment", "prod"),
        ("app-id", "web"),
        (
            "web-push-endpoint",
            "https://push.example.test/restricted-runtime",
        ),
        ("web-push-p256dh", "restricted-p256-key"),
        ("web-push-auth", "restricted-auth-secret"),
    ] {
        form = form.append(form_field(var, value));
    }
    let completed = iq_result(
        client,
        iq_request(
            true,
            "push-register-complete",
            Some("push.localhost"),
            command("complete", Some(&session))
                .append(form.build())
                .build(),
        ),
    )
    .await
    .expect("register-device result");
    assert_eq!(completed.attr("status"), Some("completed"), "{completed:?}");
    find_field_value(&completed, "node").expect("registered push node")
}

/// Enable XEP-0198 with resumption, have a stanza acknowledged, lose the
/// transport without closing the stream, and resume on a new one.
async fn exercise_stream_management(server: &TestServer) {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("stream management socket");
    client
        .authenticate(DOMAIN, "bob", "bob-password")
        .await
        .expect("authenticate");
    client.bind("tablet").await.expect("bind");
    send_element(
        &mut client,
        Element::builder("enable", SM_NS)
            .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
            .build(),
    )
    .await;
    let enabled = collect_until(&mut client, "stream management enabled", |stanzas| {
        stanzas.iter().any(|stanza| stanza.is("enabled", SM_NS))
    })
    .await
    .into_iter()
    .find(|stanza| stanza.is("enabled", SM_NS))
    .expect("enabled");
    let stream = enabled.attr("id").expect("resumable stream").to_owned();

    send_iq(&mut client, Iq::from_get("sm-ping", Ping)).await;
    send_element(&mut client, Element::builder("r", SM_NS).build()).await;
    let seen = collect_until(&mut client, "ping result and acknowledgement", |stanzas| {
        stanzas.iter().any(|stanza| stanza.is("a", SM_NS))
            && stanzas
                .iter()
                .any(|stanza| stanza.attr("id") == Some("sm-ping"))
    })
    .await;
    let acknowledged = seen
        .iter()
        .find(|stanza| stanza.is("a", SM_NS))
        .and_then(|ack| ack.attr("h"));
    assert_eq!(acknowledged, Some("1"), "the server handled one stanza");
    let handled = seen
        .iter()
        .filter(|stanza| matches!(stanza.name(), "iq" | "message" | "presence"))
        .count();
    // Dropping the socket ends the transport without a stream close.
    drop(client);

    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("resumption socket");
    resumed
        .authenticate(DOMAIN, "bob", "bob-password")
        .await
        .expect("authenticate for resumption");
    send_element(
        &mut resumed,
        Element::builder("resume", SM_NS)
            .attr(
                minidom::rxml::xml_ncname!("previd").to_owned(),
                stream.as_str(),
            )
            .attr(
                minidom::rxml::xml_ncname!("h").to_owned(),
                handled.to_string(),
            )
            .build(),
    )
    .await;
    let response = collect_until(&mut resumed, "resumption", |stanzas| {
        stanzas
            .iter()
            .any(|stanza| stanza.is("resumed", SM_NS) || stanza.is("failed", SM_NS))
    })
    .await;
    assert!(
        response
            .iter()
            .any(|stanza| stanza.is("resumed", SM_NS) && stanza.attr("previd") == Some(&stream)),
        "the stream must resume on the same node: {response:?}"
    );
    resumed.close().await.expect("close resumed stream");
}

/// The real server under the runtime login. A dangling roster row is seeded
/// first, so startup also runs its roster prune. The session then covers
/// login and bind, a direct message, roster set/get with its push, vCard,
/// private XML storage, the block list, a room created, joined, written to,
/// read back from its archive and left, a personal archive query, an upload
/// slot, push enable/disable, and XEP-0198 enable, acknowledgement and
/// same-node resumption.
///
/// Three witnesses show the runtime ran no DDL and was never refused a
/// privilege: the catalog list is unchanged, the role owns nothing, and the
/// relay in front of PostgreSQL saw neither a catalog-changing statement nor
/// an `insufficient_privilege` error on any runtime connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restricted_runtime_role_serves_without_ddl() {
    let Some(url) = require_test_postgres_url() else {
        return;
    };
    let prepared = Prepared::new(&url).await;
    // An upgraded database can hold a roster item whose contact is not a
    // local account. Startup prunes it together with the owner's version.
    for statement in [
        format!(
            "INSERT INTO {SCHEMA}.roster_items (user_jid, contact_jid) \
             VALUES ('alice@localhost', 'ghost@elsewhere.example')"
        ),
        format!(
            "INSERT INTO {SCHEMA}.roster_versions (user_jid, version) \
             VALUES ('alice@localhost', 'stale')"
        ),
    ] {
        execute(&prepared.admin, &statement).await;
    }
    let before = catalog_entries(&prepared.admin).await;
    prepared.endpoint.reset_wire();
    let env = prepared.runtime_env();
    let server = tokio::task::spawn_blocking(move || {
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect();
        TestServer::start_with_extra_envs(
            &[("alice", "alice-password"), ("bob", "bob-password")],
            &env,
        )
    })
    .await
    .expect("restricted runtime server starts");

    assert_eq!(
        probe(&format!("{}/healthz", server.http_base_url())).await,
        reqwest::StatusCode::OK
    );
    assert_eq!(
        probe(&format!("{}/readyz", server.http_base_url())).await,
        reqwest::StatusCode::OK,
        "the restricted runtime must report ready"
    );
    let dangling: i64 = sqlx::query_scalar(&format!(
        "SELECT (SELECT pg_catalog.count(*) FROM {SCHEMA}.roster_items \
                  WHERE contact_jid = 'ghost@elsewhere.example') \
              + (SELECT pg_catalog.count(*) FROM {SCHEMA}.roster_versions \
                  WHERE version = 'stale')"
    ))
    .fetch_one(&prepared.admin)
    .await
    .expect("count dangling roster rows");
    assert_eq!(dangling, 0, "startup must prune the dangling roster rows");

    let connect = |user: &'static str, password: String, resource: &'static str| {
        let url = server.ws_url();
        async move {
            WsXmppClient::connect_and_auth(&url, DOMAIN, user, &password, resource)
                .await
                .unwrap_or_else(|error| panic!("{user} authenticates and binds: {error}"))
        }
    };
    let mut alice = connect("alice", "alice-password".to_owned(), "laptop").await;
    let mut bob = connect("bob", "bob-password".to_owned(), "phone").await;
    let mut admin = connect(
        "admin",
        server.fixed_account_password().to_owned(),
        "console",
    )
    .await;

    send_element(
        &mut bob,
        Element::builder("presence", xmpp_parsers::ns::JABBER_CLIENT).build(),
    )
    .await;
    let body = format!("restricted-runtime-{}", Uuid::new_v4());
    send_element(
        &mut alice,
        chat_message("bob@localhost", MessageType::Chat, "direct-message", &body),
    )
    .await;
    collect_until(&mut bob, "direct message", |stanzas| {
        stanzas.iter().any(|stanza| contains_text(stanza, &body))
    })
    .await;

    exercise_roster(&mut alice).await;
    exercise_profile_storage(&mut alice).await;
    exercise_blocking(&mut alice).await;
    exercise_muc(&mut admin, &mut alice).await;
    let personal = mam_query(&mut alice, "personal-mam", None).await;
    assert!(
        personal.iter().any(|stanza| contains_text(stanza, &body)),
        "the personal archive must return the direct message: {personal:?}"
    );
    exercise_upload_and_push(&mut alice).await;
    exercise_stream_management(&server).await;

    alice.close().await.expect("alice closes");
    bob.close().await.expect("bob closes");
    admin.close().await.expect("admin closes");

    assert_catalog_unchanged(
        &prepared.admin,
        &before,
        "the restricted runtime changed the catalog",
    )
    .await;
    assert_runtime_owns_nothing(&prepared).await;
    let wire = prepared.endpoint.take_wire();
    assert!(
        wire.catalog_statements.is_empty(),
        "the runtime login sent catalog-changing statements: {:#?}",
        wire.catalog_statements
    );
    let refused: Vec<_> = wire
        .errors
        .iter()
        .filter(|(sqlstate, _)| sqlstate == "42501")
        .collect();
    assert!(
        refused.is_empty(),
        "the runtime login was refused a privilege: {refused:#?}"
    );

    drop(server);
    prepared.cleanup().await;
}
